//! The new hire's side. Anyone holding a valid token may read their own enrollment and submit it; nothing else.

use aether_sdk::prelude::*;

use crate::common::{id_of, now_text, text, Record};
use crate::{rules, token};

#[derive(Deserialize)]
struct Token {
    token: String,
}

fn not_valid() -> Error {
    // One answer for every failure, so a guess learns nothing about which links exist.
    Error::msg("this enrollment link is not valid or has expired")
}

/// The enrollment this token belongs to, while its link is still good.
pub(crate) fn by_token(secret: &str) -> Result<Record> {
    let secret = secret.trim();
    // The reason goes to the server log only; the caller always gets the same answer.
    if secret.len() < 40 || secret.len() > 128 {
        log::warn(format!("enrollment link refused: the token is {} characters long", secret.len()));
        return Err(not_valid());
    }
    let hash = token::hash(secret);
    let Some(row) = db::find::<Record>("enrollment").filter("token_hash", hash.as_str()).first()? else {
        log::warn("enrollment link refused: no enrollment has this token");
        return Err(not_valid());
    };
    let status = text(&row, "status").unwrap_or("");
    let now = now_text()?;
    if !["invited", "in_progress", "changes_requested", "submitted"].contains(&status) {
        log::warn(format!("enrollment link refused: the enrollment is {status}"));
        return Err(not_valid());
    }
    if rules::expired(text(&row, "expires_at"), &now) {
        log::warn(format!("enrollment link refused: expired at {:?}, now {now}", text(&row, "expires_at")));
        return Err(not_valid());
    }
    Ok(row)
}

/// Where a person lands once their details are in.
fn done_page(token: &str) -> String {
    format!("/enroll/{}/done", token.trim())
}

/// The minimum working age this link was issued under.
fn minimum_age_of(row: &Record) -> u32 {
    row.get("minimum_age").and_then(Value::as_u64).filter(|a| *a > 0).map_or(rules::DEFAULT_MINIMUM_AGE, |a| a as u32)
}

fn read(input: Token) -> Result<Record> {
    let row = by_token(&input.token)?;
    let id = id_of(&row)?.to_string();
    let status = text(&row, "status").unwrap_or("").to_string();
    // Already sent in: the form is closed, the visitor sees the thank-you page.
    if status == "submitted" {
        return Ok(json!({ "open_page": done_page(&input.token) }));
    }
    if status == "invited" {
        db::update::<Record>("enrollment", &id, &json!({ "status": "in_progress", "opened_at": now_text()? }))?;
    }
    let mut view = json!({
        "name": format!("{} {}", text(&row, "first_name").unwrap_or(""), text(&row, "last_name").unwrap_or("")).trim(),
        "email": row["email"],
        "hire_date": row["hire_date"],
        "review_note": row["review_note"],
        "notice": "",
    });
    // The birth dates the date picker may offer: not in the future, not so recent that the person would be
    // under the minimum age on the day they join, and not more than 100 years ago.
    if let Ok((earliest, latest)) = rules::birth_window(text(&row, "hire_date"), &now_text()?[..10], minimum_age_of(&row)) {
        view["birth_min"] = json!(earliest.to_string());
        view["birth_max"] = json!(latest.to_string());
    }
    let sections: Vec<Record> = db::find("enrollment_section").filter("enrollment", id.as_str()).limit(20).all()?;
    let mut again = Vec::new();
    for section in sections {
        if text(&section, "status") == Some("changes_requested") {
            again.push(text(&section, "kind").unwrap_or("").to_string());
        }
        if let (Some(data), Some(fields)) = (section.get("data").and_then(Value::as_object), view.as_object_mut()) {
            for (key, value) in data {
                fields.insert(key.clone(), value.clone());
            }
        }
    }
    view["send_back"] = json!(again.join(", "));
    Ok(view)
}

fn submit(input: Record) -> Result<Record> {
    // A bot gets the same answer as a person and nothing is saved.
    if text(&input, "website").is_some_and(|w| !w.trim().is_empty()) {
        return Ok(json!({ "ok": true, "open_page": done_page(text(&input, "token").unwrap_or_default()) }));
    }
    let row = by_token(text(&input, "token").unwrap_or_default())?;
    let status = text(&row, "status").unwrap_or("");
    if !rules::OPEN_FOR_HIRE.contains(&status) {
        return Err(Error::msg("you have already submitted this form"));
    }
    let (minimum, today) = (minimum_age_of(&row), now_text()?[..10].to_string());
    rules::check_submission(&input, text(&row, "hire_date"), &today, minimum).map_err(Error::msg)?;
    let id = id_of(&row)?.to_string();
    let moment = now_text()?;
    for (kind, _) in rules::SECTIONS {
        let mut data = rules::section_data(kind, &input);
        if *kind == "acknowledgement" {
            data["acknowledged_at"] = json!(moment);
        }
        let existing: Option<Record> = db::find("enrollment_section").filter("enrollment", id.as_str()).filter("kind", *kind).first()?;
        match existing {
            Some(section) => {
                db::update::<Record>("enrollment_section", id_of(&section)?, &json!({ "data": data, "status": "submitted", "comment": null }))?;
            }
            None if data.as_object().is_some_and(|d| d.is_empty()) => {}
            None => {
                db::create::<Record>("enrollment_section", &json!({ "enrollment": id, "kind": kind, "data": data, "status": "submitted" }))?;
            }
        }
    }
    // The link is spent: it stops working now. If HR sends something back, they send a new link.
    let mut update = json!({ "status": "submitted", "submitted_at": moment, "acknowledged_at": moment, "review_note": null, "token_hash": null });
    if let Some(files) = input.get("documents").filter(|f| !f.is_null() && f.to_string().len() < 200_000) {
        update["files"] = files.clone();
    }
    db::update::<Record>("enrollment", &id, &update)?;
    events::emit("enrollment_submitted", &json!({ "enrollment": id }))?;
    Ok(json!({ "ok": true, "open_page": done_page(text(&input, "token").unwrap_or_default()) }))
}

handler! {
    /// The form's data, open to anyone holding the link.
    fn enroll_get(input: Token) -> Record {
        read(input)
    }

    fn enroll_submit(input: Record) -> Record {
        submit(input)
    }
}
