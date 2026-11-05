//! Policies: one per insurer, plan and plan year. A renewal is a new policy pointing at the old one, so history is
//! never overwritten and an employee can be enrolled in many years.

use aether_sdk::dates::{format_date, parse_date};
use aether_sdk::prelude::*;

use crate::common::{date_of, require, require_staff, text, today_date, Record};

#[derive(Deserialize)]
struct NewPolicy {
    number: String,
    plan: String,
    start_date: String,
    end_date: String,
    #[serde(default)]
    company: Option<String>,
    #[serde(default)]
    renewal_of: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

fn create(input: NewPolicy) -> Result<Record> {
    require_staff()?;
    let plan = require("ins_plan", &input.plan, "plan")?;
    if plan.get("is_active") == Some(&json!(false)) {
        return Err(Error::msg("this plan is no longer offered"));
    }
    let start = parse_date(&input.start_date)?;
    let end = parse_date(&input.end_date)?;
    if end < start {
        return Err(Error::msg("the policy ends before it starts"));
    }
    if let Some(old) = &input.renewal_of {
        let old = require("ins_policy", old, "policy")?;
        if date_of(&old, "end_date")? >= start {
            return Err(Error::msg("a renewal starts after the policy it renews ends"));
        }
        if text(&old, "plan") != Some(input.plan.as_str()) && text(&old, "insurer") != text(&plan, "insurer") {
            return Err(Error::msg("a renewal keeps the insurer"));
        }
    }
    db::create(
        "ins_policy",
        &json!({
            "number": input.number.trim(), "insurer": plan["insurer"], "plan": input.plan, "company": input.company,
            "start_date": format_date(start), "end_date": format_date(end), "state": "draft", "renewal_of": input.renewal_of, "notes": input.notes,
        }),
    )
    .map_err(|e| e.or("could not create the policy (the insurer already has this number)"))
}

fn activate(input: Id) -> Result<Record> {
    require_staff()?;
    let policy = require("ins_policy", &input.id, "policy")?;
    if text(&policy, "state") != Some("draft") {
        return Err(Error::msg("only a draft policy is activated"));
    }
    let overlapping: Vec<Record> = db::find("ins_policy").filter("plan", text(&policy, "plan").unwrap_or_default()).filter("state", "active").limit(200).all()?;
    let (start, end) = (date_of(&policy, "start_date")?, date_of(&policy, "end_date")?);
    for other in overlapping {
        if text(&other, "number") == text(&policy, "number") {
            continue;
        }
        if aether_sdk::dates::overlaps(start, Some(end), date_of(&other, "start_date")?, Some(date_of(&other, "end_date")?)) {
            return Err(Error::msg("another active policy of this plan runs on those days: end it or renew it instead"));
        }
    }
    db::update("ins_policy", &input.id, &json!({ "state": "active" }))?.ok_or_else(|| Error::msg("the policy is gone"))
}

fn cancel(input: Id) -> Result<Record> {
    require_staff()?;
    let policy = require("ins_policy", &input.id, "policy")?;
    if matches!(text(&policy, "state"), Some("cancelled" | "expired")) {
        return Err(Error::msg("this policy is already over"));
    }
    let open: Vec<Record> = db::find("ins_enrolment").filter("policy", input.id.as_str()).filter("state", "active").limit(1).all()?;
    if !open.is_empty() {
        return Err(Error::msg("end the enrolments on this policy first"));
    }
    db::update("ins_policy", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the policy is gone"))
}

/// Daily: policies past their end become `expired` and their enrolments `ended`; the ones ending within
/// 30 days are announced once as `policy_expiring`.
pub fn nightly() -> Result<Value> {
    let today = today_date()?;
    let soon = today + aether_sdk::dates::Duration::days(30);
    let (mut expired, mut expiring) = (0, 0);
    let active: Vec<Record> = db::find("ins_policy").filter("state", "active").limit(5000).all()?;
    for policy in active {
        let end = date_of(&policy, "end_date")?;
        let id = text(&policy, "id").unwrap_or_default();
        if end < today {
            let enrolments: Vec<Record> = db::find("ins_enrolment").filter("policy", id).filter("state", "active").limit(10_000).all()?;
            for e in enrolments {
                db::update::<Record>("ins_enrolment", text(&e, "id").unwrap_or_default(), &json!({ "state": "ended", "end_date": format_date(end) }))?;
            }
            db::update::<Record>("ins_policy", id, &json!({ "state": "expired" }))?;
            expired += 1;
        } else if end <= soon {
            events::emit("policy_expiring", &json!({ "policy": id, "number": policy["number"], "end_date": policy["end_date"] }))?;
            expiring += 1;
        }
    }
    Ok(json!({ "expired": expired, "expiring": expiring }))
}

handler! {
    fn create_policy(input: NewPolicy) -> Record {
        create(input)
    }

    fn activate_policy(input: Id) -> Record {
        activate(input)
    }

    fn cancel_policy(input: Id) -> Record {
        cancel(input)
    }

    fn list_policies(_: Empty) -> Vec<Record> {
        db::find("ins_policy").limit(1000).all()
    }

    fn insurance_nightly(_: Empty) -> Value {
        let out = nightly()?;
        let premiums = crate::premiums::generate_current()?;
        let expired = crate::preauth::expire_due()?;
        let reminded = crate::reports::remind_pending_claims()?;
        Ok(json!({ "policies": out, "premiums_created": premiums, "preauths_expired": expired, "claims_reminded": reminded }))
    }
}
