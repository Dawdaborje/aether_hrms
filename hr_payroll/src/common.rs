//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, require, text, today, Record};

use aether_sdk::dates::{parse_date, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

pub const ADMIN_ROLE: &str = "hr_payroll.payroll_admin";
pub const CLERK_ROLE: &str = "hr_payroll.payroll_clerk";
pub const APPROVER_ROLE: &str = "hr_payroll.payroll_approver";
pub const FINANCE_ROLE: &str = "hr_payroll.payroll_finance";

fn holds_any(roles: &[&str]) -> Result<bool> {
    let context = context::current()?;
    Ok(roles.iter().any(|r| context.has_role(r)))
}

pub fn require_admin() -> Result<()> {
    context::current()?.require_role(ADMIN_ROLE)
}

/// Prepares runs and their inputs: a clerk, or an administrator.
pub fn require_clerk() -> Result<()> {
    if holds_any(&[CLERK_ROLE, ADMIN_ROLE])? { Ok(()) } else { Err(Error::msg(format!("you need the role `{CLERK_ROLE}` to do this"))) }
}

pub fn require_approver() -> Result<()> {
    context::current()?.require_role(APPROVER_ROLE)
}

pub fn require_finance() -> Result<()> {
    if holds_any(&[FINANCE_ROLE, ADMIN_ROLE])? { Ok(()) } else { Err(Error::msg(format!("you need the role `{FINANCE_ROLE}` to do this"))) }
}

pub fn require_payroll_staff() -> Result<()> {
    if holds_any(&[CLERK_ROLE, ADMIN_ROLE, APPROVER_ROLE, FINANCE_ROLE])? { Ok(()) } else { Err(Error::msg("this is for payroll staff")) }
}

pub fn today_date() -> Result<NaiveDate> {
    parse_date(&today()?)
}

pub fn actor() -> Result<Option<String>> {
    Ok(context::current()?.actor.id)
}

pub fn my_employee() -> Result<Option<Record>> {
    plugins::call("hr", "my_employee", &json!({}))
}

pub fn int(record: &Record, field: &str) -> i64 {
    record.get(field).and_then(Value::as_i64).unwrap_or(0)
}

/// An engine amount (any scale) as a ledger-style decimal of two digits; refuses digits that would be lost.
pub fn money(text: &str) -> Result<Decimal> {
    Decimal::parse(text)?.with_scale(2)
}
