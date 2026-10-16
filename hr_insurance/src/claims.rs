//! Claims. One engine settles them (`rules::settle`); the decision is made by a reviewer who is never the claimant,
//! and nothing a person types can set the state or an amount (the rules lock those fields to this plugin's functions).
//!
//! Utilisation is never stored: what is left of a member's limit and deductible is worked out from the approved and
//! paid claims of the same policy and member when the claim is decided, inside that decision.

use aether_sdk::dates::{format_date, parse_date, Duration};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::*;
use crate::enrolments::plan_of_policy;
use crate::rules::{covered_on, settle};

#[derive(Deserialize)]
struct LineIn {
    #[serde(default)]
    service_date: Option<String>,
    #[serde(default)]
    category: Option<String>,
    description: String,
    amount: Decimal,
}

#[derive(Deserialize)]
struct NewClaim {
    enrolment: String,
    #[serde(default)]
    member: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    preauth: Option<String>,
    service_date: String,
    #[serde(default)]
    lines: Vec<LineIn>,
    /// A form with one line sends it flat: `description`, `amount`, `category`.
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    amount: Option<Decimal>,
    #[serde(default)]
    category: Option<String>,
}

#[derive(Deserialize)]
struct Decide {
    id: String,
    approve: bool,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct Reject {
    id: String,
    #[serde(default)]
    note: String,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
pub struct UtilisationIn {
    pub enrolment: String,
    #[serde(default)]
    pub member: Option<String>,
}

pub struct Used {
    pub approved: Decimal,
    pub deductible: Decimal,
}

/// What approved and paid claims of this policy and member have already used. `except` leaves out the claim being
/// decided so a re-run is stable.
pub fn used(policy_enrolments: &[String], member: Option<&str>, except: Option<&str>, places: u32) -> Result<Used> {
    let (mut approved, mut deductible) = (Decimal::zero(places), Decimal::zero(places));
    for enrolment in policy_enrolments {
        for state in ["approved", "paid"] {
            let rows: Vec<Record> = db::find("ins_claim").filter("enrolment", enrolment.as_str()).filter("state", state).limit(5000).all()?;
            for row in rows {
                if except.is_some() && text(&row, "id") == except {
                    continue;
                }
                if member.is_some() && text(&row, "member") != member {
                    continue;
                }
                approved = approved + decimal_of(&row, "approved_amount", places)?;
                deductible = deductible + decimal_of(&row, "deductible_applied", places)?;
            }
        }
    }
    Ok(Used { approved, deductible })
}

fn enrolments_of_policy(policy: &str, same_employee: &str) -> Result<Vec<String>> {
    let rows: Vec<Record> = db::find("ins_enrolment").filter("policy", policy).filter("employee", same_employee).limit(100).all()?;
    Ok(rows.iter().filter_map(|r| text(r, "id").map(str::to_string)).collect())
}

fn submit(mut input: NewClaim) -> Result<Record> {
    if input.lines.is_empty() {
        if let (Some(description), Some(amount)) = (input.description.take(), input.amount.take()) {
            input.lines.push(LineIn { service_date: None, category: input.category.take(), description, amount });
        }
    }
    let enrolment = require("ins_enrolment", &input.enrolment, "enrolment")?;
    let who = text(&enrolment, "employee").unwrap_or_default().to_string();
    require_self_or_staff(&who)?;
    if text(&enrolment, "state") != Some("active") && text(&enrolment, "state") != Some("ended") {
        return Err(Error::msg("this enrolment was cancelled"));
    }
    let policy = require("ins_policy", text(&enrolment, "policy").unwrap_or_default(), "policy")?;
    let plan = plan_of_policy(&policy)?;
    let currency = text(&plan, "currency").unwrap_or_default().to_string();
    let places = currency_places(&currency)?;
    let service = parse_date(&input.service_date)?;
    let today = today_date()?;
    if service > today {
        return Err(Error::msg("a claim is for something that has already happened"));
    }
    let window = plan.get("claim_window_days").and_then(Value::as_i64).unwrap_or(90);
    if today - service > Duration::days(window) && !is_staff()? {
        return Err(Error::msg(format!("claims are made within {window} days of the service")));
    }
    if service < date_of(&policy, "start_date")? || service > date_of(&policy, "end_date")? {
        return Err(Error::msg("the service date is outside the policy's dates"));
    }
    if input.lines.is_empty() {
        return Err(Error::msg("a claim has at least one line"));
    }
    let member_id = match input.member.clone() {
        Some(id) => id,
        None => {
            let own: Vec<Record> = db::find("ins_member").filter("enrolment", input.enrolment.as_str()).filter("relationship", "self").limit(1).all()?;
            text(own.first().ok_or_else(|| Error::msg("this enrolment has no covered employee"))?, "id").unwrap_or_default().to_string()
        }
    };
    let member = require("ins_member", &member_id, "member")?;
    if text(&member, "enrolment") != Some(input.enrolment.as_str()) {
        return Err(Error::msg("that person is not covered by this enrolment"));
    }
    if !covered_on(date_of(&member, "coverage_start")?, date_opt(&member, "coverage_end")?, service) {
        return Err(Error::msg("this person was not covered on the service date"));
    }
    if let Some(provider) = &input.provider {
        let p = require("ins_provider", provider, "provider")?;
        if p.get("is_active") == Some(&json!(false)) {
            return Err(Error::msg("this provider is no longer used"));
        }
        if flag(&plan, "network_only") && !flag(&p, "is_network") {
            return Err(Error::msg("this plan pays only providers in its network"));
        }
    } else if flag(&plan, "network_only") {
        return Err(Error::msg("name the provider: this plan pays only providers in its network"));
    }
    let mut total = Decimal::zero(places);
    let mut lines = Vec::new();
    for line in &input.lines {
        if line.amount.is_negative() || line.amount.is_zero() {
            return Err(Error::msg("a line is for more than zero"));
        }
        if line.description.trim().is_empty() {
            return Err(Error::msg("say what each line is for"));
        }
        let amount = line.amount.with_scale(places)?;
        total = total + amount;
        lines.push(json!({
            "service_date": line.service_date.clone().unwrap_or_else(|| format_date(service)),
            "category": line.category.clone().unwrap_or_else(|| "consultation".into()), "description": line.description, "amount": amount,
        }));
    }
    if let Some(threshold) = plan.get("preauth_threshold").filter(|v| !v.is_null()) {
        let threshold: Decimal = serde_json::from_value(threshold.clone()).map_err(|e| Error::msg(e.to_string()))?;
        if total >= threshold.with_scale(places)? {
            let pre = input.preauth.as_deref().ok_or_else(|| Error::msg("a claim of this size needs an approved preauthorisation"))?;
            crate::preauth::check_usable(pre, &input.enrolment, &member_id, service, total, places)?;
        }
    }
    db::create(
        "ins_claim",
        &json!({
            "reference": next_number("claim", "CLM-", 6)?, "enrolment": input.enrolment, "employee": who, "member": member_id,
            "provider": input.provider, "preauth": input.preauth, "currency": currency, "service_date": format_date(service),
            "state": "submitted", "submitted_on": format_date(today), "lines": lines,
        }),
    )
    .map(|c: Record| {
        let _ = events::emit("claim_submitted", &json!({ "claim": c["id"], "employee": who, "amount": total, "currency": currency }));
        c
    })
}

fn decide(input: Decide) -> Result<Record> {
    require_staff()?;
    let claim = db::find::<Record>("ins_claim").filter("id", input.id.as_str()).expand("lines").first()?.ok_or_else(|| Error::msg("there is no such claim"))?;
    if text(&claim, "state") != Some("submitted") {
        return Err(Error::msg("this claim is not waiting for a decision"));
    }
    let me = my_employee_id()?;
    if me.is_some() && me.as_deref() == text(&claim, "employee") {
        return Err(Error::msg("nobody decides their own claim"));
    }
    let by = actor()?;
    let on = format_date(today_date()?);
    if !input.approve {
        if input.note.as_deref().is_none_or(|n| n.trim().is_empty()) {
            return Err(Error::msg("say why the claim is refused"));
        }
        return db::update("ins_claim", &input.id, &json!({ "state": "rejected", "decided_by": by, "decided_on": on, "rejection_reason": input.note }))?
            .ok_or_else(|| Error::msg("the claim is gone"));
    }
    let enrolment = require("ins_enrolment", text(&claim, "enrolment").unwrap_or_default(), "enrolment")?;
    let policy_id = text(&enrolment, "policy").unwrap_or_default().to_string();
    let policy = require("ins_policy", &policy_id, "policy")?;
    let plan = plan_of_policy(&policy)?;
    let places = currency_places(text(&claim, "currency").unwrap_or_default())?;
    let claimed = claim
        .get("lines")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().try_fold(Decimal::zero(places), |sum, r| Ok::<_, Error>(sum + decimal_of(r, "amount", places)?)))
        .transpose()?
        .unwrap_or(Decimal::zero(places));
    let others = enrolments_of_policy(&policy_id, text(&claim, "employee").unwrap_or_default())?;
    let member_key = text(&claim, "member").unwrap_or_default().to_string();
    // Optimistic lock: each decision on a member takes the next number in `ins_decision`, whose key is unique. Two
    // reviewers deciding for one member at once compute the same number; the second write is refused and that
    // reviewer's decision is worked out again against what the first one used.
    let mut s = None;
    for attempt in 0..4 {
        // The number is read before the sums: a decision that commits in between makes the sums include it and this
        // number collide, so it is worked out again; the other order could miss its amounts and still not collide.
        let prior: Vec<Record> = db::find("ins_decision").filter("enrolment", text(&claim, "enrolment").unwrap_or_default()).limit(5000).all()?;
        let seq = prior
            .iter()
            .filter(|r| text(r, "member").unwrap_or_default() == member_key)
            .filter_map(|r| r.get("seq").and_then(Value::as_i64))
            .max()
            .unwrap_or(0)
            + 1;
        let spent = used(&others, text(&claim, "member"), Some(&input.id), places)?;
        let deductible_left = decimal_of(&plan, "deductible", places)? - spent.deductible;
        let limit_left = decimal_of(&plan, "member_limit", places)? - spent.approved;
        let settled = settle(claimed, deductible_left, decimal_of(&plan, "copay_percent", 2)?, limit_left)?;
        let mut tx = db::transaction()
            .create(
                "ins_decision",
                &json!({ "key": format!("{}:{}:{}", text(&claim, "enrolment").unwrap_or_default(), member_key, seq), "enrolment": claim["enrolment"], "member": member_key, "seq": seq, "claim": input.id }),
            )
            .update(
                "ins_claim",
                &input.id,
                &json!({
                    "state": "approved", "approved_amount": settled.approved, "deductible_applied": settled.deductible, "copay_applied": settled.copay,
                    "decided_by": by, "decided_on": on,
                }),
            );
        if let Some(pre) = text(&claim, "preauth") {
            tx = crate::preauth::consume(tx, pre, settled.approved, places)?;
        }
        match tx.run() {
            Ok(_) => {
                s = Some(settled);
                break;
            }
            Err(e) if attempt < 3 => {
                let still = require("ins_claim", &input.id, "claim")?;
                if text(&still, "state") != Some("submitted") {
                    return Err(e);
                }
            }
            Err(e) => return Err(e),
        }
    }
    let s = s.ok_or_else(|| Error::msg("the claim could not be decided: try again"))?;
    events::emit(
        "claim_approved",
        &json!({ "claim": input.id, "employee": claim["employee"], "claimed": claimed, "approved": s.approved, "currency": claim["currency"], "date": on }),
    )?;
    require("ins_claim", &input.id, "claim")
}

fn cancel(input: Id) -> Result<Record> {
    let claim = require("ins_claim", &input.id, "claim")?;
    let mine = my_employee_id()?.as_deref() == text(&claim, "employee");
    if !mine && !is_staff()? {
        return Err(Error::msg("only the claimant (or insurance staff) can cancel"));
    }
    if text(&claim, "state") != Some("submitted") {
        return Err(Error::msg("only a claim waiting for a decision can be cancelled"));
    }
    db::update("ins_claim", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the claim is gone"))
}

/// What is left of a member's limit and deductible in the policy year.
fn utilisation(input: UtilisationIn) -> Result<Value> {
    let enrolment = require("ins_enrolment", &input.enrolment, "enrolment")?;
    let who = text(&enrolment, "employee").unwrap_or_default().to_string();
    require_self_or_staff(&who)?;
    let policy_id = text(&enrolment, "policy").unwrap_or_default().to_string();
    let policy = require("ins_policy", &policy_id, "policy")?;
    let plan = plan_of_policy(&policy)?;
    let places = currency_places(text(&plan, "currency").unwrap_or_default())?;
    let enrolments = enrolments_of_policy(&policy_id, &who)?;
    let spent = used(&enrolments, input.member.as_deref(), None, places)?;
    let limit = decimal_of(&plan, "member_limit", places)?;
    let deductible = decimal_of(&plan, "deductible", places)?;
    let mut left = limit - spent.approved;
    if left.is_negative() {
        left = Decimal::zero(places);
    }
    let rate = if limit.is_zero() { Decimal::zero(2) } else { spent.approved.times(Decimal::whole(100, 0)?, places)?.div(limit, 2)? };
    Ok(json!({
        "policy": policy_id, "member": input.member, "limit": limit, "used": spent.approved, "remaining": left,
        "deductible": deductible, "deductible_used": spent.deductible, "utilisation_percent": rate, "currency": plan["currency"],
    }))
}

#[derive(Deserialize)]
struct ForEmployee {
    #[serde(default)]
    employee: Option<String>,
}

/// One row per covered member of the employee's active enrolments: limit, used and left for the policy year.
fn summary(input: ForEmployee) -> Result<Vec<Value>> {
    let who = match input.employee {
        Some(id) => id,
        None => my_employee_id()?.ok_or_else(|| Error::msg("you are not an employee"))?,
    };
    require_self_or_staff(&who)?;
    let enrolments: Vec<Record> = db::find("ins_enrolment").filter("employee", who.as_str()).filter("state", "active").limit(50).all()?;
    let mut out = Vec::new();
    for e in enrolments {
        let enrolment = id_of(&e)?.to_string();
        let policy = require("ins_policy", text(&e, "policy").unwrap_or_default(), "policy")?;
        let plan = plan_of_policy(&policy)?;
        let members: Vec<Record> = db::find("ins_member").filter("enrolment", enrolment.as_str()).limit(50).all()?;
        for m in members {
            let u = utilisation(UtilisationIn { enrolment: enrolment.clone(), member: Some(id_of(&m)?.to_string()) })?;
            out.push(json!({
                "enrolment": enrolment, "policy": policy["number"], "plan": plan["name"], "member": m["name"], "relationship": m["relationship"],
                "limit": u["limit"], "used": u["used"], "remaining": u["remaining"], "utilisation_percent": u["utilisation_percent"], "currency": u["currency"],
            }));
        }
    }
    Ok(out)
}

handler! {
    fn employee_insurance_summary(input: ForEmployee) -> Vec<Value> {
        summary(input)
    }

    fn submit_insurance_claim(input: NewClaim) -> Record {
        submit(input)
    }

    fn decide_insurance_claim(input: Decide) -> Record {
        decide(input)
    }

    fn approve_insurance_claim(input: Id) -> Record {
        decide(Decide { id: input.id, approve: true, note: None })
    }

    fn reject_insurance_claim(input: Reject) -> Record {
        decide(Decide { id: input.id, approve: false, note: Some(input.note) })
    }

    /// The people the caller may claim for: those covered by their own enrolments.
    fn my_insurance_members(_: Empty) -> Vec<Record> {
        let Some(me) = my_employee_id()? else { return Ok(Vec::new()) };
        let enrolments: Vec<Record> = db::find("ins_enrolment").filter("employee", me.as_str()).limit(200).all()?;
        let mut out = Vec::new();
        for e in enrolments {
            out.extend(db::find::<Record>("ins_member").filter("enrolment", text(&e, "id").unwrap_or_default()).limit(50).all()?);
        }
        Ok(out)
    }

    fn cancel_insurance_claim(input: Id) -> Record {
        cancel(input)
    }

    fn insurance_utilisation(input: UtilisationIn) -> Value {
        utilisation(input)
    }

    fn my_insurance_claims(_: Empty) -> Vec<Record> {
        let Some(me) = my_employee_id()? else { return Ok(Vec::new()) };
        db::find("ins_claim").filter("employee", me.as_str()).limit(500).all()
    }

    fn insurance_claims_waiting(_: Empty) -> Vec<Record> {
        require_staff()?;
        db::find("ins_claim").filter("state", "submitted").limit(1000).all()
    }

    fn insurance_claims_to_pay(_: Empty) -> Vec<Record> {
        require_staff()?;
        db::find("ins_claim").filter("state", "approved").limit(1000).all()
    }

    fn get_insurance_claim(input: Id) -> Record {
        let row = db::find::<Record>("ins_claim").filter("id", input.id.as_str()).expand("lines").first()?.ok_or_else(|| Error::msg("there is no such claim"))?;
        require_self_or_staff(text(&row, "employee").unwrap_or_default())?;
        Ok(row)
    }
}
