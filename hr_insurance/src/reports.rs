//! Reports: claims by state, a dashboard, who is covered, premiums against claims paid, and the review reminder. Read through the caller's rules, so a manager sees a team and
//! staff see everything; nothing is interpolated into a query.

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use aether_sdk::dates::Duration;

use crate::common::*;

#[derive(Deserialize)]
struct ClaimsIn {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
}

fn claims_analysis(input: ClaimsIn) -> Result<Value> {
    let mut q = db::find::<Record>("ins_claim");
    if let Some(s) = input.state.as_deref() {
        q = q.filter("state", s);
    }
    let rows = q.limit(20_000).all()?;
    let mut by_state: std::collections::BTreeMap<String, (i64, Decimal)> = std::collections::BTreeMap::new();
    for r in &rows {
        let day = text(r, "service_date").unwrap_or_default();
        if input.from.as_deref().is_some_and(|f| day < f) || input.to.as_deref().is_some_and(|t| day > t) {
            continue;
        }
        let places = 2;
        let entry = by_state.entry(text(r, "state").unwrap_or("?").to_string()).or_insert((0, Decimal::zero(places)));
        entry.0 += 1;
        entry.1 = entry.1 + decimal_of(r, "approved_amount", places)?;
    }
    Ok(json!(by_state.into_iter().map(|(state, (count, approved))| json!({ "state": state, "claims": count, "approved": approved })).collect::<Vec<_>>()))
}

type Money = std::collections::BTreeMap<String, Decimal>;

fn add(map: &mut Money, currency: &str, amount: Decimal) {
    let slot = map.entry(currency.to_string()).or_insert_with(|| Decimal::zero(amount.scale()));
    *slot = *slot + amount;
}

/// A claim waiting for a decision for over a week is announced once a night until someone decides it.
pub fn remind_pending_claims() -> Result<i64> {
    let cutoff = today_date()? - Duration::days(7);
    let waiting: Vec<Record> = db::find("ins_claim").filter("state", "submitted").limit(5000).all()?;
    let mut n = 0;
    for c in waiting {
        if date_opt(&c, "submitted_on")?.is_some_and(|d| d < cutoff) {
            events::emit("claim_pending_review", &json!({ "claim": c["id"], "reference": c["reference"], "employee": c["employee"], "submitted_on": c["submitted_on"] }))?;
            n += 1;
        }
    }
    Ok(n)
}

/// Numbers for the landing page: what is waiting, who is covered and what has been paid out.
fn dashboard() -> Result<Value> {
    let waiting = db::find::<Record>("ins_claim").filter("state", "submitted").limit(20_000).all()?.len();
    let preauths = db::find::<Record>("ins_preauth").filter("state", "submitted").limit(20_000).all()?.len();
    let enrolments = db::find::<Record>("ins_enrolment").filter("state", "active").limit(20_000).all()?;
    let policies = db::find::<Record>("ins_policy").filter("state", "active").limit(5000).all()?.len();
    let mut covered = 0;
    for e in &enrolments {
        covered += db::find::<Record>("ins_member").filter("enrolment", text(e, "id").unwrap_or_default()).filter("is_active", true).limit(50).all()?.len();
    }
    let mut approved = Money::new();
    for c in db::find::<Record>("ins_claim").limit(20_000).all()? {
        if let Some("approved" | "paid") = text(&c, "state") {
            add(&mut approved, text(&c, "currency").unwrap_or("?"), decimal_of(&c, "approved_amount", 2)?);
        }
    }
    let by_state = claims_analysis(ClaimsIn { state: None, from: None, to: None })?;
    Ok(json!({
        "claims_waiting": waiting, "preauths_waiting": preauths, "active_policies": policies, "active_enrolments": enrolments.len(),
        "covered_people": covered, "approved_total": approved, "claims_by_state": by_state,
    }))
}

/// One row per covered person on an active enrolment.
fn dependants_coverage() -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for e in db::find::<Record>("ins_enrolment").filter("state", "active").limit(20_000).all()? {
        let policy = require("ins_policy", text(&e, "policy").unwrap_or_default(), "policy")?;
        for m in db::find::<Record>("ins_member").filter("enrolment", text(&e, "id").unwrap_or_default()).limit(50).all()? {
            out.push(json!({
                "employee": e["employee"], "policy": policy["number"], "member": m["name"], "relationship": m["relationship"],
                "date_of_birth": m["date_of_birth"], "coverage_start": m["coverage_start"], "coverage_end": m["coverage_end"], "active": m["is_active"],
            }));
        }
    }
    Ok(out)
}

/// Per policy: premiums charged, split between employer and employee, against what was paid out in claims.
fn premium_utilisation() -> Result<Vec<Value>> {
    let mut of_enrolment = std::collections::HashMap::new();
    for e in db::find::<Record>("ins_enrolment").limit(50_000).all()? {
        of_enrolment.insert(text(&e, "id").unwrap_or_default().to_string(), text(&e, "policy").unwrap_or_default().to_string());
    }
    #[derive(Default)]
    struct Sums { premium: Option<Decimal>, employer: Option<Decimal>, employee: Option<Decimal>, claims: Option<Decimal>, count: i64 }
    let places = 2;
    let mut sums: std::collections::BTreeMap<String, Sums> = std::collections::BTreeMap::new();
    let zero = || Decimal::zero(places);
    for p in db::find::<Record>("ins_premium").limit(50_000).all()? {
        let Some(policy) = of_enrolment.get(text(&p, "enrolment").unwrap_or_default()) else { continue };
        let s = sums.entry(policy.clone()).or_default();
        s.premium = Some(s.premium.unwrap_or_else(zero) + decimal_of(&p, "total", places)?);
        s.employer = Some(s.employer.unwrap_or_else(zero) + decimal_of(&p, "employer_share", places)?);
        s.employee = Some(s.employee.unwrap_or_else(zero) + decimal_of(&p, "employee_share", places)?);
    }
    for c in db::find::<Record>("ins_claim").limit(50_000).all()? {
        if !matches!(text(&c, "state"), Some("approved" | "paid")) {
            continue;
        }
        let Some(policy) = of_enrolment.get(text(&c, "enrolment").unwrap_or_default()) else { continue };
        let s = sums.entry(policy.clone()).or_default();
        s.claims = Some(s.claims.unwrap_or_else(zero) + decimal_of(&c, "approved_amount", places)?);
        s.count += 1;
    }
    let mut out = Vec::new();
    for (id, s) in sums {
        let policy = require("ins_policy", &id, "policy")?;
        let premium = s.premium.unwrap_or_else(zero);
        let claims = s.claims.unwrap_or_else(zero);
        let ratio = if premium.is_zero() { Decimal::zero(2) } else { claims.times(Decimal::whole(100, 0)?, places)?.div(premium, 2)? };
        out.push(json!({
            "policy": policy["number"], "premiums": premium, "employer_share": s.employer.unwrap_or_else(zero),
            "employee_share": s.employee.unwrap_or_else(zero), "claims_paid": claims, "claims": s.count, "loss_ratio_percent": ratio,
        }));
    }
    Ok(out)
}

handler! {
    fn insurance_dashboard(_: Empty) -> Value {
        require_staff()?;
        dashboard()
    }

    fn insurance_dependants_coverage(_: Empty) -> Vec<Value> {
        require_staff()?;
        dependants_coverage()
    }

    fn insurance_premium_utilisation(_: Empty) -> Vec<Value> {
        require_staff()?;
        premium_utilisation()
    }

    fn insurance_claims_analysis(input: ClaimsIn) -> Value {
        require_staff()?;
        claims_analysis(input)
    }
}
