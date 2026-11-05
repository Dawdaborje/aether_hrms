//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, next_number, pick, require, text, today, Record};

use aether_sdk::dates::{format_date, parse_date, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::rules::{cross_rate, exact_rate, RATE_SCALE};

pub const FINANCE: &str = "hr_expense.expense_finance";
pub const ADMIN: &str = "hr_expense.expense_admin";
pub const PROXY: &str = "hr_expense.expense_proxy";

pub fn is_finance() -> Result<bool> {
    let context = context::current()?;
    Ok(context.has_role(FINANCE) || context.has_role(ADMIN))
}

pub fn require_finance() -> Result<()> {
    if is_finance()? { Ok(()) } else { Err(Error::msg(format!("you need the role `{FINANCE}` to do this"))) }
}

pub fn require_admin() -> Result<()> {
    context::current()?.require_role(ADMIN)
}

pub fn today_date() -> Result<NaiveDate> {
    parse_date(&today()?)
}

pub fn employee(id: &str) -> Result<Record> {
    let found: Option<Record> = plugins::call("hr", "get_employee", &json!({ "id": id }))?;
    found.ok_or_else(|| Error::msg(format!("there is no employee `{id}`")))
}

pub fn my_employee() -> Result<Option<Record>> {
    plugins::call("hr", "my_employee", &json!({}))
}

/// Whether the person still works here.
pub fn works_here(employee: &Record) -> bool {
    matches!(text(employee, "status"), Some("active" | "probation" | "on_leave" | "suspended") | None)
}

/// The first active person above `employee` in the reporting line: the one who approves.
pub fn approver_of(employee_id: &str) -> Result<Option<String>> {
    let chain: Vec<Record> = plugins::call("hr", "employee_chain", &json!({ "id": employee_id }))?;
    Ok(chain.iter().find(|boss| works_here(boss) && text(boss, "id") != Some(employee_id)).and_then(|boss| text(boss, "id").map(str::to_string)))
}

/// Digits after the point of a currency, and proof that it exists and is active.
pub fn currency_places(code: &str) -> Result<u32> {
    let found: Option<Record> = plugins::call("currency", "get_currency_by_code", &json!({ "code": code }))?;
    let found = found.ok_or_else(|| Error::msg(format!("`{code}` is not a currency here")))?;
    if found.get("active") == Some(&json!(false)) {
        return Err(Error::msg(format!("the currency {code} is not in use")));
    }
    Ok(found.get("decimal_places").and_then(Value::as_u64).unwrap_or(2) as u32)
}

/// A currency's rate against the base on a day, as an exact decimal.
fn rate_on(code: &str, day: NaiveDate) -> Result<Decimal> {
    let found: Value = plugins::call("currency", "get_rate", &json!({ "code": code, "date": format_date(day) }))?;
    exact_rate(found.get("rate").and_then(Value::as_f64).ok_or_else(|| Error::msg(format!("no exchange rate for {code}")))?)
}

/// One unit of `from` in `to` on a day, to nine digits. The same currency is exactly 1.
pub fn rate_between(from: &str, to: &str, day: NaiveDate) -> Result<Decimal> {
    if from.eq_ignore_ascii_case(to) {
        return Decimal::whole(1, RATE_SCALE);
    }
    cross_rate(rate_on(from, day)?, rate_on(to, day)?)
}

/// A decimal field, zero when absent.
pub fn decimal_of(record: &Record, field: &str) -> Result<Decimal> {
    match record.get(field).filter(|v| !v.is_null()) {
        Some(value) => serde_json::from_value(value.clone()).map_err(|e| Error::msg(format!("{field}: {e}"))),
        None => Ok(Decimal::zero(2)),
    }
}

pub fn optional_decimal(record: &Record, field: &str) -> Result<Option<Decimal>> {
    match record.get(field).filter(|v| !v.is_null()) {
        Some(value) => serde_json::from_value(value.clone()).map(Some).map_err(|e| Error::msg(format!("{field}: {e}"))),
        None => Ok(None),
    }
}

pub fn flags_of(record: &Record) -> Vec<String> {
    text(record, "flags").map(|f| f.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect()).unwrap_or_default()
}

pub fn join(items: &[String]) -> String {
    items.join(",")
}
