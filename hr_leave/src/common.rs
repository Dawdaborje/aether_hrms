//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, is_off, next_number, pick, require, text, today, Record};

use aether_sdk::dates::{parse_date, weekdays, Calendar, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

/// The role that sets leave up and may override its limits.
pub const ADMIN_ROLE: &str = "hr_leave.leave_admin";

/// Whether the caller administers leave.
pub fn is_admin() -> Result<bool> {
    Ok(context::current()?.has_role(ADMIN_ROLE))
}

pub fn require_admin() -> Result<()> {
    context::current()?.require_role(ADMIN_ROLE)
}

pub fn today_date() -> Result<NaiveDate> {
    parse_date(&today()?)
}

/// A date field that must be there.
pub fn date_of(record: &Record, field: &str) -> Result<NaiveDate> {
    parse_date(text(record, field).ok_or_else(|| Error::msg(format!("the record has no {field}")))?)
}

/// A decimal field, zero when absent.
pub fn decimal_of(record: &Record, field: &str) -> Result<Decimal> {
    match record.get(field).filter(|v| !v.is_null()) {
        Some(value) => serde_json::from_value(value.clone()).map_err(|e| Error::msg(format!("{field}: {e}"))),
        None => Ok(Decimal::zero(2)),
    }
}

/// The employee, through `hr`.
pub fn employee(id: &str) -> Result<Record> {
    let found: Option<Record> = plugins::call("hr", "get_employee", &json!({ "id": id }))?;
    found.ok_or_else(|| Error::msg(format!("there is no employee `{id}`")))
}

/// The caller's own employee record, if they have one.
pub fn my_employee() -> Result<Option<Record>> {
    plugins::call("hr", "my_employee", &json!({}))
}

/// Whether the person is employed and can take leave: not ended, not suspended.
pub fn can_take_leave(employee: &Record) -> bool {
    matches!(text(employee, "status"), Some("active" | "probation" | "on_leave") | None)
}

/// Whether the person works here at all (suspended included), for yearly grants.
pub fn works_here(employee: &Record) -> bool {
    matches!(text(employee, "status"), Some("active" | "probation" | "on_leave" | "suspended") | None)
}

/// The working calendar that applies to a person between two days: their own holiday calendar if
/// they have one, else their company's, else only Saturdays and Sundays off.
pub fn calendar_for(employee: &Record, from: NaiveDate, to: NaiveDate) -> Result<Calendar> {
    let mut subjects = vec![json!({ "kind": "employee", "id": id_of(employee)? })];
    if let Some(company) = text(employee, "company") {
        subjects.push(json!({ "kind": "company", "id": company }));
    }
    let resolved: Value = plugins::call("calendar", "resolve_calendar", &json!({ "subjects": subjects, "on": aether_sdk::dates::format_date(from) }))?;
    let Some(calendar) = resolved.get("calendar").and_then(Value::as_str) else { return Ok(Calendar::default()) };
    let span: Value = plugins::call(
        "calendar",
        "calendar_span",
        &json!({ "calendar": calendar, "from": aether_sdk::dates::format_date(from), "to": aether_sdk::dates::format_date(to) }),
    )?;
    let weekend: Vec<String> = span["weekend"].as_array().cloned().unwrap_or_default().iter().filter_map(|d| d.as_str().map(str::to_string)).collect();
    let mut calendar = Calendar { weekend: weekdays(&weekend)?, holidays: Vec::new() };
    for holiday in span["holidays"].as_array().cloned().unwrap_or_default() {
        // Optional holidays are working days; a half-day holiday is a working day too (it is
        // not free in full).
        if holiday["kind"] == "optional" || holiday["is_half_day"] == true {
            continue;
        }
        if let Some(day) = holiday["date"].as_str().and_then(|d| parse_date(d).ok()) {
            calendar.holidays.push(day);
        }
    }
    Ok(calendar)
}
