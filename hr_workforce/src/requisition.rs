//! Hiring requisitions: a request for seats on a position.
//!
//! * **Growth** must fit the seats the position really has free, net of seats other approved requisitions have
//!   claimed and not filled (a requisition can never promise what recruitment would later refuse), and is counted
//!   against the plan covering its date; with no plan it is allowed but marked `unplanned`, and a plan line that is
//!   used up refuses more. **Replacement** and **temporary** requests are not growth: they skip both checks (the
//!   seat is held by someone leaving, or is short-lived).
//! * Someone other than the requester decides, and nobody decides a requisition they raised themselves.
//! * Fills and time-to-fill are **read back from the opening** in `hr_recruitment`, never typed.

use aether_sdk::dates::{format_date, parse_date};
use aether_sdk::db::Filter;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{actor, decimal_of, id_of, is_admin, my_employee, next_number, require, require_admin, text, today_date, Record};
use crate::plan::{line_for, seats_used};
use crate::rules::{days_to_fill, free_seats, is_filled, plan_room};

#[derive(Deserialize)]
struct Ask {
    position: String,
    seats: i64,
    reason: String,
    #[serde(default)]
    details: Option<String>,
    #[serde(default)]
    needed_by: Option<String>,
}

#[derive(Deserialize)]
struct Decide {
    id: String,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize, Default)]
struct Search {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    position: Option<String>,
}

fn committed_open(position: &str, except: Option<&str>) -> Result<u64> {
    let rows: Vec<Record> = db::find::<Record>("wf_requisition").filter("position", position).filter("state", "approved").limit(500).all()?;
    Ok(rows
        .iter()
        .filter(|r| except.is_none_or(|id| text(r, "id") != Some(id)))
        .map(|r| r.get("seats").and_then(Value::as_u64).unwrap_or(0).saturating_sub(r.get("seats_filled").and_then(Value::as_u64).unwrap_or(0)))
        .sum())
}

/// What a growth request for `seats` is worth against the position and the plan; fails when it does not fit.
/// Returns the plan line (if any) and the expected cost.
fn check_growth(position: &str, seats: u64, day: aether_sdk::dates::NaiveDate, except: Option<&str>) -> Result<(Option<Record>, Option<Decimal>)> {
    let free: Value = plugins::call("hr", "position_seats", &json!({ "id": position }))?;
    if free["frozen"] == true {
        return Err(Error::msg("this position is frozen: no hiring into it"));
    }
    let room = free_seats(free["headcount"].as_u64().unwrap_or(0), free["held"].as_u64().unwrap_or(0), committed_open(position, except)?);
    if seats > room {
        return Err(Error::msg(format!("the position has {room} seat(s) free after other requisitions: raise its headcount first, or ask for fewer")));
    }
    let Some((_, line)) = line_for(position, day)? else { return Ok((None, None)) };
    let line_id = id_of(&line)?.to_string();
    let planned = line.get("planned_hires").and_then(Value::as_u64).unwrap_or(0);
    let (mut asked, _) = seats_used(&line_id)?;
    if let Some(id) = except {
        // Do not count the requisition being re-checked against itself.
        if let Some(own) = db::get::<Record>("wf_requisition", id)? {
            if text(&own, "plan_line") == Some(line_id.as_str()) && matches!(text(&own, "state"), Some("pending" | "approved" | "filled")) {
                asked = asked.saturating_sub(own.get("seats").and_then(Value::as_u64).unwrap_or(0));
            }
        }
    }
    let left = plan_room(planned, asked);
    if seats > left {
        return Err(Error::msg(format!("the staffing plan has {left} hire(s) left for this position in this period: ask the planners to change the plan")));
    }
    let cost = match decimal_of(&line, "cost_per_hire")? {
        Some(cost) => Some(cost.times(Decimal::whole(i64::try_from(seats).unwrap_or(0), 2)?, 2)?),
        None => None,
    };
    Ok((Some(line), cost))
}

