//! Gratuity: end-of-service pay by slabs, calculated from the dates and a stored copy of the rule.
//!
//! Frappe computes it in a controller from the rule's rows as they are when the document is saved, with slabs that
//! include both ends and years rounded as the controller sees fit. Here the rule names how years are counted
//! (`exact`, `round`, `floor`) and how slabs combine (`current_slab`, `cumulative`), slab boundaries are half-open,
//! and the calculation keeps a snapshot of the rule it used, so changing a rule later never changes a figure that
//! was approved. Approval turns the amount into an adjustment; nothing else writes payroll.

use aether_sdk::dates::{format_date, parse_date};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::adjust::{add, ensure_code, NewAdjustment};
use crate::common::*;
use crate::rules::{check_slabs, gratuity_amount, years_of_service, Mode, Slab, YearsMethod};

#[derive(Deserialize)]
struct NewRule {
    name: String,
    years_method: String,
    days_per_year: i64,
    min_years: Decimal,
    mode: String,
    slabs: Vec<Slab>,
}

fn make_rule(input: NewRule) -> Result<Record> {
    require_admin()?;
    if YearsMethod::parse(&input.years_method).is_none() {
        return Err(Error::msg("years are counted `exact`, `round` or `floor`"));
    }
    if Mode::parse(&input.mode).is_none() {
        return Err(Error::msg("slabs combine as `current_slab` or `cumulative`"));
    }
    if !(360..=366).contains(&input.days_per_year) {
        return Err(Error::msg("days per year is 360 to 366"));
    }
    check_slabs(&input.slabs)?;
    db::create(
        "comp_gratuity_rule",
        &json!({
            "name": input.name, "years_method": input.years_method, "days_per_year": input.days_per_year, "min_years": input.min_years,
            "mode": input.mode, "slabs": input.slabs, "is_active": true,
        }),
    )
}

#[derive(Deserialize)]
struct Calculate {
    employee: String,
    rule: String,
    /// The monthly pay the slabs are a fraction of.
    base_amount: Decimal,
    #[serde(default)]
    relieving_date: Option<String>,
    #[serde(default)]
    unpaid_days: i64,
}

fn calculate(input: Calculate) -> Result<Record> {
    require_admin()?;
    let rule = require("comp_gratuity_rule", &input.rule, "gratuity rule")?;
    let person = employee(&input.employee)?;
    let joining = date_of(&person, "hire_date")?;
    let relieving = match input.relieving_date.as_deref().or_else(|| text(&person, "end_date")) {
        Some(day) => parse_date(day)?,
        None => return Err(Error::msg("say the relieving date: this person has no end date yet")),
    };
    let base = money(input.base_amount, "the monthly base")?;
    let method = YearsMethod::parse(text(&rule, "years_method").unwrap_or_default()).ok_or_else(|| Error::msg("the rule's way of counting years is not valid"))?;
    let mode = Mode::parse(text(&rule, "mode").unwrap_or_default()).ok_or_else(|| Error::msg("the rule's way of combining slabs is not valid"))?;
    let slabs: Vec<Slab> = serde_json::from_value(rule["slabs"].clone()).map_err(|e| Error::msg(format!("the rule's slabs: {e}")))?;
    let days_per_year = rule.get("days_per_year").and_then(Value::as_i64).unwrap_or(365);
    let years = years_of_service(joining, relieving, input.unpaid_days, days_per_year, method)?;
    let amount = gratuity_amount(years, base, &slabs, mode, decimal_of(&rule, "min_years")?)?.with_scale(2)?;
    db::create(
        "comp_gratuity",
        &json!({
            "employee": input.employee, "rule": input.rule, "joining_date": format_date(joining), "relieving_date": format_date(relieving),
            "unpaid_days": input.unpaid_days, "years": years, "base_amount": base, "amount": amount, "status": "calculated",
            "snapshot": { "rule_name": rule["name"], "years_method": rule["years_method"], "days_per_year": days_per_year, "mode": rule["mode"], "slabs": rule["slabs"], "min_years": rule["min_years"] },
        }),
    )
}

#[derive(Deserialize)]
struct Approve {
    id: String,
    pay_date: String,
}

fn approve(input: Approve) -> Result<Record> {
    require_admin()?;
    let row = require("comp_gratuity", &input.id, "gratuity")?;
    if text(&row, "status") != Some("calculated") {
        return Err(Error::msg("only a calculated gratuity is approved"));
    }
    ensure_code("gratuity", "earning")?;
    let adjustment = add(NewAdjustment {
        employee: text(&row, "employee").unwrap_or_default().to_string(),
        code: "gratuity".into(),
        amount: decimal_of(&row, "amount")?,
        mode: "one_off".into(),
        pay_date: Some(input.pay_date.clone()),
        date_from: None,
        date_to: None,
        prorate: false,
        reason: Some(format!("Gratuity for {} years of service", decimal_of(&row, "years")?)),
        source_plugin: Some("hr_compensation".into()),
        source_ref: Some(format!("gratuity:{}", input.id)),
    })?;
    db::update("comp_gratuity", &input.id, &json!({ "status": "approved", "pay_date": input.pay_date, "decided_by": actor()?, "adjustment": adjustment["id"] }))?
        .ok_or_else(|| Error::msg("the gratuity is gone"))
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

fn cancel(input: Id) -> Result<Record> {
    require_admin()?;
    let row = require("comp_gratuity", &input.id, "gratuity")?;
    match text(&row, "status") {
        Some("cancelled") => return Ok(row),
        Some("approved") => {
            // The adjustment goes with it, if its period is still open; otherwise this is refused.
            crate::adjust::cancel_adjustment_by_id(text(&row, "adjustment").unwrap_or_default())?;
        }
        _ => {}
    }
    db::update("comp_gratuity", &input.id, &json!({ "status": "cancelled" }))?.ok_or_else(|| Error::msg("the gratuity is gone"))
}

handler! {
    fn create_gratuity_rule(input: NewRule) -> Record {
        make_rule(input)
    }

    fn list_gratuity_rules(_: Empty) -> Vec<Record> {
        db::find("comp_gratuity_rule").order_by("name").limit(100).all()
    }

    fn calculate_gratuity(input: Calculate) -> Record {
        calculate(input)
    }

    fn approve_gratuity(input: Approve) -> Record {
        approve(input)
    }

    fn cancel_gratuity(input: Id) -> Record {
        cancel(input)
    }

    fn list_gratuities(input: Id) -> Vec<Record> {
        require_admin()?;
        db::find("comp_gratuity").filter("employee", input.id.as_str()).order_by("-id").limit(50).all()
    }
}
