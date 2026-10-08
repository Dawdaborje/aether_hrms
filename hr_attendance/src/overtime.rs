//! Overtime: bands with multipliers, and a decision on each day.
//!
//! Odoo 19 splits and retypes time records with a rule engine; Frappe has overtime types and
//! slabs but computes them away from the day. Here the data shape is a table of **bands**: for a
//! kind of day (working, weekly off, holiday), overtime minutes from `from_min` up to `to_min` count
//! at a `multiplier`. The day's overtime is cut over the bands; minutes no band covers count once.
//! The result is stored per person and date as a *claim* for payroll: `pending` until the manager
//! or an attendance administrator decides it, and never by the person. If the day is later
//! recomputed to a different number of minutes, an earlier decision no longer applies and the claim
//! goes back to `pending` with a note, so payroll never pays for minutes nobody approved.

use aether_sdk::dates::{format_date, parse_date, NaiveDate};
use aether_sdk::db::Filter;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{employee, id_of, is_admin, my_employee, require, require_admin, text, Record};
use crate::rules::DayKind;

/// A band as the arithmetic sees it; the multiplier is in hundredths (150 is one and a half).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Band {
    pub kind: DayKind,
    pub from: i64,
    pub to: Option<i64>,
    pub hundredths: i64,
}

pub fn kind_name(kind: DayKind) -> &'static str {
    match kind {
        DayKind::Working => "working",
        DayKind::WeeklyOff => "weekly_off",
        DayKind::Holiday => "holiday",
    }
}

/// Cut `minutes` of overtime over the bands of a day kind. Returns `(weighted minutes, the pieces)`
/// where a piece is `(from, to, hundredths)`. Minutes outside every band are one-for-one.
pub fn weigh(kind: DayKind, minutes: i64, bands: &[Band]) -> (i64, Vec<(i64, i64, i64)>) {
    let mut mine: Vec<&Band> = bands.iter().filter(|b| b.kind == kind).collect();
    mine.sort_by_key(|b| b.from);
    let mut pieces: Vec<(i64, i64, i64)> = Vec::new();
    let mut cursor = 0i64;
    for band in mine {
        let start = band.from.max(cursor);
        let end = band.to.map_or(minutes, |to| to.min(minutes));
        if start >= minutes {
            break;
        }
        if start > cursor {
            pieces.push((cursor, start, 100));
        }
        if end > start {
            pieces.push((start, end, band.hundredths));
            cursor = end;
        }
    }
    if cursor < minutes {
        pieces.push((cursor, minutes, 100));
    }
    let weighted_hundredths: i64 = pieces.iter().map(|(from, to, h)| (to - from) * h).sum();
    // Rounded to the nearest whole minute, half up.
    ((weighted_hundredths + 50) / 100, pieces)
}

fn hundredths(value: &Value) -> Result<i64> {
    let decimal: Decimal = serde_json::from_value(value.clone()).map_err(|e| Error::msg(format!("multiplier: {e}")))?;
    let text = decimal.with_scale(2)?.to_string();
    text.replace('.', "").parse().map_err(|_| Error::msg("the multiplier is not a number"))
}

fn kind_of(name: &str) -> Option<DayKind> {
    match name {
        "working" => Some(DayKind::Working),
        "weekly_off" => Some(DayKind::WeeklyOff),
        "holiday" => Some(DayKind::Holiday),
        _ => None,
    }
}

fn bands() -> Result<Vec<Band>> {
    let rows: Vec<Record> = db::find::<Record>("att_overtime_rule").filter("is_active", true).limit(200).all()?;
    let mut out = Vec::new();
    for row in rows {
        let Some(kind) = kind_of(text(&row, "day_kind").unwrap_or_default()) else { continue };
        out.push(Band {
            kind,
            from: row.get("from_min").and_then(Value::as_i64).unwrap_or(0),
            to: row.get("to_min").and_then(Value::as_i64),
            hundredths: hundredths(&row["multiplier"])?,
        });
    }
    Ok(out)
}

