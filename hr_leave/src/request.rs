//! Requests for leave, from asking to taking.
//!
//! * Asking checks, in order: the person and the type, eligibility, notice, blocked dates,
//!   overlap, the sandwich rule, and the balance. A request is **held** against the balance (a
//!   `reservation` in the ledger) while it waits.
//! * Deciding follows the type's `validation`, copied onto the request when it is made so changing
//!   the type later does not change the flow of requests already open: the person's manager, HR (a
//!   leave administrator), or the manager and then HR. **Nobody decides their own request**, not
//!   even an administrator, and a manager can only decide for the people who report to them.
//! * Approval turns the reservation into `usage`; rejecting or cancelling reverses it.

use std::collections::HashMap;

use aether_sdk::dates::{format_date, parse_date, Duration, NaiveDate};
use aether_sdk::db::Filter;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{
    calendar_for, can_take_leave, date_of, decimal_of, employee, id_of, is_admin, is_off, my_employee, next_number, require,
    require_admin, text, today_date, Record,
};
use crate::ledger::{balance_on, nightly_ledger, post, reverse_key, Post};
use crate::rules::{eligible, leave_days, overlap, sandwiched_days, Half};

#[derive(Deserialize)]
struct Ask {
    /// Whose leave; the caller's own when left out (a leave administrator may name anyone).
    #[serde(default)]
    employee: Option<String>,
    leave_type: String,
    start_date: String,
    #[serde(default)]
    end_date: Option<String>,
    #[serde(default)]
    half_day: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
struct Preview {
    leave_type: String,
    start_date: String,
    #[serde(default)]
    end_date: Option<String>,
    #[serde(default)]
    half_day: Option<String>,
    #[serde(default)]
    employee: Option<String>,
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
    employee: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Deserialize)]
struct Range {
    from: String,
    to: String,
}

/// States that count for overlap and sandwich purposes.
const LIVE: [&str; 4] = ["submitted", "first_approved", "approved", "taken"];

fn res_key(request: &str) -> String {
    format!("res:{request}")
}

fn use_key(request: &str) -> String {
    format!("use:{request}")
}

struct Cost {
    days: Decimal,
    start: NaiveDate,
    end: NaiveDate,
    half: Half,
}

/// What a request costs by the person's calendar, before the sandwich rule.
fn cost(person: &Record, kind: &Record, start: &str, end: Option<&str>, half: Option<&str>) -> Result<Cost> {
    let start = parse_date(start)?;
    let end = match end {
        Some(end) => parse_date(end)?,
        None => start,
    };
    let half = Half::parse(half.unwrap_or("none")).ok_or_else(|| Error::msg("half_day is none, morning or afternoon"))?;
    if half != Half::Whole && is_off(kind, "half_days") {
        return Err(Error::msg("this kind of leave cannot be taken by the half day"));
    }
    if end < start {
        return Err(Error::msg("the leave ends before it starts"));
    }
    let calendar = calendar_for(person, start, end)?;
    let days = leave_days(&calendar, start, end, half, kind.get("count_all_days") == Some(&json!(true)))?;
    Ok(Cost { days, start, end, half })
}

/// Who the request is for: the caller, or (leave administrators only) somebody else.
fn subject(named: &Option<String>) -> Result<Record> {
    let me = my_employee()?;
    match (named, me) {
        (Some(id), Some(me)) if id_of(&me)? == id => Ok(me),
        (Some(id), _) => {
            require_admin().map_err(|_| Error::msg("you can only ask for leave for yourself"))?;
            employee(id)
        }
        (None, Some(me)) => Ok(me),
        (None, None) => Err(Error::msg("you are not an employee: a leave administrator must say whose leave this is")),
    }
}

fn preview(input: Preview) -> Result<Value> {
    let person = subject(&input.employee)?;
    let kind = require("leave_type", &input.leave_type, "leave type")?;
    let cost = cost(&person, &kind, &input.start_date, input.end_date.as_deref(), input.half_day.as_deref())?;
    Ok(json!({ "days": cost.days, "start_date": format_date(cost.start), "end_date": format_date(cost.end) }))
}

