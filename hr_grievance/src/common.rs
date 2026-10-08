//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, next_number, require, text, today, Record};

use aether_sdk::dates::{parse_date, NaiveDate};
use aether_sdk::prelude::*;

pub const ADMIN_ROLE: &str = "hr_grievance.grievance_admin";

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