/// Called whenever a day is written: keep the person's overtime claim in step with it.
pub fn settle(person_id: &str, date: NaiveDate, kind: DayKind, minutes: i64) -> Result<()> {
    let existing: Option<Record> = db::find::<Record>("att_overtime")
        .filter("employee", person_id)
        .filter("work_date", format_date(date).as_str())
        .first()?;
    if minutes <= 0 {
        // Nothing to claim any more: a pending claim goes; a decided one stays as the record.
        if let Some(row) = existing.filter(|r| text(r, "status") == Some("pending")) {
            db::delete::<Record>("att_overtime", id_of(&row)?)?;
        }
        return Ok(());
    }
    let (weighted, pieces) = weigh(kind, minutes, &bands()?);
    let shape: Vec<String> = pieces.iter().map(|(from, to, h)| format!("{from}-{to}@{h}")).collect();
    let mut data = json!({ "minutes": minutes, "weighted_min": weighted, "bands": shape.join(","), "day_kind": kind_name(kind) });
    match existing {
        Some(row) => {
            let same = row.get("minutes").and_then(Value::as_i64) == Some(minutes);
            if !same && text(&row, "status") != Some("pending") {
                data["status"] = json!("pending");
                data["note"] = json!("the day was recomputed: decide again");
                data["decided_by"] = Value::Null;
            }
            db::update::<Record>("att_overtime", id_of(&row)?, &data)?;
        }
        None => {
            data["employee"] = json!(person_id);
            data["work_date"] = json!(format_date(date));
            data["status"] = json!("pending");
            db::create::<Record>("att_overtime", &data)?;
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct NewRule {
    name: String,
    day_kind: String,
    #[serde(default)]
    from_min: Option<i64>,
    #[serde(default)]
    to_min: Option<i64>,
    multiplier: Decimal,
}

fn make_rule(input: NewRule) -> Result<Record> {
    require_admin()?;
    let kind = kind_of(&input.day_kind).ok_or_else(|| Error::msg("day_kind is working, weekly_off or holiday"))?;
    let from = input.from_min.unwrap_or(0);
    if from < 0 || input.to_min.is_some_and(|to| to <= from) {
        return Err(Error::msg("a band starts at 0 or later and ends after it starts"));
    }
    if input.multiplier.is_zero() || input.multiplier.is_negative() {
        return Err(Error::msg("the multiplier must be above zero"));
    }
    let value = json!(input.multiplier.with_scale(2).map_err(|_| Error::msg("the multiplier has at most 2 digits after the point"))?);
    // Two bands of one kind may not cover the same minutes: which would win is a guess.
    let others: Vec<Record> = db::find::<Record>("att_overtime_rule").filter("day_kind", input.day_kind.as_str()).filter("is_active", true).limit(100).all()?;
    for other in others {
        let (of, ot) = (other.get("from_min").and_then(Value::as_i64).unwrap_or(0), other.get("to_min").and_then(Value::as_i64));
        if of < input.to_min.unwrap_or(i64::MAX) && from < ot.unwrap_or(i64::MAX) {
            return Err(Error::msg(format!("it overlaps the band `{}`: bands of one kind of day must not share minutes", text(&other, "name").unwrap_or("?"))));
        }
    }
    let _ = kind;
    let mut data = json!({ "name": input.name, "day_kind": input.day_kind, "from_min": from, "multiplier": value, "is_active": true });
    if let Some(to) = input.to_min {
        data["to_min"] = json!(to);
    }
    db::create("att_overtime_rule", &data)
}

#[derive(Deserialize)]
struct Decide {
    id: String,
    #[serde(default)]
    note: Option<String>,
}

fn decide(input: Decide, approve: bool) -> Result<Record> {
    let row = require("att_overtime", &input.id, "overtime claim")?;
    if text(&row, "status") != Some("pending") {
        return Err(Error::msg("this overtime was already decided"));
    }
    let me = my_employee()?;
    let owner = text(&row, "employee").unwrap_or_default();
    if me.as_ref().is_some_and(|m| text(m, "id") == Some(owner)) {
        return Err(Error::msg("nobody decides their own overtime"));
    }
    let manager = employee(owner)?.get("manager").and_then(Value::as_str).map(str::to_string);
    let their_manager = matches!((&manager, &me), (Some(m), Some(me)) if text(me, "id") == Some(m.as_str()));
    if !their_manager && !is_admin()? {
        return Err(Error::msg("only the person's manager or an attendance administrator can decide overtime"));
    }
    let who = context::current()?.actor.id.unwrap_or_default();
    let status = if approve { "approved" } else { "rejected" };
    db::update("att_overtime", &input.id, &json!({ "status": status, "decided_by": who, "note": input.note }))?.ok_or_else(|| Error::msg("the claim is gone"))
}

#[derive(Deserialize)]
struct Range {
    #[serde(default)]
    employee: Option<String>,
    from: String,
    to: String,
    #[serde(default)]
    status: Option<String>,
}

fn list(input: Range) -> Result<Vec<Record>> {
    let mut filter = Filter::gte("work_date", format_date(parse_date(&input.from)?)).and(Filter::lte("work_date", format_date(parse_date(&input.to)?)));
    if let Some(person) = input.employee.as_deref() {
        filter = filter.and(Filter::eq("employee", person));
    }
    if let Some(status) = input.status.as_deref() {
        filter = filter.and(Filter::eq("status", status));
    }
    db::find("att_overtime").matching(filter).order_by("-work_date").limit(1000).all()
}

/// For payroll: approved overtime, weighted, per person; and what still waits.
pub fn payroll_overtime(employees: &[String], from: NaiveDate, to: NaiveDate) -> Result<Value> {
    let rows = db::find::<Record>("att_overtime")
        .matching(
            Filter::one_of("employee", employees.to_vec())
                .and(Filter::gte("work_date", format_date(from)))
                .and(Filter::lte("work_date", format_date(to))),
        )
        .limit(10_000)
        .all()?;
    let mut out = serde_json::Map::new();
    for row in rows {
        let id = text(&row, "employee").unwrap_or_default().to_string();
        let entry = out.entry(id).or_insert_with(|| json!({ "approved_weighted_minutes": 0, "approved_minutes": 0, "pending_minutes": 0 }));
        let minutes = row.get("minutes").and_then(Value::as_i64).unwrap_or(0);
        let weighted = row.get("weighted_min").and_then(Value::as_i64).unwrap_or(0);
        let add = |entry: &mut Value, key: &str, n: i64| {
            let sum = entry[key].as_i64().unwrap_or(0) + n;
            entry[key] = json!(sum);
        };
        match text(&row, "status") {
            Some("approved") => {
                add(entry, "approved_weighted_minutes", weighted);
                add(entry, "approved_minutes", minutes);
            }
            Some("pending") => add(entry, "pending_minutes", minutes),
            _ => {}
        }
    }
    Ok(Value::Object(out))
}

handler! {
    /// Add a band to the overtime table (attendance administrators).
    fn create_overtime_rule(input: NewRule) -> Record {
        make_rule(input)
    }

    fn list_overtime_rules(_: Empty) -> Vec<Record> {
        db::find("att_overtime_rule").order_by("day_kind").limit(200).all()
    }

    fn approve_overtime(input: Decide) -> Record {
        decide(input, true)
    }

    fn reject_overtime(input: Decide) -> Record {
        decide(input, false)
    }

    /// Overtime claims between two dates.
    fn list_overtime(input: Range) -> Vec<Record> {
        list(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band(kind: DayKind, from: i64, to: Option<i64>, h: i64) -> Band {
        Band { kind, from, to, hundredths: h }
    }

    #[test]
    fn minutes_are_cut_over_the_bands_of_the_day_kind() {
        let bands = vec![
            band(DayKind::Working, 0, Some(120), 125),
            band(DayKind::Working, 120, None, 150),
            band(DayKind::Holiday, 0, None, 300),
        ];
        let (weighted, pieces) = weigh(DayKind::Working, 180, &bands);
        assert_eq!(weighted, 120 * 125 / 100 + 60 * 150 / 100);
        assert_eq!(pieces, vec![(0, 120, 125), (120, 180, 150)]);
        assert_eq!(weigh(DayKind::Working, 60, &bands).0, 75);
        assert_eq!(weigh(DayKind::Holiday, 100, &bands).0, 300);
    }

    #[test]
    fn minutes_no_band_covers_count_once() {
        let bands = vec![band(DayKind::Working, 30, Some(90), 200)];
        let (weighted, pieces) = weigh(DayKind::Working, 120, &bands);
        assert_eq!(pieces, vec![(0, 30, 100), (30, 90, 200), (90, 120, 100)]);
        assert_eq!(weighted, 30 + 120 + 30);
        assert_eq!(weigh(DayKind::WeeklyOff, 45, &bands).0, 45);
        assert_eq!(weigh(DayKind::Working, 0, &bands).0, 0);
    }

    #[test]
    fn a_band_beyond_the_overtime_changes_nothing() {
        let bands = vec![band(DayKind::Working, 240, None, 200)];
        assert_eq!(weigh(DayKind::Working, 100, &bands).0, 100);
    }
}
