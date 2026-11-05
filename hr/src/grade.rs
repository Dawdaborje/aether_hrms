//! The grade ladder: each grade has a rank and, optionally, a band the wage must stay inside. Maintained in HR
//! Configuration and picked on positions; terms keep the grade's code, so a renamed grade changes nowhere else.

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{explain, pick, require, text, Record};

const FIELDS: &[&str] = &["code", "name", "rank", "min_wage", "max_wage", "is_active"];

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Code {
    code: String,
}

#[derive(Deserialize)]
struct Change {
    id: String,
    #[serde(flatten)]
    fields: Record,
}

fn require_hr() -> Result<()> {
    let who = context::current()?;
    if who.has_role("hr.hr_user") || who.has_role("hr.hr_manager") {
        Ok(())
    } else {
        Err(Error::msg("only HR can change the grades"))
    }
}

fn wage_of(data: &Record, field: &str) -> Result<Option<Decimal>> {
    match data.get(field).filter(|v| !v.is_null() && v.as_str() != Some("")) {
        Some(value) => serde_json::from_value(value.clone()).map(Some).map_err(|e| Error::msg(format!("{field}: {e}"))),
        None => Ok(None),
    }
}

fn check(data: &Record) -> Result<()> {
    let (min, max) = (wage_of(data, "min_wage")?, wage_of(data, "max_wage")?);
    if let (Some(min), Some(max)) = (&min, &max) {
        if min > max {
            return Err(Error::msg("the lowest wage of a grade is above its highest"));
        }
    }
    if min.iter().chain(max.iter()).any(|w| w.is_negative()) {
        return Err(Error::msg("a wage band cannot be negative"));
    }
    Ok(())
}

/// The grade with this code.
fn by_code(code: &str) -> Result<Option<Record>> {
    db::find::<Record>("grade").filter("code", code).first()
}

fn new_grade(input: Record) -> Result<Record> {
    require_hr()?;
    let data = pick(&input, FIELDS);
    if text(&data, "code").is_none() || text(&data, "name").is_none() || data.get("rank").and_then(Value::as_i64).is_none() {
        return Err(Error::msg("a grade needs a code, a name and a rank"));
    }
    check(&data)?;
    explain(db::create("grade", &data), "could not create the grade (the code may be taken)")
}

fn change_grade(input: Change) -> Result<Record> {
    require_hr()?;
    let existing = require("grade", &input.id, "grade")?;
    let data = pick(&input.fields, FIELDS);
    let mut merged = existing.clone();
    for (key, value) in data.as_object().cloned().unwrap_or_default() {
        merged[key] = value;
    }
    check(&merged)?;
    // People already on the grade keep their wage: a narrower band only applies to future changes.
    db::update::<Record>("grade", &input.id, &data)?.ok_or_else(|| Error::msg("the grade is gone"))
}

fn ladder(all: bool) -> Result<Vec<Record>> {
    let rows: Vec<Record> = db::find("grade").order_by("rank").limit(500).all()?;
    Ok(if all { rows } else { rows.into_iter().filter(|r| r.get("is_active") != Some(&json!(false))).collect() })
}

/// Make a grade for each grade written as free text on positions and employment, ranked in the order met.
/// Idempotent: a code that already has a grade is left alone.
fn run_migrate_grades() -> Result<Value> {
    let mut codes: Vec<String> = Vec::new();
    for model in ["position", "employment"] {
        for row in db::find::<Record>(model).limit(5000).all()? {
            if let Some(code) = text(&row, "grade").map(str::trim).filter(|c| !c.is_empty()) {
                if !codes.iter().any(|c| c == code) {
                    codes.push(code.to_string());
                }
            }
        }
    }
    let mut next = ladder(true)?.iter().filter_map(|g| g.get("rank").and_then(Value::as_i64)).max().unwrap_or(0);
    let mut made = 0;
    for code in codes {
        if by_code(&code)?.is_some() {
            continue;
        }
        next += 1;
        db::create::<Record>("grade", &json!({ "code": code, "name": code, "rank": next, "is_active": true }))?;
        made += 1;
    }
    Ok(json!({ "grades": made }))
}

handler! {
    fn create_grade(input: Record) -> Record {
        new_grade(input)
    }

    fn update_grade(input: Change) -> Record {
        change_grade(input)
    }

    fn get_grade(input: Id) -> Option<Record> {
        db::get("grade", &input.id)
    }

    /// The grade with this code: for plugins that keep the code on their records.
    fn grade_by_code(input: Code) -> Option<Record> {
        by_code(&input.code)
    }

    /// The ladder, lowest rank first, active grades only.
    fn list_grades(_: Empty) -> Vec<Record> {
        ladder(false)
    }

    fn list_all_grades(_: Empty) -> Vec<Record> {
        require_hr()?;
        ladder(true)
    }

    /// For a picker: the code is the `id`, because terms keep the code of the grade.
    fn list_grade_options(_: Empty) -> Vec<Value> {
        Ok(ladder(false)?
            .iter()
            .filter_map(|g| {
                let code = text(g, "code")?;
                Some(json!({ "id": code, "name": format!("{code} · {}", text(g, "name").unwrap_or(code)) }))
            })
            .collect())
    }

    fn migrate_grades(_: Empty) -> Value {
        run_migrate_grades()
    }
}
