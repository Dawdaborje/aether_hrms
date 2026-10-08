//! Products, requests, approval, disbursement, repayment, payroll deductions, foreclosure and reversal.
//!
//! * A request is checked against the product (amount, term, tenure, one open loan unless the product allows more)
//!   and gets its whole schedule at once, from the product's terms as they are then.
//! * The manager (or a loan administrator) approves, never the borrower. Disbursement is by a loan administrator
//!   who is neither the borrower nor the approver.
//! * Money moves only as entries: disbursement, repayment, reversal. Each carries a reference that is applied once.
//!   A repayment is spread over the oldest unpaid instalments and records exactly what it did, so a reversal can
//!   undo exactly that.
//! * Payroll asks for a plan (what is due, capped by the pay available) and later reports what it took, once per
//!   run and loan; what was not taken stays due.

use aether_sdk::dates::{add_months, format_date, months_between, parse_date, Datelike, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{
    approver_of, currency_places, decimal_of, id_of, is_admin, my_employee, next_number, require, require_admin, text, today_date,
    Record,
};
use crate::rules::{allocate, deduction, foreclosure, schedule, Instalment, Method};

#[derive(Deserialize)]
struct ProductInput {
    code: String,
    name: String,
    currency: String,
    max_amount: Decimal,
    max_months: i64,
    #[serde(default)]
    annual_bps: Option<i64>,
    method: String,
    #[serde(default)]
    max_deduction_percent: Option<i64>,
    #[serde(default)]
    min_tenure_months: Option<i64>,
    #[serde(default)]
    allow_multiple: bool,
}

#[derive(Deserialize)]
struct Ask {
    product: String,
    amount: Decimal,
    months: i64,
    #[serde(default)]
    purpose: Option<String>,
    #[serde(default)]
    first_due: Option<String>,
}

#[derive(Deserialize)]
struct Decide {
    id: String,
    approve: bool,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct Disburse {
    loan: String,
    reference: String,
}

#[derive(Deserialize)]
struct Repay {
    loan: String,
    amount: Decimal,
    reference: String,
}

#[derive(Deserialize)]
struct Foreclose {
    loan: String,
    reference: String,
    /// Must equal the quote of the day: a stale figure is refused.
    amount: Decimal,
}

#[derive(Deserialize)]
struct Reverse {
    entry: String,
    reason: String,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Item {
    employee: String,
    available: Decimal,
}

#[derive(Deserialize)]
struct Plan {
    items: Vec<Item>,
    through: String,
}

#[derive(Deserialize)]
struct Taken {
    loan: String,
    amount: Decimal,
}

#[derive(Deserialize)]
struct Report {
    run: String,
    items: Vec<Taken>,
}

#[derive(Deserialize)]
struct Who {
    employee: String,
}

fn me_id() -> Result<Option<String>> {
    Ok(my_employee()?.and_then(|m| text(&m, "id").map(str::to_string)))
}

fn places_of(loan: &Record) -> Result<u32> {
    currency_places(text(loan, "currency").unwrap_or_default())
}

fn money(record: &Record, field: &str, places: u32) -> Result<Decimal> {
    Ok(decimal_of(record, field)?.unwrap_or(Decimal::zero(places)).with_scale(places)?)
}

fn date_of(record: &Record, field: &str) -> Result<NaiveDate> {
    parse_date(text(record, field).unwrap_or_default())
}

fn percent_of(value: Option<i64>, default: i64) -> Result<i64> {
    let v = value.unwrap_or(default);
    if (0..=100).contains(&v) { Ok(v) } else { Err(Error::msg("a share is a percentage between 0 and 100")) }
}

fn product(input: ProductInput) -> Result<Record> {
    require_admin()?;
    let currency = input.currency.to_uppercase();
    let places = currency_places(&currency)?;
    Method::parse(&input.method).ok_or_else(|| Error::msg("the method is reducing or flat"))?;
    if input.max_months < 1 || input.max_months > 120 {
        return Err(Error::msg("the longest term is between 1 and 120 months"));
    }
    let bps = input.annual_bps.unwrap_or(0);
    if !(0..=10_000_00).contains(&bps) {
        return Err(Error::msg("the yearly rate is between 0 and 100 percent (in hundredths of a percent)"));
    }
    if input.max_amount.is_negative() || input.max_amount.is_zero() {
        return Err(Error::msg("the largest loan is more than zero"));
    }
    db::create(
        "loan_product",
        &json!({
            "code": input.code, "name": input.name, "currency": currency, "max_amount": input.max_amount.with_scale(places)?, "max_months": input.max_months,
            "annual_bps": bps, "method": input.method, "max_deduction_percent": percent_of(input.max_deduction_percent, 30)?,
            "min_tenure_months": input.min_tenure_months.unwrap_or(0), "allow_multiple": input.allow_multiple, "is_active": true,
        }),
    )
    .map_err(|e| e.or("could not create the product (the code may be taken)"))
}

fn instalments_of(loan: &str) -> Result<Vec<Record>> {
    let mut rows: Vec<Record> = db::find("loan_instalment").filter("loan", loan).limit(500).all()?;
    rows.sort_by_key(|r| r.get("n").and_then(Value::as_i64).unwrap_or(0));
    Ok(rows)
}

fn owed(inst: &Record, places: u32) -> Result<Decimal> {
    Ok(money(inst, "principal", places)? + money(inst, "interest", places)? - money(inst, "paid", places)?)
}

fn ask(input: Ask) -> Result<Record> {
    let me = my_employee()?.ok_or_else(|| Error::msg("you are not an employee"))?;
    let me_id = id_of(&me)?.to_string();
    if !matches!(text(&me, "status"), Some("active" | "probation") | None) {
        return Err(Error::msg("only someone who is working can ask for a loan"));
    }
    let prod = require("loan_product", &input.product, "product")?;
    if prod.get("is_active") == Some(&json!(false)) {
        return Err(Error::msg("this loan product is no longer offered"));
    }
    let currency = text(&prod, "currency").unwrap_or_default().to_string();
    let places = currency_places(&currency)?;
    let amount = input.amount.with_scale(places).map_err(|_| Error::msg(format!("{currency} has {places} digits after the point")))?;
    if amount > money(&prod, "max_amount", places)? {
        return Err(Error::msg(format!("the most this product lends is {}", money(&prod, "max_amount", places)?)));
    }
    if input.months < 1 || input.months > prod.get("max_months").and_then(Value::as_i64).unwrap_or(0) {
        return Err(Error::msg(format!("the term is between 1 and {} months", prod.get("max_months").and_then(Value::as_i64).unwrap_or(0))));
    }
    let today = today_date()?;
    let tenure_needed = prod.get("min_tenure_months").and_then(Value::as_i64).unwrap_or(0);
    if let Some(hired) = text(&me, "first_hire_date").or_else(|| text(&me, "hire_date")).map(parse_date).transpose()? {
        if (months_between(hired, today) as i64) < tenure_needed {
            return Err(Error::msg(format!("this loan needs {tenure_needed} months of service")));
        }
    }
    if !prod.get("allow_multiple").and_then(Value::as_bool).unwrap_or(false) {
        let mine: Vec<Record> = db::find("loan").filter("employee", me_id.as_str()).filter("product", input.product.as_str()).limit(200).all()?;
        if mine.iter().any(|l| matches!(text(l, "state"), Some("requested" | "approved" | "disbursed"))) {
            return Err(Error::msg("you already have an open loan of this kind"));
        }
    }
    let first_due = match input.first_due {
        Some(d) => parse_date(&d)?,
        None => {
            let first = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today);
            add_months(first, 1)
        }
    };
    if first_due <= today {
        return Err(Error::msg("the first instalment falls after today"));
    }
    let bps = prod.get("annual_bps").and_then(Value::as_i64).unwrap_or(0);
    let method = Method::parse(text(&prod, "method").unwrap_or_default()).ok_or_else(|| Error::msg("the product has an unknown method"))?;
    let plan = schedule(amount, input.months, bps, method, first_due)?;
    let approver = approver_of(&me_id)?;
    let loan: Record = db::create(
        "loan",
        &json!({
            "reference": next_number("loan", "LN-", 5)?, "employee": me_id, "product": input.product, "purpose": input.purpose, "principal": amount,
            "months": input.months, "annual_bps": bps, "method": text(&prod, "method"), "currency": currency,
            "max_deduction_percent": prod.get("max_deduction_percent"), "first_due": format_date(first_due), "state": "requested", "approver": approver,
            "paid_total": Decimal::zero(places), "interest_waived": Decimal::zero(places), "entry_seq": 0,
        }),
    )?;
    for row in &plan {
        db::create::<Record>(
            "loan_instalment",
            &json!({
                "loan": loan["id"], "employee": me_id, "n": row.n, "due_date": format_date(row.due), "principal": row.principal, "interest": row.interest,
                "paid": Decimal::zero(places), "state": "due", "overdue_notified": false,
            }),
        )?;
    }
    events::emit("loan_requested", &json!({ "loan": loan["id"], "employee": me_id, "amount": amount, "approver": approver }))?;
    Ok(loan)
}

fn decide(input: Decide) -> Result<Record> {
    let loan = require("loan", &input.id, "loan")?;
    if text(&loan, "state") != Some("requested") {
        return Err(Error::msg("this loan is not waiting for a decision"));
    }
    let me = me_id()?;
    if me.is_some() && me.as_deref() == text(&loan, "employee") {
        return Err(Error::msg("nobody decides their own loan"));
    }
    if !(is_admin()? || (me.is_some() && me.as_deref() == text(&loan, "approver"))) {
        return Err(Error::msg("only the borrower's approver (or a loan administrator) can decide"));
    }
    if !input.approve && input.note.as_deref().is_none_or(|n| n.trim().is_empty()) {
        return Err(Error::msg("say why the loan is refused"));
    }
    db::update("loan", &input.id, &json!({ "state": if input.approve { "approved" } else { "rejected" }, "decided_by": me, "note": input.note }))?
        .ok_or_else(|| Error::msg("the loan is gone"))
}

fn cancel(input: Id) -> Result<Record> {
    let loan = require("loan", &input.id, "loan")?;
    if me_id()?.as_deref() != text(&loan, "employee") && !is_admin()? {
        return Err(Error::msg("only the borrower (or a loan administrator) can cancel a request"));
    }
    if !matches!(text(&loan, "state"), Some("requested" | "approved")) {
        return Err(Error::msg("only a loan not yet paid out can be cancelled"));
    }
    db::update("loan", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the loan is gone"))
}

/// Write one entry and update the loan in one step; the entry's number is kept on the loan.
#[allow(clippy::too_many_arguments)]
fn entry(loan_id: &str, kind: &str, amount: Decimal, reference: &str, allocations: Option<Value>, reverses: Option<&str>, reason: Option<&str>, mut changes: Value) -> Result<Record> {
    let fresh = require("loan", loan_id, "loan")?;
    let seq = fresh.get("entry_seq").and_then(Value::as_i64).unwrap_or(0) + 1;
    let made: Record = db::create(
        "loan_entry",
        &json!({
            "loan": loan_id, "employee": fresh["employee"], "seq": seq, "kind": kind, "amount": amount, "on_date": format_date(today_date()?),
            "reference": reference, "allocations": allocations.map(|a| a.to_string()), "reverses": reverses, "reason": reason,
            "actor": context::current()?.actor.id.unwrap_or_default(),
        }),
    )?;
    changes["entry_seq"] = json!(seq);
    db::update::<Record>("loan", loan_id, &changes)?;
    Ok(made)
}

fn existing(reference: &str) -> Result<Option<Record>> {
    db::find("loan_entry").filter("reference", reference).first()
}

fn disburse(input: Disburse) -> Result<Value> {
    require_admin()?;
    if let Some(done) = existing(&input.reference)? {
        return Ok(json!({ "entry": done, "repeated": true }));
    }
    let loan = require("loan", &input.loan, "loan")?;
    if text(&loan, "state") != Some("approved") {
        return Err(Error::msg("only an approved loan is paid out"));
    }
    let me = me_id()?;
    if me.is_some() && (me.as_deref() == text(&loan, "employee") || me.as_deref() == text(&loan, "decided_by")) {
        return Err(Error::msg("the person who pays out is neither the borrower nor the approver"));
    }
    let places = places_of(&loan)?;
    let amount = money(&loan, "principal", places)?;
    let made = entry(
        &input.loan, "disbursement", amount, &input.reference, None, None, None,
        json!({ "state": "disbursed", "disbursed_on": format_date(today_date()?) }),
    )?;
    events::emit("loan_disbursed", &json!({ "loan": input.loan, "employee": loan["employee"], "amount": amount, "currency": loan["currency"] }))?;
    Ok(json!({ "entry": made, "repeated": false }))
}

/// Apply money to a loan's instalments (or, for a foreclosure, close it). Each reference is applied once.
fn apply(loan_id: &str, amount: Decimal, reference: &str, close_out: bool) -> Result<Value> {
    if let Some(done) = existing(reference)? {
        return Ok(json!({ "entry": done, "repeated": true }));
    }
    let loan = require("loan", loan_id, "loan")?;
    if text(&loan, "state") != Some("disbursed") {
        return Err(Error::msg("only a loan that has been paid out takes repayments"));
    }
    let places = places_of(&loan)?;
    let amount = amount.with_scale(places)?;
    if amount.is_negative() || amount.is_zero() {
        return Err(Error::msg("a repayment is more than zero"));
    }
    let rows = instalments_of(loan_id)?;
    let today = today_date()?;
    let mut allocations: Vec<Value> = Vec::new();
    let mut waived_total = Decimal::zero(places);
    if close_out {
        let mut open = Vec::new();
        for r in &rows {
            let inst = Instalment { n: r["n"].as_i64().unwrap_or(0), due: date_of(r, "due_date")?, principal: money(r, "principal", places)?, interest: money(r, "interest", places)? };
            open.push((inst, money(r, "paid", places)?));
        }
        let quote = foreclosure(&open, today)?;
        if amount != quote {
            return Err(Error::msg(format!("the figure to close this loan today is {quote}, not {amount}")));
        }
        for (inst, paid) in &open {
            let owes = inst.total() - *paid;
            if owes.is_zero() || owes.is_negative() {
                continue;
            }
            if inst.due <= today {
                allocations.push(json!({ "n": inst.n, "amount": owes, "waived": Decimal::zero(places) }));
            } else {
                let interest_paid = if *paid < inst.interest { *paid } else { inst.interest };
                let waived = inst.interest - interest_paid;
                let principal_owed = inst.principal - (if *paid > inst.interest { *paid - inst.interest } else { Decimal::zero(places) });
                waived_total = waived_total + waived;
                allocations.push(json!({ "n": inst.n, "amount": principal_owed, "waived": waived }));
            }
        }
    } else {
        let mut owing = Vec::new();
        for r in &rows {
            owing.push((r["n"].as_i64().unwrap_or(0), owed(r, places)?));
        }
        let (taken, left) = allocate(&owing, amount)?;
        if !left.is_zero() {
            return Err(Error::msg(format!("that is {left} more than the loan still owes")));
        }
        for (n, take) in taken {
            allocations.push(json!({ "n": n, "amount": take, "waived": Decimal::zero(places) }));
        }
    }
    let mut closed = true;
    for r in &rows {
        let n = r["n"].as_i64().unwrap_or(0);
        let alloc = allocations.iter().find(|a| a["n"].as_i64() == Some(n));
        let (take, waived) = match alloc {
            Some(a) => (decimal_of(a, "amount")?.unwrap_or(Decimal::zero(places)), decimal_of(a, "waived")?.unwrap_or(Decimal::zero(places))),
            None => (Decimal::zero(places), Decimal::zero(places)),
        };
        let interest = money(r, "interest", places)? - waived;
        let paid = money(r, "paid", places)? + take;
        let settled = paid >= money(r, "principal", places)? + interest;
        if !settled {
            closed = false;
        }
        if alloc.is_some() {
            db::update::<Record>("loan_instalment", id_of(r)?, &json!({ "paid": paid, "interest": interest, "state": if settled { "paid" } else { "due" } }))?;
        }
    }
    let paid_total = money(&loan, "paid_total", places)? + amount;
    let mut changes = json!({ "paid_total": paid_total, "interest_waived": money(&loan, "interest_waived", places)? + waived_total });
    if closed {
        changes["state"] = json!("closed");
        changes["closed_on"] = json!(format_date(today));
    }
    let made = entry(loan_id, "repayment", amount, reference, Some(Value::Array(allocations)), None, None, changes)?;
    if closed {
        events::emit("loan_closed", &json!({ "loan": loan_id, "employee": loan["employee"] }))?;
    }
    Ok(json!({ "entry": made, "repeated": false, "closed": closed }))
}

fn reverse(input: Reverse) -> Result<Value> {
    require_admin()?;
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why the entry is reversed"));
    }
    let target = require("loan_entry", &input.entry, "entry")?;
    let reference = format!("reversal:{}", input.entry);
    if let Some(done) = existing(&reference)? {
        return Ok(json!({ "entry": done, "repeated": true }));
    }
    let loan_id = text(&target, "loan").unwrap_or_default().to_string();
    let loan = require("loan", &loan_id, "loan")?;
    let places = places_of(&loan)?;
    let amount = money(&target, "amount", places)?;
    let mut changes = json!({});
    match text(&target, "kind") {
        Some("repayment") => {
            let allocations: Vec<Value> = serde_json::from_str(text(&target, "allocations").unwrap_or("[]")).map_err(|e| Error::msg(format!("allocations: {e}")))?;
            let rows = instalments_of(&loan_id)?;
            let mut waived_total = Decimal::zero(places);
            for a in &allocations {
                let n = a["n"].as_i64().unwrap_or(0);
                let Some(row) = rows.iter().find(|r| r["n"].as_i64() == Some(n)) else { continue };
                let waived = decimal_of(a, "waived")?.unwrap_or(Decimal::zero(places));
                waived_total = waived_total + waived;
                let paid = money(row, "paid", places)? - decimal_of(a, "amount")?.unwrap_or(Decimal::zero(places));
                let interest = money(row, "interest", places)? + waived;
                db::update::<Record>("loan_instalment", id_of(row)?, &json!({ "paid": paid, "interest": interest, "state": "due" }))?;
            }
            changes = json!({ "paid_total": money(&loan, "paid_total", places)? - amount, "interest_waived": money(&loan, "interest_waived", places)? - waived_total });
            if text(&loan, "state") == Some("closed") {
                changes["state"] = json!("disbursed");
                changes["closed_on"] = Value::Null;
            }
        }
        Some("disbursement") => {
            if !money(&loan, "paid_total", places)?.is_zero() {
                return Err(Error::msg("reverse the repayments first"));
            }
            changes = json!({ "state": "approved", "disbursed_on": null });
        }
        _ => return Err(Error::msg("a reversal cannot be reversed")),
    }
    let made = entry(&loan_id, "reversal", amount, &reference, None, text(&target, "id"), Some(&input.reason), changes)?;
    Ok(json!({ "entry": made, "repeated": false }))
}

fn quote(input: Id) -> Result<Value> {
    let loan = require("loan", &input.id, "loan")?;
    let places = places_of(&loan)?;
    let today = today_date()?;
    let mut open = Vec::new();
    let mut remaining = Decimal::zero(places);
    for r in instalments_of(&input.id)? {
        let inst = Instalment { n: r["n"].as_i64().unwrap_or(0), due: date_of(&r, "due_date")?, principal: money(&r, "principal", places)?, interest: money(&r, "interest", places)? };
        remaining = remaining + owed(&r, places)?;
        open.push((inst, money(&r, "paid", places)?));
    }
    let to_close = foreclosure(&open, today)?;
    Ok(json!({ "loan": input.id, "on": format_date(today), "to_close_today": to_close, "scheduled_remaining": remaining, "interest_saved": remaining - to_close }))
}

fn plan(input: Plan) -> Result<Value> {
    let through = parse_date(&input.through)?;
    let mut out = Vec::new();
    for item in input.items {
        let loans: Vec<Record> = db::find::<Record>("loan").filter("employee", item.employee.as_str()).filter("state", "disbursed").limit(100).all()?;
        let mut per_loan = Vec::new();
        for loan in loans {
            let places = places_of(&loan)?;
            let mut due = Decimal::zero(places);
            let mut oldest: Option<NaiveDate> = None;
            for r in instalments_of(id_of(&loan)?)? {
                if date_of(&r, "due_date")? <= through {
                    let o = owed(&r, places)?;
                    if o.is_zero() || o.is_negative() {
                        continue;
                    }
                    due = due + o;
                    let d = date_of(&r, "due_date")?;
                    oldest = Some(oldest.map_or(d, |x| x.min(d)));
                }
            }
            if !due.is_zero() {
                per_loan.push((oldest.unwrap_or(through), loan, due));
            }
        }
        per_loan.sort_by_key(|(d, _, _)| *d);
        let mut lines = Vec::new();
        // Everything is in the loans' currency (one currency per person is assumed; a mix is refused).
        let scale = match per_loan.first() {
            Some((_, loan, _)) => places_of(loan)?,
            None => item.available.scale(),
        };
        for (_, loan, _) in &per_loan {
            if places_of(loan)? != scale || text(loan, "currency") != text(&per_loan[0].1, "currency") {
                return Err(Error::msg("this person has loans in different currencies: plan them separately"));
            }
        }
        let (mut total_due, mut total_deduct) = (Decimal::zero(scale), Decimal::zero(scale));
        let mut left_available = item.available.with_scale(scale)?;
        for (_, loan, due) in per_loan {
            let percent = loan.get("max_deduction_percent").and_then(Value::as_i64).unwrap_or(30);
            // The cap is on what is available for the whole person, shared by their loans in the order of age.
            let cap_left = deduction(due, left_available, percent)?;
            left_available = left_available - cap_left;
            total_due = total_due + due;
            total_deduct = total_deduct + cap_left;
            lines.push(json!({ "loan": loan["id"], "reference": loan["reference"], "due": due, "deduct": cap_left }));
        }
        out.push(json!({ "employee": item.employee, "due": total_due, "deduct": total_deduct, "deferred": total_due - total_deduct, "loans": lines }));
    }
    Ok(json!({ "through": input.through, "employees": out }))
}

fn report(input: Report) -> Result<Value> {
    require_admin()?;
    let mut applied = Vec::new();
    for taken in input.items {
        if taken.amount.is_zero() {
            continue;
        }
        applied.push(apply(&taken.loan, taken.amount, &format!("payroll:{}:{}", input.run, taken.loan), false)?);
    }
    Ok(json!({ "run": input.run, "applied": applied }))
}

fn leaver(input: Who) -> Result<Value> {
    if me_id()?.as_deref() != Some(input.employee.as_str()) && !is_admin()? {
        return Err(Error::msg("you can see your own loans, or anyone's if you run loans"));
    }
    let loans: Vec<Record> = db::find::<Record>("loan").filter("employee", input.employee.as_str()).filter("state", "disbursed").limit(100).all()?;
    let mut lines = Vec::new();
    for loan in loans {
        lines.push(quote(Id { id: id_of(&loan)?.to_string() })?);
    }
    Ok(json!({ "employee": input.employee, "loans": lines }))
}

fn statement(input: Id) -> Result<Value> {
    let loan = require("loan", &input.id, "loan")?;
    let mut entries: Vec<Record> = db::find("loan_entry").filter("loan", input.id.as_str()).limit(2000).all()?;
    entries.sort_by_key(|r| r.get("seq").and_then(Value::as_i64).unwrap_or(0));
    Ok(json!({ "loan": loan, "instalments": instalments_of(&input.id)?, "entries": entries }))
}

fn mine() -> Result<Vec<Record>> {
    let Some(me) = me_id()? else { return Ok(Vec::new()) };
    db::find("loan").filter("employee", me.as_str()).limit(500).all()
}

fn waiting() -> Result<Vec<Record>> {
    let me = me_id()?;
    let admin = is_admin()?;
    let mut rows: Vec<Record> = db::find::<Record>("loan").filter("state", "requested").limit(2000).all()?;
    rows.retain(|r| text(r, "employee") != me.as_deref() && (admin || (me.is_some() && me.as_deref() == text(r, "approver"))));
    Ok(rows)
}

/// Report an instalment that is overdue, once.
fn nightly() -> Result<Value> {
    let today = today_date()?;
    let rows: Vec<Record> = db::find::<Record>("loan_instalment").filter("state", "due").limit(20_000).all()?;
    let mut flagged = 0;
    for r in rows {
        if r.get("overdue_notified") == Some(&json!(true)) || date_of(&r, "due_date")? >= today {
            continue;
        }
        db::update::<Record>("loan_instalment", id_of(&r)?, &json!({ "overdue_notified": true }))?;
        events::emit("loan_instalment_overdue", &json!({ "loan": r["loan"], "employee": r["employee"], "n": r["n"], "due": r["due_date"] }))?;
        flagged += 1;
    }
    Ok(json!({ "flagged": flagged }))
}

handler! {
    fn create_loan_product(input: ProductInput) -> Record {
        product(input)
    }

    fn list_loan_products(_: Empty) -> Vec<Record> {
        db::find("loan_product").limit(500).all()
    }

    /// Ask for a loan; the schedule is made at once from the product's terms.
    fn request_loan(input: Ask) -> Record {
        ask(input)
    }

    fn decide_loan(input: Decide) -> Record {
        decide(input)
    }

    fn cancel_loan(input: Id) -> Record {
        cancel(input)
    }

    /// Pay out an approved loan (a loan administrator who is neither borrower nor approver); once per reference.
    fn disburse_loan(input: Disburse) -> Value {
        disburse(input)
    }

    /// Record a repayment over the oldest unpaid instalments (loan administrators); once per reference.
    fn record_loan_repayment(input: Repay) -> Value {
        require_admin()?;
        apply(&input.loan, input.amount, &input.reference, false)
    }

    /// What it costs to close the loan today: unpaid principal plus interest only on instalments already due.
    fn loan_foreclosure_quote(input: Id) -> Value {
        quote(input)
    }

    /// Close the loan at today's quote (the amount must equal it).
    fn foreclose_loan(input: Foreclose) -> Value {
        require_admin()?;
        apply(&input.loan, input.amount, &input.reference, true)
    }

    /// Undo an entry by appending a reversal.
    fn reverse_loan_entry(input: Reverse) -> Value {
        reverse(input)
    }

    /// For payroll: what to deduct for each person, capped by the pay available. Nothing is changed.
    fn payroll_loan_plan(input: Plan) -> Value {
        plan(input)
    }

    /// For payroll: what it actually deducted, once per run and loan.
    fn payroll_loan_report(input: Report) -> Value {
        report(input)
    }

    /// For the final settlement: what each open loan costs to close today.
    fn leaver_loan_balance(input: Who) -> Value {
        leaver(input)
    }

    fn loan_statement(input: Id) -> Value {
        statement(input)
    }

    fn my_loans(_: Empty) -> Vec<Record> {
        mine()
    }

    fn loans_waiting_for_me(_: Empty) -> Vec<Record> {
        waiting()
    }

    fn loan_nightly(_: Empty) -> Value {
        nightly()
    }
}
