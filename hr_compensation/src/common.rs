//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, next_number, require, text, today, Record};

use aether_sdk::dates::{parse_date, NaiveDate};
use aether_sdk::db::Filter;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

/// Sets up codes and gratuity rules, decides awards, starts and releases withholdings.
pub const ADMIN_ROLE: &str = "hr_compensation.comp_admin";

pub fn is_admin() -> Result<bool> {
    Ok(context::current()?.has_role(ADMIN_ROLE))
}

pub fn require_admin() -> Result<()> {
    context::current()?.require_role(ADMIN_ROLE)
}

pub fn today_date() -> Result<NaiveDate> {
    parse_date(&today()?)
}

pub fn date_of(record: &Record, field: &str) -> Result<NaiveDate> {
    parse_date(text(record, field).ok_or_else(|| Error::msg(format!("the record has no {field}")))?)
}

pub fn decimal_of(record: &Record, field: &str) -> Result<Decimal> {
    match record.get(field).filter(|v| !v.is_null()) {
        Some(value) => serde_json::from_value(value.clone()).map_err(|e| Error::msg(format!("{field}: {e}"))),
        None => Ok(Decimal::zero(2)),
    }
}

/// An amount above zero, with two decimals and no silent rounding.
pub fn money(amount: Decimal, what: &str) -> Result<Decimal> {
    let amount = amount.with_scale(2).map_err(|_| Error::msg(format!("{what} has at most two decimals")))?;
    if amount <= Decimal::zero(2) {
        return Err(Error::msg(format!("{what} is more than zero")));
    }
    Ok(amount)
}

pub fn employee(id: &str) -> Result<Record> {
    let found: Option<Record> = plugins::call("hr", "get_employee", &json!({ "id": id }))?;
    found.ok_or_else(|| Error::msg(format!("there is no employee `{id}`")))
}

/// Whether the person still works here (a departed employee gets no new adjustment).
pub fn employed(employee: &Record) -> bool {
    !matches!(text(employee, "status"), Some("ended" | "terminated" | "resigned" | "archived"))
}

/// The day payroll has approved through; nothing dated on or before it can change.
pub fn locked_through() -> Result<Option<NaiveDate>> {
    let row: Option<Record> = db::find("comp_lock").first()?;
    match row.as_ref().and_then(|r| text(r, "locked_through")) {
        Some(day) => Ok(Some(parse_date(day)?)),
        None => Ok(None),
    }
}

/// Refuse a change that lands on or before the locked day.
pub fn require_open(day: NaiveDate, what: &str) -> Result<()> {
    if let Some(locked) = locked_through()? {
        if day <= locked {
            return Err(Error::msg(format!("{what} falls on {day}, in a period payroll has approved (through {locked}): put the correction in a later period")));
        }
    }
    Ok(())
}

pub fn actor() -> Result<Option<String>> {
    Ok(context::current()?.actor.id)
}

pub fn count_where(model: &str, filter: Filter) -> Result<u64> {
    db::count(model, filter)
}
