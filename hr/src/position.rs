//! Positions: seats in the organization, each with a headcount.

use aether_sdk::db::Filter;
use aether_sdk::prelude::*;
use crate::rules::status;

use crate::common::{explain, id_of, pick, require, text, Record};

const EDITABLE: &[&str] = &["name", "code", "job", "department", "parent", "headcount", "status", "grade", "notes", "base_wage", "currency"];

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Change {
    id: String,
    /// With a new base wage: also move the people who hold the position onto it, as new terms from today.
    #[serde(default)]
    apply_to_holders: bool,
    #[serde(flatten)]
    fields: Record,
}

#[derive(Deserialize)]
struct Vacancies {
    #[serde(default)]
    department: Option<String>,
}

/// A position as shown to someone who may not see its pay: HR's own code reads the wage, only an HR manager is shown it.
fn without_pay(mut position: Record) -> Record {
    if !context::current().is_ok_and(|who| who.has_role("hr.hr_manager")) {
        if let Some(fields) = position.as_object_mut() {
            fields.remove("base_wage");
            fields.remove("currency");
        }
    }
    position
}

/// People who hold the position now.
pub fn holders(position: &str) -> Result<u64> {
    let present: Vec<&str> = status::PRESENT.to_vec();
    db::count("employee", Filter::eq("position", position).and(Filter::one_of("status", present)))
}

fn check(data: &Record, current: Option<&Record>) -> Result<()> {
    if let Some(job) = text(data, "job") {
        require("job", job, "job")?;
    }
    if let Some(department) = text(data, "department") {
        let department = require("department", department, "department")?;
        if department.get("is_active") == Some(&json!(false)) {
            return Err(Error::msg("the department is closed"));
        }
    }
    if let Some(parent) = text(data, "parent") {
        if current.and_then(|c| text(c, "id")) == Some(parent) {
            return Err(Error::msg("a position cannot report to itself"));
        }
        require("position", parent, "position")?;
    }
    if let Some(headcount) = data.get("headcount") {
        let headcount = headcount.as_i64().ok_or_else(|| Error::msg("headcount is a whole number"))?;
        if headcount < 0 {
            return Err(Error::msg("headcount cannot be negative"));
        }
        if let Some(id) = current.and_then(|c| text(c, "id")) {
            let held = holders(id)?;
            if (headcount as u64) < held {
                return Err(Error::msg(format!("{held} people hold this position: the headcount cannot go below that")));
            }
        }
    }
    if let Some(state) = text(data, "status") {
        if !["open", "filled", "frozen"].contains(&state) {
            return Err(Error::msg("status is open, filled or frozen"));
        }
    }
    Ok(())
}

fn new_position(input: Record) -> Result<Record> {
    if ["base_wage", "currency"].iter().any(|f| input.get(*f).is_some_and(|v| !v.is_null())) {
        crate::common::require_pay_manager()?;
    }
    for required in ["name", "job", "department"] {
        if text(&input, required).is_none() {
            return Err(Error::msg(format!("a position needs its {required}")));
        }
    }
    let data = pick(&input, EDITABLE);
    check(&data, None)?;
    explain(db::create("position", &data), "could not create the position")
}

/// Used by Desk demo generation (`demo` module).
pub(crate) fn create_for_demo(input: Record) -> Result<Record> {
    new_position(input)
}

/// Used by Desk demo generation (`demo` module).
pub(crate) fn create_job_for_demo(input: Record) -> Result<Record> {
    if text(&input, "name").is_none() {
        return Err(Error::msg("a job needs a name"));
    }
    explain(
        db::create(
            "job",
            &crate::common::pick(&input, &["name", "code", "department", "description", "is_active"]),
        ),
        "could not create the job",
    )
}

