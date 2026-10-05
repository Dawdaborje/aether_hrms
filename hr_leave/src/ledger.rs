//! The ledger of leave days: reading it, writing it, and the jobs that fill and tidy it.
//!
//! Entries are never edited or deleted. A grant is `allocation`, `accrual` or `carry_forward`;
//! leave is first a `reservation` and then, when approved, a `usage`; a cancellation is a
//! `reversal` of the entry it cancels. Every entry has a `key` unique per person and type, so a
//! system entry (this year's grant, this month's accrual) can never be written twice.

use aether_sdk::dates::{format_date, parse_date, Datelike, Duration, NaiveDate};
use aether_sdk::db::Filter;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{
    date_of, decimal_of, id_of, is_off, next_number, require, require_admin, text, today_date, works_here, Record,
};
use crate::rules::{
    balance, earned_amount, earned_due, lapsed, share_of_year, year_bounds, AllocateOn, Balance, Entry, Frequency, Kind, SCALE,
};

#[derive(Deserialize)]
struct Grant {
    employee: String,
    leave_type: String,
    days: Decimal,
    valid_from: String,
    #[serde(default)]
    valid_to: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct ForEmployee {
    employee: String,
    #[serde(default)]
    leave_type: Option<String>,
    /// The day to work the balance out for; today when left out.
    #[serde(default)]
    on: Option<String>,
}

#[derive(Deserialize, Default)]
struct YearlyRun {
    #[serde(default)]
    year: Option<i32>,
}

fn to_entry(record: &Record) -> Result<Entry> {
    Ok(Entry {
        id: id_of(record)?.to_string(),
        kind: Kind::parse(text(record, "kind").unwrap_or_default()).ok_or_else(|| Error::msg("a ledger entry has an unknown kind"))?,
        days: decimal_of(record, "days")?,
        date: date_of(record, "date")?,
        valid_to: text(record, "valid_to").map(parse_date).transpose()?,
        reverses: text(record, "reverses").map(str::to_string),
    })
}

/// Every ledger entry of one person and type.
pub fn entries(employee: &str, leave_type: &str) -> Result<Vec<Entry>> {
    let rows: Vec<Record> = db::find::<Record>("leave_ledger")
        .filter("employee", employee)
        .filter("leave_type", leave_type)
        .limit(5000)
        .all()?;
    rows.iter().map(to_entry).collect()
}

/// The balance of one person and type on a day.
pub fn balance_on(employee: &str, leave_type: &str, on: NaiveDate) -> Result<Balance> {
    let soon = on + Duration::days(30);
    balance(&entries(employee, leave_type)?, on, soon)
}

/// Write an entry. `key` makes it unique; none: a fresh one is numbered.
pub struct Post<'a> {
    pub employee: &'a str,
    pub leave_type: &'a str,
    pub kind: &'a str,
    pub days: Decimal,
    pub date: NaiveDate,
    pub valid_to: Option<NaiveDate>,
    pub key: Option<String>,
    pub reverses: Option<&'a str>,
    pub source: &'a str,
    pub note: Option<&'a str>,
}

pub fn post(entry: Post<'_>) -> Result<Record> {
    let key = match entry.key {
        Some(key) => key,
        None => next_number("ledger", "M-", 8)?,
    };
    let mut data = json!({
        "employee": entry.employee, "leave_type": entry.leave_type, "kind": entry.kind,
        "days": entry.days.with_scale(SCALE)?, "date": format_date(entry.date), "key": key, "source": entry.source,
    });
    if let Some(end) = entry.valid_to {
        data["valid_to"] = json!(format_date(end));
    }
    if let Some(of) = entry.reverses {
        data["reverses"] = json!(of);
    }
    if let Some(note) = entry.note {
        data["note"] = json!(note);
    }
    db::create("leave_ledger", &data)
}

/// Write a system entry once: `None` when this key was written already.
pub fn post_once(entry: Post<'_>) -> Result<Option<Record>> {
    let key = entry.key.clone().ok_or_else(|| Error::msg("a system entry needs a key"))?;
    let existing = db::count(
        "leave_ledger",
        Filter::eq("employee", entry.employee).and(Filter::eq("leave_type", entry.leave_type)).and(Filter::eq("key", key.as_str())),
    )?;
    if existing > 0 {
        return Ok(None);
    }
    post(entry).map(Some)
}

/// Cancel the entry with this key by writing its mirror. Cancelling twice, or something never
/// written, does nothing and says so.
pub fn reverse_key(employee: &str, leave_type: &str, key: &str, note: &str) -> Result<bool> {
    let found = db::find::<Record>("leave_ledger")
        .filter("employee", employee)
        .filter("leave_type", leave_type)
        .filter("key", key)
        .first()?;
    let Some(target) = found else { return Ok(false) };
    let target_id = id_of(&target)?.to_string();
    let already = db::count("leave_ledger", Filter::eq("reverses", target_id.as_str()))?;
    if already > 0 {
        return Ok(false);
    }
    let days = -decimal_of(&target, "days")?;
    post(Post {
        employee,
        leave_type,
        kind: "reversal",
        days,
        date: today_date()?,
        valid_to: None,
        key: Some(format!("rev:{target_id}")),
        reverses: Some(&target_id),
        source: text(&target, "source").unwrap_or("reversal"),
        note: Some(note),
    })?;
    Ok(true)
}

