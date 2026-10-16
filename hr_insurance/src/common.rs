//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, next_number, require, text, today, Record};

use aether_sdk::dates::{parse_date, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

pub const ADMIN_ROLE: &str = "hr_insurance.ins_admin";
pub const REVIEWER_ROLE: &str = "hr_insurance.ins_reviewer";

pub fn require_admin() -> Result<()> {
    context::current()?.require_role(ADMIN_ROLE)
}

/// Administrators and reviewers decide claims and run the money side.
pub fn is_staff() -> Result<bool> {
    let ctx = context::current()?;
    Ok(ctx.has_role(ADMIN_ROLE) || ctx.has_role(REVIEWER_ROLE))
}

pub fn require_staff() -> Result<()> {
    if is_staff()? {
        Ok(())
    } else {
        Err(Error::msg("this is for insurance administrators and reviewers"))
    }
}

pub fn today_date() -> Result<NaiveDate> {
    parse_date(&today()?)
}

pub fn date_of(record: &Record, field: &str) -> Result<NaiveDate> {
    parse_date(text(record, field).ok_or_else(|| Error::msg(format!("`{field}` is missing")))?)
}

pub fn date_opt(record: &Record, field: &str) -> Result<Option<NaiveDate>> {
    text(record, field).map(parse_date).transpose()
}

pub fn decimal_of(record: &Record, field: &str, places: u32) -> Result<Decimal> {
    match record.get(field).filter(|v| !v.is_null()) {
        Some(value) => {
            let d: Decimal = serde_json::from_value(value.clone()).map_err(|e| Error::msg(format!("{field}: {e}")))?;
            d.with_scale(places)
        }
        None => Ok(Decimal::zero(places)),
    }
}

pub fn flag(record: &Record, field: &str) -> bool {
    record.get(field) == Some(&json!(true))
}

pub fn actor() -> Result<Option<String>> {
    Ok(context::current()?.actor.id)
}

pub fn my_employee() -> Result<Option<Record>> {
    plugins::call("hr", "my_employee", &json!({}))
}

pub fn my_employee_id() -> Result<Option<String>> {
    Ok(my_employee()?.and_then(|m| text(&m, "id").map(str::to_string)))
}

pub fn employee(id: &str) -> Result<Record> {
    let found: Option<Record> = plugins::call("hr", "get_employee", &json!({ "id": id }))?;
    found.ok_or_else(|| Error::msg(format!("there is no employee `{id}`")))
}

/// Digits after the point of a currency, and proof that it exists and is in use.
pub fn currency_places(code: &str) -> Result<u32> {
    let found: Option<Record> = plugins::call("currency", "get_currency_by_code", &json!({ "code": code }))?;
    let found = found.ok_or_else(|| Error::msg(format!("`{code}` is not a currency here")))?;
    if found.get("active") == Some(&json!(false)) {
        return Err(Error::msg(format!("the currency {code} is not in use")));
    }
    Ok(found.get("decimal_places").and_then(Value::as_u64).unwrap_or(2) as u32)
}

/// The caller may see this employee's records: their own, or staff.
pub fn require_self_or_staff(employee_id: &str) -> Result<()> {
    if is_staff()? || my_employee_id()?.as_deref() == Some(employee_id) {
        Ok(())
    } else {
        Err(Error::msg("these are another person's insurance records"))
    }
}
