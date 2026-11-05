//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, next_number, require, text, today, Record};

use aether_sdk::dates::{parse_date, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

pub const ADMIN_ROLE: &str = "hr_loan.loan_admin";

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

pub fn decimal_of(record: &Record, field: &str) -> Result<Option<Decimal>> {
    match record.get(field).filter(|v| !v.is_null()) {
        Some(value) => serde_json::from_value(value.clone()).map(Some).map_err(|e| Error::msg(format!("{field}: {e}"))),
        None => Ok(None),
    }
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

/// The first person above this employee who is still working here.
pub fn approver_of(employee_id: &str) -> Result<Option<String>> {
    let chain: Vec<Record> = plugins::call("hr", "employee_chain", &json!({ "id": employee_id }))?;
    Ok(chain
        .iter()
        .find(|boss| matches!(text(boss, "status"), Some("active" | "probation") | None) && text(boss, "id") != Some(employee_id))
        .and_then(|boss| text(boss, "id").map(str::to_string)))
}
