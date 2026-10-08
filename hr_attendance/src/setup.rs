//! Shifts, locations and who works which shift.

use aether_sdk::dates::{format_date, parse_date, parse_time};
use aether_sdk::db::Filter;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{employee, id_of, pick, require, require_admin, text, Record};
use crate::rules::Shift;

const SHIFT_FIELDS: &[&str] = &[
    "code", "name", "start_time", "end_time", "utc_offset_min", "early_window_min", "late_window_min", "break_min",
    "grace_late_min", "grace_early_min", "half_day_below_min", "absent_below_min", "is_active",
];
const LOCATION_FIELDS: &[&str] = &["name", "latitude", "longitude", "radius_m", "geo_mode", "is_active"];

#[derive(Deserialize)]
struct Assign {
    employee: String,
    #[serde(default)]
    shift: Option<String>,
    /// Instead of a shift: a rotation that decides the shift each day.
    #[serde(default)]
    rotation: Option<String>,
    #[serde(default)]
    location: Option<String>,
    valid_from: String,
    #[serde(default)]
    valid_to: Option<String>,
}

/// A shift record as the rules see it.
pub fn shift_of(record: &Record) -> Result<Shift> {
    let int = |field: &str| record.get(field).and_then(Value::as_i64).unwrap_or(0);
    Ok(Shift {
        start: parse_time(text(record, "start_time").ok_or_else(|| Error::msg("the shift has no start time"))?)?,
        end: parse_time(text(record, "end_time").ok_or_else(|| Error::msg("the shift has no end time"))?)?,
        offset_min: int("utc_offset_min"),
        early_window: int("early_window_min"),
        late_window: int("late_window_min"),
        break_min: int("break_min"),
        grace_late: int("grace_late_min"),
        grace_early: int("grace_early_min"),
        absent_below: int("absent_below_min"),
        half_day_below: int("half_day_below_min"),
    })
}

fn new_shift(input: Record) -> Result<Record> {
    require_admin()?;
    let data = pick(&input, SHIFT_FIELDS);
    let mut merged = data.clone();
    merged["utc_offset_min"] = data.get("utc_offset_min").cloned().unwrap_or(json!(0));
    merged["early_window_min"] = data.get("early_window_min").cloned().unwrap_or(json!(60));
    merged["late_window_min"] = data.get("late_window_min").cloned().unwrap_or(json!(60));
    let shift = shift_of(&merged)?;
    if !shift.is_sound() {
        return Err(Error::msg("the shift overlaps itself: it starts when it ends, or its length plus both windows reaches a full day"));
    }
    for field in ["break_min", "grace_late_min", "grace_early_min", "half_day_below_min", "absent_below_min", "early_window_min", "late_window_min"] {
        if data.get(field).and_then(Value::as_i64).is_some_and(|n| n < 0) {
            return Err(Error::msg(format!("{field} cannot be negative")));
        }
    }
    if shift.scheduled_minutes() == 0 {
        return Err(Error::msg("the break takes the whole shift"));
    }
    db::create("att_shift", &data).map_err(|e| e.or("could not create the shift (the code may be taken)"))
}

fn new_location(input: Record) -> Result<Record> {
    require_admin()?;
    let data = pick(&input, LOCATION_FIELDS);
    if text(&data, "name").is_none() {
        return Err(Error::msg("a location needs a name"));
    }
    for (field, limit) in [("latitude", 90), ("longitude", 180)] {
        if let Some(value) = data.get(field).filter(|v| !v.is_null()) {
            let degrees: Decimal = serde_json::from_value(value.clone()).map_err(|e| Error::msg(format!("{field}: {e}")))?;
            let bound = Decimal::whole(limit, 6)?;
            if degrees > bound || degrees < -bound {
                return Err(Error::msg(format!("{field} is between -{limit} and {limit}")));
            }
        }
    }
    if text(&data, "geo_mode").unwrap_or("record") != "off" && data.get("radius_m").and_then(Value::as_i64).unwrap_or(0) > 0
        && (data.get("latitude").is_none_or(Value::is_null) || data.get("longitude").is_none_or(Value::is_null))
    {
        return Err(Error::msg("a location with a radius needs its latitude and longitude"));
    }
    db::create("att_location", &data).map_err(|e| e.or("could not create the location (the name may be taken)"))
}

