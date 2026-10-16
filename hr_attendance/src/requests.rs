//! Shift requests (a different shift for some days) and attendance requests (work from home, on duty, a half
//! day). Same rule as corrections: the manager or an attendance administrator decides, never the person.
//! An approved shift request makes a temporary, dated shift assignment; an approved attendance request puts an
//! override on each day, for days not yet come when they arrive (the nightly job).

use aether_sdk::dates::{format_date, parse_date, Duration, NaiveDate};
use aether_sdk::db::Filter;
use aether_sdk::prelude::*;

use crate::common::{employee, id_of, is_admin, my_employee, next_number, require, text, today_date, Record};
use crate::day::{recompute, set_override};

const MAX_SPAN_DAYS: i64 = 31;

#[derive(Deserialize)]
struct AskShift {
    #[serde(default)]
    employee: Option<String>,
    shift: String,
    date_from: String,
    date_to: String,
    reason: String,
}

#[derive(Deserialize)]
struct AskAttendance {
    #[serde(default)]
    employee: Option<String>,
    kind: String,
    date_from: String,
    date_to: String,
    reason: String,
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

fn requester(named: &Option<String>) -> Result<Record> {
    let me = my_employee()?;
    match (named, me) {
        (Some(id), Some(me)) if id_of(&me)? == id => Ok(me),
        (Some(id), _) => {
            crate::common::require_admin().map_err(|_| Error::msg("you can only ask for yourself"))?;
            employee(id)
        }
        (None, Some(me)) => Ok(me),
        (None, None) => Err(Error::msg("you are not an employee")),
    }
}

fn span(from: &str, to: &str) -> Result<(NaiveDate, NaiveDate)> {
    let (from, to) = (parse_date(from)?, parse_date(to)?);
    if to < from {
        return Err(Error::msg("it ends before it starts"));
    }
    if (to - from).num_days() >= MAX_SPAN_DAYS {
        return Err(Error::msg(format!("at most {MAX_SPAN_DAYS} days in one request")));
    }
    if from < today_date()? - Duration::days(60) {
        return Err(Error::msg("requests go back at most 60 days; ask an administrator"));
    }
    Ok((from, to))
}

fn overlapping(model: &str, person: &str, from: NaiveDate, to: NaiveDate) -> Result<bool> {
    let rows: Vec<Record> = db::find(model).matching(Filter::eq("employee", person).and(Filter::ne("status", "rejected")).and(Filter::ne("status", "cancelled"))).limit(500).all()?;
    for row in rows {
        let (f, t) = (parse_date(text(&row, "date_from").unwrap_or_default())?, parse_date(text(&row, "date_to").unwrap_or_default())?);
        if f <= to && from <= t {
            return Ok(true);
        }
    }
    Ok(false)
}

fn ask_shift(input: AskShift) -> Result<Record> {
    let person = requester(&input.employee)?;
    let (from, to) = span(&input.date_from, &input.date_to)?;
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why"));
    }
    let shift = require("att_shift", &input.shift, "shift")?;
    if shift.get("is_active") == Some(&json!(false)) {
        return Err(Error::msg("that shift is switched off"));
    }
    let person_id = id_of(&person)?.to_string();
    if overlapping("att_shift_request", &person_id, from, to)? {
        return Err(Error::msg("there is already a shift request in those days"));
    }
    let mut data = json!({
        "reference": next_number("shift_request", "SR-", 5)?, "employee": person_id, "shift": input.shift,
        "date_from": format_date(from), "date_to": format_date(to), "reason": input.reason, "status": "pending",
    });
    if let Some(manager) = text(&person, "manager") {
        data["approver"] = json!(manager);
    }
    db::create("att_shift_request", &data)
}

fn ask_attendance(input: AskAttendance) -> Result<Record> {
    let person = requester(&input.employee)?;
    if !matches!(input.kind.as_str(), "work_from_home" | "on_duty" | "half_day") {
        return Err(Error::msg("kind is work_from_home, on_duty or half_day"));
    }
    let (from, to) = span(&input.date_from, &input.date_to)?;
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why"));
    }
    let person_id = id_of(&person)?.to_string();
    if overlapping("att_request", &person_id, from, to)? {
        return Err(Error::msg("there is already an attendance request in those days"));
    }
    let mut data = json!({
        "reference": next_number("att_request", "AR-", 5)?, "employee": person_id, "kind": input.kind,
        "date_from": format_date(from), "date_to": format_date(to), "reason": input.reason, "status": "pending",
    });
    if let Some(manager) = text(&person, "manager") {
        data["approver"] = json!(manager);
    }
    db::create("att_request", &data)
}

