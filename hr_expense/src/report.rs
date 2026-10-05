//! Expense reports and their lines.
//!
//! A line is converted to the report's currency **once**, when it is made, at a rate that is stored
//! with it, so later rate changes never alter history and the report is the sum of its lines. A line's
//! policy limits are looked up for the *expense date* (Odoo uses the employee's job today).

use std::collections::HashMap;

use aether_sdk::dates::{format_date, parse_date, Datelike, Duration, NaiveDate};
use aether_sdk::db::Filter;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use sha2::{Digest, Sha256};

use crate::common::{
    approver_of, currency_places, decimal_of, employee, flags_of, id_of, join, my_employee, next_number, optional_decimal, pick, rate_between,
    require, text, today_date, works_here, Record, PROXY,
};
use crate::rules::{check_limit, convert, duplicate_key, quantity_amount, total, Enforcement, Outcome, Period};

const LINE_INPUT: &[&str] = &["category", "date", "description", "merchant", "payer", "amount", "currency", "quantity", "note"];
/// How far back an expense may be dated.
const MAX_AGE_DAYS: i64 = 180;

#[derive(Deserialize)]
struct NewReport {
    purpose: String,
    #[serde(default)]
    employee: Option<String>,
    #[serde(default)]
    settlement_currency: Option<String>,
}

#[derive(Deserialize)]
struct AddLine {
    report: String,
    #[serde(flatten)]
    fields: Record,
}