/// Give a person a shift from a day. An open-ended assignment before it ends the day before; one
/// that would overlap a dated assignment is refused.
fn assign(input: Assign) -> Result<Record> {
    require_admin()?;
    employee(&input.employee)?;
    match (&input.shift, &input.rotation) {
        (Some(shift), None) => {
            require("att_shift", shift, "shift")?;
        }
        (None, Some(rotation)) => {
            require("att_rotation", rotation, "rotation")?;
        }
        _ => return Err(Error::msg("give a shift or a rotation, not both and not neither")),
    }
    if let Some(location) = &input.location {
        require("att_location", location, "location")?;
    }
    let from = parse_date(&input.valid_from)?;
    let to = input.valid_to.as_deref().map(parse_date).transpose()?;
    if to.is_some_and(|to| to < from) {
        return Err(Error::msg("the assignment ends before it starts"));
    }
    let existing: Vec<Record> = db::find::<Record>("att_shift_assignment").filter("employee", input.employee.as_str()).limit(500).all()?;
    for other in &existing {
        let (of, ot) = (
            parse_date(text(other, "valid_from").unwrap_or_default())?,
            text(other, "valid_to").map(parse_date).transpose()?,
        );
        let overlaps = of <= to.unwrap_or(from.max(of)) && ot.is_none_or(|ot| ot >= from) && to.is_none_or(|to| to >= of);
        if !overlaps {
            continue;
        }
        if ot.is_none() && of < from && to.is_none() {
            // Another open-ended one starts later: the earlier ends the day before.
            db::update::<Record>("att_shift_assignment", id_of(other)?, &json!({ "valid_to": format_date(from - aether_sdk::dates::Duration::days(1)) }))?;
        } else {
            return Err(Error::msg(format!(
                "it overlaps the assignment that starts {}: end that one first, or make this one open-ended to replace it from its start",
                format_date(of)
            )));
        }
    }
    let mut data = json!({ "employee": input.employee, "valid_from": format_date(from) });
    if let Some(shift) = &input.shift {
        data["shift"] = json!(shift);
    }
    if let Some(rotation) = &input.rotation {
        data["rotation"] = json!(rotation);
    }
    if let Some(to) = to {
        data["valid_to"] = json!(format_date(to));
    }
    if let Some(location) = input.location {
        data["location"] = json!(location);
    }
    db::create("att_shift_assignment", &data).map_err(|e| e.or("could not assign the shift"))
}

/// The assignment in force for a person on a day, shift or rotation.
fn assignment_row(employee_id: &str, day: aether_sdk::dates::NaiveDate) -> Result<Option<Record>> {
    let on = format_date(day);
    let found = db::find::<Record>("att_shift_assignment")
        .matching(Filter::eq("employee", employee_id).and(Filter::lte("valid_from", on.as_str())))
        .order_by("-valid_from")
        .first()?;
    Ok(found.filter(|a| !text(a, "valid_to").is_some_and(|end| end < on.as_str())))
}

/// The assignment and shift in force for a person on a day.
pub fn assignment_on(employee_id: &str, day: aether_sdk::dates::NaiveDate) -> Result<Option<(Record, Record)>> {
    let Some(assignment) = assignment_row(employee_id, day)? else { return Ok(None) };
    let shift_id = match text(&assignment, "rotation") {
        // A rotation names the shift of each day, or a day off (no shift that day).
        Some(rotation) => match rotation_shift(rotation, day)? {
            Some(id) => id,
            None => return Ok(None),
        },
        None => text(&assignment, "shift").unwrap_or_default().to_string(),
    };
    let shift = require("att_shift", &shift_id, "shift")?;
    Ok(Some((assignment, shift)))
}

