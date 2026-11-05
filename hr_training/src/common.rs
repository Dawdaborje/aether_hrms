//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, next_number, pick, require, text, today, Record};

use aether_sdk::dates::{parse_date, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

pub const ADMIN_ROLE: &str = "hr_training.training_admin";

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

pub fn actor() -> Result<String> {
    Ok(context::current()?.actor.id.unwrap_or_default())
}

pub fn employee(id: &str) -> Result<Record> {
    let found: Option<Record> = plugins::call("hr", "get_employee", &json!({ "id": id }))?;
    found.ok_or_else(|| Error::msg(format!("there is no employee `{id}`")))
}