/// Leave on the same days is not allowed twice.
fn check_overlap(person_id: &str, cost: &Cost) -> Result<()> {
    let clashing: Vec<Record> = db::find::<Record>("leave_request")
        .matching(
            Filter::eq("employee", person_id)
                .and(Filter::one_of("state", LIVE))
                .and(Filter::lte("start_date", format_date(cost.end)))
                .and(Filter::gte("end_date", format_date(cost.start))),
        )
        .limit(20)
        .all()?;
    for other in &clashing {
        if overlap((cost.start, cost.end), (date_of(other, "start_date")?, date_of(other, "end_date")?)) {
            return Err(Error::msg(format!("it overlaps leave request {}", text(other, "reference").unwrap_or("?"))));
        }
    }
    Ok(())
}

/// Blocked periods (year-end close, a peak season) refuse requests unless an administrator asks.
fn check_blocks(kind_id: &str, cost: &Cost) -> Result<()> {
    if is_admin()? {
        return Ok(());
    }
    let blocks: Vec<Record> = db::find::<Record>("leave_block")
        .matching(
            Filter::eq("is_active", true)
                .and(Filter::lte("start_date", format_date(cost.end)))
                .and(Filter::gte("end_date", format_date(cost.start))),
        )
        .limit(50)
        .all()?;
    for block in blocks {
        if text(&block, "leave_type").is_none_or(|t| t == kind_id) {
            return Err(Error::msg(format!(
                "leave cannot be taken then: {} ({} to {})",
                text(&block, "name").unwrap_or("blocked"),
                text(&block, "start_date").unwrap_or("?"),
                text(&block, "end_date").unwrap_or("?")
            )));
        }
    }
    Ok(())
}

/// The sandwich rule: days off between this leave and another of the same kind count as leave.
fn sandwich_extra(person: &Record, kind_id: &str, cost: &Cost) -> Result<Decimal> {
    let person_id = id_of(person)?;
    let before: Option<Record> = db::find::<Record>("leave_request")
        .matching(
            Filter::eq("employee", person_id)
                .and(Filter::eq("leave_type", kind_id))
                .and(Filter::one_of("state", LIVE))
                .and(Filter::lt("end_date", format_date(cost.start))),
        )
        .order_by("-end_date")
        .first()?;
    let after: Option<Record> = db::find::<Record>("leave_request")
        .matching(
            Filter::eq("employee", person_id)
                .and(Filter::eq("leave_type", kind_id))
                .and(Filter::one_of("state", LIVE))
                .and(Filter::gt("start_date", format_date(cost.end))),
        )
        .order_by("start_date")
        .first()?;
    let window_start = before.as_ref().map(|b| date_of(b, "end_date")).transpose()?.unwrap_or(cost.start);
    let window_end = after.as_ref().map(|a| date_of(a, "start_date")).transpose()?.unwrap_or(cost.end);
    let calendar = calendar_for(person, window_start, window_end)?;
    let mut extra = 0u32;
    if let Some(before) = &before {
        extra += sandwiched_days(&calendar, date_of(before, "end_date")?, cost.start).unwrap_or(0);
    }
    if let Some(after) = &after {
        extra += sandwiched_days(&calendar, cost.end, date_of(after, "start_date")?).unwrap_or(0);
    }
    Decimal::whole(i64::from(extra), 2)
}