#[derive(Deserialize)]
struct ChangeLine {
    id: String,
    #[serde(flatten)]
    fields: Record,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Receipt {
    line: String,
    name: String,
    content_base64: String,
}

#[derive(Deserialize)]
struct Reason {
    id: String,
    reason: String,
}

fn caller_may_act_for(report_employee: &str) -> Result<()> {
    let me = my_employee()?;
    if me.as_ref().is_some_and(|m| text(m, "id") == Some(report_employee)) {
        return Ok(());
    }
    if context::current()?.has_role(PROXY) {
        return Ok(());
    }
    Err(Error::msg("only the person (or an expense proxy) can change their report"))
}

fn create(input: NewReport) -> Result<Record> {
    if input.purpose.trim().is_empty() {
        return Err(Error::msg("say what the report is for"));
    }
    let me = my_employee()?;
    let person = match (&input.employee, me) {
        (Some(id), Some(me)) if text(&me, "id") == Some(id.as_str()) => me,
        (Some(id), _) => {
            if !context::current()?.has_role(PROXY) {
                return Err(Error::msg("you can only create reports for yourself"));
            }
            employee(id)?
        }
        (None, Some(me)) => me,
        (None, None) => return Err(Error::msg("you are not an employee")),
    };
    if !works_here(&person) {
        return Err(Error::msg("this person no longer works here"));
    }
    let currency = match input.settlement_currency {
        Some(code) => code.to_uppercase(),
        None => {
            let base: Value = plugins::call("currency", "get_base", &json!({}))?;
            base.get("code").and_then(Value::as_str).ok_or_else(|| Error::msg("no base currency is set: name the settlement currency"))?.to_string()
        }
    };
    currency_places(&currency)?;
    let created_by = context::current()?.actor.id.unwrap_or_default();
    db::create(
        "expense_report",
        &json!({
            "number": next_number("report", "EXP-", 5)?, "employee": id_of(&person)?, "created_by": created_by, "purpose": input.purpose,
            "state": "draft", "settlement_currency": currency,
        }),
    )
}

/// A report that its person may still change: theirs, and a draft.
fn editable(report_id: &str) -> Result<Record> {
    let report = require("expense_report", report_id, "report")?;
    caller_may_act_for(text(&report, "employee").unwrap_or_default())?;
    if text(&report, "state") != Some("draft") {
        return Err(Error::msg("only a draft report can be changed: withdraw it first"));
    }
    Ok(report)
}

/// Build the stored fields of a line: the category's rules, the amount, the conversion.
fn build_line(report: &Record, input: &Record, existing: Option<&Record>) -> Result<Record> {
    let merged = |field: &str| input.get(field).or_else(|| existing.and_then(|e| e.get(field))).cloned();
    let category_id = merged("category").and_then(|v| v.as_str().map(str::to_string)).ok_or_else(|| Error::msg("a line has a category"))?;
    let category = require("expense_category", &category_id, "category")?;
    if category.get("is_active") == Some(&json!(false)) {
        return Err(Error::msg("this category is no longer used"));
    }
    let date = parse_date(merged("date").as_ref().and_then(Value::as_str).ok_or_else(|| Error::msg("a line has a date"))?)?;
    let today = today_date()?;
    if date > today {
        return Err(Error::msg("that expense is dated in the future"));
    }
    if today - date > Duration::days(MAX_AGE_DAYS) {
        return Err(Error::msg(format!("expenses older than {MAX_AGE_DAYS} days cannot be claimed here")));
    }
    let settlement = text(report, "settlement_currency").unwrap_or("").to_string();
    let places = currency_places(&settlement)?;
    let description = merged("description").and_then(|v| v.as_str().map(str::to_string)).filter(|d| !d.trim().is_empty()).ok_or_else(|| Error::msg("describe the expense"))?;

    let mut data = json!({
        "report": id_of(report)?, "employee": text(report, "employee"), "category": category_id,
        "date": format_date(date), "description": description,
    });
    for field in ["merchant", "payer", "note"] {
        if let Some(value) = merged(field) {
            data[field] = value;
        }
    }
    let kind = text(&category, "kind").unwrap_or("receipt");
    let (amount, currency) = if matches!(kind, "mileage" | "per_diem") {
        // Quantity times the category's rate, copied now so a later change does not rewrite history.
        let quantity: Decimal = serde_json::from_value(merged("quantity").ok_or_else(|| Error::msg("give the distance or the days"))?)
            .map_err(|e| Error::msg(format!("quantity: {e}")))?;
        let rate = decimal_of(&category, "unit_rate")?;
        let currency = text(&category, "unit_currency").unwrap_or("").to_uppercase();
        let paid = currency_places(&currency)?;
        data["quantity"] = json!(quantity.with_scale(2).map_err(|_| Error::msg("the quantity has at most 2 digits after the point"))?);
        data["unit_rate"] = json!(rate);
        (quantity_amount(quantity, rate, paid)?, currency)
    } else {
        let amount: Decimal = serde_json::from_value(merged("amount").ok_or_else(|| Error::msg("give the amount"))?)
            .map_err(|e| Error::msg(format!("amount: {e}")))?;
        if amount.is_negative() || amount.is_zero() {
            return Err(Error::msg("the amount must be more than zero"));
        }
        let currency = merged("currency").and_then(|v| v.as_str().map(str::to_uppercase)).ok_or_else(|| Error::msg("give the currency of the receipt"))?;
        let paid = currency_places(&currency)?;
        (amount.with_scale(paid).map_err(|_| Error::msg(format!("{currency} has {paid} digits after the point")))?, currency)
    };
    let rate = rate_between(&currency, &settlement, date)?;
    let claimed = convert(amount, rate, places)?;
    if claimed.is_zero() {
        return Err(Error::msg("that comes to nothing once converted"));
    }
    data["amount"] = json!(amount);
    data["currency"] = json!(currency);
    data["rate"] = json!(rate);
    data["rate_date"] = json!(format_date(date));
    data["claimed"] = json!(claimed);
    data["dup_key"] = json!(duplicate_key(&format_date(date), text(&data, "merchant").unwrap_or(""), &amount, &currency));
    Ok(data)
}

/// The report's total is the sum of its lines' claimed amounts.
pub fn refresh_total(report: &Record) -> Result<Record> {
    let id = id_of(report)?;
    let lines: Vec<Record> = db::find::<Record>("expense_line").filter("report", id).limit(500).all()?;
    let places = currency_places(text(report, "settlement_currency").unwrap_or(""))?;
    let amounts: Vec<Decimal> = lines.iter().map(|l| decimal_of(l, "claimed")).collect::<Result<_>>()?;
    let claimed = total(&amounts, places);
    db::update::<Record>("expense_report", id, &json!({ "claimed_total": claimed }))?.ok_or_else(|| Error::msg("the report is gone"))
}

fn add_line(input: AddLine) -> Result<Record> {
    let report = editable(&input.report)?;
    let mut data = build_line(&report, &pick(&input.fields, LINE_INPUT), None)?;
    data["sequence"] = json!(db::count("expense_line", Filter::eq("report", input.report.as_str()))? + 1);
    let line: Record = db::create("expense_line", &data).map_err(|e| e.or("could not add the line"))?;
    refresh_total(&report)?;
    Ok(line)
}

fn change_line(input: ChangeLine) -> Result<Record> {
    let existing = require("expense_line", &input.id, "line")?;
    let report = editable(text(&existing, "report").unwrap_or_default())?;
    let changes = pick(&input.fields, LINE_INPUT);
    if changes.as_object().is_some_and(|f| f.is_empty()) {
        return Err(Error::msg("there is nothing to change"));
    }
    let data = build_line(&report, &changes, Some(&existing))?;
    // Receipts and flags stay with the line.
    let updated = db::update::<Record>("expense_line", &input.id, &data)?.ok_or_else(|| Error::msg("the line is gone"))?;
    refresh_total(&report)?;
    Ok(updated)
}

fn remove_line(input: Id) -> Result<Record> {
    let existing = require("expense_line", &input.id, "line")?;
    let report = editable(text(&existing, "report").unwrap_or_default())?;
    let gone = db::delete::<Record>("expense_line", &input.id)?.ok_or_else(|| Error::msg("the line is gone"))?;
    refresh_total(&report)?;
    Ok(gone)
}

/// Keep a receipt file with a line. Its content hash is kept too, so the same receipt on another
/// claim is noticed.
fn attach(input: Receipt) -> Result<Record> {
    let line = require("expense_line", &input.line, "line")?;
    editable(text(&line, "report").unwrap_or_default())?;
    let bytes = STANDARD.decode(input.content_base64.trim()).map_err(|_| Error::msg("the receipt is not valid base64"))?;
    if bytes.is_empty() {
        return Err(Error::msg("the receipt is empty"));
    }
    let safe: String = input.name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' }).collect();
    let safe = safe.trim_matches('.').to_string();
    if safe.is_empty() {
        return Err(Error::msg("name the receipt file"));
    }
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let mut keys: Vec<String> = text(&line, "receipts").map(|s| s.split(',').map(str::to_string).collect()).unwrap_or_default();
    let mut hashes: Vec<String> = text(&line, "receipt_hashes").map(|s| s.split(',').map(str::to_string).collect()).unwrap_or_default();
    if hashes.contains(&hash) {
        return Err(Error::msg("this receipt is already on the line"));
    }
    if keys.len() >= 5 {
        return Err(Error::msg("a line has at most 5 receipts"));
    }
    let key = format!("receipts/{}/{}-{safe}", id_of(&line)?.replace([':', '/'], "_"), keys.len() + 1);
    aether_sdk::storage::write_bytes(&key, &bytes)?;
    keys.push(key);
    hashes.push(hash);
    db::update::<Record>("expense_line", &input.line, &json!({ "receipts": join(&keys), "receipt_hashes": join(&hashes) }))?
        .ok_or_else(|| Error::msg("the line is gone"))
}

/// The policies that apply to a category for a person on a day, the most specific per period.
fn policies_for(category: &str, employee_id: &str, job: Option<&str>, day: NaiveDate) -> Result<Vec<Record>> {
    let all: Vec<Record> = db::find::<Record>("expense_policy").filter("category", category).limit(200).all()?;
    let on = format_date(day);
    let mut best: HashMap<String, (u8, Record)> = HashMap::new();
    for policy in all {
        if text(&policy, "valid_from").is_some_and(|from| from > on.as_str()) || text(&policy, "valid_to").is_some_and(|to| to < on.as_str()) {
            continue;
        }
        let rank = match (text(&policy, "scope_kind").unwrap_or("all"), text(&policy, "scope_ref")) {
            ("employee", Some(r)) if r == employee_id => 3,
            ("job", Some(r)) if Some(r) == job => 2,
            ("all", _) => 1,
            _ => continue,
        };
        let period = text(&policy, "period").unwrap_or("per_item").to_string();
        if best.get(&period).is_none_or(|(had, _)| rank > *had) {
            best.insert(period, (rank, policy));
        }
    }
    Ok(best.into_values().map(|(_, p)| p).collect())
}

/// The person's job on a day, from their dated employment records.
fn job_on(employee_id: &str, day: NaiveDate) -> Result<Option<String>> {
    let history: Vec<Record> = plugins::call("hr", "employment_history", &json!({ "employee": employee_id }))?;
    let on = format_date(day);
    Ok(history.iter().filter(|h| text(h, "date_from").is_some_and(|d| d <= on.as_str())).max_by_key(|h| text(h, "date_from").map(str::to_string)).and_then(|h| text(h, "job").map(str::to_string)))
}

/// Claimed so far in the policy's period, by this person for this category.
fn already(period: Period, employee_id: &str, category: &str, day: NaiveDate, report_id: &str, running: &HashMap<(String, String), Decimal>, places: u32) -> Result<Decimal> {
    let mut filter = Filter::eq("employee", employee_id).and(Filter::eq("category", category));
    match period {
        Period::PerItem => return Ok(Decimal::zero(places)),
        Period::PerDay => filter = filter.and(Filter::eq("date", format_date(day))),
        Period::PerReport => filter = filter.and(Filter::eq("report", report_id)),
        Period::PerMonth => {
            let first = NaiveDate::from_ymd_opt(day.year(), day.month(), 1).unwrap_or(day);
            let next = if day.month() == 12 { NaiveDate::from_ymd_opt(day.year() + 1, 1, 1) } else { NaiveDate::from_ymd_opt(day.year(), day.month() + 1, 1) }.unwrap_or(day);
            filter = filter.and(Filter::gte("date", format_date(first))).and(Filter::lt("date", format_date(next)));
        }
    }
    let lines: Vec<Record> = db::find::<Record>("expense_line").matching(filter).limit(1000).all()?;
    // Other reports count when they were sent (not drafts the person may never send); this report's
    // own lines are counted as they are processed.
    let mut sum = Decimal::zero(places);
    for line in &lines {
        let line_report = text(line, "report").unwrap_or_default();
        if line_report == report_id {
            continue;
        }
        let state = text(&require("expense_report", line_report, "report")?, "state").map(str::to_string);
        if matches!(state.as_deref(), Some("submitted" | "approved" | "closed")) {
            sum = sum + decimal_of(line, "claimed")?;
        }
    }
    if let Some(mine) = running.get(&(bucket(period, day), category.to_string())) {
        sum = sum + *mine;
    }
    Ok(sum)
}

/// Which running total a line belongs to for a period: the same day, month or report.
fn bucket(period: Period, day: NaiveDate) -> String {
    match period {
        Period::PerItem => "item".into(),
        Period::PerDay => format!("day:{}", format_date(day)),
        Period::PerMonth => format!("month:{}-{:02}", day.year(), day.month()),
        Period::PerReport => "report".into(),
    }
}

fn submit(input: Id) -> Result<Record> {
    let report = require("expense_report", &input.id, "report")?;
    caller_may_act_for(text(&report, "employee").unwrap_or_default())?;
    if text(&report, "state") != Some("draft") {
        return Err(Error::msg("only a draft report can be sent"));
    }
    let person_id = text(&report, "employee").unwrap_or_default().to_string();
    let person = employee(&person_id)?;
    if !works_here(&person) {
        return Err(Error::msg("this person no longer works here"));
    }
    let settlement = text(&report, "settlement_currency").unwrap_or("").to_string();
    let places = currency_places(&settlement)?;
    let mut lines: Vec<Record> = db::find::<Record>("expense_line").filter("report", input.id.as_str()).limit(500).all()?;
    // Oldest expense first, and within a day in the order they were added: a limit bites the later claim.
    lines.sort_by_key(|l| (text(l, "date").map(str::to_string), l.get("sequence").and_then(Value::as_i64)));
    if lines.is_empty() {
        return Err(Error::msg("a report needs at least one line"));
    }

    // Other sent lines of this person, for duplicates.
    let sent: Vec<Record> = {
        let mine: Vec<Record> = db::find::<Record>("expense_line").filter("employee", person_id.as_str()).limit(2000).all()?;
        let mut kept = Vec::new();
        for line in mine {
            let their_report = text(&line, "report").unwrap_or_default();
            if their_report == input.id {
                continue;
            }
            if matches!(text(&require("expense_report", their_report, "report")?, "state"), Some("submitted" | "approved" | "closed")) {
                kept.push(line);
            }
        }
        kept
    };

    // Two lines of this report that are the same expense.
    let mut seen_keys: HashMap<String, u32> = HashMap::new();
    for line in &lines {
        *seen_keys.entry(text(line, "dup_key").unwrap_or_default().to_string()).or_insert(0) += 1;
    }
    let mut problems: Vec<String> = Vec::new();
    let mut running: HashMap<(String, String), Decimal> = HashMap::new();
    for line in lines.iter() {
        let line_id = id_of(line)?.to_string();
        let claimed = decimal_of(line, "claimed")?;
        let day = parse_date(text(line, "date").unwrap_or_default())?;
        let category = require("expense_category", text(line, "category").unwrap_or_default(), "category")?;
        let category_id = id_of(&category)?.to_string();
        let name = text(&category, "name").unwrap_or("expense").to_string();
        let mut flags: Vec<String> = Vec::new();
        let mut cap: Option<Decimal> = None;
        let note_given = text(line, "note").is_some_and(|n| !n.trim().is_empty());

        // Receipts and notes.
        let needs_receipt = optional_decimal(&category, "receipt_required_over")?.is_some_and(|over| claimed > over);
        if needs_receipt && text(line, "receipts").is_none() {
            if note_given {
                flags.push("no_receipt".into());
            } else {
                problems.push(format!("{name} on {day}: a receipt is needed (or a note saying why there is none)"));
            }
        }
        if category.get("requires_note") == Some(&json!(true)) && !note_given {
            problems.push(format!("{name} on {day}: say what it was for"));
        }

        // Duplicates: the same expense or the same receipt already claimed.
        let key = text(line, "dup_key").unwrap_or_default();
        let hashes: Vec<&str> = text(line, "receipt_hashes").map(|h| h.split(',').collect()).unwrap_or_default();
        let duplicate = sent.iter().any(|other| text(other, "dup_key") == Some(key) || hashes.iter().any(|h| text(other, "receipt_hashes").is_some_and(|o| o.split(',').any(|x| x == *h))))
            || seen_keys.get(key).is_some_and(|count| *count > 1);
        if duplicate {
            flags.push("duplicate".into());
        }

        // Policy for the expense date.
        let job = job_on(&person_id, day)?;
        for policy in policies_for(&category_id, &person_id, job.as_deref(), day)? {
            let period = Period::parse(text(&policy, "period").unwrap_or("per_item")).unwrap_or(Period::PerItem);
            let enforcement = Enforcement::parse(text(&policy, "enforcement").unwrap_or("warn")).unwrap_or(Enforcement::Warn);
            let policy_currency = text(&policy, "currency").unwrap_or("").to_string();
            let limit = convert(decimal_of(&policy, "limit_amount")?, rate_between(&policy_currency, &settlement, day)?, places)?;
            let before = already(period, &person_id, &category_id, day, &input.id, &running, places)?;
            if let Outcome::Over { excess, allowed, .. } = check_limit(limit, enforcement, before, claimed) {
                match enforcement {
                    Enforcement::Block => problems.push(format!("{name} on {day}: {excess} over the {limit} limit")),
                    Enforcement::Justify if !note_given => problems.push(format!("{name} on {day}: {excess} over the {limit} limit: say why")),
                    Enforcement::Cap => {
                        cap = Some(cap.map_or(allowed, |c| if allowed < c { allowed } else { c }));
                        flags.push("over_limit".into());
                    }
                    _ => flags.push("over_limit".into()),
                }
            }
        }
        for period in [Period::PerDay, Period::PerReport, Period::PerMonth] {
            let entry = running.entry((bucket(period, day), category_id.clone())).or_insert_with(|| Decimal::zero(places));
            *entry = *entry + claimed;
        }
        flags.sort();
        flags.dedup();
        let mut changes = json!({ "flags": join(&flags) });
        changes["cap"] = cap.map_or(Value::Null, |c| json!(c));
        db::update::<Record>("expense_line", &line_id, &changes)?;
    }
    if !problems.is_empty() {
        return Err(Error::msg(problems.join("; ")));
    }

    let approver = approver_of(&person_id)?;
    let updated = refresh_total(&report)?;
    let mut changes = json!({ "state": "submitted", "submitted_on": format_date(today_date()?), "acknowledged_flags": false });
    changes["approver"] = approver.map_or(Value::Null, |a| json!(a));
    let sent = db::update::<Record>("expense_report", &input.id, &changes)?.ok_or_else(|| Error::msg("the report is gone"))?;
    let _ = updated;
    Ok(sent)
}

/// Take a sent report back to a draft, before anyone has decided.
fn withdraw(input: Id) -> Result<Record> {
    let report = require("expense_report", &input.id, "report")?;
    caller_may_act_for(text(&report, "employee").unwrap_or_default())?;
    if text(&report, "state") != Some("submitted") {
        return Err(Error::msg("only a report waiting for a decision can be withdrawn"));
    }
    db::update::<Record>("expense_report", &input.id, &json!({ "state": "draft", "approver": null }))?.ok_or_else(|| Error::msg("the report is gone"))
}

/// A rejected report goes back to draft to be fixed and sent again.
fn reopen_rejected(input: Id) -> Result<Record> {
    let report = require("expense_report", &input.id, "report")?;
    caller_may_act_for(text(&report, "employee").unwrap_or_default())?;
    if text(&report, "state") != Some("rejected") {
        return Err(Error::msg("only a rejected report can be taken back to a draft"));
    }
    db::update::<Record>("expense_report", &input.id, &json!({ "state": "draft", "decision_note": null }))?.ok_or_else(|| Error::msg("the report is gone"))
}

fn cancel(input: Reason) -> Result<Record> {
    let report = require("expense_report", &input.id, "report")?;
    caller_may_act_for(text(&report, "employee").unwrap_or_default())?;
    if !matches!(text(&report, "state"), Some("draft" | "submitted")) {
        return Err(Error::msg("only a draft or waiting report can be cancelled"));
    }
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why"));
    }
    db::update::<Record>("expense_report", &input.id, &json!({ "state": "cancelled", "decision_note": input.reason }))?.ok_or_else(|| Error::msg("the report is gone"))
}

