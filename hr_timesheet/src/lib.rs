//! Timesheets: hours against projects, approved by the manager, summarised by project and person.
//!
//! A person logs hours for a day (at most 24 in total for the day) and submits them; their manager (or a
//! timesheet administrator) approves or rejects. An approved entry no longer changes. Cost per hour sits on
//! the project, so a summary can say what the hours cost.

mod common;

use aether_sdk::db::Filter;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use common::{is_admin, my_id, require, require_admin, text, today, Record};

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Log {
    project: String,
    date: String,
    hours: String,
    #[serde(default)]
    task: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct Range {
    from: String,
    to: String,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize)]
struct Project {
    code: String,
    name: String,
    #[serde(default)]
    cost_per_hour: Option<String>,
}

fn new_project(input: Project) -> Result<Record> {
    require_admin()?;
    if input.code.trim().is_empty() || input.name.trim().is_empty() {
        return Err(Error::msg("a project needs a code and a name"));
    }
    let mut data = json!({ "code": input.code.trim(), "name": input.name.trim(), "is_active": true });
    if let Some(c) = input.cost_per_hour.filter(|c| !c.is_empty()) {
        let d = Decimal::parse(&c).map_err(|_| Error::msg("cost per hour is a number"))?.with_scale(2)?;
        if d.is_negative() {
            return Err(Error::msg("cost per hour is not negative"));
        }
        data["cost_per_hour"] = json!(d.to_string());
    }
    db::create("ts_project", &data).map_err(|e| e.or("could not create the project (the code may exist)"))
}

fn log_time(input: Log) -> Result<Record> {
    let me = my_id()?.ok_or_else(|| Error::msg("you are not an employee here"))?;
    let project = require("ts_project", &input.project, "project")?;
    if project.get("is_active") == Some(&json!(false)) {
        return Err(Error::msg("this project is closed"));
    }
    let date = aether_sdk::dates::format_date(aether_sdk::dates::parse_date(&input.date)?);
    if date > today()? {
        return Err(Error::msg("hours are logged for a day that has come"));
    }
    let hours = Decimal::parse(&input.hours).map_err(|_| Error::msg("hours is a number"))?.with_scale(2)?;
    if hours.is_negative() || hours.is_zero() || hours > Decimal::whole(24, 2)? {
        return Err(Error::msg("hours are above 0 and at most 24"));
    }
    let mut total = hours;
    for other in db::find::<Record>("ts_entry").matching(Filter::eq("employee", me.as_str()).and(Filter::eq("work_date", date.as_str())).and(Filter::ne("state", "rejected"))).limit(100).all()? {
        total = total.checked_add(Decimal::parse(text(&other, "hours").unwrap_or("0"))?)?;
    }
    if total > Decimal::whole(24, 2)? {
        return Err(Error::msg("that makes more than 24 hours for the day"));
    }
    db::create(
        "ts_entry",
        &json!({ "employee": me, "project": input.project, "work_date": date, "hours": hours.to_string(), "task": input.task, "note": input.note, "state": "draft" }),
    )
}

fn mine(id: &str) -> Result<Record> {
    let row = require("ts_entry", id, "entry")?;
    let me = my_id()?;
    let owner = text(&row, "employee").map(str::to_string);
    if me.is_none() || me != owner {
        return Err(Error::msg("this is not your entry"));
    }
    Ok(row)
}

fn submit(input: Id) -> Result<Record> {
    let row = mine(&input.id)?;
    if !matches!(text(&row, "state"), Some("draft" | "rejected")) {
        return Err(Error::msg("only a draft or rejected entry is submitted"));
    }
    db::update("ts_entry", &input.id, &json!({ "state": "submitted" }))?.ok_or_else(|| Error::msg("the entry is gone"))
}

fn remove(input: Id) -> Result<Value> {
    let row = mine(&input.id)?;
    if text(&row, "state") == Some("approved") {
        return Err(Error::msg("an approved entry is final"));
    }
    db::delete::<Record>("ts_entry", &input.id)?;
    Ok(json!({ "deleted": input.id }))
}

/// Who may decide an entry: a timesheet administrator, or the manager of its owner; never the owner.
fn may_decide(row: &Record) -> Result<()> {
    let me = my_id()?;
    let owner = text(row, "employee").unwrap_or_default().to_string();
    if me.as_deref() == Some(owner.as_str()) {
        return Err(Error::msg("nobody approves their own hours"));
    }
    if is_admin()? {
        return Ok(());
    }
    let Some(me) = me else { return Err(Error::msg("you are not an employee here")) };
    let person: Option<Record> = plugins::call("hr", "get_employee", &json!({ "id": owner }))?;
    let manager = person.as_ref().and_then(|p| text(p, "manager")).unwrap_or_default();
    if manager == me || manager.ends_with(me.as_str()) || me.ends_with(manager) && !manager.is_empty() {
        Ok(())
    } else {
        Err(Error::msg("only the person's manager or a timesheet administrator decides this"))
    }
}

