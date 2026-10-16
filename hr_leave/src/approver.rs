//! Leave approvers per department and leave periods.
//!
//! A person with a manager is decided on by that manager. A person with none is decided on by the approvers
//! named for their department (or the nearest department above it that has any); with none of those, HR.

use aether_sdk::dates::{format_date, parse_date};
use aether_sdk::db::Filter;
use aether_sdk::prelude::*;

use crate::common::{employee, id_of, my_employee, require, require_admin, text, Record};

#[derive(Deserialize)]
struct SetApprover {
    department: String,
    approver: String,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct NewPeriod {
    name: String,
    start_date: String,
    end_date: String,
}

/// The approvers of a person's department, or of the nearest department above it with any.
pub fn department_approvers(person: &Record) -> Result<Vec<String>> {
    let mut department = text(person, "department").map(str::to_string);
    for _ in 0..12 {
        let Some(id) = department else { return Ok(Vec::new()) };
        let rows: Vec<Record> = db::find("leave_approver").filter("department", id.as_str()).limit(50).all()?;
        let own = id_of(person)?;
        let found: Vec<String> = rows.iter().filter_map(|r| text(r, "approver")).filter(|a| *a != own).map(str::to_string).collect();
        if !found.is_empty() {
            return Ok(found);
        }
        let row: Option<Record> = plugins::call("hr", "get_department", &json!({ "id": id }))?;
        department = row.and_then(|d| text(&d, "parent").map(str::to_string));
    }
    Ok(Vec::new())
}

/// Who decides at the manager step for this person: their manager, else their department's approvers.
pub fn deciders(person: &Record) -> Result<Vec<String>> {
    if let Some(manager) = text(person, "manager") {
        return Ok(vec![manager.to_string()]);
    }
    department_approvers(person)
}

/// People without a manager whom the caller approves for through their department.
pub fn people_i_approve_for() -> Result<Vec<String>> {
    let Some(me) = my_employee()? else { return Ok(Vec::new()) };
    let rows: Vec<Record> = db::find("leave_approver").filter("approver", id_of(&me)?).limit(100).all()?;
    let mut out = Vec::new();
    for row in rows {
        let Some(department) = text(&row, "department") else { continue };
        let people: Vec<Record> = plugins::call("hr", "list_employees", &json!({ "department": department, "with_subdepartments": true, "limit": 500 }))?;
        for person in people {
            if text(&person, "manager").is_none() && department_approvers(&person)?.iter().any(|a| Some(a.as_str()) == text(&me, "id")) {
                if let Some(id) = text(&person, "id") {
                    out.push(id.to_string());
                }
            }
        }
    }
    Ok(out)
}

fn set_approver(input: SetApprover) -> Result<Record> {
    require_admin()?;
    employee(&input.approver)?;
    let department: Option<Record> = plugins::call("hr", "get_department", &json!({ "id": input.department }))?;
    if department.is_none() {
        return Err(Error::msg("there is no such department"));
    }
    if db::count("leave_approver", Filter::eq("department", input.department.as_str()).and(Filter::eq("approver", input.approver.as_str())))? > 0 {
        return Err(Error::msg("that person already approves for this department"));
    }
    db::create("leave_approver", &json!({ "department": input.department, "approver": input.approver }))
}

fn new_period(input: NewPeriod) -> Result<Record> {
    require_admin()?;
    let (from, to) = (parse_date(&input.start_date)?, parse_date(&input.end_date)?);
    if to < from {
        return Err(Error::msg("the period ends after it starts"));
    }
    for other in db::find::<Record>("leave_period").filter("is_active", true).limit(200).all()? {
        let (of, ot) = (parse_date(text(&other, "start_date").unwrap_or_default())?, parse_date(text(&other, "end_date").unwrap_or_default())?);
        if from <= ot && of <= to {
            return Err(Error::msg(format!("it overlaps the period `{}`", text(&other, "name").unwrap_or_default())));
        }
    }
    db::create("leave_period", &json!({ "name": input.name, "start_date": format_date(from), "end_date": format_date(to), "is_active": true }))
}

/// A period's dates.
pub fn period_dates(id: &str) -> Result<(String, String)> {
    let row = require("leave_period", id, "leave period")?;
    Ok((text(&row, "start_date").unwrap_or_default().to_string(), text(&row, "end_date").unwrap_or_default().to_string()))
}

handler! {
    fn set_leave_approver(input: SetApprover) -> Record {
        set_approver(input)
    }

    fn remove_leave_approver(input: Id) -> Option<Record> {
        require_admin()?;
        db::delete("leave_approver", &input.id)
    }

    fn list_leave_approvers(_: Empty) -> Vec<Record> {
        db::find("leave_approver").order_by("department").limit(500).all()
    }

    fn create_leave_period(input: NewPeriod) -> Record {
        new_period(input)
    }

    fn list_leave_periods(_: Empty) -> Vec<Record> {
        db::find("leave_period").order_by("-start_date").limit(100).all()
    }

    fn close_leave_period(input: Id) -> Record {
        require_admin()?;
        db::update("leave_period", &input.id, &json!({ "is_active": false }))?.ok_or_else(|| Error::msg("there is no such period"))
    }
}