fn ask(input: Ask) -> Result<Record> {
    if !matches!(input.reason.as_str(), "growth" | "replacement" | "temporary") {
        return Err(Error::msg("reason is growth, replacement or temporary"));
    }
    if !(1..=1000).contains(&input.seats) {
        return Err(Error::msg("ask for 1 to 1000 seats"));
    }
    let me = my_employee()?;
    if me.is_none() && !is_admin()? {
        return Err(Error::msg("only an employee (or a workforce planner) can raise a requisition"));
    }
    let position: Option<Record> = plugins::call("hr", "get_position", &json!({ "id": input.position }))?;
    let position = position.ok_or_else(|| Error::msg("there is no such position"))?;
    let day = match input.needed_by.as_deref() {
        Some(d) => parse_date(d)?,
        None => today_date()?,
    };
    let duplicate = db::count(
        "wf_requisition",
        Filter::eq("position", input.position.as_str()).and(Filter::eq("state", "pending")).and(Filter::eq("reason", input.reason.as_str())),
    )?;
    if duplicate > 0 {
        return Err(Error::msg("a requisition for this position and reason is already waiting: add to it or decide it"));
    }
    let seats = u64::try_from(input.seats).unwrap_or(1);
    let (mut line, mut cost, mut unplanned) = (None, None, false);
    if input.reason == "growth" {
        let checked = check_growth(&input.position, seats, day, None)?;
        unplanned = checked.0.is_none();
        line = checked.0;
        cost = checked.1;
    }
    let mut data = json!({
        "reference": next_number("requisition", "REQ-", 6)?, "position": input.position, "seats": input.seats, "seats_filled": 0,
        "reason": input.reason, "state": "pending", "unplanned": unplanned, "requested_by": actor()?,
    });
    if let Some(department) = text(&position, "department") {
        data["department"] = json!(department);
    }
    if let Some(me) = &me {
        data["requested_by_employee"] = json!(id_of(me)?);
    }
    if let Some(details) = input.details {
        data["details"] = json!(details);
    }
    if input.needed_by.is_some() {
        data["needed_by"] = json!(format_date(day));
    }
    if let Some(line) = &line {
        data["plan_line"] = json!(id_of(line)?);
    }
    if let Some(cost) = cost {
        data["expected_cost"] = json!(cost);
    }
    db::create("wf_requisition", &data)
}

fn decide(input: Decide, approve: bool) -> Result<Record> {
    require_admin()?;
    let row = require("wf_requisition", &input.id, "requisition")?;
    if text(&row, "state") != Some("pending") {
        return Err(Error::msg("this requisition was already decided"));
    }
    let who = actor()?;
    let me = my_employee()?;
    if text(&row, "requested_by") == Some(who.as_str()) || me.as_ref().is_some_and(|m| text(m, "id") == text(&row, "requested_by_employee")) {
        return Err(Error::msg("nobody decides their own requisition"));
    }
    if !approve {
        if input.note.as_deref().is_none_or(|n| n.trim().is_empty()) {
            return Err(Error::msg("say why it is rejected"));
        }
        return db::update("wf_requisition", &input.id, &json!({ "state": "rejected", "decided_by": who, "decision_note": input.note }))?
            .ok_or_else(|| Error::msg("the requisition is gone"));
    }
    // The position and the plan may have changed since it was asked.
    if text(&row, "reason") == Some("growth") {
        let day = match text(&row, "needed_by") {
            Some(d) => parse_date(d)?,
            None => today_date()?,
        };
        check_growth(text(&row, "position").unwrap_or_default(), row.get("seats").and_then(Value::as_u64).unwrap_or(0), day, Some(&input.id))?;
    }
    let approved: Record = db::update("wf_requisition", &input.id, &json!({ "state": "approved", "decided_by": who, "decision_note": input.note }))?
        .ok_or_else(|| Error::msg("the requisition is gone"))?;
    events::emit("requisition_approved", &json!({ "requisition": input.id, "position": row["position"], "seats": row["seats"] }))?;
    Ok(approved)
}

/// Start the hiring in `hr_recruitment` for an approved requisition. The opening is a draft there: a recruiter
/// opens it (and recruitment's own rules still decide who).
fn open_hiring(input: Id) -> Result<Record> {
    let row = require("wf_requisition", &input.id, "requisition")?;
    if text(&row, "state") != Some("approved") {
        return Err(Error::msg("only an approved requisition can start hiring"));
    }
    if text(&row, "opening").is_some() {
        return Err(Error::msg("hiring has already been started for this requisition"));
    }
    let position: Record = plugins::call::<Option<Record>>("hr", "get_position", &json!({ "id": text(&row, "position").unwrap_or_default() }))?
        .ok_or_else(|| Error::msg("the position is gone"))?;
    let mut data = json!({
        "title": text(&position, "name").unwrap_or("Opening"), "position": row["position"], "seats": row["seats"],
        "description": row.get("details").cloned().unwrap_or(Value::Null),
    });
    if let Some(department) = text(&row, "department") {
        data["department"] = json!(department);
    }
    let opening: Record = plugins::call("hr_recruitment", "create_opening", &data)?;
    db::update("wf_requisition", &input.id, &json!({ "opening": opening["id"], "posted_on": format_date(today_date()?) }))?.ok_or_else(|| Error::msg("the requisition is gone"))
}

