//! Moves the health-insurance enrolments of `hr_benefits` (`bnf_enrolment` with a carrier or policy number, and
//! their `bnf_dependent` rows) into this plugin. Safe to run again: an enrolment already moved is skipped (its
//! reference is `BNF-<old id>`), and the old records are left as they are.

use aether_sdk::dates::parse_date;
use aether_sdk::prelude::*;
use std::collections::HashMap;

use crate::common::*;

fn slug(text: &str) -> String {
    let s: String = text.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' }).collect();
    s.trim_matches('_').chars().take(30).collect()
}

fn one(model: &str, field: &str, value: &str) -> Result<Option<Record>> {
    db::find::<Record>(model).filter(field, value).first()
}

fn migrate() -> Result<Value> {
    require_admin()?;
    let enrolments: Vec<Record> = plugins::call("hr_benefits", "list_benefit_enrolments", &json!({}))?;
    let plans: Vec<Record> = plugins::call("hr_benefits", "list_benefit_plans", &json!({}))?;
    let dependents: Vec<Record> = plugins::call("hr_benefits", "list_benefit_dependents", &json!({}))?;
    let plan_by_id: HashMap<String, &Record> = plans.iter().filter_map(|p| text(p, "id").map(|id| (id.to_string(), p))).collect();
    let (mut moved, mut skipped) = (0, 0);
    let mut notes: Vec<String> = Vec::new();
    for old in &enrolments {
        let old_id = id_of(old)?;
        let carrier = text(old, "carrier").map(str::trim).filter(|c| !c.is_empty());
        let number = text(old, "policy_number").map(str::trim).filter(|c| !c.is_empty());
        if carrier.is_none() && number.is_none() {
            continue; // a benefit enrolment that is not insurance
        }
        let reference = format!("BNF-{old_id}");
        if one("ins_enrolment", "reference", &reference)?.is_some() {
            skipped += 1;
            continue;
        }
        let Some(old_plan) = plan_by_id.get(text(old, "plan").unwrap_or_default()) else {
            notes.push(format!("{old_id}: its benefit plan is gone"));
            continue;
        };
        let currency = text(old_plan, "currency").unwrap_or("USD").to_string();
        let places = currency_places(&currency)?;
        let carrier_name = carrier.unwrap_or("Unknown insurer");
        let insurer = match one("ins_insurer", "code", &slug(carrier_name))? {
            Some(i) => i,
            None => db::create("ins_insurer", &json!({ "code": slug(carrier_name), "name": carrier_name, "is_active": true }))?,
        };
        let plan_code = format!("{}-{}", slug(carrier_name), slug(text(old_plan, "code").unwrap_or("PLAN")));
        let plan = match one("ins_plan", "code", &plan_code)? {
            Some(p) => p,
            None => db::create(
                "ins_plan",
                &json!({
                    "code": plan_code, "name": text(old_plan, "name").unwrap_or("Migrated plan"), "insurer": insurer["id"], "coverage_type": "standard",
                    "currency": currency, "member_limit": decimal_of(old_plan, "yearly_ceiling", places)?, "deductible": 0, "copay_percent": 0,
                    "premium_amount": decimal_of(old, "premium", places)?, "premium_period": "annual", "employer_share_percent": 100,
                    "allow_spouse": flag(old_plan, "allow_spouse"), "allow_children": flag(old_plan, "allow_children"),
                    "max_children": old_plan.get("max_dependents").cloned().unwrap_or(Value::Null), "is_active": true,
                }),
            )?,
        };
        let year = old.get("year").and_then(Value::as_i64).unwrap_or(0);
        let pnumber = number.map(str::to_string).unwrap_or_else(|| format!("{}-{year}", slug(text(old_plan, "code").unwrap_or("PLAN"))));
        let policy = match db::find::<Record>("ins_policy").filter("insurer", id_of(&insurer)?).filter("number", pnumber.as_str()).first()? {
            Some(p) => p,
            None => db::create(
                "ins_policy",
                &json!({
                    "number": pnumber, "insurer": insurer["id"], "plan": plan["id"], "start_date": format!("{year}-01-01"), "end_date": format!("{year}-12-31"),
                    "state": if parse_date(&format!("{year}-12-31"))? < today_date()? { "expired" } else { "active" },
                }),
            )?,
        };
        let start = text(old, "start_date").unwrap_or_default().to_string();
        let end = text(old, "end_date").map(str::to_string).unwrap_or_else(|| format!("{year}-12-31"));
        let mut members = vec![json!({ "employee": old["employee"], "relationship": "self", "name": "Employee", "coverage_start": start, "coverage_end": end, "is_active": true })];
        for dep in dependents.iter().filter(|d| text(d, "enrolment") == Some(old_id)) {
            let relation = match text(dep, "relation") {
                Some("spouse") => "spouse",
                Some("child") => "child",
                _ => "other",
            };
            members.push(json!({
                "employee": old["employee"], "relationship": relation, "name": text(dep, "name").unwrap_or("Dependent"), "date_of_birth": dep.get("born"),
                "coverage_start": start, "coverage_end": end, "is_active": true,
            }));
        }
        if members.iter().any(|m| m["relationship"] == "other") {
            notes.push(format!("{old_id}: a dependent that is neither spouse nor child was moved as `other`"));
        }
        let state = if text(old, "state") == Some("active") { "active" } else { "ended" };
        let _: Record = db::create(
            "ins_enrolment",
            &json!({
                "reference": reference, "policy": policy["id"], "employee": old["employee"], "start_date": start, "end_date": end,
                "state": state, "members": members,
            }),
        )?;
        moved += 1;
    }
    Ok(json!({ "moved": moved, "already_moved": skipped, "notes": notes }))
}

handler! {
    /// Copy health-insurance enrolments from `hr_benefits`; run again to pick up new ones.
    fn migrate_from_benefits(_: Empty) -> Value {
        migrate()
    }
}
