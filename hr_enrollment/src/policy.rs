//! The policies a new hire reads before accepting. HR keeps them (HR Configuration); the enrollment form shows
//! the shown ones in a dialog.

use aether_sdk::prelude::*;

use crate::common::{id_of, pick, require_hr, text, Empty, Id, Record};
use crate::portal;

const FIELDS: &[&str] = &["title", "body", "position", "is_active"];

#[derive(Deserialize)]
struct Token {
    token: String,
}

#[derive(Deserialize)]
struct Change {
    id: String,
    #[serde(flatten)]
    fields: Record,
}

fn trimmed(mut data: Record) -> Result<Record> {
    if let Some(title) = text(&data, "title") {
        data["title"] = json!(title.trim());
    }
    if let Some(fields) = data.as_object_mut() {
        if fields.get("body").is_some_and(Value::is_null) {
            fields.insert("body".into(), json!(""));
        }
        if let Some(position) = fields.get("position").and_then(Value::as_str).and_then(|s| s.trim().parse::<i64>().ok()) {
            fields.insert("position".into(), json!(position));
        }
    }
    Ok(data)
}

fn create(input: Record) -> Result<Record> {
    require_hr()?;
    let data = trimmed(pick(&input, FIELDS))?;
    if text(&data, "title").is_none() {
        return Err(Error::msg("give the policy a title"));
    }
    db::create("enr_policy", &data)
}

fn update(input: Change) -> Result<Record> {
    require_hr()?;
    let data = trimmed(pick(&input.fields, FIELDS))?;
    if data.get("title").is_some() && text(&data, "title").is_none() {
        return Err(Error::msg("a policy needs a title"));
    }
    db::update("enr_policy", &input.id, &data)?.ok_or_else(|| Error::msg("there is no such policy"))
}

/// What the new hire reads: every shown policy, in order, as one text. Only for someone holding a good link.
fn for_link(input: Token) -> Result<Record> {
    portal::by_token(&input.token)?;
    let rows: Vec<Record> = db::find("enr_policy").filter("is_active", true).order_by("position").limit(100).all()?;
    let mut parts = Vec::new();
    for row in &rows {
        let title = text(row, "title").unwrap_or("");
        let body = text(row, "body").unwrap_or("");
        parts.push(if body.is_empty() { title.to_string() } else { format!("{title}\n\n{body}") });
    }
    let policies = if parts.is_empty() {
        "No policies have been published yet.".to_string()
    } else {
        parts.join("\n\n--------------------\n\n")
    };
    Ok(json!({ "policies": policies, "count": rows.len() }))
}

handler! {
    fn list_enrollment_policies(_: Empty) -> Vec<Record> {
        require_hr()?;
        db::find("enr_policy").order_by("position").limit(200).all()
    }

    fn get_enrollment_policy(input: Id) -> Option<Record> {
        require_hr()?;
        db::get("enr_policy", &input.id)
    }

    fn create_enrollment_policy(input: Record) -> Record {
        create(input)
    }

    fn update_enrollment_policy(input: Change) -> Record {
        update(input)
    }

    fn delete_enrollment_policy(input: Id) -> Record {
        require_hr()?;
        let row = db::get::<Record>("enr_policy", &input.id)?.ok_or_else(|| Error::msg("there is no such policy"))?;
        db::delete::<Record>("enr_policy", id_of(&row)?)?;
        Ok(json!({ "ok": true }))
    }

    /// The policies for the person holding this enrollment link.
    fn enroll_policies(input: Token) -> Record {
        for_link(input)
    }
}