/// A report with its lines.
fn full(input: Id) -> Result<Value> {
    let report = require("expense_report", &input.id, "report")?;
    let lines: Vec<Record> = db::find::<Record>("expense_line").filter("report", input.id.as_str()).order_by("date").limit(500).all()?;
    Ok(json!({ "report": report, "lines": lines, "flagged": lines.iter().any(|l| !flags_of(l).is_empty()) }))
}

handler! {
    fn create_report(input: NewReport) -> Record {
        create(input)
    }

    fn add_expense_line(input: AddLine) -> Record {
        add_line(input)
    }

    fn update_expense_line(input: ChangeLine) -> Record {
        change_line(input)
    }

    fn remove_expense_line(input: Id) -> Record {
        remove_line(input)
    }

    /// Keep a receipt file with a line (base64 content).
    fn attach_receipt(input: Receipt) -> Record {
        attach(input)
    }

    /// Send the report for approval: receipts, policy and duplicates are checked, and the approver is fixed.
    fn submit_report(input: Id) -> Record {
        submit(input)
    }

    fn withdraw_report(input: Id) -> Record {
        withdraw(input)
    }

    fn reopen_rejected_report(input: Id) -> Record {
        reopen_rejected(input)
    }

    fn cancel_report(input: Reason) -> Record {
        cancel(input)
    }

    fn get_report(input: Id) -> Value {
        full(input)
    }

    fn my_reports(_: Empty) -> Vec<Record> {
        let Some(me) = my_employee()? else { return Ok(Vec::new()) };
        db::find("expense_report").filter("employee", id_of(&me)?).order_by("-number").limit(200).all()
    }

    fn list_reports(input: Option<Record>) -> Vec<Record> {
        let state = input.as_ref().and_then(|i| text(i, "state")).map(str::to_string);
        let mut find = db::find::<Record>("expense_report").order_by("-number").limit(500);
        if let Some(state) = state {
            find = find.filter("state", state.as_str());
        }
        find.all()
    }
}