/// Whether the person's rotation gives them this day off: a day off the schedule itself decides, so
/// it counts like a weekly off.
pub fn rotation_day_off(employee_id: &str, day: aether_sdk::dates::NaiveDate) -> Result<bool> {
    let Some(assignment) = assignment_row(employee_id, day)? else { return Ok(false) };
    let Some(rotation) = text(&assignment, "rotation") else { return Ok(false) };
    let steps = db::count("att_rotation_step", Filter::eq("rotation", rotation))?;
    Ok(steps > 0 && rotation_shift(rotation, day)?.is_none())
}

/// Which step of a cycle of `len` days a day falls on, counting from the anchor (step 1). Days before
/// the anchor count backwards, so the pattern is the same on both sides of it.
pub fn step_index(anchor: aether_sdk::dates::NaiveDate, day: aether_sdk::dates::NaiveDate, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    ((day - anchor).num_days().rem_euclid(len as i64)) as usize
}

/// The shift a rotation puts on a day; `None` on a day off.
fn rotation_shift(rotation: &str, day: aether_sdk::dates::NaiveDate) -> Result<Option<String>> {
    let plan = require("att_rotation", rotation, "rotation")?;
    let steps: Vec<Record> = db::find("att_rotation_step").filter("rotation", rotation).order_by("position").limit(400).all()?;
    if steps.is_empty() {
        return Ok(None);
    }
    let anchor = parse_date(text(&plan, "anchor_date").unwrap_or_default())?;
    Ok(text(&steps[step_index(anchor, day, steps.len())], "shift").map(str::to_string))
}

#[derive(Deserialize)]
struct NewRotation {
    name: String,
    anchor_date: String,
    /// One entry per day of the cycle: a shift, or null for a day off.
    steps: Vec<Option<String>>,
}

fn new_rotation(input: NewRotation) -> Result<Record> {
    require_admin()?;
    if input.steps.is_empty() || input.steps.len() > 365 {
        return Err(Error::msg("a rotation has 1 to 365 days"));
    }
    if input.steps.iter().all(Option::is_none) {
        return Err(Error::msg("a rotation of only days off is not a rotation"));
    }
    for shift in input.steps.iter().flatten() {
        require("att_shift", shift, "shift")?;
    }
    parse_date(&input.anchor_date)?;
    let plan: Record = db::create("att_rotation", &json!({ "name": input.name, "anchor_date": input.anchor_date, "is_active": true }))
        .map_err(|e| e.or("could not create the rotation (the name may be taken)"))?;
    for (n, shift) in input.steps.iter().enumerate() {
        let mut data = json!({ "rotation": id_of(&plan)?, "position": n + 1 });
        if let Some(shift) = shift {
            data["shift"] = json!(shift);
        }
        db::create::<Record>("att_rotation_step", &data)?;
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use aether_sdk::dates::parse_date;

    use super::step_index;

    #[test]
    fn a_rotation_repeats_from_its_anchor_both_ways() {
        let anchor = parse_date("2026-10-05").unwrap_or_default();
        let day = |t: &str| parse_date(t).unwrap_or_default();
        assert_eq!(step_index(anchor, day("2026-10-05"), 4), 0);
        assert_eq!(step_index(anchor, day("2026-10-08"), 4), 3);
        assert_eq!(step_index(anchor, day("2026-10-09"), 4), 0);
        assert_eq!(step_index(anchor, day("2026-10-04"), 4), 3);
        assert_eq!(step_index(anchor, day("2026-09-01"), 1), 0);
    }
}

handler! {
    /// A cycle of shifts and days off that repeats from an anchor day (attendance administrators).
    fn create_rotation(input: NewRotation) -> Record {
        new_rotation(input)
    }

    fn create_shift(input: Record) -> Record {
        new_shift(input)
    }

    fn list_shifts(_: Empty) -> Vec<Record> {
        db::find("att_shift").order_by("code").limit(200).all()
    }

    fn create_location(input: Record) -> Record {
        new_location(input)
    }

    fn list_locations(_: Empty) -> Vec<Record> {
        db::find("att_location").order_by("name").limit(200).all()
    }

    /// Put a person on a shift from a day.
    fn assign_shift(input: Assign) -> Record {
        assign(input)
    }
}
