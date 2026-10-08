//! Pay codes, adjustments (the one funnel for extra pay), awards, and the period lock.
//!
//! Frappe's Additional Salary is overloaded by a `ref_doctype` and revert-by-cancel rewrites what was paid; Odoo
//! leaves extra pay to input lines typed on each payslip. Here every extra amount is an adjustment with a code, a
//! kind and either one pay date or a recurring span, carrying the plugin and reference that produced it so the same
//! source twice is one adjustment. Payroll asks only this plugin (`payroll_adjustments`). Once payroll approves a
//! period it locks through that day: nothing dated inside can change, a correction is a new adjustment later.

use aether_sdk::dates::{format_date, parse_date, NaiveDate};
use aether_sdk::db::Filter;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;
use std::collections::BTreeMap;

use crate::common::*;
use crate::rules::{period_amount, Terms};

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct NewCode {
    code: String,
    label: String,
    kind: String,
}

fn make_code(input: NewCode) -> Result<Record> {
    require_admin()?;
    let code = input.code.trim().to_lowercase();
    if code.is_empty() || code.len() > 40 || !code.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        return Err(Error::msg("a code is lower-case letters, digits and `_`, up to 40 characters: payroll reads it as the input `adj_<code>`"));
    }
    if !matches!(input.kind.as_str(), "earning" | "deduction") {
        return Err(Error::msg("a code is an earning or a deduction"));
    }
    if db::count("comp_code", Filter::eq("code", code.as_str()))? > 0 {
        return Err(Error::msg(format!("the code `{code}` exists")));
    }
    db::create("comp_code", &json!({ "code": code, "label": input.label, "kind": input.kind, "is_active": true }))
}

pub fn code_row(code: &str) -> Result<Record> {
    let row: Option<Record> = db::find("comp_code").filter("code", code).first()?;
    let row = row.ok_or_else(|| Error::msg(format!("there is no pay code `{code}`: create it first")))?;
    if is_off(&row, "is_active") {
        return Err(Error::msg(format!("the pay code `{code}` is switched off")));
    }
    Ok(row)
}

fn is_off(record: &Record, field: &str) -> bool {
    record.get(field) == Some(&Value::Bool(false))
}

#[derive(Deserialize)]
pub struct NewAdjustment {
    pub employee: String,
    pub code: String,
    pub amount: Decimal,
    /// `one_off` (with `pay_date`) or `recurring` (with `date_from`, optionally `date_to`).
    pub mode: String,
    #[serde(default)]
    pub pay_date: Option<String>,
    #[serde(default)]
    pub date_from: Option<String>,
    #[serde(default)]
    pub date_to: Option<String>,
    #[serde(default)]
    pub prorate: bool,
    #[serde(default)]
    pub reason: Option<String>,
    /// The plugin and document that asked for this; together they are the idempotency key.
    #[serde(default)]
    pub source_plugin: Option<String>,
    #[serde(default)]
    pub source_ref: Option<String>,
}

/// Make an adjustment; the same source twice returns the first. Only compensation administrators and other
/// plugins (the rules grant `via:*`) may call it.
pub fn add(input: NewAdjustment) -> Result<Record> {
    let amount = money(input.amount, "an adjustment")?;
    let code = code_row(&input.code)?;
    let person = employee(&input.employee)?;
    if !employed(&person) {
        return Err(Error::msg("this person has left: new extra pay is not added for them"));
    }
    let key = match (&input.source_plugin, &input.source_ref) {
        (Some(plugin), Some(reference)) if !plugin.is_empty() && !reference.is_empty() => format!("{plugin}:{reference}"),
        (None, None) => format!("manual:{}", next_number("adjustment", "ADJ-", 6)?),
        _ => return Err(Error::msg("a source names both the plugin and the reference, or neither")),
    };
    if let Some(existing) = db::find::<Record>("comp_adjustment").filter("idem_key", key.as_str()).first()? {
        return Ok(existing);
    }
    let mut data = json!({
        "employee": input.employee, "code_ref": id_of(&code)?, "code": input.code, "kind": code["kind"], "amount": amount, "mode": input.mode,
        "prorate": input.prorate, "status": "active", "idem_key": key,
    });
    match input.mode.as_str() {
        "one_off" => {
            let day = parse_date(input.pay_date.as_deref().ok_or_else(|| Error::msg("a one-off adjustment has a pay date"))?)?;
            require_open(day, "this adjustment")?;
            data["pay_date"] = json!(format_date(day));
        }
        "recurring" => {
            let from = parse_date(input.date_from.as_deref().ok_or_else(|| Error::msg("a recurring adjustment has a start date"))?)?;
            require_open(from, "this adjustment's start")?;
            if let Some(to) = input.date_to.as_deref() {
                let to = parse_date(to)?;
                if to < from {
                    return Err(Error::msg("the end is before the start"));
                }
                data["date_to"] = json!(format_date(to));
            }
            data["date_from"] = json!(format_date(from));
        }
        _ => return Err(Error::msg("an adjustment is `one_off` or `recurring`")),
    }
    if let Some(plugin) = &input.source_plugin {
        data["source_plugin"] = json!(plugin);
        data["source_ref"] = json!(input.source_ref);
    }
    if let Some(reason) = &input.reason {
        data["reason"] = json!(reason);
    }
    if let Some(who) = actor()? {
        data["created_by"] = json!(who);
    }
    db::create("comp_adjustment", &data)
}

