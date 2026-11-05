//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, next_number, pick, require, text, today, Record};

use aether_sdk::dates::{format_date, parse_datetime, weekdays, Datelike, Duration, NaiveDate, NaiveDateTime};
use aether_sdk::prelude::*;

use crate::rules::{DayKind, LeaveCover};

pub const ADMIN_ROLE: &str = "hr_attendance.att_admin";

pub fn is_admin() -> Result<bool> {
    Ok(context::current()?.has_role(ADMIN_ROLE))
}

pub fn require_admin() -> Result<()> {
    context::current()?.require_role(ADMIN_ROLE)
}

/// Now, in UTC, to the second.
pub fn now_utc() -> Result<NaiveDateTime> {
    let now = context::current()?.now;
    if now.is_empty() {
        return Err(Error::msg("the kernel did not say what time it is"));
    }
    parse_datetime(&now)
}

pub fn today_date() -> Result<NaiveDate> {
    aether_sdk::dates::parse_date(&today()?)
}

pub fn employee(id: &str) -> Result<Record> {
    let found: Option<Record> = plugins::call("hr", "get_employee", &json!({ "id": id }))?;
    found.ok_or_else(|| Error::msg(format!("there is no employee `{id}`")))
}

pub fn my_employee() -> Result<Option<Record>> {
    plugins::call("hr", "my_employee", &json!({}))
}

/// Whether the person is employed on the day: joined, and not gone.
pub fn employed_on(person: &Record, day: NaiveDate) -> bool {
    let joined = text(person, "hire_date").and_then(|d| aether_sdk::dates::parse_date(d).ok());
    let left = text(person, "end_date").and_then(|d| aether_sdk::dates::parse_date(d).ok());
    joined.is_none_or(|j| j <= day) && left.is_none_or(|l| day <= l)
}

/// What the person's holiday calendar says about a day.
pub fn day_kind(person: &Record, day: NaiveDate) -> Result<DayKind> {
    let mut subjects = vec![json!({ "kind": "employee", "id": id_of(person)? })];
    if let Some(company) = text(person, "company") {
        subjects.push(json!({ "kind": "company", "id": company }));
    }
    let resolved: Value = plugins::call("calendar", "resolve_calendar", &json!({ "subjects": subjects, "on": format_date(day) }))?;
    let Some(calendar) = resolved.get("calendar").and_then(Value::as_str) else {
        // No calendar assigned: Saturday and Sunday off.
        return Ok(if matches!(day.weekday(), aether_sdk::dates::Weekday::Sat | aether_sdk::dates::Weekday::Sun) { DayKind::WeeklyOff } else { DayKind::Working });
    };
    let span: Value = plugins::call("calendar", "calendar_span", &json!({ "calendar": calendar, "from": format_date(day), "to": format_date(day) }))?;
    let weekend: Vec<String> = span["weekend"].as_array().cloned().unwrap_or_default().iter().filter_map(|d| d.as_str().map(str::to_string)).collect();
    if weekdays(&weekend)?.contains(&day.weekday()) {
        return Ok(DayKind::WeeklyOff);
    }
    for holiday in span["holidays"].as_array().cloned().unwrap_or_default() {
        // Optional holidays and half-day holidays are working days.
        if holiday["kind"] != "optional" && holiday["is_half_day"] != true {
            return Ok(DayKind::Holiday);
        }
    }
    Ok(DayKind::Working)
}

/// Approved leave on the day.
pub fn leave_cover(employee_id: &str, day: NaiveDate) -> Result<LeaveCover> {
    let found: Value = plugins::call("hr_leave", "leave_on_date", &json!({ "employee": employee_id, "date": format_date(day) }))?;
    Ok(match found["cover"].as_str() {
        Some("full") => LeaveCover::Full,
        Some("half") => LeaveCover::Half,
        _ => LeaveCover::None,
    })
}

/// The day before and after, for finding which shift a punch near midnight belongs to.
pub fn around(day: NaiveDate) -> [NaiveDate; 3] {
    [day - Duration::days(1), day, day + Duration::days(1)]
}
