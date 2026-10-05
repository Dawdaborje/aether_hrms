//! Positions: seats in the organization, each with a headcount.

use aether_sdk::db::Filter;
use aether_sdk::prelude::*;
use crate::rules::status;

use crate::common::{explain, id_of, pick, require, text, Record};

const EDITABLE: &[&str] = &["name", "code", "job", "department", "parent", "headcount", "status", "grade", "notes"];

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Change {
    id: String,
    #[serde(flatten)]
    fields: Record,
}

#[derive(Deserialize)]
struct Vacancies {
    #[serde(default)]
    department: Option<String>,
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
    for required in ["name", "job", "department"] {
        if text(&input, required).is_none() {
            return Err(Error::msg(format!("a position needs its {required}")));
        }
    }
    let data = pick(&input, EDITABLE);
    check(&data, None)?;
    explain(db::create("position", &data), "could not create the position")
}

fn change_position(input: Change) -> Result<Record> {
    let existing = require("position", &input.id, "position")?;
    let data = pick(&input.fields, EDITABLE);
    if data.as_object().is_some_and(|fields| fields.is_empty()) {
        return Err(Error::msg("there is nothing to change"));
    }
    check(&data, Some(&existing))?;
    explain(db::update::<Record>("position", id_of(&existing)?, &data), "could not change the position")?
        .ok_or_else(|| Error::msg("the position is gone"))
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
            out.push(position);
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
        db::get("position", &input.id)
    }

    fn list_positions(_: Empty) -> Vec<Record> {
        db::find("position").order_by("name").limit(1000).all()
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