pub fn cancel_adjustment_by_id(id: &str) -> Result<Record> {
    cancel(Id { id: id.to_string() })
}

/// Cancel an adjustment that has not reached a closed period. One that has is ended instead (`end_adjustment`).
fn cancel(input: Id) -> Result<Record> {
    let row = require("comp_adjustment", &input.id, "adjustment")?;
    if text(&row, "status") == Some("cancelled") {
        return Ok(row);
    }
    let first = if text(&row, "mode") == Some("one_off") { date_of(&row, "pay_date")? } else { date_of(&row, "date_from")? };
    require_open(first, "cancelling this adjustment (part of it is already paid)")?;
    db::update("comp_adjustment", &input.id, &json!({ "status": "cancelled" }))?.ok_or_else(|| Error::msg("the adjustment is gone"))
}

#[derive(Deserialize)]
struct End {
    id: String,
    date_to: String,
}

/// Stop a recurring adjustment after a day. The end cannot be before the locked day: what was paid stays paid.
fn end_recurring(input: End) -> Result<Record> {
    let row = require("comp_adjustment", &input.id, "adjustment")?;
    if text(&row, "mode") != Some("recurring") {
        return Err(Error::msg("only a recurring adjustment is ended; cancel a one-off"));
    }
    let to = parse_date(&input.date_to)?;
    if to < date_of(&row, "date_from")? {
        return Err(Error::msg("the end is before the start: cancel it instead"));
    }
    require_open(to, "the new end")?;
    db::update("comp_adjustment", &input.id, &json!({ "date_to": format_date(to) }))?.ok_or_else(|| Error::msg("the adjustment is gone"))
}

#[derive(Deserialize)]
struct Summary {
    employees: Vec<String>,
    from: String,
    to: String,
}

fn terms_of(row: &Record) -> Result<Terms> {
    Ok(if text(row, "mode") == Some("one_off") {
        Terms::OneOff { pay_date: date_of(row, "pay_date")? }
    } else {
        let to = text(row, "date_to").map(parse_date).transpose()?;
        Terms::Recurring { from: date_of(row, "date_from")?, to, prorate: row.get("prorate") == Some(&json!(true)) }
    })
}

/// What payroll asks about a period: per employee, each code's total as `adj_<code>`, the amounts as text, and
/// whether a withholding holds their pay back. Payroll puts a structure's `adj_*` inputs from this answer and
/// asks nobody else.
fn payroll_summary(input: Summary) -> Result<Value> {
    let (start, end): (NaiveDate, NaiveDate) = (parse_date(&input.from)?, parse_date(&input.to)?);
    if end < start {
        return Err(Error::msg("the period ends before it starts"));
    }
    let mut out: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
    for chunk in input.employees.chunks(100) {
        let rows: Vec<Record> = db::find("comp_adjustment").matching(Filter::one_of("employee", chunk.to_vec()).and(Filter::eq("status", "active"))).limit(1000).all()?;
        let mut totals: BTreeMap<(String, String), Decimal> = BTreeMap::new();
        for row in &rows {
            let amount = period_amount(decimal_of(row, "amount")?, terms_of(row)?, start, end)?;
            if amount.is_zero() {
                continue;
            }
            let key = (text(row, "employee").unwrap_or_default().to_string(), text(row, "code").unwrap_or_default().to_string());
            let sum = totals.remove(&key).unwrap_or_else(|| Decimal::zero(2));
            totals.insert(key, sum + amount);
        }
        for ((employee, code), sum) in totals {
            out.entry(employee).or_default().insert(format!("adj_{code}"), json!(sum));
        }
        for held in crate::withhold::withheld_in(chunk, start, end)? {
            out.entry(held).or_default().insert("withheld".to_string(), json!(true));
        }
    }
    Ok(json!(out))
}

#[derive(Deserialize)]
struct Lock {
    through: String,
}

/// Payroll calls this when it approves a run: nothing dated on or before the day can change any more. The day
/// only moves forward.
fn lock(input: Lock) -> Result<Record> {
    let day = parse_date(&input.through)?;
    // Who may close a period is the rule on `comp_lock` (administrators and calls that came through payroll).
    let existing: Option<Record> = db::find("comp_lock").first()?;
    match existing {
        Some(row) => {
            if day < date_of(&row, "locked_through")? {
                return Ok(row);
            }
            db::update("comp_lock", id_of(&row)?, &json!({ "locked_through": format_date(day) }))?.ok_or_else(|| Error::msg("the lock is gone"))
        }
        None => db::create("comp_lock", &json!({ "locked_through": format_date(day) })),
    }
}

// ---- awards: incentives and retention bonuses, approved by someone other than the requester

#[derive(Deserialize)]
struct NewAward {
    employee: String,
    kind: String,
    amount: Decimal,
    pay_date: String,
    #[serde(default)]
    reason: Option<String>,
}

