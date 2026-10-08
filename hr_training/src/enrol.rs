//! Enrolment, waiting lists, results and certificates.
//!
//! * A seat goes to whoever asks while seats are free; after that people wait in the order they asked, and the
//!   first who fits the budget is promoted when a seat frees (Frappe has neither a capacity nor a queue).
//! * A seat's price is copied from the course when it is booked and charged to the person's department for the
//!   year of the session; a budget refuses the seat that would overspend it unless a planner states why.
//! * Only a training administrator records a result, never for themselves. A pass issues one certificate (by
//!   enrolment, so recording twice cannot issue two) and credits the course's skill level, raise-only.
//! * A result is final; a wrong one is corrected by revoking the certificate with a reason.

use aether_sdk::dates::{format_date, parse_date, Datelike, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{
    actor, decimal_of, employee, id_of, is_admin, my_employee, require, require_admin, text, today_date, Record,
};
use crate::course::{budget_of, spent};
use crate::rules::{expiry, fits, passes, seat, sessions_overlap, standing, Seat, Standing};

const WARN_DAYS: i64 = 30;

#[derive(Deserialize)]
struct Enrol {
    session: String,
    #[serde(default)]
    employee: Option<String>,
    #[serde(default)]
    override_reason: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Result_ {
    enrolment: String,
    attended: bool,
    #[serde(default)]
    score: Option<Decimal>,
}

#[derive(Deserialize)]
struct Revoke {
    id: String,
    reason: String,
}

#[derive(Deserialize, Default)]
struct Search {
    #[serde(default)]
    employee: Option<String>,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

fn taken(session: &str) -> Result<u64> {
    let rows: Vec<Record> = db::find("trn_enrolment").filter("session", session).limit(10_000).all()?;
    Ok(rows.iter().filter(|r| matches!(text(r, "state"), Some("enrolled" | "attended" | "absent"))).count() as u64)
}

fn dates_of(session: &Record) -> Result<(NaiveDate, NaiveDate)> {
    Ok((parse_date(text(session, "start_date").unwrap_or_default())?, parse_date(text(session, "end_date").unwrap_or_default())?))
}

/// The person's valid certificate for a course, if any.
fn valid_certificate(person: &str, course: &str, today: NaiveDate) -> Result<Option<Record>> {
    let rows: Vec<Record> = db::find("trn_certificate").filter("employee", person).filter("course", course).filter("state", "valid").limit(50).all()?;
    for row in rows {
        let end = text(&row, "expires_on").map(parse_date).transpose()?;
        if standing(today, end, 0) != Standing::Expired {
            return Ok(Some(row));
        }
    }
    Ok(None)
}

fn enrol(input: Enrol) -> Result<Record> {
    let session = require("trn_session", &input.session, "session")?;
    if text(&session, "state") != Some("scheduled") {
        return Err(Error::msg("this session is not open for enrolment"));
    }
    let today = today_date()?;
    let (start, end) = dates_of(&session)?;
    if start < today {
        return Err(Error::msg("this session has already started"));
    }
    let course = require("trn_course", text(&session, "course").unwrap_or_default(), "course")?;
    let me = my_employee()?;
    let person_id = match (&input.employee, &me) {
        (Some(other), Some(me)) if other == id_of(me)? => other.clone(),
        (Some(other), _) => other.clone(),
        (None, Some(me)) => id_of(me)?.to_string(),
        (None, None) => return Err(Error::msg("name the employee: you have no employee record")),
    };
    let person = employee(&person_id)?;
    let myself = me.as_ref().is_some_and(|m| text(m, "id") == Some(person_id.as_str()));
    let their_manager = match (&me, text(&person, "manager")) {
        (Some(me), Some(manager)) => text(me, "id") == Some(manager),
        _ => false,
    };
    if !myself && !their_manager && !is_admin()? {
        return Err(Error::msg("you can enrol yourself, your reports, or anyone if you run training"));
    }
    if !matches!(text(&person, "status"), Some("active" | "probation") | None) {
        return Err(Error::msg("only someone who is working can be enrolled"));
    }
    // Already certified and not about to lapse: no second seat.
    if let Some(certificate) = valid_certificate(&person_id, id_of(&course)?, today)? {
        let until = text(&certificate, "expires_on").map(parse_date).transpose()?;
        if standing(today, until, WARN_DAYS) == Standing::Valid {
            return Err(Error::msg("this person already holds a valid certificate for this course"));
        }
    }
    // One seat at a time: no two sessions the person is in on the same days.
    let mine: Vec<Record> = db::find("trn_enrolment").filter("employee", person_id.as_str()).limit(500).all()?;
    let mut existing: Option<Record> = None;
    for row in mine {
        if text(&row, "session") == Some(input.session.as_str()) {
            existing = Some(row);
            continue;
        }
        if !matches!(text(&row, "state"), Some("enrolled" | "waitlisted")) {
            continue;
        }
        let other = require("trn_session", text(&row, "session").unwrap_or_default(), "session")?;
        if text(&other, "state") == Some("scheduled") && sessions_overlap((start, end), dates_of(&other)?) {
            return Err(Error::msg("this person is already booked on another session on those days"));
        }
    }
    if existing.as_ref().is_some_and(|e| matches!(text(e, "state"), Some("enrolled" | "waitlisted"))) {
        return Err(Error::msg("this person is already in this session"));
    }
    if existing.as_ref().is_some_and(|e| matches!(text(e, "state"), Some("attended" | "absent"))) {
        return Err(Error::msg("this person already has a result in this session"));
    }

    let capacity = session.get("capacity").and_then(Value::as_u64).unwrap_or(0);
    let place = seat(capacity, taken(&input.session)?);
    let price = decimal_of(&course, "cost_per_seat")?.unwrap_or_else(|| Decimal::zero(2));
    let department = text(&person, "department").map(str::to_string);
    let year = i64::from(start.year());
    let mut override_reason = None;
    if place == Seat::Enrolled {
        if let (Some(department), false) = (&department, price.is_zero()) {
            if let Some(budget) = budget_of(department, year)? {
                let amount = decimal_of(&budget, "amount")?.unwrap_or_else(|| Decimal::zero(2));
                if !fits(amount, spent(department, year)?, price) {
                    match input.override_reason.as_deref().filter(|r| r.trim().len() >= 5) {
                        Some(reason) if is_admin()? => override_reason = Some(reason.trim().to_string()),
                        _ => return Err(Error::msg("the department's training budget for the year cannot cover this seat: a training administrator can book it with a reason")),
                    }
                }
            }
        }
    }
    let now = context::current()?.now;
    let mut data = json!({
        "session": input.session, "course": id_of(&course)?, "employee": person_id, "cost": price, "budget_year": year,
        "state": if place == Seat::Enrolled { "enrolled" } else { "waitlisted" }, "queued_at": now, "enrolled_by": actor()?,
    });
    if let Some(department) = &department {
        data["department"] = json!(department);
    }
    if let Some(reason) = override_reason {
        data["override_reason"] = json!(reason);
    }
    match existing {
        Some(row) => db::update("trn_enrolment", id_of(&row)?, &data)?.ok_or_else(|| Error::msg("the enrolment is gone")),
        None => db::create("trn_enrolment", &data),
    }
}

/// Fill free seats from the waiting list, oldest first; someone the budget cannot cover keeps their place.
fn promote(session_id: &str) -> Result<u64> {
    let session = require("trn_session", session_id, "session")?;
    if text(&session, "state") != Some("scheduled") {
        return Ok(0);
    }
    let capacity = session.get("capacity").and_then(Value::as_u64).unwrap_or(0);
    let year = i64::from(dates_of(&session)?.0.year());
    let mut moved = 0;
    let waiting: Vec<Record> = db::find::<Record>("trn_enrolment").filter("session", session_id).filter("state", "waitlisted").order_by("queued_at").limit(1000).all()?;
    for row in waiting {
        if taken(session_id)? >= capacity {
            break;
        }
        let price = decimal_of(&row, "cost")?.unwrap_or_else(|| Decimal::zero(2));
        if let (Some(department), false) = (text(&row, "department"), price.is_zero()) {
            if let Some(budget) = budget_of(department, year)? {
                let amount = decimal_of(&budget, "amount")?.unwrap_or_else(|| Decimal::zero(2));
                if !fits(amount, spent(department, year)?, price) {
                    continue;
                }
            }
        }
        db::update::<Record>("trn_enrolment", id_of(&row)?, &json!({ "state": "enrolled" }))?;
        moved += 1;
    }
    Ok(moved)
}

fn cancel(input: Id) -> Result<Record> {
    let row = require("trn_enrolment", &input.id, "enrolment")?;
    if !matches!(text(&row, "state"), Some("enrolled" | "waitlisted")) {
        return Err(Error::msg("only a seat or a place in the queue can be cancelled"));
    }
    let person = employee(text(&row, "employee").unwrap_or_default())?;
    let me = my_employee()?;
    let myself = me.as_ref().is_some_and(|m| text(m, "id") == text(&person, "id"));
    let their_manager = match (&me, text(&person, "manager")) {
        (Some(me), Some(manager)) => text(me, "id") == Some(manager),
        _ => false,
    };
    if !myself && !their_manager && !is_admin()? {
        return Err(Error::msg("only the person, their manager or a training administrator can cancel this"));
    }
    let session = require("trn_session", text(&row, "session").unwrap_or_default(), "session")?;
    if dates_of(&session)?.0 <= today_date()? && !is_admin()? {
        return Err(Error::msg("the session has started: a training administrator must cancel this"));
    }
    let cancelled: Record = db::update("trn_enrolment", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the enrolment is gone"))?;
    promote(text(&row, "session").unwrap_or_default())?;
    Ok(cancelled)
}

fn record(input: Result_) -> Result<Record> {
    require_admin()?;
    let row = require("trn_enrolment", &input.enrolment, "enrolment")?;
    if text(&row, "state") != Some("enrolled") {
        return Err(Error::msg("a result can be recorded once, for someone who holds a seat"));
    }
    let me = my_employee()?;
    if me.as_ref().is_some_and(|m| text(m, "id") == text(&row, "employee")) {
        return Err(Error::msg("nobody records their own result"));
    }
    let session = require("trn_session", text(&row, "session").unwrap_or_default(), "session")?;
    if dates_of(&session)?.0 > today_date()? {
        return Err(Error::msg("the session has not started"));
    }
    if text(&session, "state") == Some("cancelled") {
        return Err(Error::msg("the session was cancelled"));
    }
    let course = require("trn_course", text(&row, "course").unwrap_or_default(), "course")?;
    if let Some(score) = input.score {
        if score.is_negative() {
            return Err(Error::msg("a score cannot be negative"));
        }
    }
    let passed = passes(input.attended, input.score, decimal_of(&course, "pass_score")?);
    let mut data = json!({ "state": if input.attended { "attended" } else { "absent" }, "passed": passed, "result_by": actor()? });
    if let Some(score) = input.score {
        data["score"] = json!(score);
    }
    let mut updated: Record = db::update("trn_enrolment", &input.enrolment, &data)?.ok_or_else(|| Error::msg("the enrolment is gone"))?;
    if passed {
        let certificate = issue(&row, &course)?;
        updated = db::update("trn_enrolment", &input.enrolment, &json!({ "certificate": certificate["id"] }))?.ok_or_else(|| Error::msg("the enrolment is gone"))?;
    }
    Ok(updated)
}

fn credit_skill(person: &str, course: &Record, certificate: &str) -> String {
    let (Some(skill), Some(level)) = (text(course, "skill"), course.get("grants_level").and_then(Value::as_i64)) else { return "none".into() };
    let done: Result<Value> = plugins::call(
        "hr_performance",
        "credit_skill",
        &json!({ "employee": person, "skill": skill, "level": level, "source": format!("training certificate {certificate}") }),
    );
    match done {
        Ok(v) if v["credited"] == true => format!("level {level}"),
        Ok(_) => "already at or above".into(),
        Err(error) => format!("failed: {error}"),
    }
}

fn issue(enrolment: &Record, course: &Record) -> Result<Record> {
    let person = text(enrolment, "employee").unwrap_or_default();
    let today = today_date()?;
    let months = course.get("validity_months").and_then(Value::as_u64).and_then(|m| u32::try_from(m).ok());
    let mut data = json!({
        "employee": person, "course": id_of(course)?, "enrolment": id_of(enrolment)?, "issued_on": format_date(today), "state": "valid",
    });
    if let Some(end) = expiry(today, months) {
        data["expires_on"] = json!(format_date(end));
    }
    let made: Record = db::create("trn_certificate", &data).map_err(|e| e.or("could not issue the certificate"))?;
    let credit = credit_skill(person, course, id_of(&made)?);
    let made = db::update("trn_certificate", id_of(&made)?, &json!({ "skill_credit": credit }))?.unwrap_or(made);
    events::emit("certificate_issued", &json!({ "employee": person, "course": course["id"], "certificate": made["id"] }))?;
    Ok(made)
}

fn revoke(input: Revoke) -> Result<Record> {
    require_admin()?;
    if input.reason.trim().len() < 5 {
        return Err(Error::msg("say why the certificate is revoked"));
    }
    let row = require("trn_certificate", &input.id, "certificate")?;
    if text(&row, "state") != Some("valid") {
        return Err(Error::msg("this certificate is already revoked"));
    }
    // The skill credit stays: levels are a history of what was assessed, corrected by a person with a rating.
    db::update("trn_certificate", &input.id, &json!({ "state": "revoked", "revoked_reason": input.reason.trim() }))?.ok_or_else(|| Error::msg("the certificate is gone"))
}

fn certificates(input: Search) -> Result<Vec<Value>> {
    let mut query = db::find::<Record>("trn_certificate").order_by("-issued_on").limit(1000);
    if let Some(person) = input.employee.as_deref() {
        query = query.filter("employee", person);
    }
    let today = today_date()?;
    let mut out = Vec::new();
    for row in query.all()? {
        let end = text(&row, "expires_on").map(parse_date).transpose()?;
        let standing = if text(&row, "state") == Some("revoked") {
            "revoked"
        } else {
            match standing(today, end, WARN_DAYS) {
                Standing::Valid => "valid",
                Standing::ExpiringSoon => "expiring_soon",
                Standing::Expired => "expired",
            }
        };
        if input.state.as_deref().is_some_and(|s| s != standing) {
            continue;
        }
        let mut row = row;
        row["standing"] = json!(standing);
        out.push(row);
    }
    Ok(out)
}

/// Active people missing a valid certificate for each mandatory course.
fn mandatory_gaps() -> Result<Vec<Value>> {
    let courses: Vec<Record> = db::find::<Record>("trn_course").filter("is_mandatory", true).filter("is_active", true).limit(100).all()?;
    if courses.is_empty() {
        return Ok(Vec::new());
    }
    let today = today_date()?;
    let mut people: Vec<Record> = Vec::new();
    let mut offset = 0u32;
    loop {
        let page: Vec<Record> = plugins::call("hr", "list_employees", &json!({ "limit": 200, "offset": offset }))?;
        let count = page.len();
        people.extend(page.into_iter().filter(|p| matches!(text(p, "status"), Some("active" | "probation") | None)));
        if count < 200 {
            break;
        }
        offset += 200;
    }
    let mut out = Vec::new();
    for course in &courses {
        for person in &people {
            if valid_certificate(id_of(person)?, id_of(course)?, today)?.is_none() {
                out.push(json!({ "employee": person["id"], "name": person["display_name"], "course": course["id"], "code": course["code"] }));
            }
        }
    }
    Ok(out)
}

/// Tell people when a certificate has 30, 7 or 0 days left.
fn nightly() -> Result<Value> {
    let today = today_date()?;
    let rows: Vec<Record> = db::find::<Record>("trn_certificate").filter("state", "valid").limit(10_000).all()?;
    let mut warned = 0;
    for row in rows {
        let Some(end) = text(&row, "expires_on").map(parse_date).transpose()? else { continue };
        let left = (end - today).num_days();
        if matches!(left, 30 | 7 | 0) {
            events::emit("certificate_expiring", &json!({ "employee": row["employee"], "course": row["course"], "certificate": row["id"], "days_left": left }))?;
            warned += 1;
        }
    }
    Ok(json!({ "warned": warned }))
}

handler! {
    /// Take a seat (or a place in the queue): yourself, your report, or anyone if you run training.
    fn enrol_in_session(input: Enrol) -> Record {
        enrol(input)
    }

    fn cancel_enrolment(input: Id) -> Record {
        cancel(input)
    }

    /// Record attendance and score (training administrators; never for oneself). A pass issues the certificate.
    fn record_training_result(input: Result_) -> Record {
        record(input)
    }

    fn revoke_certificate(input: Revoke) -> Record {
        revoke(input)
    }

    /// Certificates with their standing today (valid, expiring_soon, expired, revoked).
    fn list_certificates(input: Search) -> Vec<Value> {
        certificates(input)
    }

    fn list_enrolments(input: Search) -> Vec<Record> {
        let mut query = db::find::<Record>("trn_enrolment").order_by("queued_at").limit(1000);
        if let Some(person) = input.employee.as_deref() {
            query = query.filter("employee", person);
        }
        if let Some(session) = input.session.as_deref() {
            query = query.filter("session", session);
        }
        if let Some(state) = input.state.as_deref() {
            query = query.filter("state", state);
        }
        query.all()
    }

    /// Who lacks a valid certificate for a mandatory course.
    fn mandatory_training_gaps(_: Empty) -> Vec<Value> {
        require_admin()?;
        mandatory_gaps()
    }

    fn training_nightly(_: Empty) -> Value {
        nightly()
    }
}
