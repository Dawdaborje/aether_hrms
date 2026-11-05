//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, next_number, pick, require, text, today, Record};

use aether_sdk::dates::{parse_date, weekdays, Calendar, NaiveDate};
use aether_sdk::prelude::*;

pub const MANAGER_ROLE: &str = "hr_onboarding.ob_manager";
pub const OFFICER_ROLE: &str = "hr_onboarding.ob_officer";

pub fn is_manager() -> Result<bool> {
    Ok(context::current()?.has_role(MANAGER_ROLE))
}

pub fn is_officer() -> Result<bool> {
    let context = context::current()?;
    Ok(context.has_role(OFFICER_ROLE) || context.has_role(MANAGER_ROLE))
}

pub fn require_officer() -> Result<()> {
    if is_officer()? { Ok(()) } else { Err(Error::msg(format!("you need the role `{OFFICER_ROLE}` to do this"))) }
}

pub fn require_manager() -> Result<()> {
    context::current()?.require_role(MANAGER_ROLE)
}

pub fn today_date() -> Result<NaiveDate> {
    parse_date(&today()?)
}

pub fn caller_user() -> Result<Option<String>> {
    Ok(context::current()?.actor.id)
}

pub fn my_employee() -> Result<Option<Record>> {
    plugins::call("hr", "my_employee", &json!({}))
}

pub fn employee(id: &str) -> Result<Record> {
    let found: Option<Record> = plugins::call("hr", "get_employee", &json!({ "id": id }))?;
    found.ok_or_else(|| Error::msg(format!("there is no employee `{id}`")))
}

pub fn works_here(employee: &Record) -> bool {
    matches!(text(employee, "status"), Some("active" | "probation" | "on_leave" | "suspended") | None)
}

pub fn user_of(employee: &Record) -> Option<String> {
    text(employee, "user").filter(|u| !u.is_empty()).map(str::to_string)
}

/// The working calendar for a person between two days (their own, else their company's, else a
/// Saturday and Sunday weekend); a person who is not an employee yet gets the default.
pub fn calendar_for(employee: Option<&Record>, from: NaiveDate, to: NaiveDate) -> Result<Calendar> {
    let Some(employee) = employee else { return Ok(Calendar::default()) };
    let mut subjects = vec![json!({ "kind": "employee", "id": id_of(employee)? })];
    if let Some(company) = text(employee, "company") {
        subjects.push(json!({ "kind": "company", "id": company }));
    }
    let resolved: Value = plugins::call("calendar", "resolve_calendar", &json!({ "subjects": subjects, "on": aether_sdk::dates::format_date(from) }))?;
    let Some(calendar) = resolved.get("calendar").and_then(Value::as_str) else { return Ok(Calendar::default()) };
    let (from, to) = (aether_sdk::dates::format_date(from), aether_sdk::dates::format_date(to));
    let span: Value = plugins::call("calendar", "calendar_span", &json!({ "calendar": calendar, "from": from, "to": to }))?;
    let weekend: Vec<String> = span["weekend"].as_array().cloned().unwrap_or_default().iter().filter_map(|d| d.as_str().map(str::to_string)).collect();
    let mut calendar = Calendar { weekend: weekdays(&weekend)?, holidays: Vec::new() };
    for holiday in span["holidays"].as_array().cloned().unwrap_or_default() {
        if holiday["kind"] == "optional" || holiday["is_half_day"] == true {
            continue;
        }
        if let Some(day) = holiday["date"].as_str().and_then(|d| parse_date(d).ok()) {
            calendar.holidays.push(day);
        }
    }
    Ok(calendar)
}