fn withdraw(input: Id) -> Result<Record> {
    let row = require("wf_requisition", &input.id, "requisition")?;
    let me = my_employee()?;
    let mine = text(&row, "requested_by") == Some(actor()?.as_str()) || me.as_ref().is_some_and(|m| text(m, "id") == text(&row, "requested_by_employee"));
    if !mine && !is_admin()? {
        return Err(Error::msg("only who raised it, or a workforce planner, can withdraw this"));
    }
    if !matches!(text(&row, "state"), Some("pending" | "approved")) {
        return Err(Error::msg("only a waiting or approved requisition can be withdrawn"));
    }
    if row.get("seats_filled").and_then(Value::as_u64).unwrap_or(0) > 0 {
        return Err(Error::msg("some seats are already filled: it cannot be withdrawn"));
    }
    if let Some(opening) = text(&row, "opening") {
        // Close the hiring too, so recruitment is not left chasing seats nobody wants.
        let closed: Result<Value> = plugins::call("hr_recruitment", "close_opening", &json!({ "id": opening, "reason": "requisition withdrawn" }));
        if let Err(error) = closed {
            return Err(Error::msg(format!("the opening could not be closed ({error}): close it in recruitment first")));
        }
    }
    db::update("wf_requisition", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the requisition is gone"))
}

/// Read the fills back from the opening; mark a requisition filled when all its seats are.
fn sync_one(row: &Record) -> Result<bool> {
    let Some(opening) = text(row, "opening") else { return Ok(false) };
    let found: Option<Record> = plugins::call("hr_recruitment", "get_opening", &json!({ "id": opening }))?;
    let Some(found) = found else { return Ok(false) };
    let filled = found.get("seats_filled").and_then(Value::as_u64).unwrap_or(0);
    let seats = row.get("seats").and_then(Value::as_u64).unwrap_or(0);
    if row.get("seats_filled").and_then(Value::as_u64) == Some(filled) {
        return Ok(false);
    }
    let mut data = json!({ "seats_filled": filled });
    if is_filled(seats, filled) && text(row, "state") == Some("approved") {
        let today = today_date()?;
        data["state"] = json!("filled");
        data["filled_on"] = json!(format_date(today));
        if let Some(posted) = text(row, "posted_on") {
            data["days_to_fill"] = json!(days_to_fill(parse_date(posted)?, today)?);
        }
    }
    db::update::<Record>("wf_requisition", id_of(row)?, &data)?;
    if data.get("state").is_some() {
        events::emit("requisition_filled", &json!({ "requisition": id_of(row)?, "position": row["position"] }))?;
    }
    Ok(true)
}

fn sync_all() -> Result<u64> {
    let rows: Vec<Record> = db::find::<Record>("wf_requisition").filter("state", "approved").limit(1000).all()?;
    let mut changed = 0;
    for row in rows {
        if sync_one(&row)? {
            changed += 1;
        }
    }
    Ok(changed)
}

fn list(input: Search) -> Result<Vec<Record>> {
    let mut query = db::find::<Record>("wf_requisition").order_by("-reference").limit(500);
    if let Some(state) = input.state.as_deref() {
        query = query.filter("state", state);
    }
    if let Some(position) = input.position.as_deref() {
        query = query.filter("position", position);
    }
    query.all()
}

handler! {
    /// Ask for seats on a position: growth, replacement or temporary.
    fn request_requisition(input: Ask) -> Record {
        ask(input)
    }

    /// Approve (a workforce planner; never the requester).
    fn approve_requisition(input: Decide) -> Record {
        decide(input, true)
    }

    fn reject_requisition(input: Decide) -> Record {
        decide(input, false)
    }

    /// Create the draft opening in recruitment for an approved requisition.
    fn start_hiring(input: Id) -> Record {
        open_hiring(input)
    }

    fn withdraw_requisition(input: Id) -> Record {
        withdraw(input)
    }

    /// Read fills back from recruitment now (also nightly).
    fn sync_requisition(input: Id) -> Record {
        let row = require("wf_requisition", &input.id, "requisition")?;
        sync_one(&row)?;
        require("wf_requisition", &input.id, "requisition")
    }

    fn workforce_nightly(_: Empty) -> Value {
        Ok(json!({ "updated": sync_all()? }))
    }

    fn list_requisitions(input: Search) -> Vec<Record> {
        list(input)
    }
}
