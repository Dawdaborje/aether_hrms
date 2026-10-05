//! Helpers shared by the parts of the plugin.

pub use aether_sdk::records::{id_of, pick, require, text, today, Record};

use aether_sdk::dates::{parse_date, NaiveDate, NaiveDateTime};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::rules::StageKind;

pub const MANAGER_ROLE: &str = "hr_recruitment.recruit_manager";
pub const RECRUITER_ROLE: &str = "hr_recruitment.recruiter";

pub fn is_manager() -> Result<bool> {
    Ok(context::current()?.has_role(MANAGER_ROLE))
}

pub fn is_recruiter() -> Result<bool> {
    let context = context::current()?;
    Ok(context.has_role(RECRUITER_ROLE) || context.has_role(MANAGER_ROLE))
}

pub fn require_recruiter() -> Result<()> {
    if is_recruiter()? { Ok(()) } else { Err(Error::msg(format!("you need the role `{RECRUITER_ROLE}` to do this"))) }
}

pub fn require_manager() -> Result<()> {
    context::current()?.require_role(MANAGER_ROLE)
}

pub fn today_date() -> Result<NaiveDate> {
    parse_date(&today()?)
}

pub fn now_utc() -> Result<NaiveDateTime> {
    let now = context::current()?.now;
    if now.is_empty() {
        return Err(Error::msg("the kernel did not say what time it is"));
    }
    aether_sdk::dates::parse_datetime(&now)
}

pub fn my_employee() -> Result<Option<Record>> {
    plugins::call("hr", "my_employee", &json!({}))
}

pub fn employee(id: &str) -> Result<Record> {
    let found: Option<Record> = plugins::call("hr", "get_employee", &json!({ "id": id }))?;
    found.ok_or_else(|| Error::msg(format!("there is no employee `{id}`")))
}

pub fn works_here(employee: &Record) -> bool {
    matches!(text(employee, "status"), Some("active" | "probation" | "on_leave" | "suspended") | None)
}

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

/// The first active stage of a kind, by sequence.
pub fn stage_of_kind(kind: &str) -> Result<Record> {
    db::find::<Record>("rec_stage")
        .matching(aether_sdk::db::Filter::eq("kind", kind).and(aether_sdk::db::Filter::ne("is_active", false)))
        .order_by("sequence")
        .first()?
        .ok_or_else(|| Error::msg(format!("there is no `{kind}` stage: a recruitment manager sets the stages up first")))
}

pub fn kind_of(stage: &Record) -> Result<StageKind> {
    StageKind::parse(text(stage, "kind").unwrap_or_default()).ok_or_else(|| Error::msg("a stage has an unknown kind"))
}

pub fn sequence_of(stage: &Record) -> i64 {
    stage.get("sequence").and_then(Value::as_i64).unwrap_or(0)
}
