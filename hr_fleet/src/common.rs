//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, pick, require, text, today, Record};

use aether_sdk::prelude::*;

pub const ADMIN_ROLE: &str = "hr_fleet.fleet_admin";

pub fn is_admin() -> Result<bool> {
    Ok(context::current()?.has_role(ADMIN_ROLE))
}

pub fn require_admin() -> Result<()> {
    context::current()?.require_role(ADMIN_ROLE)
}

pub fn my_employee() -> Result<Option<Record>> {
    plugins::call("hr", "my_employee", &json!({}))
}

pub fn my_id() -> Result<Option<String>> {
    Ok(my_employee()?.and_then(|m| text(&m, "id").map(str::to_string)))
}
