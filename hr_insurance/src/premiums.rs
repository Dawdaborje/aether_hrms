//! Premiums: one row per enrolment and month, split into the employee's and the employer's share. The employee's
//! share becomes a deduction adjustment in `hr_compensation` (once, keyed by the premium); when payroll approves a
//! run covering the period the premium is marked deducted. Remittance to the insurer is a payment.

use aether_sdk::dates::{add_months, format_date, parse_date, Datelike, NaiveDate};
use aether_sdk::events::Received;
use aether_sdk::prelude::*;

use crate::common::*;
use crate::enrolments::plan_of_policy;
use crate::rules::{covered_on, monthly_premium, split_premium};

pub const DEDUCTION_CODE: &str = "insurance_premium";

#[derive(Deserialize)]
struct Month {
    /// `YYYY-MM`; the current month when left out.
    #[serde(default)]
    month: Option<String>,
}

fn month_bounds(month: Option<&str>) -> Result<(NaiveDate, NaiveDate)> {
    let first = match month {
        Some(m) => parse_date(&format!("{m}-01"))?,
        None => today_date()?.with_day(1).ok_or_else(|| Error::msg("no such day"))?,
    };
    Ok((first, add_months(first, 1) - aether_sdk::dates::Duration::days(1)))
}

/// Makes the month's premiums for every enrolment covered on the month's last day; running it again is a no-op.
pub fn generate(month: Option<&str>) -> Result<i64> {
    let (first, last) = month_bounds(month)?;
    let key = format!("{}-{:02}", first.year(), first.month());
    let _: Record = plugins::call("hr_compensation", "ensure_pay_code", &json!({ "code": DEDUCTION_CODE, "kind": "deduction" }))?;
    let enrolments: Vec<Record> = db::find("ins_enrolment").filter("state", "active").limit(20_000).all()?;
    let mut made = 0;
    for e in enrolments {
        let id = id_of(&e)?;
        let reference = format!("prem:{id}:{key}");
        if db::find::<Record>("ins_premium").filter("reference", reference.as_str()).first()?.is_some() {
            continue;
        }
        if !covered_on(date_of(&e, "start_date")?, date_opt(&e, "end_date")?, last) && !covered_on(date_of(&e, "start_date")?, date_opt(&e, "end_date")?, first) {
            continue;
        }
        let policy = require("ins_policy", text(&e, "policy").unwrap_or_default(), "policy")?;
        let plan = plan_of_policy(&policy)?;
        let currency = text(&plan, "currency").unwrap_or_default().to_string();
        let places = currency_places(&currency)?;
        let total = monthly_premium(decimal_of(&plan, "premium_amount", places)?, text(&plan, "premium_period").unwrap_or("monthly"))?;
        let (employee_share, employer_share) = split_premium(total, decimal_of(&plan, "employer_share_percent", 2)?)?;
        let who = text(&e, "employee").unwrap_or_default().to_string();
        let premium: Record = db::create(
            "ins_premium",
            &json!({
                "reference": reference, "enrolment": id, "employee": who, "period_start": format_date(first), "period_end": format_date(last),
                "currency": currency, "total": total, "employee_share": employee_share, "employer_share": employer_share, "state": "scheduled",
            }),
        )?;
        if !employee_share.is_zero() {
            let adj: Record = plugins::call(
                "hr_compensation",
                "add_adjustment",
                &json!({
                    "employee": who, "code": DEDUCTION_CODE, "amount": employee_share, "mode": "one_off", "pay_date": format_date(last),
                    "source_plugin": "hr_insurance", "source_ref": id_of(&premium)?, "reason": format!("Insurance premium {key}"),
                }),
            )?;
            db::update::<Record>("ins_premium", id_of(&premium)?, &json!({ "adjustment": adj.get("id") }))?;
        } else {
            db::update::<Record>("ins_premium", id_of(&premium)?, &json!({ "state": "deducted" }))?;
        }
        events::emit(
            "premium_accrued",
            &json!({ "premium": id_of(&premium)?, "employee": who, "total": total, "employee_share": employee_share, "employer_share": employer_share, "currency": currency, "period_end": format_date(last), "date": format_date(last) }),
        )?;
        made += 1;
    }
    Ok(made)
}

/// The nightly run: this month's premiums exist from the first night of the month.
pub fn generate_current() -> Result<i64> {
    generate(None)
}

/// Payroll approved a run: premiums scheduled for periods ending on or before the run's end are deducted.
fn on_payroll(received: Received) -> Result<Value> {
    let end = received.payload.get("period_end").and_then(Value::as_str).ok_or_else(|| Error::msg("the event has no period_end"))?.to_string();
    let rows: Vec<Record> = db::find("ins_premium").filter("state", "scheduled").limit(20_000).all()?;
    let mut n = 0;
    for r in rows {
        if text(&r, "period_end").is_some_and(|p| p <= end.as_str()) {
            db::update::<Record>("ins_premium", id_of(&r)?, &json!({ "state": "deducted" }))?;
            n += 1;
        }
    }
    Ok(json!({ "deducted": n }))
}

handler! {
    fn generate_insurance_premiums(input: Month) -> Value {
        require_staff()?;
        Ok(json!({ "created": generate(input.month.as_deref())? }))
    }

    fn list_insurance_premiums(_: Empty) -> Vec<Record> {
        if is_staff()? {
            return db::find("ins_premium").limit(5000).all();
        }
        let Some(me) = my_employee_id()? else { return Ok(Vec::new()) };
        db::find("ins_premium").filter("employee", me.as_str()).limit(500).all()
    }

    fn on_payroll_run_approved(received: Received) -> Value {
        on_payroll(received)
    }
}
