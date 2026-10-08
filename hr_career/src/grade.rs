//! The grade ladder: each grade has a rank and, optionally, a band the wage must stay inside.

use aether_sdk::prelude::*;

use crate::common::{decimal_of, pick, require, require_admin, text, Record};

const FIELDS: &[&str] = &["code", "name", "rank", "min_wage", "max_wage", "is_active"];

#[derive(Deserialize)]
struct Change {
    id: String,
    #[serde(flatten)]
    fields: Record,
}

fn check(data: &Record) -> Result<()> {
    if let (Some(min), Some(max)) = (decimal_of(data, "min_wage")?, decimal_of(data, "max_wage")?) {
        if min > max {
            return Err(Error::msg("the lowest wage of a grade is above its highest"));
        }
    }
    for field in ["min_wage", "max_wage"] {
        if decimal_of(data, field)?.is_some_and(|w| w.is_negative()) {
            return Err(Error::msg(format!("{field} cannot be negative")));
        }
    }
    Ok(())
}

/// The grade with this code.
pub fn by_code(code: &str) -> Result<Option<Record>> {
    db::find::<Record>("career_grade").filter("code", code).first()
}

fn new_grade(input: Record) -> Result<Record> {
    require_admin()?;
    let data = pick(&input, FIELDS);
    if text(&data, "code").is_none() || text(&data, "name").is_none() || data.get("rank").and_then(Value::as_i64).is_none() {
        return Err(Error::msg("a grade needs a code, a name and a rank"));
    }
    check(&data)?;
    db::create("career_grade", &data).map_err(|e| e.or("could not create the grade (the code may be taken)"))
}

fn change_grade(input: Change) -> Result<Record> {
    require_admin()?;
    let existing = require("career_grade", &input.id, "grade")?;
    let data = pick(&input.fields, FIELDS);
    let mut merged = existing.clone();
    for (key, value) in data.as_object().cloned().unwrap_or_default() {
        merged[key] = value;
    }
    check(&merged)?;
    // People already on the grade keep their wage: a narrower band only applies to future changes.
    db::update::<Record>("career_grade", &input.id, &data)?.ok_or_else(|| Error::msg("the grade is gone"))
}

handler! {
    fn create_grade(input: Record) -> Record {
        new_grade(input)
    }

    fn update_grade(input: Change) -> Record {
        change_grade(input)
    }

    /// The ladder, lowest rank first.
    fn list_grades(_: Empty) -> Vec<Record> {
        db::find("career_grade").order_by("rank").limit(200).all()
    }
}
