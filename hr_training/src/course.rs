//! Courses, sessions and budgets.

use aether_sdk::dates::{check_order, format_date, parse_date};
use aether_sdk::db::Filter;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{decimal_of, id_of, next_number, pick, require, require_admin, text, today_date, Record};
use crate::rules::budget_left;

const COURSE_FIELDS: &[&str] = &[
    "code", "name", "description", "mode", "hours", "skill", "grants_level", "pass_score", "validity_months", "cost_per_seat",
    "is_mandatory", "is_active",
];

#[derive(Deserialize)]
struct Change {
    id: String,
    #[serde(flatten)]
    fields: Record,
}

#[derive(Deserialize)]
struct NewSession {
    course: String,
    start_date: String,
    end_date: String,
    capacity: i64,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    trainer: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct SetBudget {
    department: String,
    year: i64,
    amount: Decimal,
    #[serde(default)]
    currency: Option<String>,
}

#[derive(Deserialize, Default)]
struct BudgetQuery {
    #[serde(default)]
    department: Option<String>,
    #[serde(default)]
    year: Option<i64>,
}

#[derive(Deserialize, Default)]
struct SessionSearch {
    #[serde(default)]
    course: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

fn check_course(data: &Record) -> Result<()> {
    for field in ["hours", "pass_score", "cost_per_seat"] {
        if decimal_of(data, field)?.is_some_and(|v| v.is_negative()) {
            return Err(Error::msg(format!("{field} cannot be negative")));
        }
    }
    if data.get("validity_months").and_then(Value::as_i64).is_some_and(|m| !(1..=600).contains(&m)) {
        return Err(Error::msg("validity is 1 to 600 months (leave it out for a certificate that does not expire)"));
    }
    let has_skill = text(data, "skill").is_some();
    let has_level = data.get("grants_level").and_then(Value::as_i64).is_some();
    if has_skill != has_level {
        return Err(Error::msg("a course that builds a skill names the skill and the level a pass credits"));
    }
    if data.get("grants_level").and_then(Value::as_i64).is_some_and(|l| l < 1) {
        return Err(Error::msg("a level starts at 1"));
    }
    Ok(())
}

fn new_course(input: Record) -> Result<Record> {
    require_admin()?;
    let data = pick(&input, COURSE_FIELDS);
    if text(&data, "code").is_none() || text(&data, "name").is_none() || text(&data, "mode").is_none() {
        return Err(Error::msg("a course needs a code, a name and a mode"));
    }
    check_course(&data)?;
    db::create("trn_course", &data).map_err(|e| e.or("could not create the course (the code may be taken)"))
}

fn change_course(input: Change) -> Result<Record> {
    require_admin()?;
    let existing = require("trn_course", &input.id, "course")?;
    let data = pick(&input.fields, COURSE_FIELDS);
    let mut merged = existing.clone();
    for (key, value) in data.as_object().cloned().unwrap_or_default() {
        merged[key] = value;
    }
    check_course(&merged)?;
    // Certificates already issued keep what they were issued under.
    db::update::<Record>("trn_course", &input.id, &data)?.ok_or_else(|| Error::msg("the course is gone"))
}

fn new_session(input: NewSession) -> Result<Record> {
    require_admin()?;
    let course = require("trn_course", &input.course, "course")?;
    if course.get("is_active") == Some(&json!(false)) {
        return Err(Error::msg("this course is no longer offered"));
    }
    let (from, to) = (parse_date(&input.start_date)?, parse_date(&input.end_date)?);
    check_order(from, Some(to), "session")?;
    if !(1..=10_000).contains(&input.capacity) {
        return Err(Error::msg("a session has 1 to 10000 seats"));
    }
    let mut data = json!({
        "reference": next_number("session", "SES-", 6)?, "course": input.course, "start_date": format_date(from), "end_date": format_date(to),
        "capacity": input.capacity, "state": "scheduled",
    });
    for (key, value) in [("location", &input.location), ("trainer", &input.trainer), ("notes", &input.notes)] {
        if let Some(value) = value {
            data[key] = json!(value);
        }
    }
    db::create("trn_session", &data)
}

fn cancel_session(input: Id) -> Result<Record> {
    require_admin()?;
    let session = require("trn_session", &input.id, "session")?;
    if text(&session, "state") != Some("scheduled") {
        return Err(Error::msg("only a scheduled session can be cancelled"));
    }
    let rows: Vec<Record> = db::find("trn_enrolment").filter("session", input.id.as_str()).limit(10_000).all()?;
    for row in rows.iter().filter(|r| matches!(text(r, "state"), Some("enrolled" | "waitlisted"))) {
        db::update::<Record>("trn_enrolment", id_of(row)?, &json!({ "state": "cancelled" }))?;
    }
    db::update("trn_session", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the session is gone"))
}

fn complete_session(input: Id) -> Result<Record> {
    require_admin()?;
    let session = require("trn_session", &input.id, "session")?;
    if text(&session, "state") != Some("scheduled") {
        return Err(Error::msg("only a scheduled session can be completed"));
    }
    if parse_date(text(&session, "start_date").unwrap_or_default())? > today_date()? {
        return Err(Error::msg("the session has not started"));
    }
    // Seats nobody got a result for are not silently passed: the planner must record them first.
    let open = db::count("trn_enrolment", Filter::eq("session", input.id.as_str()).and(Filter::eq("state", "enrolled")))?;
    if open > 0 {
        return Err(Error::msg(format!("{open} enrolment(s) have no result yet: record attended or absent for each")));
    }
    db::update("trn_session", &input.id, &json!({ "state": "completed" }))?.ok_or_else(|| Error::msg("the session is gone"))
}

/// Money spent against a department's budget in a year: seats that were booked and not cancelled.
pub fn spent(department: &str, year: i64) -> Result<Decimal> {
    let rows: Vec<Record> = db::find("trn_enrolment").filter("department", department).filter("budget_year", year).limit(10_000).all()?;
    let mut sum = Decimal::zero(2);
    for row in rows.iter().filter(|r| matches!(text(r, "state"), Some("enrolled" | "attended" | "absent"))) {
        if let Some(cost) = decimal_of(row, "cost")? {
            sum = sum + cost;
        }
    }
    Ok(sum)
}

pub fn budget_of(department: &str, year: i64) -> Result<Option<Record>> {
    db::find::<Record>("trn_budget").filter("department", department).filter("year", year).first()
}

fn set_budget(input: SetBudget) -> Result<Record> {
    require_admin()?;
    if input.amount.is_negative() {
        return Err(Error::msg("a budget cannot be negative"));
    }
    input.amount.with_scale(2).map_err(|_| Error::msg("an amount has at most 2 digits after the point"))?;
    let _: Record = plugins::call::<Option<Record>>("hr", "get_department", &json!({ "id": input.department }))?
        .ok_or_else(|| Error::msg("there is no such department"))?;
    match budget_of(&input.department, input.year)? {
        Some(existing) => {
            let used = spent(&input.department, input.year)?;
            if input.amount < used {
                return Err(Error::msg(format!("{used} is already committed this year: a budget cannot go below it")));
            }
            db::update("trn_budget", id_of(&existing)?, &json!({ "amount": input.amount, "currency": input.currency }))?.ok_or_else(|| Error::msg("the budget is gone"))
        }
        None => db::create("trn_budget", &json!({ "department": input.department, "year": input.year, "amount": input.amount, "currency": input.currency })),
    }
}

fn budgets(input: BudgetQuery) -> Result<Vec<Value>> {
    let mut query = db::find::<Record>("trn_budget").limit(500);
    if let Some(department) = input.department.as_deref() {
        query = query.filter("department", department);
    }
    if let Some(year) = input.year {
        query = query.filter("year", year);
    }
    let rows = query.all()?;
    let mut out = Vec::new();
    for row in rows {
        let department = text(&row, "department").unwrap_or_default();
        let year = row.get("year").and_then(Value::as_i64).unwrap_or(0);
        let used = spent(department, year)?;
        let amount = decimal_of(&row, "amount")?.unwrap_or_else(|| Decimal::zero(2));
        out.push(json!({ "department": department, "year": year, "amount": amount, "spent": used, "left": budget_left(amount, used) }));
    }
    Ok(out)
}

handler! {
    fn create_course(input: Record) -> Record {
        new_course(input)
    }

    fn update_course(input: Change) -> Record {
        change_course(input)
    }

    fn list_courses(_: Empty) -> Vec<Record> {
        db::find("trn_course").order_by("code").limit(500).all()
    }

    fn create_session(input: NewSession) -> Record {
        new_session(input)
    }

    /// Cancel a scheduled session and everyone's seat in it.
    fn cancel_session_of_course(input: Id) -> Record {
        cancel_session(input)
    }

    /// Close a session once every seat has a result.
    fn complete_training_session(input: Id) -> Record {
        complete_session(input)
    }

    fn list_sessions(input: SessionSearch) -> Vec<Record> {
        let mut query = db::find::<Record>("trn_session").order_by("-start_date").limit(500);
        if let Some(course) = input.course.as_deref() {
            query = query.filter("course", course);
        }
        if let Some(state) = input.state.as_deref() {
            query = query.filter("state", state);
        }
        query.all()
    }

    /// A department's training budget for a year; it cannot go below what is already committed.
    fn set_training_budget(input: SetBudget) -> Record {
        set_budget(input)
    }

    fn training_budgets(input: BudgetQuery) -> Vec<Value> {
        budgets(input)
    }
}