fn submit(input: Ask) -> Result<Record> {
    let person = subject(&input.employee)?;
    let person_id = id_of(&person)?.to_string();
    if !can_take_leave(&person) {
        return Err(Error::msg("this person cannot take leave right now"));
    }
    let kind = require("leave_type", &input.leave_type, "leave type")?;
    let kind_id = id_of(&kind)?.to_string();
    if is_off(&kind, "is_active") {
        return Err(Error::msg("this kind of leave is no longer offered"));
    }
    let admin = is_admin()?;
    let mut cost = cost(&person, &kind, &input.start_date, input.end_date.as_deref(), input.half_day.as_deref())?;
    let today = today_date()?;

    eligible(
        text(&kind, "gender").unwrap_or("any"),
        text(&person, "gender"),
        kind.get("applicable_after_days").and_then(Value::as_i64).unwrap_or(0),
        text(&person, "hire_date").and_then(|d| parse_date(d).ok()),
        cost.start,
    )?;
    let notice = kind.get("min_notice_days").and_then(Value::as_i64).unwrap_or(0);
    if notice > 0 && !admin && cost.start < today + Duration::days(notice) {
        return Err(Error::msg(format!("this kind of leave needs {notice} days of notice")));
    }
    check_blocks(&kind_id, &cost)?;
    check_overlap(&person_id, &cost)?;
    if kind.get("sandwich") == Some(&json!(true)) && cost.half == Half::Whole {
        cost.days = cost.days + sandwich_extra(&person, &kind_id, &cost)?;
    }
    if cost.days.is_zero() {
        return Err(Error::msg("there are no working days in that period"));
    }
    let limit = decimal_of(&kind, "max_per_request")?;
    if !limit.is_zero() && cost.days > limit {
        return Err(Error::msg(format!("this kind of leave is at most {limit} days at a time")));
    }
    if kind.get("requires_allocation") != Some(&json!(false)) {
        let held = balance_on(&person_id, &kind_id, cost.start)?;
        let floor = if kind.get("allow_negative") == Some(&json!(true)) { -decimal_of(&kind, "max_negative")? } else { Decimal::zero(2) };
        if held.available - cost.days < floor {
            return Err(Error::msg(format!("not enough leave: {} days asked, {} available", cost.days, held.available)));
        }
    }

    // The flow is copied now; a person with nobody above them is decided by HR instead of a manager.
    let manager = text(&person, "manager").map(str::to_string);
    let mut validation = text(&kind, "validation").unwrap_or("manager").to_string();
    if manager.is_none() && matches!(validation.as_str(), "manager" | "both") {
        validation = "hr".into();
    }
    let at_once = validation == "none";

    let reference = next_number("leave", "LV-", 5)?;
    let mut data = json!({
        "reference": reference, "employee": person_id, "leave_type": kind_id,
        "start_date": format_date(cost.start), "end_date": format_date(cost.end),
        "half_day": input.half_day.clone().unwrap_or_else(|| "none".into()),
        "days": cost.days, "reason": input.reason, "validation": validation,
        "state": if at_once { "approved" } else { "submitted" },
    });
    if let Some(manager) = &manager {
        data["approver"] = json!(manager);
    }
    if at_once {
        data["decided_on"] = json!(format_date(today));
    }
    let created: Record = db::create("leave_request", &data)?;
    let request_id = id_of(&created)?.to_string();

    // Hold the days; when nobody has to approve, spend them straight away.
    let source = format!("leave_request:{request_id}");
    post(Post {
        employee: &person_id,
        leave_type: &kind_id,
        kind: if at_once { "usage" } else { "reservation" },
        days: -cost.days,
        date: cost.start,
        valid_to: None,
        key: Some(if at_once { use_key(&request_id) } else { res_key(&request_id) }),
        reverses: None,
        source: &source,
        note: None,
    })?;
    events::emit("leave_requested", &json!({ "request": request_id, "employee": person_id, "days": cost.days }))?;
    if at_once {
        events::emit("leave_approved", &json!({ "request": request_id, "employee": person_id }))?;
    }
    Ok(created)
}

/// Which step a request is waiting at.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Manager,
    Hr,
}

fn step_of(request: &Record) -> Option<Step> {
    match (text(request, "state"), text(request, "validation")) {
        (Some("submitted"), Some("hr")) => Some(Step::Hr),
        (Some("submitted"), Some("manager" | "both")) => Some(Step::Manager),
        (Some("first_approved"), _) => Some(Step::Hr),
        _ => None,
    }
}

/// Whether the caller may decide this request at this step.
fn may_decide(me: Option<&Record>, request: &Record, step: Step) -> Result<bool> {
    let requester = text(request, "employee").unwrap_or_default();
    if let Some(me) = me {
        // Nobody decides their own leave, an administrator included.
        if id_of(me)? == requester {
            return Ok(false);
        }
    }
    if is_admin()? {
        return Ok(true);
    }
    Ok(match (step, me) {
        (Step::Manager, Some(me)) => {
            let current_manager = employee(requester)?.get("manager").and_then(Value::as_str).map(str::to_string);
            current_manager.as_deref() == Some(id_of(me)?)
        }
        _ => false,
    })
}

