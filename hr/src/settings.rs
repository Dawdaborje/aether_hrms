//! Organisation-wide HR defaults: probation, notice, retirement age.

use aether_sdk::dates::{format_date, parse_date, Duration};
use aether_sdk::prelude::*;

use crate::common::{id_of, Record};

#[derive(Deserialize)]
struct Change {
    #[serde(default)]
    default_probation_days: Option<i64>,
    #[serde(default)]
    default_notice_days: Option<i64>,
    #[serde(default)]
    retirement_age: Option<i64>,
    #[serde(default)]
    probation_alert_days: Option<i64>,
}

pub fn current() -> Result<Record> {
    Ok(db::find::<Record>("hr_setting").first()?.unwrap_or_else(|| json!({ "default_probation_days": 0, "default_notice_days": 30, "retirement_age": 0, "probation_alert_days": 14 })))
}

fn int(setting: &Record, field: &str) -> i64 {
    setting.get(field).and_then(Value::as_i64).unwrap_or(0)
}

/// A new hire's probation end when none was given: the join date plus the default days (none when 0).
pub fn default_probation_end(start: &str) -> Result<Option<String>> {
    let days = int(&current()?, "default_probation_days");
    if days <= 0 {
        return Ok(None);
    }
    Ok(Some(format_date(parse_date(start)? + Duration::days(days))))
}

pub fn default_notice_days() -> Result<i64> {
    Ok(int(&current()?, "default_notice_days"))
}

fn save(input: Change) -> Result<Record> {
    let mut data = serde_json::Map::new();
    for (name, value, max) in [
        ("default_probation_days", input.default_probation_days, 730),
        ("default_notice_days", input.default_notice_days, 365),
        ("retirement_age", input.retirement_age, 100),
        ("probation_alert_days", input.probation_alert_days, 365),
    ] {
        if let Some(value) = value {
            if !(0..=max).contains(&value) {
                return Err(Error::msg(format!("{name} is between 0 and {max}")));
            }
            data.insert(name.into(), json!(value));
        }
    }
    match db::find::<Record>("hr_setting").first()? {
        Some(existing) => db::update("hr_setting", id_of(&existing)?, &Value::Object(data))?.ok_or_else(|| Error::msg("the setting is gone")),
        None => db::create("hr_setting", &Value::Object(data)),
    }
}

handler! {
    fn get_hr_setting(_: Empty) -> Record {
        current()
    }

    /// Change organisation-wide HR defaults (HR managers).
    fn set_hr_setting(input: Change) -> Record {
        save(input)
    }
}
