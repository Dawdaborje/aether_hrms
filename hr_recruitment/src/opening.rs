//! Openings: a request to hire for seats of a position.
//!
//! An opening opens only when the position has free seats that no other open opening has already
//! promised (neither Odoo's integer that floors at zero nor Frappe's count of offers), and only by
//! someone other than the person who asked for it.

use aether_sdk::dates::{format_date, parse_optional};
use aether_sdk::db::Filter;
use aether_sdk::prelude::*;

use crate::common::{
    id_of, my_employee, optional_decimal, pick, require, require_manager, require_recruiter, text, today_date, Record,
};
use crate::rules::{check_seats, seats_available};

const FIELDS: &[&str] = &[
    "title", "position", "department", "seats", "recruiter", "opens_on", "closes_on", "salary_min", "salary_max", "currency", "description",
];

#[derive(Deserialize)]
struct New {
    #[serde(flatten)]
    fields: Record,
    #[serde(default)]
    panel: Vec<String>,
}

#[derive(Deserialize)]
struct Change {
    id: String,
    #[serde(flatten)]
    fields: Record,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Reason {
    id: String,
    reason: String,
}

#[derive(Deserialize, Default)]
struct Search {
    #[serde(default)]
    status: Option<String>,
}

fn check_fields(data: &Record) -> Result<()> {
    if let (Some(min), Some(max)) = (optional_decimal(data, "salary_min")?, optional_decimal(data, "salary_max")?) {
        if min > max {
            return Err(Error::msg("the salary band's minimum is above its maximum"));
        }
    }
    if let Some(seats) = data.get("seats").and_then(Value::as_i64) {
        if !(1..=1000).contains(&seats) {
            return Err(Error::msg("an opening is for 1 to 1000 seats"));
        }
    }
    if let (Some(from), Some(to)) = (parse_optional(text(data, "opens_on"))?, parse_optional(text(data, "closes_on"))?) {
        aether_sdk::dates::check_order(from, Some(to), "opening")?;
    }
    for field in ["department", "recruiter"] {
        if let Some(id) = text(data, field) {
            let model = if field == "department" { "department" } else { "employee" };
            let _: Value = plugins::call("hr", if model == "employee" { "get_employee" } else { "get_department" }, &json!({ "id": id }))?;
        }
    }
    Ok(())
}

fn create(input: New) -> Result<Record> {
    require_recruiter()?;
    let mut data = pick(&input.fields, FIELDS);
    if text(&data, "title").is_none() {
        return Err(Error::msg("an opening needs a title"));
    }
    let position = text(&data, "position").ok_or_else(|| Error::msg("an opening is for a position"))?.to_string();
    let _: Value = plugins::call("hr", "get_position", &json!({ "id": position }))?;
    if data.get("seats").is_none() {
        data["seats"] = json!(1);
    }
    check_fields(&data)?;
    data["status"] = json!("draft");
    if let Some(me) = my_employee()? {
        data["requested_by"] = json!(id_of(&me)?);
    }
    let made: Record = db::create("rec_opening", &data).map_err(|e| e.or("could not create the opening"))?;
    if !input.panel.is_empty() {
        db::relate::<Record>("rec_opening", "panel", id_of(&made)?, &input.panel.iter().map(String::as_str).collect::<Vec<_>>())?;
    }
    Ok(made)
}

fn change(input: Change) -> Result<Record> {
    require_recruiter()?;
    let opening = require("rec_opening", &input.id, "opening")?;
    let mut allowed: Vec<&str> = vec!["title", "recruiter", "closes_on", "salary_min", "salary_max", "currency", "description"];
    match text(&opening, "status") {
        Some("draft") => allowed.extend(["position", "department", "seats", "opens_on"]),
        Some("open" | "on_hold") => {}
        _ => return Err(Error::msg("a closed opening cannot be changed")),
    }
    let data = pick(&input.fields, &allowed);
    if data.as_object().is_some_and(|f| f.is_empty()) {
        return Err(Error::msg("there is nothing to change (seats and position are fixed once the opening is open)"));
    }
    check_fields(&data)?;
    db::update::<Record>("rec_opening", &input.id, &data)?.ok_or_else(|| Error::msg("the opening is gone"))
}

/// Seats of other openings for the same position that are still promised.
fn promised_elsewhere(position: &str, this: &str) -> Result<i64> {
    let others: Vec<Record> = db::find::<Record>("rec_opening")
        .matching(Filter::eq("position", position).and(Filter::one_of("status", ["open", "on_hold"])))
        .limit(500)
        .all()?;
    Ok(others
        .iter()
        .filter(|o| text(o, "id") != Some(this))
        .map(|o| o.get("seats").and_then(Value::as_i64).unwrap_or(0) - o.get("seats_filled").and_then(Value::as_i64).unwrap_or(0))
        .sum())
}

/// How many seats can still be promised for an opening's position.
pub fn free_seats(opening: &Record) -> Result<i64> {
    let position = text(opening, "position").ok_or_else(|| Error::msg("the opening has no position"))?;
    let seats: Value = plugins::call("hr", "position_seats", &json!({ "id": position }))?;
    if seats["frozen"] == true {
        return Ok(0);
    }
    Ok(seats_available(
        seats["headcount"].as_i64().unwrap_or(0),
        seats["held"].as_i64().unwrap_or(0),
        promised_elsewhere(position, text(opening, "id").unwrap_or_default())?,
    ))
}

fn open(input: Id) -> Result<Record> {
    require_manager()?;
    let opening = require("rec_opening", &input.id, "opening")?;
    if !matches!(text(&opening, "status"), Some("draft" | "on_hold")) {
        return Err(Error::msg("only a draft or held opening can be opened"));
    }
    let me = my_employee()?;
    if me.as_ref().is_some_and(|m| text(m, "id") == text(&opening, "requested_by")) {
        return Err(Error::msg("someone other than the person who asked for it opens an opening"));
    }
    if text(&opening, "status") == Some("draft") {
        let outstanding = opening.get("seats").and_then(Value::as_i64).unwrap_or(0) - opening.get("seats_filled").and_then(Value::as_i64).unwrap_or(0);
        check_seats(outstanding, free_seats(&opening)?)?;
    }
    let opened = db::update::<Record>(
        "rec_opening",
        &input.id,
        &json!({ "status": "open", "opened_by": me.as_ref().and_then(|m| text(m, "id")), "opens_on": text(&opening, "opens_on").map_or_else(|| today_date().map(format_date), |d| Ok(d.to_string()))? }),
    )?
    .ok_or_else(|| Error::msg("the opening is gone"))?;
    events::emit("opening_opened", &json!({ "opening": input.id, "position": opening["position"], "seats": opening["seats"] }))?;
    Ok(opened)
}

fn hold(input: Id) -> Result<Record> {
    require_recruiter()?;
    let opening = require("rec_opening", &input.id, "opening")?;
    if text(&opening, "status") != Some("open") {
        return Err(Error::msg("only an open opening can be put on hold"));
    }
    db::update::<Record>("rec_opening", &input.id, &json!({ "status": "on_hold" }))?.ok_or_else(|| Error::msg("the opening is gone"))
}

fn close(input: Reason) -> Result<Record> {
    require_recruiter()?;
    let opening = require("rec_opening", &input.id, "opening")?;
    if !matches!(text(&opening, "status"), Some("draft" | "open" | "on_hold")) {
        return Err(Error::msg("this opening is already closed"));
    }
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why the opening is closed"));
    }
    let status = if text(&opening, "status") == Some("draft") { "cancelled" } else { "closed" };
    db::update::<Record>("rec_opening", &input.id, &json!({ "status": status, "close_reason": input.reason }))?.ok_or_else(|| Error::msg("the opening is gone"))
}