fn decide(input: Decide, approve: bool) -> Result<Record> {
    let request = require("leave_request", &input.id, "leave request")?;
    let step = step_of(&request).ok_or_else(|| Error::msg("this request is not waiting for a decision"))?;
    let me = my_employee()?;
    if me.is_none() && !is_admin()? {
        return Err(Error::msg("only an employee or a leave administrator can decide on leave"));
    }
    if !may_decide(me.as_ref(), &request, step)? {
        return Err(Error::msg(match step {
            Step::Manager => "only this person's manager (or a leave administrator) can decide on their leave, and never the person themselves",
            Step::Hr => "only a leave administrator can decide on this leave, and never on their own",
        }));
    }
    if !approve && input.note.as_deref().is_none_or(|n| n.trim().is_empty()) {
        return Err(Error::msg("say why the request is rejected"));
    }
    let person = text(&request, "employee").unwrap_or_default().to_string();
    let kind = text(&request, "leave_type").unwrap_or_default().to_string();
    let me_id = me.as_ref().and_then(|m| text(m, "id")).map(str::to_string);
    let today = format_date(today_date()?);

    if !approve {
        reverse_key(&person, &kind, &res_key(&input.id), "rejected")?;
        let rejected = db::update::<Record>(
            "leave_request",
            &input.id,
            &json!({ "state": "rejected", "decided_by": me_id, "decided_on": today, "decision_note": input.note }),
        )?
        .ok_or_else(|| Error::msg("the request is gone"))?;
        events::emit("leave_rejected", &json!({ "request": input.id, "employee": person }))?;
        return Ok(rejected);
    }
    // Two steps: the manager's approval only moves it on to HR.
    if step == Step::Manager && text(&request, "validation") == Some("both") {
        return db::update::<Record>("leave_request", &input.id, &json!({ "state": "first_approved", "first_decided_by": me_id, "decision_note": input.note }))?
            .ok_or_else(|| Error::msg("the request is gone"));
    }
    // Final: the held days are spent.
    reverse_key(&person, &kind, &res_key(&input.id), "approved")?;
    let days = decimal_of(&request, "days")?;
    post(Post {
        employee: &person,
        leave_type: &kind,
        kind: "usage",
        days: -days,
        date: date_of(&request, "start_date")?,
        valid_to: None,
        key: Some(use_key(&input.id)),
        reverses: None,
        source: &format!("leave_request:{}", input.id),
        note: None,
    })?;
    let approved = db::update::<Record>(
        "leave_request",
        &input.id,
        &json!({ "state": "approved", "decided_by": me_id, "decided_on": today, "decision_note": input.note }),
    )?
    .ok_or_else(|| Error::msg("the request is gone"))?;
    events::emit("leave_approved", &json!({ "request": input.id, "employee": person }))?;
    Ok(approved)
}

/// Give the days back: a waiting request releases its hold, an approved one its usage.
fn release(request: &Record, why: &str) -> Result<()> {
    let id = id_of(request)?;
    let person = text(request, "employee").unwrap_or_default();
    let kind = text(request, "leave_type").unwrap_or_default();
    match text(request, "state") {
        Some("submitted" | "first_approved") => reverse_key(person, kind, &res_key(id), why).map(|_| ()),
        Some("approved" | "taken") => reverse_key(person, kind, &use_key(id), why).map(|_| ()),
        _ => Ok(()),
    }
}

