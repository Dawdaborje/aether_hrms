//! Accrual plans: leave earned by length of service.
//!
//! A plan belongs to a leave type and has **levels**. A level starts after some months of service
//! and says how often days arrive and how many, with an optional cap per year and in total. The
//! level that applies is the one in force *on the day the grant is due*, so a person moves up a
//! level when their service reaches the next milestone and nothing is recalculated afterwards.
//! A plan is assigned to a person over a dated span; the nightly job writes `accrual` entries with
//! a key that names the assignment and period, so running it twice (or catching up after a missed
//! night) never grants twice. Odoo keeps this on the allocation and edits it in place; here the
//! plan only decides, and the ledger keeps the history.

use aether_sdk::dates::{format_date, months_between, parse_date, Datelike, Duration, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{date_of, decimal_of, id_of, is_off, require, require_admin, text, today_date, Record};
use crate::ledger::{balance_on, post_once, present_employees, Post};
use crate::rules::{earned_due, year_bounds, AllocateOn, Frequency, SCALE};

/// One step of a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Level {
    pub after_months: u32,
    pub frequency: Frequency,
    pub amount: Decimal,
    pub on: AllocateOn,
    pub yearly_cap: Option<Decimal>,
    pub total_cap: Option<Decimal>,
    pub valid_days: Option<u32>,
}

/// The level in force after `months` of service: the one with the largest milestone not above it.
pub fn level_for(levels: &[Level], months: u32) -> Option<&Level> {
    levels.iter().filter(|l| l.after_months <= months).max_by_key(|l| l.after_months)
}

/// What may still be granted: `amount`, cut so the year's grants stay within the yearly cap and the
/// balance stays within the total cap. Never negative.
pub fn capped(amount: Decimal, granted_this_year: Decimal, yearly_cap: Option<Decimal>, balance: Decimal, total_cap: Option<Decimal>) -> Decimal {
    let mut room = amount;
    if let Some(cap) = yearly_cap {
        let left = cap - granted_this_year;
        if left < room {
            room = left;
        }
    }
    if let Some(cap) = total_cap {
        let left = cap - balance;
        if left < room {
            room = left;
        }
    }
    if room.is_negative() {
        Decimal::zero(SCALE)
    } else {
        room
    }
}

/// Every grant day of one assignment up to `today` in `year`: `(frequency, period, day, level)`,
/// where the level is the one in force on that day and has that frequency.
pub fn due_in_year<'a>(
    levels: &'a [Level],
    hired: NaiveDate,
    from: NaiveDate,
    to: Option<NaiveDate>,
    year: i32,
    today: NaiveDate,
) -> Vec<(Frequency, u32, NaiveDate, &'a Level)> {
    let mut out = Vec::new();
    let mut seen: Vec<Frequency> = Vec::new();
    for level in levels {
        if seen.contains(&level.frequency) {
            continue;
        }
        seen.push(level.frequency);
        // A frequency's days depend on when in the period it lands, which belongs to the level.
        for on in [AllocateOn::First, AllocateOn::Last] {
            for (period, day) in earned_due(level.frequency, on, year, today) {
                if day < hired || day < from || to.is_some_and(|end| day > end) {
                    continue;
                }
                let months = months_between(hired, day);
                if let Some(active) = level_for(levels, months) {
                    if active.frequency == level.frequency && active.on == on {
                        out.push((level.frequency, period, day, active));
                    }
                }
            }
        }
    }
    out.sort_by_key(|(_, _, day, _)| *day);
    out
}

fn name_of(frequency: Frequency) -> &'static str {
    match frequency {
        Frequency::Monthly => "m",
        Frequency::Quarterly => "q",
        Frequency::HalfYearly => "h",
        Frequency::Yearly => "y",
    }
}

fn level_of(row: &Record) -> Result<Level> {
    let after = row.get("after_months").and_then(Value::as_u64).unwrap_or(0);
    let optional = |field: &str| -> Result<Option<Decimal>> {
        match row.get(field).filter(|v| !v.is_null()) {
            Some(_) => Ok(Some(decimal_of(row, field)?)),
            None => Ok(None),
        }
    };
    Ok(Level {
        after_months: u32::try_from(after).map_err(|_| Error::msg("after_months is too large"))?,
        frequency: Frequency::parse(text(row, "frequency").unwrap_or("monthly")).ok_or_else(|| Error::msg("a level has an unknown frequency"))?,
        amount: decimal_of(row, "amount")?,
        on: if text(row, "allocate_on") == Some("first") { AllocateOn::First } else { AllocateOn::Last },
        yearly_cap: optional("yearly_cap")?,
        total_cap: optional("total_cap")?,
        valid_days: row.get("valid_days").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()),
    })
}