fn parse_on(on: &Option<String>) -> Result<NaiveDate> {
    match on {
        Some(day) => parse_date(day),
        None => today_date(),
    }
}

fn balance_report(input: ForEmployee) -> Result<Vec<Value>> {
    let on = parse_on(&input.on)?;
    let types: Vec<Record> = match &input.leave_type {
        Some(kind) => vec![require("leave_type", kind, "leave type")?],
        None => db::find("leave_type").order_by("name").limit(200).all()?,
    };
    let mut out = Vec::new();
    for kind in types {
        let b = balance_on(&input.employee, id_of(&kind)?, on)?;
        out.push(json!({
            "leave_type": id_of(&kind)?, "name": kind["name"],
            "allocated": b.allocated, "spent": b.spent, "pending": b.pending,
            "available": b.available, "expiring_soon": b.expiring_soon,
        }));
    }
    Ok(out)
}

/// A manual grant or adjustment by a leave administrator.
fn grant(input: Grant, kind: &str) -> Result<Record> {
    require_admin()?;
    let person = crate::common::employee(&input.employee)?;
    if !works_here(&person) {
        return Err(Error::msg("this person no longer works here"));
    }
    require("leave_type", &input.leave_type, "leave type")?;
    if input.days.is_zero() || (kind == "allocation" && input.days.is_negative()) {
        return Err(Error::msg("a grant is a positive number of days"));
    }
    input.days.with_scale(SCALE).map_err(|_| Error::msg("days have at most 2 digits after the point"))?;
    let from = parse_date(&input.valid_from)?;
    let to = input.valid_to.as_deref().map(parse_date).transpose()?;
    if to.is_some_and(|to| to < from) {
        return Err(Error::msg("the grant ends before it starts"));
    }
    let who = context::current()?.actor.id.unwrap_or_default();
    post(Post {
        employee: &input.employee,
        leave_type: &input.leave_type,
        kind,
        days: input.days,
        date: from,
        valid_to: to,
        key: None,
        reverses: None,
        source: &format!("manual:{who}"),
        note: input.note.as_deref(),
    })
}

/// The people the yearly and nightly runs work for.
fn present_employees() -> Result<Vec<Record>> {
    let mut all = Vec::new();
    let mut offset = 0u32;
    loop {
        let page: Vec<Record> = plugins::call("hr", "list_employees", &json!({ "limit": 200, "offset": offset }))?;
        let count = page.len();
        all.extend(page.into_iter().filter(works_here));
        if count < 200 {
            break;
        }
        offset += count as u32;
    }
    Ok(all)
}

fn hired_on(person: &Record) -> Option<NaiveDate> {
    text(person, "hire_date").and_then(|d| parse_date(d).ok())
}

/// Carried-over days for a new year: what was left on its last day, up to the type's cap.
fn carry_over(person: &Record, kind: &Record, year: i32) -> Result<Option<Record>> {
    if !kind.get("carry_forward").is_some_and(|v| v == &json!(true)) {
        return Ok(None);
    }
    let (first, last) = year_bounds(year).ok_or_else(|| Error::msg("that is not a year"))?;
    let previous_end = first - Duration::days(1);
    let left = balance_on(id_of(person)?, id_of(kind)?, previous_end)?.available;
    if left.is_negative() || left.is_zero() {
        return Ok(None);
    }
    let cap = decimal_of(kind, "carry_forward_max")?;
    let days = if !cap.is_zero() && left > cap { cap } else { left };
    let lasts = kind.get("carry_forward_days").and_then(Value::as_i64).unwrap_or(0);
    let valid_to = if lasts > 0 { (first + Duration::days(lasts - 1)).min(last) } else { last };
    post_once(Post {
        employee: id_of(person)?,
        leave_type: id_of(kind)?,
        kind: "carry_forward",
        days,
        date: first,
        valid_to: Some(valid_to),
        key: Some(format!("carry:{year}")),
        reverses: None,
        source: "yearly",
        note: Some(&format!("carried over from {}", year - 1)),
    })
}