/// Withdraw a request: its owner while it waits or before it starts, or a leave administrator.
fn cancel(input: Id) -> Result<Record> {
    let request = require("leave_request", &input.id, "leave request")?;
    let me = my_employee()?;
    let mine = me.as_ref().is_some_and(|m| text(&request, "employee") == text(m, "id"));
    let admin = is_admin()?;
    if !mine && !admin {
        return Err(Error::msg("only the person (or a leave administrator) can cancel this request"));
    }
    match text(&request, "state") {
        Some("submitted" | "first_approved") => {}
        Some("approved") if date_of(&request, "start_date")? > today_date()? || admin => {}
        Some("approved") => return Err(Error::msg("this leave has started: a leave administrator can revert it")),
        _ => return Err(Error::msg("only a waiting or upcoming request can be cancelled")),
    }
    release(&request, "cancelled")?;
    db::update::<Record>("leave_request", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the request is gone"))
}

/// Undo approved or taken leave, with a reason (leave administrators). The days go back through a
/// reversal, so the ledger shows both what happened and that it was undone.
fn revert(input: Decide) -> Result<Record> {
    require_admin()?;
    let request = require("leave_request", &input.id, "leave request")?;
    if !matches!(text(&request, "state"), Some("approved" | "taken")) {
        return Err(Error::msg("only approved or taken leave can be reverted"));
    }
    let why = input.note.as_deref().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| Error::msg("say why the leave is reverted"))?;
    release(&request, why)?;
    db::update::<Record>("leave_request", &input.id, &json!({ "state": "cancelled", "decision_note": why }))?
        .ok_or_else(|| Error::msg("the request is gone"))
}

fn search(input: Search) -> Result<Vec<Record>> {
    let mut filter = Filter::all();
    if let Some(employee) = &input.employee {
        filter = filter.and(Filter::eq("employee", employee.as_str()));
    }
    if let Some(state) = &input.state {
        filter = filter.and(Filter::eq("state", state.as_str()));
    }
    db::find::<Record>("leave_request").matching(filter).order_by("-start_date").limit(input.limit.unwrap_or(100).min(500)).all()
}

/// Requests waiting for the caller: those of the people who report to them, and (for a leave
/// administrator) anything waiting for HR.
fn waiting_for_me() -> Result<Vec<Record>> {
    let me = my_employee()?;
    let mut out: Vec<Record> = Vec::new();
    if let Some(me) = &me {
        let reports: Vec<Record> = plugins::call("hr", "list_employees", &json!({ "manager": id_of(me)?, "limit": 500 }))?;
        let ids: Vec<&str> = reports.iter().filter_map(|e| text(e, "id")).collect();
        if !ids.is_empty() {
            out.extend(
                db::find::<Record>("leave_request")
                    .matching(Filter::eq("state", "submitted").and(Filter::ne("validation", "hr")).and(Filter::one_of("employee", ids)))
                    .order_by("start_date")
                    .limit(500)
                    .all()?,
            );
        }
    }
    if is_admin()? {
        let hr_step: Vec<Record> = db::find::<Record>("leave_request")
            .matching(Filter::eq("state", "first_approved").or(Filter::eq("state", "submitted").and(Filter::eq("validation", "hr"))))
            .order_by("start_date")
            .limit(500)
            .all()?;
        for request in hr_step {
            if !out.iter().any(|seen| text(seen, "id") == text(&request, "id")) {
                out.push(request);
            }
        }
    }
    // Never someone's own request.
    if let Some(me) = &me {
        out.retain(|request| text(request, "employee") != text(me, "id"));
    }
    Ok(out)
}

/// Who is away between two dates, with their names (those the caller may see).
fn who_is_away(input: Range) -> Result<Vec<Value>> {
    let (from, to) = (parse_date(&input.from)?, parse_date(&input.to)?);
    let away: Vec<Record> = db::find::<Record>("leave_request")
        .matching(
            Filter::one_of("state", ["approved", "taken"])
                .and(Filter::lte("start_date", format_date(to)))
                .and(Filter::gte("end_date", format_date(from))),
        )
        .order_by("start_date")
        .limit(500)
        .all()?;
    let mut names: HashMap<String, String> = HashMap::new();
    let mut out = Vec::new();
    for request in away {
        let id = text(&request, "employee").unwrap_or_default().to_string();
        if !names.contains_key(&id) {
            let person: Option<Record> = plugins::call("hr", "get_employee", &json!({ "id": id }))?;
            names.insert(id.clone(), person.as_ref().and_then(|p| text(p, "display_name")).unwrap_or("?").to_string());
        }
        out.push(json!({
            "employee": id, "name": names[&id], "leave_type": request["leave_type"],
            "start_date": request["start_date"], "end_date": request["end_date"], "days": request["days"],
        }));
    }
    Ok(out)
}