fn request_award(input: NewAward) -> Result<Record> {
    if !matches!(input.kind.as_str(), "incentive" | "retention") {
        return Err(Error::msg("an award is an incentive or a retention bonus"));
    }
    let amount = money(input.amount, "an award")?;
    let person = employee(&input.employee)?;
    if !employed(&person) {
        return Err(Error::msg("this person has left"));
    }
    require_open(parse_date(&input.pay_date)?, "this award's pay date")?;
    let mut data = json!({ "employee": input.employee, "kind": input.kind, "amount": amount, "pay_date": input.pay_date, "status": "pending" });
    if let Some(reason) = input.reason {
        data["reason"] = json!(reason);
    }
    if let Some(who) = actor()? {
        data["requested_by"] = json!(who);
    }
    db::create("comp_award", &data)
}

#[derive(Deserialize)]
struct Decide {
    id: String,
    #[serde(default = "yes")]
    approve: bool,
}

fn yes() -> bool {
    true
}

fn decide_award(input: Decide) -> Result<Record> {
    require_admin()?;
    let award = require("comp_award", &input.id, "award")?;
    if text(&award, "status") != Some("pending") {
        return Err(Error::msg("this award has been decided"));
    }
    let who = actor()?;
    if who.is_some() && who.as_deref() == text(&award, "requested_by") {
        return Err(Error::msg("someone other than the person who asked decides an award"));
    }
    if !input.approve {
        return db::update("comp_award", &input.id, &json!({ "status": "rejected", "decided_by": who }))?.ok_or_else(|| Error::msg("the award is gone"));
    }
    let code = format!("award_{}", text(&award, "kind").unwrap_or("incentive"));
    ensure_code(&code, "earning")?;
    let adjustment = add(NewAdjustment {
        employee: text(&award, "employee").unwrap_or_default().to_string(),
        code,
        amount: decimal_of(&award, "amount")?,
        mode: "one_off".into(),
        pay_date: text(&award, "pay_date").map(str::to_string),
        date_from: None,
        date_to: None,
        prorate: false,
        reason: text(&award, "reason").map(str::to_string),
        source_plugin: Some("hr_compensation".into()),
        source_ref: Some(format!("award:{}", input.id)),
    })?;
    db::update("comp_award", &input.id, &json!({ "status": "approved", "decided_by": who, "adjustment": adjustment["id"] }))?.ok_or_else(|| Error::msg("the award is gone"))
}

/// A code the plugin itself needs (awards, gratuity) is made on first use.
pub fn ensure_code(code: &str, kind: &str) -> Result<()> {
    if db::count("comp_code", Filter::eq("code", code))? == 0 {
        db::create::<Record>("comp_code", &json!({ "code": code, "label": code.replace('_', " "), "kind": kind, "is_active": true }))?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct CodeNeed {
    code: String,
    kind: String,
}

/// For other plugins (leave encashment, loans): make the pay code they need, once. Codes made this way are
/// lower-case snake names like any other, and an existing code is never changed.
fn need_code(input: CodeNeed) -> Result<Record> {
    let code = input.code.trim().to_lowercase();
    if code.is_empty() || code.len() > 40 || !code.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        return Err(Error::msg("a code is lower-case letters, digits and `_`"));
    }
    if !matches!(input.kind.as_str(), "earning" | "deduction") {
        return Err(Error::msg("a code is an earning or a deduction"));
    }
    ensure_code(&code, &input.kind)?;
    db::find::<Record>("comp_code").filter("code", code.as_str()).first()?.ok_or_else(|| Error::msg("the code could not be made"))
}

handler! {
    fn ensure_pay_code(input: CodeNeed) -> Record {
        need_code(input)
    }

    fn create_code(input: NewCode) -> Record {
        make_code(input)
    }

    fn list_codes(_: Empty) -> Vec<Record> {
        db::find("comp_code").order_by("code").limit(500).all()
    }

    fn add_adjustment(input: NewAdjustment) -> Record {
        add(input)
    }

    fn cancel_adjustment(input: Id) -> Record {
        cancel(input)
    }

    fn end_adjustment(input: End) -> Record {
        end_recurring(input)
    }

    fn list_adjustments(input: Id) -> Vec<Record> {
        db::find("comp_adjustment").filter("employee", input.id.as_str()).order_by("-id").limit(200).all()
    }

    /// For payroll: the `adj_<code>` totals of many people within a period, and who is withheld.
    fn payroll_adjustments(input: Summary) -> Value {
        payroll_summary(input)
    }

    /// For payroll: periods through this day are closed.
    fn lock_through(input: Lock) -> Record {
        lock(input)
    }

    fn get_lock(_: Empty) -> Option<Record> {
        db::find("comp_lock").first()
    }

    fn request_comp_award(input: NewAward) -> Record {
        request_award(input)
    }

    fn decide_comp_award(input: Decide) -> Record {
        decide_award(input)
    }

    fn list_awards(_: Empty) -> Vec<Record> {
        db::find("comp_award").order_by("-id").limit(200).all()
    }
}