fn levels_of(plan: &str) -> Result<Vec<Level>> {
    let rows: Vec<Record> = db::find("leave_plan_level").filter("plan", plan).order_by("after_months").limit(50).all()?;
    rows.iter().map(level_of).collect()
}

/// Days of this plan already granted to the assignment within a year.
fn granted_in_year(person: &str, leave_type: &str, assignment: &str, year: i32) -> Result<Decimal> {
    let prefix = format!("plan:{assignment}:");
    let mut sum = Decimal::zero(SCALE);
    let (first, last) = year_bounds(year).ok_or_else(|| Error::msg("that is not a year"))?;
    let rows: Vec<Record> = db::find::<Record>("leave_ledger")
        .filter("employee", person)
        .filter("leave_type", leave_type)
        .filter("kind", "accrual")
        .limit(5000)
        .all()?;
    for row in rows {
        let key = text(&row, "key").unwrap_or_default();
        if !key.starts_with(&prefix) {
            continue;
        }
        let day = date_of(&row, "date")?;
        if day >= first && day <= last {
            sum = sum + decimal_of(&row, "days")?;
        }
    }
    Ok(sum)
}

/// Run every active assignment up to `today`; returns the grants written. Safe to repeat.
pub fn run(today: NaiveDate) -> Result<u64> {
    let assignments: Vec<Record> = db::find("leave_plan_assignment").limit(5000).all()?;
    if assignments.is_empty() {
        return Ok(0);
    }
    let people: Vec<Record> = present_employees()?;
    let mut written = 0u64;
    for assignment in assignments {
        let plan = require("leave_plan", text(&assignment, "plan").unwrap_or_default(), "plan")?;
        if is_off(&plan, "is_active") {
            continue;
        }
        let person_id = text(&assignment, "employee").unwrap_or_default();
        let Some(person) = people.iter().find(|p| text(p, "id") == Some(person_id)) else { continue };
        let Some(hired) = text(person, "hire_date").and_then(|d| parse_date(d).ok()) else { continue };
        let kind = text(&plan, "leave_type").unwrap_or_default();
        let from = date_of(&assignment, "date_from")?;
        let to = text(&assignment, "date_to").map(parse_date).transpose()?;
        let levels = levels_of(id_of(&plan)?)?;
        if levels.is_empty() {
            continue;
        }
        let assignment_id = id_of(&assignment)?;
        // This year, and last year in case the job missed the turn of the year.
        for year in [today.year() - 1, today.year()] {
            if year < from.year() || year < hired.year() {
                continue;
            }
            for (frequency, period, day, level) in due_in_year(&levels, hired, from, to, year, today) {
                let key = format!("plan:{assignment_id}:{}:{year}:{period}", name_of(frequency));
                let granted = granted_in_year(person_id, kind, assignment_id, year)?;
                let balance = balance_on(person_id, kind, day)?.available;
                let amount = capped(level.amount, granted, level.yearly_cap, balance, level.total_cap);
                if amount.is_zero() {
                    // Record nothing: the cap may have room by the next grant day. The key stays unused.
                    continue;
                }
                let (_, last) = year_bounds(year).ok_or_else(|| Error::msg("that is not a year"))?;
                let valid_to = level.valid_days.map_or(last, |n| day + Duration::days(i64::from(n)));
                let entry = post_once(Post {
                    employee: person_id,
                    leave_type: kind,
                    kind: "accrual",
                    days: amount,
                    date: day,
                    valid_to: Some(valid_to),
                    key: Some(key),
                    reverses: None,
                    source: &format!("plan:{assignment_id}"),
                    note: None,
                })?;
                if entry.is_some() {
                    written += 1;
                }
            }
        }
    }
    Ok(written)
}

// ---- setting up -----------------------------------------------------------------------------

#[derive(Deserialize)]
struct NewPlan {
    name: String,
    leave_type: String,
    #[serde(default)]
    levels: Vec<Record>,
}

fn check_level(level: &Record) -> Result<()> {
    if Frequency::parse(text(level, "frequency").unwrap_or("monthly")).is_none() {
        return Err(Error::msg("a level's frequency is monthly, quarterly, half_yearly or yearly"));
    }
    let amount = decimal_of(level, "amount")?;
    if amount.is_negative() || amount.is_zero() {
        return Err(Error::msg("a level grants a positive number of days"));
    }
    amount.with_scale(SCALE).map_err(|_| Error::msg("days have at most 2 digits after the point"))?;
    for field in ["yearly_cap", "total_cap"] {
        if level.get(field).is_some_and(|v| !v.is_null()) && decimal_of(level, field)?.is_negative() {
            return Err(Error::msg(format!("{field} cannot be negative")));
        }
    }
    Ok(())
}