/// People whose approved leave has started go on leave, people whose leave has ended come back and
/// the request is marked taken. Running it again changes nothing.
fn roll(today: NaiveDate) -> Result<Value> {
    let started: Vec<Record> = db::find::<Record>("leave_request")
        .matching(Filter::eq("state", "approved").and(Filter::lte("start_date", format_date(today))))
        .limit(500)
        .all()?;
    let (mut away, mut back) = (0u64, 0u64);
    for request in started {
        let person_id = text(&request, "employee").unwrap_or_default().to_string();
        let person = employee(&person_id)?;
        if date_of(&request, "end_date")? >= today {
            if text(&person, "status") == Some("active") {
                let _: Record = plugins::call("hr", "change_employee_status", &json!({ "id": person_id, "status": "on_leave", "reason": "leave" }))?;
                away += 1;
            }
            continue;
        }
        db::update::<Record>("leave_request", id_of(&request)?, &json!({ "state": "taken" }))?;
        let still_away = db::count(
            "leave_request",
            Filter::eq("employee", person_id.as_str())
                .and(Filter::eq("state", "approved"))
                .and(Filter::lte("start_date", format_date(today)))
                .and(Filter::gte("end_date", format_date(today))),
        )?;
        if still_away == 0 && text(&person, "status") == Some("on_leave") {
            let _: Record = plugins::call("hr", "change_employee_status", &json!({ "id": person_id, "status": "active", "reason": "back from leave" }))?;
            back += 1;
        }
    }
    Ok(json!({ "went_on_leave": away, "came_back": back }))
}

#[derive(Deserialize)]
struct OnDate {
    employee: String,
    date: String,
}

/// Whether a person is on approved leave on a day: `none`, `half` or `full`. For attendance.
fn leave_on(input: OnDate) -> Result<Value> {
    let day = format_date(parse_date(&input.date)?);
    let found: Vec<Record> = db::find::<Record>("leave_request")
        .matching(
            Filter::eq("employee", input.employee.as_str())
                .and(Filter::one_of("state", ["approved", "taken"]))
                .and(Filter::lte("start_date", day.as_str()))
                .and(Filter::gte("end_date", day.as_str())),
        )
        .limit(10)
        .all()?;
    let cover = if found.is_empty() {
        "none"
    } else if found.iter().all(|r| text(r, "half_day").is_some_and(|h| h != "none") && text(r, "start_date") == text(r, "end_date")) {
        "half"
    } else {
        "full"
    };
    Ok(json!({ "cover": cover, "requests": found.iter().filter_map(|r| text(r, "reference")).collect::<Vec<_>>() }))
}

handler! {
    /// Whether a person is on approved leave on a day (for attendance).
    fn leave_on_date(input: OnDate) -> Value {
        leave_on(input)
    }

    /// What a request would cost in days, before asking.
    fn preview_leave_days(input: Preview) -> Value {
        preview(input)
    }

    fn request_leave(input: Ask) -> Record {
        submit(input)
    }

    fn approve_leave(input: Decide) -> Record {
        decide(input, true)
    }

    fn reject_leave(input: Decide) -> Record {
        decide(input, false)
    }

    fn cancel_leave(input: Id) -> Record {
        cancel(input)
    }

    /// Undo approved or taken leave, with a reason (leave administrators).
    fn revert_leave(input: Decide) -> Record {
        revert(input)
    }

    fn get_leave_request(input: Id) -> Option<Record> {
        db::get("leave_request", &input.id)
    }

    fn list_leave_requests(input: Search) -> Vec<Record> {
        search(input)
    }

    /// The caller's own requests.
    fn my_leave_requests(_: Empty) -> Vec<Record> {
        match my_employee()? {
            Some(me) => search(Search { employee: Some(id_of(&me)?.to_string()), ..Default::default() }),
            None => Ok(Vec::new()),
        }
    }

    fn leave_waiting_for_me(_: Empty) -> Vec<Record> {
        waiting_for_me()
    }

    fn who_is_away_between(input: Range) -> Vec<Value> {
        who_is_away(input)
    }

    /// Nightly: earned leave, lapsed days, and people going on leave and coming back.
    fn leave_nightly(_: Empty) -> Value {
        let today = today_date()?;
        let ledger = nightly_ledger(today)?;
        let status = roll(today)?;
        Ok(json!({ "ledger": ledger, "status": status }))
    }
}
