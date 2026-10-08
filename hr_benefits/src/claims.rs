//! Plans, enrolment, claims, the ledger, payroll payouts and nightly accrual.
//!
//! * A plan's figures are copied onto nothing: remaining is always the sum of the ledger against the current
//!   ceiling. Changing a ceiling mid-year changes what is left, which is why a benefits administrator does it
//!   once, at the start of a year, not as a correction of a past claim.
//! * Accrual is one entry per person, plan and month (`accrual:<plan>:<employee>:<year>-<month>`), so running the
//!   nightly job twice in a month is a no-op.
//! * A claim that would go over the remaining balance is refused. Approval is by the manager or a benefits
//!   administrator, never the claimant; payout is an hr_compensation adjustment keyed by the claim.

use aether_sdk::dates::{format_date, parse_date, Datelike, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{
    approver_of, currency_places, decimal_of, employee, id_of, is_admin, my_employee, next_number, require, require_admin, text, today_date,
    Record,
};
use crate::rules::{accrued_by, claim_fits, dependents_fit, remaining, windows_overlap, year_of, Kind, Relation};

#[derive(Deserialize)]
struct PlanInput {
    code: String,
    name: String,
    kind: String,
    currency: String,
    yearly_ceiling: Decimal,
    #[serde(default)]
    pay_code: Option<String>,
    #[serde(default)]
    max_dependents: Option<i64>,
    #[serde(default)]
    allow_spouse: bool,
    #[serde(default)]
    allow_children: bool,
}

#[derive(Deserialize)]
struct Enrol {
    plan: String,
    #[serde(default)]
    employee: Option<String>,
    start_date: String,
    #[serde(default)]
    end_date: Option<String>,
    #[serde(default)]
    policy_number: Option<String>,
    #[serde(default)]
    carrier: Option<String>,
    #[serde(default)]
    premium: Option<Decimal>,
    #[serde(default)]
    dependents: Vec<DependentIn>,
}

#[derive(Deserialize)]
struct DependentIn {
    name: String,
    relation: String,
    #[serde(default)]
    born: Option<String>,
}

#[derive(Deserialize)]
struct ClaimIn {
    plan: String,
    amount: Decimal,
    incurred_on: String,
    description: String,
}

#[derive(Deserialize)]
struct Decide {
    id: String,
    approve: bool,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct BalanceIn {
    plan: String,
    #[serde(default)]
    employee: Option<String>,
    #[serde(default)]
    year: Option<i32>,
}

fn me_id() -> Result<Option<String>> {
    Ok(my_employee()?.and_then(|m| text(&m, "id").map(str::to_string)))
}

fn money(record: &Record, field: &str, places: u32) -> Result<Decimal> {
    Ok(decimal_of(record, field)?.unwrap_or(Decimal::zero(places)).with_scale(places)?)
}

fn plan_row(id: &str) -> Result<Record> {
    require("bnf_plan", id, "plan")
}

fn entries(plan: &str, employee: &str, year: i32) -> Result<Vec<Record>> {
    let mut rows: Vec<Record> = db::find::<Record>("bnf_entry").filter("plan", plan).filter("employee", employee).filter("year", year).limit(5000).all()?;
    rows.sort_by_key(|r| r.get("seq").and_then(Value::as_i64).unwrap_or(0));
    Ok(rows)
}

fn sums(plan: &str, employee: &str, year: i32, places: u32) -> Result<(Decimal, Decimal, Decimal, i64)> {
    let rows = entries(plan, employee, year)?;
    let (mut accrued, mut claimed, mut reversed) = (Decimal::zero(places), Decimal::zero(places), Decimal::zero(places));
    let seq = rows.last().map(|r| r.get("seq").and_then(Value::as_i64).unwrap_or(0)).unwrap_or(0);
    for r in rows {
        let amount = money(&r, "amount", places)?;
        match text(&r, "kind") {
            Some("accrual") => accrued = accrued + amount,
            Some("claim") => claimed = claimed + amount,
            Some("reversal") => reversed = reversed + amount,
            _ => {}
        }
    }
    Ok((accrued, claimed, reversed, seq))
}

fn leftover(plan: &Record, employee: &str, year: i32) -> Result<(Decimal, Decimal, Decimal, Decimal, i64)> {
    let places = currency_places(text(plan, "currency").unwrap_or_default())?;
    let ceiling = money(plan, "yearly_ceiling", places)?;
    let kind = Kind::parse(text(plan, "kind").unwrap_or_default()).ok_or_else(|| Error::msg("the plan has an unknown kind"))?;
    let (accrued, claimed, reversed, seq) = sums(id_of(plan)?, employee, year, places)?;
    let pool = match kind {
        Kind::ClaimAgainstCeiling => ceiling,
        Kind::AccrueThenClaim => accrued,
        Kind::Payroll => Decimal::zero(places),
    };
    let left = remaining(ceiling, pool, claimed, reversed)?;
    Ok((left, accrued, claimed, reversed, seq))
}

fn write_entry(plan: &str, employee: &str, year: i32, seq: i64, kind: &str, amount: Decimal, source: &str, claim: Option<&str>, reverses: Option<&str>) -> Result<Record> {
    if let Some(done) = db::find::<Record>("bnf_entry").filter("source", source).first()? {
        return Ok(done);
    }
    db::create(
        "bnf_entry",
        &json!({
            "plan": plan, "employee": employee, "year": year, "seq": seq + 1, "kind": kind, "amount": amount,
            "on_date": format_date(today_date()?), "source": source, "claim": claim, "reverses": reverses,
            "actor": context::current()?.actor.id.unwrap_or_default(),
        }),
    )
}

fn plan(input: PlanInput) -> Result<Record> {
    require_admin()?;
    let kind = Kind::parse(&input.kind).ok_or_else(|| Error::msg("kind is accrue_then_claim, claim_against_ceiling or payroll"))?;
    let currency = input.currency.to_uppercase();
    let places = currency_places(&currency)?;
    if input.yearly_ceiling.is_negative() || input.yearly_ceiling.is_zero() {
        return Err(Error::msg("a yearly ceiling is more than zero"));
    }
    let _ = kind;
    let pay_code = input.pay_code.unwrap_or_else(|| input.code.to_lowercase());
    if pay_code.trim().is_empty() {
        return Err(Error::msg("name the pay code payroll will use"));
    }
    let _: Record = plugins::call("hr_compensation", "ensure_pay_code", &json!({ "code": pay_code, "kind": "earning" }))?;
    db::create(
        "bnf_plan",
        &json!({
            "code": input.code, "name": input.name, "kind": input.kind, "currency": currency,
            "yearly_ceiling": input.yearly_ceiling.with_scale(places)?, "pay_code": pay_code,
            "max_dependents": input.max_dependents.unwrap_or(0), "allow_spouse": input.allow_spouse,
            "allow_children": input.allow_children, "is_active": true,
        }),
    )
    .map_err(|e| e.or("could not create the plan (the code may be taken)"))
}

fn enrol(input: Enrol) -> Result<Record> {
    require_admin()?;
    let plan = plan_row(&input.plan)?;
    if plan.get("is_active") == Some(&json!(false)) {
        return Err(Error::msg("this plan is no longer offered"));
    }
    let who = match input.employee {
        Some(id) => id,
        None => me_id()?.ok_or_else(|| Error::msg("name the employee"))?,
    };
    employee(&who)?;
    let start = parse_date(&input.start_date)?;
    let end = input.end_date.as_deref().map(parse_date).transpose()?;
    if let Some(end) = end {
        if end < start {
            return Err(Error::msg("the enrolment ends before it starts"));
        }
    }
    let year = year_of(start);
    let relations: Vec<Relation> = input
        .dependents
        .iter()
        .map(|d| Relation::parse(&d.relation).ok_or_else(|| Error::msg("a dependent is a spouse, a child or other")))
        .collect::<Result<_>>()?;
    dependents_fit(
        plan.get("max_dependents").and_then(Value::as_i64).unwrap_or(0),
        plan.get("allow_spouse") == Some(&json!(true)),
        plan.get("allow_children") == Some(&json!(true)),
        &relations,
    )?;
    let others: Vec<Record> = db::find("bnf_enrolment").filter("plan", input.plan.as_str()).filter("employee", who.as_str()).limit(50).all()?;
    for other in &others {
        if text(other, "state") != Some("active") {
            continue;
        }
        let other_end = text(other, "end_date").map(parse_date).transpose()?;
        if windows_overlap((start, end), (parse_date(text(other, "start_date").unwrap_or_default())?, other_end)) {
            return Err(Error::msg("this person is already enrolled in this plan on those days"));
        }
    }
    let made: Record = db::create(
        "bnf_enrolment",
        &json!({
            "plan": input.plan, "employee": who, "year": year, "start_date": format_date(start), "end_date": end.map(format_date),
            "state": "active", "policy_number": input.policy_number, "carrier": input.carrier, "premium": input.premium,
        }),
    )
    .map_err(|e| e.or("could not enrol (already enrolled this year on this plan)"))?;
    for dep in input.dependents {
        db::create::<Record>(
            "bnf_dependent",
            &json!({ "enrolment": made["id"], "employee": who, "name": dep.name, "relation": dep.relation, "born": dep.born }),
        )?;
    }
    Ok(made)
}

fn end_enrolment(input: Id) -> Result<Record> {
    require_admin()?;
    let row = require("bnf_enrolment", &input.id, "enrolment")?;
    if text(&row, "state") != Some("active") {
        return Err(Error::msg("this enrolment has already ended"));
    }
    db::update("bnf_enrolment", &input.id, &json!({ "state": "ended", "end_date": format_date(today_date()?) }))?.ok_or_else(|| Error::msg("the enrolment is gone"))
}

fn claim(input: ClaimIn) -> Result<Record> {
    let me = my_employee()?.ok_or_else(|| Error::msg("you are not an employee"))?;
    let me_id = id_of(&me)?.to_string();
    let plan = plan_row(&input.plan)?;
    let kind = Kind::parse(text(&plan, "kind").unwrap_or_default()).ok_or_else(|| Error::msg("the plan has an unknown kind"))?;
    if kind == Kind::Payroll {
        return Err(Error::msg("this plan is paid through payroll: there is nothing to claim"));
    }
    let incurred = parse_date(&input.incurred_on)?;
    if incurred > today_date()? {
        return Err(Error::msg("a claim is for something that has already happened"));
    }
    if input.description.trim().is_empty() {
        return Err(Error::msg("say what the claim is for"));
    }
    let year = year_of(incurred);
    let places = currency_places(text(&plan, "currency").unwrap_or_default())?;
    let amount = input.amount.with_scale(places)?;
    let (left, _, _, _, _) = leftover(&plan, &me_id, year)?;
    claim_fits(left, amount)?;
    let enrolled: Vec<Record> = db::find("bnf_enrolment").filter("plan", input.plan.as_str()).filter("employee", me_id.as_str()).filter("state", "active").limit(20).all()?;
    let covered = enrolled.iter().any(|e| {
        let start = parse_date(text(e, "start_date").unwrap_or_default()).ok();
        let end = text(e, "end_date").map(parse_date).transpose().ok().flatten();
        start.is_some_and(|s| s <= incurred && end.map_or(true, |t| incurred <= t))
    });
    if !covered {
        return Err(Error::msg("you are not enrolled in this plan on that day"));
    }
    let made: Record = db::create(
        "bnf_claim",
        &json!({
            "reference": next_number("claim", "BNF-", 5)?, "plan": input.plan, "employee": me_id, "year": year, "amount": amount,
            "incurred_on": format_date(incurred), "description": input.description, "state": "submitted", "approver": approver_of(&me_id)?,
        }),
    )?;
    events::emit("benefit_claimed", &json!({ "claim": made["id"], "employee": me_id, "amount": amount }))?;
    Ok(made)
}

fn decide(input: Decide) -> Result<Record> {
    let claim = require("bnf_claim", &input.id, "claim")?;
    if text(&claim, "state") != Some("submitted") {
        return Err(Error::msg("this claim is not waiting for a decision"));
    }
    let me = me_id()?;
    if me.is_some() && me.as_deref() == text(&claim, "employee") {
        return Err(Error::msg("nobody decides their own claim"));
    }
    if !(is_admin()? || (me.is_some() && me.as_deref() == text(&claim, "approver"))) {
        return Err(Error::msg("only the claimant's approver (or a benefits administrator) can decide"));
    }
    if !input.approve && input.note.as_deref().is_none_or(|n| n.trim().is_empty()) {
        return Err(Error::msg("say why the claim is refused"));
    }
    if !input.approve {
        return db::update("bnf_claim", &input.id, &json!({ "state": "rejected", "decided_by": me, "note": input.note }))?
            .ok_or_else(|| Error::msg("the claim is gone"));
    }
    let plan = plan_row(text(&claim, "plan").unwrap_or_default())?;
    let employee = text(&claim, "employee").unwrap_or_default();
    let year = claim.get("year").and_then(Value::as_i64).unwrap_or(0) as i32;
    let places = currency_places(text(&plan, "currency").unwrap_or_default())?;
    let amount = money(&claim, "amount", places)?;
    let (left, _, _, _, seq) = leftover(&plan, employee, year)?;
    claim_fits(left, amount)?;
    let source = format!("claim:{}", input.id);
    write_entry(id_of(&plan)?, employee, year, seq, "claim", amount, &source, Some(&input.id), None)?;
    let pay_code = text(&plan, "pay_code").unwrap_or_default();
    let adj: Record = plugins::call(
        "hr_compensation",
        "add_adjustment",
        &json!({
            "employee": employee, "code": pay_code, "amount": amount, "mode": "one_off",
            "pay_date": format_date(today_date()?), "source_plugin": "hr_benefits", "source_ref": input.id,
            "reason": format!("Benefit claim {}", text(&claim, "reference").unwrap_or("?")),
        }),
    )?;
    let out = db::update(
        "bnf_claim",
        &input.id,
        &json!({ "state": "approved", "decided_by": me, "note": input.note, "adjustment": adj.get("id") }),
    )?
    .ok_or_else(|| Error::msg("the claim is gone"))?;
    events::emit("benefit_approved", &json!({ "claim": input.id, "employee": employee, "amount": amount }))?;
    Ok(out)
}

fn cancel(input: Id) -> Result<Record> {
    let claim = require("bnf_claim", &input.id, "claim")?;
    if me_id()?.as_deref() != text(&claim, "employee") && !is_admin()? {
        return Err(Error::msg("only the claimant (or a benefits administrator) can cancel"));
    }
    if text(&claim, "state") != Some("submitted") {
        return Err(Error::msg("only a claim waiting for a decision can be cancelled"));
    }
    db::update("bnf_claim", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the claim is gone"))
}

fn reverse(input: Id) -> Result<Value> {
    require_admin()?;
    let claim = require("bnf_claim", &input.id, "claim")?;
    if text(&claim, "state") != Some("approved") {
        return Err(Error::msg("only an approved claim is reversed"));
    }
    let plan = plan_row(text(&claim, "plan").unwrap_or_default())?;
    let employee = text(&claim, "employee").unwrap_or_default();
    let year = claim.get("year").and_then(Value::as_i64).unwrap_or(0) as i32;
    let places = currency_places(text(&plan, "currency").unwrap_or_default())?;
    let amount = money(&claim, "amount", places)?;
    let (_, _, _, _, seq) = leftover(&plan, employee, year)?;
    let source = format!("reversal:{}", input.id);
    let made = write_entry(id_of(&plan)?, employee, year, seq, "reversal", amount, &source, Some(&input.id), text(&claim, "id"))?;
    db::update::<Record>("bnf_claim", &input.id, &json!({ "state": "cancelled" }))?;
    Ok(json!({ "entry": made }))
}

fn balance(input: BalanceIn) -> Result<Value> {
    let me = me_id()?;
    let who = match input.employee {
        Some(other) if Some(&other) != me.as_ref() => {
            require_admin()?;
            other
        }
        Some(own) => own,
        None => me.ok_or_else(|| Error::msg("name the employee"))?,
    };
    let plan = plan_row(&input.plan)?;
    let year = input.year.unwrap_or(year_of(today_date()?));
    let places = currency_places(text(&plan, "currency").unwrap_or_default())?;
    let ceiling = money(&plan, "yearly_ceiling", places)?;
    let (left, accrued, claimed, reversed, _) = leftover(&plan, &who, year)?;
    Ok(json!({
        "plan": input.plan, "employee": who, "year": year, "ceiling": ceiling, "accrued": accrued, "claimed": claimed,
        "reversed": reversed, "remaining": left, "kind": plan["kind"], "currency": plan["currency"],
    }))
}

/// Accrue this month's share for every active enrolment on an accrue-then-claim plan, once per month.
fn accrue_month(today: NaiveDate) -> Result<i64> {
    let year = year_of(today);
    let month = today.month() as i64;
    let plans: Vec<Record> = db::find::<Record>("bnf_plan").filter("kind", "accrue_then_claim").limit(500).all()?;
    let mut written = 0;
    for plan in plans {
        if plan.get("is_active") == Some(&json!(false)) {
            continue;
        }
        let places = currency_places(text(&plan, "currency").unwrap_or_default())?;
        let ceiling = money(&plan, "yearly_ceiling", places)?;
        let target = accrued_by(ceiling, month, 12)?;
        let enrolments: Vec<Record> = db::find("bnf_enrolment").filter("plan", id_of(&plan)?).filter("state", "active").limit(10_000).all()?;
        for enrolment in enrolments {
            let who = text(&enrolment, "employee").unwrap_or_default();
            let start = parse_date(text(&enrolment, "start_date").unwrap_or_default())?;
            if start > today {
                continue;
            }
            if let Some(end) = text(&enrolment, "end_date").map(parse_date).transpose()? {
                if end < today {
                    continue;
                }
            }
            let (accrued, _, _, seq) = sums(id_of(&plan)?, who, year, places)?;
            if accrued >= target {
                continue;
            }
            let add = target - accrued;
            let source = format!("accrual:{}:{}:{}-{:02}", id_of(&plan)?, who, year, month);
            write_entry(id_of(&plan)?, who, year, seq, "accrual", add, &source, None, None)?;
            written += 1;
        }
    }
    Ok(written)
}

/// Payroll plans: a recurring yearly amount as an adjustment, once per enrolment.
fn payroll_plans() -> Result<i64> {
    let plans: Vec<Record> = db::find::<Record>("bnf_plan").filter("kind", "payroll").limit(500).all()?;
    let mut written = 0;
    for plan in plans {
        if plan.get("is_active") == Some(&json!(false)) {
            continue;
        }
        let places = currency_places(text(&plan, "currency").unwrap_or_default())?;
        let amount = money(&plan, "yearly_ceiling", places)?;
        let code = text(&plan, "pay_code").unwrap_or_default();
        let enrolments: Vec<Record> = db::find("bnf_enrolment").filter("plan", id_of(&plan)?).filter("state", "active").limit(10_000).all()?;
        for enrolment in enrolments {
            let who = text(&enrolment, "employee").unwrap_or_default();
            let start = text(&enrolment, "start_date").unwrap_or_default();
            let _: Record = plugins::call(
                "hr_compensation",
                "add_adjustment",
                &json!({
                    "employee": who, "code": code, "amount": amount, "mode": "recurring", "date_from": start,
                    "date_to": enrolment.get("end_date"), "source_plugin": "hr_benefits", "source_ref": enrolment["id"],
                    "reason": format!("Benefit plan {}", text(&plan, "code").unwrap_or("?")),
                }),
            )?;
            written += 1;
        }
    }
    Ok(written)
}

fn nightly() -> Result<Value> {
    let today = today_date()?;
    Ok(json!({ "accrued": accrue_month(today)?, "payroll_synced": payroll_plans()? }))
}

handler! {
    fn create_benefit_plan(input: PlanInput) -> Record {
        plan(input)
    }

    fn list_benefit_plans(_: Empty) -> Vec<Record> {
        db::find("bnf_plan").limit(500).all()
    }

    /// Enrol someone on a plan (benefits administrators), with dependents if the plan allows them.
    fn enrol_in_benefit(input: Enrol) -> Record {
        enrol(input)
    }

    fn end_benefit_enrolment(input: Id) -> Record {
        end_enrolment(input)
    }

    fn claim_benefit(input: ClaimIn) -> Record {
        claim(input)
    }

    fn decide_benefit_claim(input: Decide) -> Record {
        decide(input)
    }

    fn cancel_benefit_claim(input: Id) -> Record {
        cancel(input)
    }

    /// Undo an approved claim by appending a reversal; the remaining balance goes back up.
    fn reverse_benefit_claim(input: Id) -> Value {
        reverse(input)
    }

    fn benefit_balance(input: BalanceIn) -> Value {
        balance(input)
    }

    fn my_benefit_claims(_: Empty) -> Vec<Record> {
        let Some(me) = me_id()? else { return Ok(Vec::new()) };
        db::find("bnf_claim").filter("employee", me.as_str()).limit(500).all()
    }

    fn list_benefit_enrolments(_: Empty) -> Vec<Record> {
        db::find("bnf_enrolment").limit(2000).all()
    }

    fn benefits_nightly(_: Empty) -> Value {
        nightly()
    }
}