fn may_decide(me: Option<&Record>, request: &Record) -> Result<bool> {
    let requester = text(request, "employee").unwrap_or_default();
    if me.is_some_and(|m| text(m, "id") == Some(requester)) {
        return Ok(false);
    }
    if is_admin()? {
        return Ok(true);
    }
    let Some(me) = me else { return Ok(false) };
    let manager = employee(requester)?.get("manager").and_then(Value::as_str).map(str::to_string);
    Ok(manager.as_deref() == text(me, "id"))
}

fn status_for(kind: &str) -> &'static str {
    if kind == "half_day" { "half_day" } else { "present" }
}

/// Put approved attendance requests on the days that have come, once each.
fn apply_request(request: &Record, today: NaiveDate) -> Result<()> {
    let person = employee(text(request, "employee").unwrap_or_default())?;
    let (from, to) = (parse_date(text(request, "date_from").unwrap_or_default())?, parse_date(text(request, "date_to").unwrap_or_default())?);
    let start = text(request, "applied_through").map(parse_date).transpose()?.map_or(from, |d| (d + Duration::days(1)).max(from));
    let end = to.min(today);
    let mut day = start;
    let status = status_for(text(request, "kind").unwrap_or("work_from_home"));
    let reason = format!("{} {}", text(request, "reference").unwrap_or(""), text(request, "kind").unwrap_or(""));
    while day <= end {
        if crate::common::employed_on(&person, day) {
            // A locked day cannot change; skipping it is the same as the period being closed.
            let _ = set_override(&person, day, status, &reason, text(request, "decided_by").map(str::to_string));
        }
        day += Duration::days(1);
    }
    if end >= start {
        db::update::<Record>("att_request", id_of(request)?, &json!({ "applied_through": format_date(end) }))?;
    }
    Ok(())
}

/// Nightly: approved requests whose days have arrived.
pub fn apply_due(today: NaiveDate) -> Result<u64> {
    let rows: Vec<Record> = db::find("att_request").filter("status", "approved").limit(1000).all()?;
    let mut applied = 0;
    for row in rows {
        let done = text(&row, "applied_through").map(parse_date).transpose()?;
        let to = parse_date(text(&row, "date_to").unwrap_or_default())?;
        if done.is_some_and(|d| d >= to) || parse_date(text(&row, "date_from").unwrap_or_default())? > today {
            continue;
        }
        if apply_request(&row, today).is_ok() {
            applied += 1;
        }
    }
    Ok(applied)
}

fn decide_shift(input: Decide, approve: bool) -> Result<Record> {
    let request = require("att_shift_request", &input.id, "shift request")?;
    if text(&request, "status") != Some("pending") {
        return Err(Error::msg("this request was already decided"));
    }
    let me = my_employee()?;
    if !may_decide(me.as_ref(), &request)? {
        return Err(Error::msg("only this person's manager (or an attendance administrator) can decide, and never the person themselves"));
    }
    if !approve && input.note.as_deref().is_none_or(|n| n.trim().is_empty()) {
        return Err(Error::msg("say why the request is rejected"));
    }
    let person_id = text(&request, "employee").unwrap_or_default().to_string();
    let (from, to) = (parse_date(text(&request, "date_from").unwrap_or_default())?, parse_date(text(&request, "date_to").unwrap_or_default())?);
    if approve {
        // Dated and temporary: the standing assignment resumes the day after.
        db::create::<Record>(
            "att_shift_assignment",
            &json!({
                "employee": person_id, "shift": text(&request, "shift"), "valid_from": format_date(from), "valid_to": format_date(to),
                "source_request": id_of(&request)?,
            }),
        )?;
    }
    let done = db::update::<Record>(
        "att_shift_request",
        &input.id,
        &json!({
            "status": if approve { "approved" } else { "rejected" }, "decided_by": me.as_ref().and_then(|m| text(m, "id")),
            "decided_on": format_date(today_date()?), "decision_note": input.note,
        }),
    )?
    .ok_or_else(|| Error::msg("the request is gone"))?;
    if approve {
        let person = employee(&person_id)?;
        let today = today_date()?;
        let mut day = from;
        while day <= to.min(today) {
            recompute(&person, day)?;
            day += Duration::days(1);
        }
    }
    events::emit("shift_request_decided", &json!({ "request": input.id, "employee": person_id, "approved": approve }))?;
    Ok(done)
}