/// Recount the people hired against an opening; fill it when every seat is taken.
pub fn refresh_filled(opening_id: &str) -> Result<Record> {
    let opening = require("rec_opening", opening_id, "opening")?;
    let hired = db::count("rec_application", Filter::eq("opening", opening_id).and(Filter::eq("status", "hired")))?;
    let seats = opening.get("seats").and_then(Value::as_i64).unwrap_or(0);
    let mut changes = json!({ "seats_filled": hired });
    if hired as i64 >= seats && matches!(text(&opening, "status"), Some("open" | "on_hold")) {
        changes["status"] = json!("filled");
    }
    db::update::<Record>("rec_opening", opening_id, &changes)?.ok_or_else(|| Error::msg("the opening is gone"))
}

fn list(input: Search) -> Result<Vec<Record>> {
    let mut find = db::find::<Record>("rec_opening").order_by("-opens_on").limit(500);
    if let Some(status) = &input.status {
        find = find.filter("status", status.as_str());
    }
    find.all()
}

/// Openings whose closing day has passed.
pub fn close_expired() -> Result<u64> {
    let today = format_date(today_date()?);
    let due: Vec<Record> = db::find::<Record>("rec_opening")
        .matching(Filter::one_of("status", ["open", "on_hold"]).and(Filter::lt("closes_on", today.as_str())))
        .limit(500)
        .all()?;
    for opening in &due {
        db::update::<Record>("rec_opening", id_of(opening)?, &json!({ "status": "closed", "close_reason": "the closing date passed" }))?;
    }
    Ok(due.len() as u64)
}

handler! {
    fn create_opening(input: New) -> Record {
        create(input)
    }

    fn update_opening(input: Change) -> Record {
        change(input)
    }

    /// Open it: the position must have free seats that no other open opening has promised.
    fn open_opening(input: Id) -> Record {
        open(input)
    }

    fn hold_opening(input: Id) -> Record {
        hold(input)
    }

    fn close_opening(input: Reason) -> Record {
        close(input)
    }

    fn get_opening(input: Id) -> Option<Record> {
        db::get("rec_opening", &input.id)
    }

    fn list_openings(input: Option<Search>) -> Vec<Record> {
        list(input.unwrap_or_default())
    }

    /// Seats that can still be promised for an opening's position.
    fn opening_free_seats(input: Id) -> Value {
        let opening = require("rec_opening", &input.id, "opening")?;
        Ok(json!({ "free": free_seats(&opening)? }))
    }
}
