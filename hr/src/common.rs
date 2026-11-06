//! Small helpers every part of the plugin uses.

use aether_sdk::prelude::*;

pub use aether_sdk::records::{id_of, is_off, pick, require, text, today, Record};

pub use aether_sdk::records::next_number;

/// A write that fails with a plain message for the caller when the kernel refuses it.
pub fn explain<T>(result: Result<T>, what: &str) -> Result<T> {
    result.map_err(|error| match error {
        Error::Message(message) => Error::Message(message),
        other => Error::msg(format!("{what}: {other}")),
    })
}

/// Organisation-wide figures are for HR staff.
pub fn require_hr_staff() -> Result<()> {
    let me = context::current()?;
    if me.has_role("hr.hr_user") || me.has_role("hr.hr_manager") {
        Ok(())
    } else {
        Err(Error::msg("only HR staff can see the HR overview"))
    }
}

/// Pay is for HR managers: the wage, its currency and frequency, and how pay is decided.
pub fn require_pay_manager() -> Result<()> {
    if context::current()?.has_role("hr.hr_manager") {
        Ok(())
    } else {
        Err(Error::msg("only an HR manager can change pay"))
    }
}