fn decide_attendance(input: Decide, approve: bool) -> Result<Record> {
    let request = require("att_request", &input.id, "attendance request")?;
    if text(&request, "status") != Some("pending") {
        return Err(Error::msg("this request was already decided"));
    }
    let me = my_employee()?;
    if !may_decide(me.as_ref(), &request)? {
        return Err(Error::msg("only this person's manager (or an attendance administrator) can decide, and never the person themselves"));
    }
    if !approve && input.note.as_deref().is_none_or(|n| n.trim().is_empty()) {
        return Err(Error::msg("say why the request is rejected"));
    }
    let done = db::update::<Record>(
        "att_request",
        &input.id,
        &json!({
            "status": if approve { "approved" } else { "rejected" }, "decided_by": me.as_ref().and_then(|m| text(m, "id")),
            "decided_on": format_date(today_date()?), "decision_note": input.note,
        }),
    )?
    .ok_or_else(|| Error::msg("the request is gone"))?;
    if approve {
        apply_request(&done, today_date()?)?;
    }
    events::emit("attendance_request_decided", &json!({ "request": input.id, "employee": text(&request, "employee"), "approved": approve }))?;
    Ok(done)
}

fn cancel(model: &str, input: Id) -> Result<Record> {
    let request = require(model, &input.id, "request")?;
    let me = my_employee()?;
    if !me.as_ref().is_some_and(|m| text(m, "id") == text(&request, "employee")) && !is_admin()? {
        return Err(Error::msg("only the person can withdraw their request"));
    }
    if text(&request, "status") != Some("pending") {
        return Err(Error::msg("only a waiting request can be withdrawn"));
    }
    db::update::<Record>(model, &input.id, &json!({ "status": "cancelled" }))?.ok_or_else(|| Error::msg("the request is gone"))
}

fn waiting(model: &str) -> Result<Vec<Record>> {
    let me = my_employee()?;
    let mut found: Vec<Record> = Vec::new();
    if let Some(me) = &me {
        let reports: Vec<Record> = plugins::call("hr", "list_employees", &json!({ "manager": id_of(me)?, "limit": 500 }))?;
        let ids: Vec<&str> = reports.iter().filter_map(|e| text(e, "id")).collect();
        if !ids.is_empty() {
            found = db::find::<Record>(model).matching(Filter::eq("status", "pending").and(Filter::one_of("employee", ids))).order_by("date_from").limit(500).all()?;
        }
    }
    if is_admin()? {
        for row in db::find::<Record>(model).filter("status", "pending").order_by("date_from").limit(500).all()? {
            if !found.iter().any(|seen| text(seen, "id") == text(&row, "id")) {
                found.push(row);
            }
        }
    }
    if let Some(me) = &me {
        found.retain(|c| text(c, "employee") != text(me, "id"));
    }
    Ok(found)
}

fn mine(model: &str) -> Result<Vec<Record>> {
    let Some(me) = my_employee()? else { return Ok(Vec::new()) };
    db::find(model).filter("employee", id_of(&me)?).order_by("-date_from").limit(200).all()
}

handler! {
    /// Ask for another shift for some days.
    fn request_shift(input: AskShift) -> Record {
        ask_shift(input)
    }

    fn approve_shift_request(input: Decide) -> Record {
        decide_shift(input, true)
    }

    fn reject_shift_request(input: Decide) -> Record {
        decide_shift(input, false)
    }

    fn cancel_shift_request(input: Id) -> Record {
        cancel("att_shift_request", input)
    }

    fn shift_requests_waiting_for_me(_: Empty) -> Vec<Record> {
        waiting("att_shift_request")
    }

    fn my_shift_requests(_: Empty) -> Vec<Record> {
        mine("att_shift_request")
    }

    /// Ask to be marked present for days worked from home or away on duty, or half a day.
    fn request_attendance(input: AskAttendance) -> Record {
        ask_attendance(input)
    }

    fn approve_attendance_request(input: Decide) -> Record {
        decide_attendance(input, true)
    }

    fn reject_attendance_request(input: Decide) -> Record {
        decide_attendance(input, false)
    }

    fn cancel_attendance_request(input: Id) -> Record {
        cancel("att_request", input)
    }

    fn attendance_requests_waiting_for_me(_: Empty) -> Vec<Record> {
        waiting("att_request")
    }

    fn my_attendance_requests(_: Empty) -> Vec<Record> {
        mine("att_request")
    }
}
