//! What an organization decides once: kinds of leave and blocked periods.

use aether_sdk::dates::{check_order, format_date, parse_date};
use aether_sdk::db::Filter;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{pick, require, require_admin, text, Record};
use crate::rules::{Frequency, SCALE};

const TYPE_FIELDS: &[&str] = &[
    "name", "code", "color", "is_paid", "validation", "requires_allocation", "allow_negative", "max_negative", "half_days",
    "count_all_days", "sandwich", "min_notice_days", "applicable_after_days", "max_per_request", "gender", "annual_days",
    "pro_rata", "earned", "earned_frequency", "earned_rounding", "allocate_on", "carry_forward", "carry_forward_max",
    "carry_forward_days", "is_active",
];
/// What cannot change once requests exist for the type: it would change what they cost or need.
const LOCKED_ONCE_USED: &[&str] = &["count_all_days", "requires_allocation"];

#[derive(Deserialize)]
struct Change {
    id: String,
    #[serde(flatten)]
    fields: Record,
}

fn check_type(data: &Record) -> Result<()> {
    for field in ["annual_days", "max_per_request", "max_negative", "carry_forward_max"] {
        if let Some(value) = data.get(field).filter(|v| !v.is_null()) {
            let days: Decimal = serde_json::from_value(value.clone()).map_err(|e| Error::msg(format!("{field}: {e}")))?;
            if days.is_negative() {
                return Err(Error::msg(format!("{field} cannot be negative")));
            }
            days.with_scale(SCALE).map_err(|_| Error::msg(format!("{field} has at most {SCALE} digits after the point")))?;
        }
    }
    for field in ["min_notice_days", "applicable_after_days", "carry_forward_days"] {
        if data.get(field).and_then(Value::as_i64).is_some_and(|n| n < 0) {
            return Err(Error::msg(format!("{field} cannot be negative")));
        }
    }
    if let Some(frequency) = text(data, "earned_frequency") {
        if Frequency::parse(frequency).is_none() {
            return Err(Error::msg("earned_frequency is monthly, quarterly, half_yearly or yearly"));
        }
    }
    if data.get("allow_negative") == Some(&json!(true)) && data.get("max_negative").is_none_or(Value::is_null) {
        return Err(Error::msg("a type that allows a negative balance needs max_negative: how far below zero"));
    }
    Ok(())
}

fn new_type(input: Record) -> Result<Record> {
    require_admin()?;
    if text(&input, "name").is_none() {
        return Err(Error::msg("a leave type needs a name"));
    }
    let data = pick(&input, TYPE_FIELDS);
    check_type(&data)?;
    db::create("leave_type", &data).map_err(|e| e.or("could not create the leave type (the name may be taken)"))
}

fn change_type(input: Change) -> Result<Record> {
    require_admin()?;
    let existing = require("leave_type", &input.id, "leave type")?;
    let data = pick(&input.fields, TYPE_FIELDS);
    if data.as_object().is_some_and(|fields| fields.is_empty()) {
        return Err(Error::msg("there is nothing to change"));
    }
    check_type(&data)?;
    for field in LOCKED_ONCE_USED {
        if data.get(*field).is_some_and(|new| existing.get(*field) != Some(new)) {
            let used = db::count("leave_request", Filter::eq("leave_type", input.id.as_str()))?;
            if used > 0 {
                return Err(Error::msg(format!("`{field}` cannot change: {used} requests already depend on it")));
            }
        }
    }
    db::update::<Record>("leave_type", &input.id, &data)?.ok_or_else(|| Error::msg("the leave type is gone"))
}

fn new_block(input: Record) -> Result<Record> {
    require_admin()?;
    if text(&input, "name").is_none() {
        return Err(Error::msg("a block needs a name"));
    }
    let start = parse_date(text(&input, "start_date").ok_or_else(|| Error::msg("a block needs a start_date"))?)?;
    let end = parse_date(text(&input, "end_date").ok_or_else(|| Error::msg("a block needs an end_date"))?)?;
    check_order(start, Some(end), "block")?;
    if let Some(kind) = text(&input, "leave_type") {
        require("leave_type", kind, "leave type")?;
    }
    let mut data = pick(&input, &["name", "reason", "leave_type", "is_active"]);
    data["start_date"] = json!(format_date(start));
    data["end_date"] = json!(format_date(end));
    db::create("leave_block", &data)
}

handler! {
    fn create_leave_type(input: Record) -> Record {
        new_type(input)
    }

    fn update_leave_type(input: Change) -> Record {
        change_type(input)
    }

    fn list_leave_types(_: Empty) -> Vec<Record> {
        db::find("leave_type").order_by("name").limit(200).all()
    }

    /// Dates on which leave cannot be asked for (year-end close, a peak season).
    fn create_leave_block(input: Record) -> Record {
        new_block(input)
    }

    fn list_leave_blocks(_: Empty) -> Vec<Record> {
        db::find("leave_block").order_by("start_date").limit(500).all()
    }
}