fn change_position(input: Change) -> Result<Record> {
    let existing = require("position", &input.id, "position")?;
    let data = pick(&input.fields, EDITABLE);
    if data.as_object().is_some_and(|fields| fields.is_empty()) {
        return Err(Error::msg("there is nothing to change"));
    }
    check(&data, Some(&existing))?;
    let pay_changed = ["base_wage", "currency"].iter().any(|f| data.get(*f).is_some_and(|v| Some(v) != existing.get(*f)));
    if pay_changed || input.apply_to_holders {
        crate::common::require_pay_manager()?;
    }
    if input.apply_to_holders {
        // Applying is asked for on its own too (the wage may already be saved), so it does not depend on the wage changing.
        if crate::employment::pay_mode()? == "individual" {
            return Err(Error::msg(
                "pay is entered for each person: set the pay mode in HR defaults to by position or hybrid to apply a position's wage to the people in it",
            ));
        }
        let has_wage = data.get("base_wage").or_else(|| existing.get("base_wage")).is_some_and(|w| !w.is_null() && w.as_str() != Some(""));
        if !has_wage {
            return Err(Error::msg("this position has no base wage to apply: enter one first"));
        }
    }
    let updated = explain(db::update::<Record>("position", id_of(&existing)?, &data), "could not change the position")?
        .ok_or_else(|| Error::msg("the position is gone"))?;
    let mut updated = updated;
    if input.apply_to_holders {
        let moved = crate::employment::apply_position_pay(&updated)?;
        updated["saved_message"] = json!(match moved {
            0 => "Saved. No one was moved: nobody holds this position, or everyone here has their own wage.".to_string(),
            1 => "Saved. 1 person moved onto the position's wage from today.".to_string(),
            n => format!("Saved. {n} people moved onto the position's wage from today."),
        });
    }
    Ok(updated)
}

/// Positions with free seats, how many are held and how many are free.
fn open_positions(input: Vacancies) -> Result<Vec<Record>> {
    let mut filter = Filter::ne("status", "frozen");
    if let Some(department) = input.department {
        filter = filter.and(Filter::eq("department", department));
    }
    let positions: Vec<Record> = db::find::<Record>("position").matching(filter).order_by("name").limit(1000).all()?;
    let mut out = Vec::new();
    for mut position in positions {
        let held = holders(id_of(&position)?)?;
        let headcount = position.get("headcount").and_then(Value::as_u64).unwrap_or(1);
        if headcount > held {
            position["held"] = json!(held);
            position["free"] = json!(headcount - held);
            out.push(without_pay(position));
        }
    }
    Ok(out)
}

/// A position's seats and how many are held: for planning hires.
fn seats(input: Id) -> Result<Value> {
    let position = require("position", &input.id, "position")?;
    let headcount = position.get("headcount").and_then(Value::as_u64).unwrap_or(1);
    let held = holders(&input.id)?;
    Ok(json!({ "position": input.id, "headcount": headcount, "held": held, "free": headcount.saturating_sub(held), "frozen": text(&position, "status") == Some("frozen") }))
}

handler! {
    /// The seats of a position and how many are held.
    fn position_seats(input: Id) -> Value {
        seats(input)
    }

    fn create_position(input: Record) -> Record {
        new_position(input)
    }

    fn update_position(input: Change) -> Record {
        change_position(input)
    }

    fn get_position(input: Id) -> Option<Record> {
        Ok(db::get::<Record>("position", &input.id)?.map(without_pay))
    }

    fn list_positions(_: Empty) -> Vec<Record> {
        Ok(db::find::<Record>("position").order_by("name").limit(1000).all()?.into_iter().map(without_pay).collect())
    }

    /// Positions that still have a free seat.
    fn vacancies(input: Vacancies) -> Vec<Record> {
        open_positions(input)
    }

    fn create_job(input: Record) -> Record {
        if text(&input, "name").is_none() {
            return Err(Error::msg("a job needs a name"));
        }
        explain(db::create("job", &crate::common::pick(&input, &["name", "code", "department", "description", "is_active"])), "could not create the job")
    }

    fn get_job(input: Id) -> Option<Record> {
        db::get("job", &input.id)
    }

    fn list_jobs(_: Empty) -> Vec<Record> {
        db::find("job").order_by("name").limit(1000).all()
    }
}
