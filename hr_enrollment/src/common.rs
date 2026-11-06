//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, pick, text, Record};

use aether_sdk::dates::{format_datetime, parse_datetime, Duration, NaiveDateTime};
use aether_sdk::prelude::*;

pub const HR_USER: &str = "hr.hr_user";
pub const HR_MANAGER: &str = "hr.hr_manager";

#[derive(Deserialize)]
pub struct Id {
    pub id: String,
}

#[derive(Deserialize)]
pub struct Empty {}

pub fn require_hr() -> Result<()> {
    let context = context::current()?;
    if context.has_role(HR_USER) || context.has_role(HR_MANAGER) {
        Ok(())
    } else {
        Err(Error::msg(format!("you need the role `{HR_USER}` to do this")))
    }
}

pub fn now_text() -> Result<String> {
    let context = context::current()?;
    if context.now.is_empty() {
        return Err(Error::msg("the kernel did not say what time it is"));
    }
    Ok(context.now)
}

pub fn now() -> Result<NaiveDateTime> {
    parse_datetime(&now_text()?)
}

pub fn in_days(days: i64) -> Result<String> {
    Ok(format_datetime(now()? + Duration::days(days)))
}

pub fn explain<T>(result: Result<T>, what: &str) -> Result<T> {
    result.map_err(|error| match error {
        Error::Message(message) => Error::Message(message),
        other => Error::msg(format!("{what}: {other}")),
    })
}

pub fn get(id: &str) -> Result<Record> {
    db::get::<Record>("enrollment", id)?.ok_or_else(|| Error::msg("there is no such enrollment"))
}