fn make_plan(input: NewPlan) -> Result<Record> {
    require_admin()?;
    if input.name.trim().is_empty() {
        return Err(Error::msg("a plan needs a name"));
    }
    require("leave_type", &input.leave_type, "leave type")?;
    if input.levels.is_empty() {
        return Err(Error::msg("a plan needs at least one level"));
    }
    let mut milestones = Vec::new();
    for level in &input.levels {
        check_level(level)?;
        let after = level.get("after_months").and_then(Value::as_u64).unwrap_or(0);
        if milestones.contains(&after) {
            return Err(Error::msg("two levels start after the same number of months"));
        }
        milestones.push(after);
    }
    if !milestones.contains(&0) {
        return Err(Error::msg("the first level starts at 0 months, or people in their first months would earn nothing"));
    }
    let plan: Record = db::create("leave_plan", &json!({ "name": input.name.trim(), "leave_type": input.leave_type, "is_active": true }))
        .map_err(|e| e.or("could not create the plan (the name may be taken)"))?;
    let plan_id = id_of(&plan)?.to_string();
    for level in input.levels {
        let mut data = crate::common::pick(
            &level,
            &["after_months", "frequency", "amount", "allocate_on", "yearly_cap", "total_cap", "valid_days"],
        );
        data["plan"] = json!(plan_id);
        db::create::<Record>("leave_plan_level", &data)?;
    }
    Ok(plan)
}

#[derive(Deserialize)]
struct Assign {
    employee: String,
    plan: String,
    date_from: String,
    #[serde(default)]
    date_to: Option<String>,
}

fn assign(input: Assign) -> Result<Record> {
    require_admin()?;
    crate::common::employee(&input.employee)?;
    require("leave_plan", &input.plan, "plan")?;
    let from = parse_date(&input.date_from)?;
    let to = input.date_to.as_deref().map(parse_date).transpose()?;
    aether_sdk::dates::check_order(from, to, "assignment")?;
    // One plan per type at a time for a person: two would grant twice.
    let plan = require("leave_plan", &input.plan, "plan")?;
    let others: Vec<Record> = db::find("leave_plan_assignment").filter("employee", input.employee.as_str()).limit(500).all()?;
    for other in others {
        let other_plan = require("leave_plan", text(&other, "plan").unwrap_or_default(), "plan")?;
        if text(&other_plan, "leave_type") != text(&plan, "leave_type") {
            continue;
        }
        let o_from = date_of(&other, "date_from")?;
        let o_to = text(&other, "date_to").map(parse_date).transpose()?;
        if aether_sdk::dates::overlaps(from, to, o_from, o_to) {
            return Err(Error::msg("this person already has a plan for this leave type over those dates: end it first"));
        }
    }
    let mut data = json!({ "employee": input.employee, "plan": input.plan, "date_from": format_date(from) });
    if let Some(end) = to {
        data["date_to"] = json!(format_date(end));
    }
    db::create("leave_plan_assignment", &data)
}

#[derive(Deserialize)]
struct EndAssignment {
    id: String,
    date_to: String,
}

fn end_assignment(input: EndAssignment) -> Result<Record> {
    require_admin()?;
    let row = require("leave_plan_assignment", &input.id, "assignment")?;
    let end = parse_date(&input.date_to)?;
    if end < date_of(&row, "date_from")? {
        return Err(Error::msg("the assignment would end before it starts"));
    }
    db::update::<Record>("leave_plan_assignment", &input.id, &json!({ "date_to": format_date(end) }))?.ok_or_else(|| Error::msg("the assignment is gone"))
}

#[derive(Deserialize, Default)]
struct RunOn {
    #[serde(default)]
    on: Option<String>,
}

handler! {
    /// Make an accrual plan with its levels (leave administrators).
    fn create_leave_plan(input: NewPlan) -> Record {
        make_plan(input)
    }

    /// Put a person on a plan over a span of dates (leave administrators).
    fn assign_leave_plan(input: Assign) -> Record {
        assign(input)
    }

    /// End a person's plan on a day (leave administrators).
    fn end_leave_plan_assignment(input: EndAssignment) -> Record {
        end_assignment(input)
    }

    /// Plans with their levels.
    fn list_leave_plans(_: Empty) -> Vec<Value> {
        let plans: Vec<Record> = db::find("leave_plan").order_by("name").limit(200).all()?;
        let mut out = Vec::new();
        for plan in plans {
            let levels: Vec<Record> = db::find("leave_plan_level").filter("plan", id_of(&plan)?).order_by("after_months").limit(50).all()?;
            out.push(json!({ "plan": plan, "levels": levels }));
        }
        Ok(out)
    }

    /// Grant what the plans owe up to a day (today when left out). Also part of the nightly job; safe to repeat.
    fn run_leave_plans(input: RunOn) -> Value {
        require_admin()?;
        let on = match input.on.as_deref() {
            Some(day) => parse_date(day)?,
            None => today_date()?,
        };
        Ok(json!({ "granted": run(on)? }))
    }
}