/// Give everyone the year's leave of each type that has yearly days and is not earned in
/// instalments, a share for people who joined during the year, plus carried-over days. Safe to
/// run again: nothing is written twice.
fn allocate_year(year: i32) -> Result<Value> {
    let (first, last) = year_bounds(year).ok_or_else(|| Error::msg("that is not a year"))?;
    let types: Vec<Record> = db::find::<Record>("leave_type").limit(200).all()?;
    let people = present_employees()?;
    let (mut granted, mut carried, mut already) = (0u64, 0u64, 0u64);
    for kind in types.iter().filter(|t| !is_off(t, "is_active")) {
        let annual = decimal_of(kind, "annual_days")?;
        let earned = kind.get("earned") == Some(&json!(true));
        for person in &people {
            if carry_over(person, kind, year)?.is_some() {
                carried += 1;
            }
            if earned || annual.is_zero() {
                continue;
            }
            let days = if is_off(kind, "pro_rata") { annual } else { share_of_year(annual, hired_on(person), year)? };
            if days.is_zero() {
                continue;
            }
            let written = post_once(Post {
                employee: id_of(person)?,
                leave_type: id_of(kind)?,
                kind: "allocation",
                days,
                date: first,
                valid_to: Some(last),
                key: Some(format!("year:{year}")),
                reverses: None,
                source: "yearly",
                note: None,
            })?;
            if written.is_some() { granted += 1 } else { already += 1 }
        }
    }
    Ok(json!({ "year": year, "grants": granted, "carried_over": carried, "already_had": already }))
}

/// Grant the instalments of earned leave that have come due this year. A person only earns for
/// periods that start after they joined. Safe to run again.
fn accrue(today: NaiveDate) -> Result<u64> {
    let types: Vec<Record> = db::find::<Record>("leave_type").filter("earned", true).limit(200).all()?;
    let people = present_employees()?;
    let year = today.year();
    let (_, last) = year_bounds(year).ok_or_else(|| Error::msg("that is not a year"))?;
    let mut written = 0u64;
    for kind in types.iter().filter(|t| !is_off(t, "is_active")) {
        let annual = decimal_of(kind, "annual_days")?;
        if annual.is_zero() {
            continue;
        }
        let frequency = Frequency::parse(text(kind, "earned_frequency").unwrap_or("monthly")).unwrap_or(Frequency::Monthly);
        let on = if text(kind, "allocate_on") == Some("first") { AllocateOn::First } else { AllocateOn::Last };
        let rounding = text(kind, "earned_rounding").filter(|r| *r != "none");
        let amount = earned_amount(annual, frequency, rounding)?;
        if amount.is_zero() {
            continue;
        }
        for person in &people {
            if carry_over(person, kind, year)?.is_some() {
                written += 1;
            }
            let hired = hired_on(person);
            for (period, day) in earned_due(frequency, on, year, today) {
                if hired.is_some_and(|hired| hired > day) {
                    continue;
                }
                let entry = post_once(Post {
                    employee: id_of(person)?,
                    leave_type: id_of(kind)?,
                    kind: "accrual",
                    days: amount,
                    date: day,
                    valid_to: Some(last),
                    key: Some(format!("earn:{year}:{period}")),
                    reverses: None,
                    source: "accrual",
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

/// Record, once, what lapsed: grants whose last day has passed with days unused.
fn record_expiry(today: NaiveDate) -> Result<u64> {
    let mut written = 0u64;
    let types: Vec<Record> = db::find::<Record>("leave_type").limit(200).all()?;
    for person in present_employees()? {
        for kind in &types {
            let all = entries(id_of(&person)?, id_of(kind)?)?;
            for (grant, days) in lapsed(&all, today) {
                let entry = post_once(Post {
                    employee: id_of(&person)?,
                    leave_type: id_of(kind)?,
                    kind: "expiry",
                    days: -days,
                    date: today,
                    valid_to: None,
                    key: Some(format!("expire:{grant}")),
                    reverses: None,
                    source: "lapse",
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

/// What the nightly job did, for the log.
pub fn nightly_ledger(today: NaiveDate) -> Result<Value> {
    let accrued = accrue(today)?;
    let expired = record_expiry(today)?;
    Ok(json!({ "accrued": accrued, "expired": expired }))
}

handler! {
    /// Give an employee days of a leave type over a period (leave administrators).
    fn allocate_leave(input: Grant) -> Record {
        grant(input, "allocation")
    }

    /// Add or take away days by hand, with a note (leave administrators).
    fn adjust_leave(input: Grant) -> Record {
        grant(input, "adjustment")
    }

    /// Give everyone their yearly leave and carry-over (also runs on 1 January; safe to run again).
    fn allocate_yearly(input: YearlyRun) -> Value {
        let year = match input.year {
            Some(year) => year,
            None => today_date()?.year(),
        };
        allocate_year(year)
    }

    /// Allocated, spent, pending, available and expiring-soon days of each leave type.
    fn leave_balances(input: ForEmployee) -> Vec<Value> {
        balance_report(input)
    }

    /// The ledger of one person and type, newest first.
    fn leave_ledger_entries(input: ForEmployee) -> Vec<Record> {
        let kind = input.leave_type.as_deref().ok_or_else(|| Error::msg("name the leave type"))?;
        db::find("leave_ledger").filter("employee", input.employee.as_str()).filter("leave_type", kind).order_by("-date").limit(1000).all()
    }
}
