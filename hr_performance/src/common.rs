//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, pick, require, text, today, Record};

use aether_sdk::dates::{parse_date, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::rules::Phase;

pub const ADMIN_ROLE: &str = "hr_performance.perf_admin";

pub fn is_admin() -> Result<bool> {
    Ok(context::current()?.has_role(ADMIN_ROLE))
}

pub fn require_admin() -> Result<()> {
    context::current()?.require_role(ADMIN_ROLE)
}

pub fn today_date() -> Result<NaiveDate> {
    parse_date(&today()?)
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

/// The caller is this person, or their direct manager, or runs performance reviews.
pub fn may_act_for(employee_record: &Record) -> Result<bool> {
    if is_admin()? {
        return Ok(true);
    }
    let Some(me) = my_employee()? else { return Ok(false) };
    Ok(text(&me, "id") == text(employee_record, "id") || text(employee_record, "manager").is_some_and(|m| Some(m) == text(&me, "id")))
}

pub fn int(record: &Record, field: &str) -> i64 {
    record.get(field).and_then(Value::as_i64).unwrap_or(0)
}

pub fn dec(record: &Record, field: &str) -> Result<Option<Decimal>> {
    match record.get(field).filter(|v| !v.is_null()) {
        Some(value) => serde_json::from_value(value.clone()).map(Some).map_err(|e| Error::msg(format!("{field}: {e}"))),
        None => Ok(None),
    }
}

/// `[{ "name": ..., "weight": ... }]` as pairs.
pub fn weighted(value: &Value, what: &str) -> Result<Vec<(String, i64)>> {
    let list = value.as_array().ok_or_else(|| Error::msg(format!("{what} is a list of {{ name, weight }}")))?;
    list.iter()
        .map(|item| {
            Ok((
                item.get("name").and_then(Value::as_str).ok_or_else(|| Error::msg(format!("{what}: an entry has no name")))?.to_string(),
                item.get("weight").and_then(Value::as_i64).ok_or_else(|| Error::msg(format!("{what}: an entry has no weight")))?,
            ))
        })
        .collect()
}

pub fn phase_of(cycle: &Record) -> Result<Phase> {
    Phase::parse(text(cycle, "status").unwrap_or_default()).ok_or_else(|| Error::msg("a cycle has an unknown status"))
}