#[cfg(test)]
mod tests {
    use aether_sdk::dates::parse_date;

    use super::*;

    fn d(text: &str) -> NaiveDate {
        parse_date(text).unwrap_or_default()
    }

    fn n(text: &str) -> Decimal {
        Decimal::parse(text).and_then(|v| v.with_scale(SCALE)).unwrap_or_else(|_| Decimal::zero(SCALE))
    }

    fn level(after: u32, amount: &str, frequency: Frequency) -> Level {
        Level { after_months: after, frequency, amount: n(amount), on: AllocateOn::Last, yearly_cap: None, total_cap: None, valid_days: None }
    }

    #[test]
    fn the_level_in_force_is_the_last_milestone_reached() {
        let levels = vec![level(0, "1", Frequency::Monthly), level(24, "1.5", Frequency::Monthly), level(60, "2", Frequency::Monthly)];
        assert_eq!(level_for(&levels, 0).map(|l| l.after_months), Some(0));
        assert_eq!(level_for(&levels, 23).map(|l| l.after_months), Some(0));
        assert_eq!(level_for(&levels, 24).map(|l| l.after_months), Some(24));
        assert_eq!(level_for(&levels, 200).map(|l| l.after_months), Some(60));
        assert!(level_for(&[level(6, "1", Frequency::Monthly)], 3).is_none());
    }

    #[test]
    fn caps_cut_the_grant_and_never_go_negative() {
        let amount = n("2");
        assert_eq!(capped(amount, n("0"), None, n("0"), None), n("2"));
        assert_eq!(capped(amount, n("9"), Some(n("10")), n("0"), None), n("1"));
        assert_eq!(capped(amount, n("10"), Some(n("10")), n("0"), None), n("0"));
        assert_eq!(capped(amount, n("0"), None, n("19.5"), Some(n("20"))), n("0.5"));
        assert_eq!(capped(amount, n("0"), None, n("25"), Some(n("20"))), n("0"));
    }

    #[test]
    fn a_person_moves_up_a_level_when_their_service_reaches_it() {
        let levels = vec![level(0, "1", Frequency::Monthly), level(12, "2", Frequency::Monthly)];
        let due = due_in_year(&levels, d("2025-07-01"), d("2025-01-01"), None, 2026, d("2026-12-31"));
        let amounts: Vec<String> = due.iter().map(|(_, _, _, l)| l.amount.to_string()).collect();
        // Months of service on each month end of 2026: Jan 31 is 6 months; Jun 30 is 11; Jul 31 is 12.
        assert_eq!(due.len(), 12);
        assert_eq!(amounts[5], "1.00");
        assert_eq!(amounts[6], "2.00");
    }

    #[test]
    fn nothing_is_granted_before_hire_or_outside_the_assignment() {
        let levels = vec![level(0, "1", Frequency::Monthly)];
        let due = due_in_year(&levels, d("2026-03-15"), d("2026-01-01"), Some(d("2026-06-30")), 2026, d("2026-12-31"));
        let days: Vec<NaiveDate> = due.iter().map(|(_, _, day, _)| *day).collect();
        assert_eq!(days.first().copied(), Some(d("2026-03-31")));
        assert_eq!(days.last().copied(), Some(d("2026-06-30")));
    }

    #[test]
    fn a_change_of_frequency_grants_each_period_by_the_level_in_force() {
        let levels = vec![level(0, "6", Frequency::HalfYearly), level(12, "1", Frequency::Monthly)];
        let due = due_in_year(&levels, d("2025-07-01"), d("2025-01-01"), None, 2026, d("2026-12-31"));
        // January 31 is 6 months of service: half-yearly level, but only the June 30 half-year day exists
        // (6 months at Jan 31 does not match a half-yearly day); from July 31 (12 months) monthly applies.
        assert!(due.iter().any(|(f, _, day, _)| *f == Frequency::HalfYearly && *day == d("2026-06-30")));
        assert!(due.iter().filter(|(f, _, _, _)| *f == Frequency::Monthly).all(|(_, _, day, _)| *day >= d("2026-07-01")));
    }
}