fn decide(id: &str, approve: bool, note: Option<String>) -> Result<Record> {
    let row = require("ts_entry", id, "entry")?;
    if text(&row, "state") != Some("submitted") {
        return Err(Error::msg("this entry is not waiting for a decision"));
    }
    may_decide(&row)?;
    if !approve && note.as_deref().is_none_or(|n| n.trim().is_empty()) {
        return Err(Error::msg("say why the hours are rejected"));
    }
    let state = if approve { "approved" } else { "rejected" };
    db::update("ts_entry", id, &json!({ "state": state, "decided_by": my_id()?, "decision_note": note }))?.ok_or_else(|| Error::msg("the entry is gone"))
}

fn waiting() -> Result<Vec<Record>> {
    let rows: Vec<Record> = db::find::<Record>("ts_entry").matching(Filter::eq("state", "submitted")).order_by("work_date").limit(500).all()?;
    if is_admin()? {
        return Ok(rows);
    }
    let Some(me) = my_id()? else { return Ok(Vec::new()) };
    let reports: Vec<Record> = plugins::call("hr", "list_employees", &json!({ "manager": me, "limit": 500 }))?;
    let ids: Vec<&str> = reports.iter().filter_map(|e| text(e, "id")).collect();
    Ok(rows.into_iter().filter(|r| text(r, "employee").is_some_and(|e| ids.contains(&e))).collect())
}

/// Approved hours between two days, by project (and cost, where the project has a rate).
fn summary(input: Range) -> Result<Vec<Value>> {
    if !is_admin()? && my_id()?.is_none() {
        return Ok(Vec::new());
    }
    let mut filter = Filter::eq("state", "approved").and(Filter::gte("work_date", input.from.as_str())).and(Filter::lte("work_date", input.to.as_str()));
    if let Some(p) = &input.project {
        filter = filter.and(Filter::eq("project", p.as_str()));
    }
    // Everyone sees their own hours; the administrator sees everyone's.
    if !is_admin()? {
        filter = filter.and(Filter::eq("employee", my_id()?.unwrap_or_default()));
    }
    let rows: Vec<Record> = db::find("ts_entry").matching(filter).limit(5000).all()?;
    let mut by: std::collections::BTreeMap<String, Decimal> = std::collections::BTreeMap::new();
    for r in &rows {
        let h = Decimal::parse(text(r, "hours").unwrap_or("0"))?;
        let slot = by.entry(text(r, "project").unwrap_or_default().to_string()).or_insert_with(|| Decimal::zero(2));
        *slot = slot.checked_add(h)?;
    }
    let mut out = Vec::new();
    for (project, hours) in by {
        let rec = db::get::<Record>("ts_project", &project)?;
        let rate = rec.as_ref().and_then(|p| text(p, "cost_per_hour")).and_then(|r| Decimal::parse(r).ok());
        let cost = rate.and_then(|r| hours.times(r, 2).ok()).map(|c| c.to_string());
        out.push(json!({
            "project": project, "name": rec.as_ref().and_then(|p| text(p, "name")), "hours": hours.to_string(), "cost": cost,
        }));
    }
    Ok(out)
}

handler! {
    fn create_ts_project(input: Project) -> Record {
        new_project(input)
    }

    fn list_ts_projects(_: Empty) -> Vec<Record> {
        db::find("ts_project").order_by("code").limit(500).all()
    }

    fn log_hours(input: Log) -> Record {
        log_time(input)
    }

    fn submit_hours(input: Id) -> Record {
        submit(input)
    }

    fn delete_hours(input: Id) -> Value {
        remove(input)
    }

    fn approve_hours(input: Id) -> Record {
        decide(&input.id, true, None)
    }

    /// One click from a list: the reason recorded is "Declined".
    fn reject_hours(input: Id) -> Record {
        decide(&input.id, false, Some("Declined".into()))
    }

    fn my_timesheet(_: Empty) -> Vec<Record> {
        let Some(me) = my_id()? else { return Ok(Vec::new()) };
        db::find("ts_entry").filter("employee", me).order_by("-work_date").limit(200).all()
    }

    fn timesheets_waiting_for_me(_: Empty) -> Vec<Record> {
        waiting()
    }

    fn hours_by_project(input: Range) -> Vec<Value> {
        summary(input)
    }
}
