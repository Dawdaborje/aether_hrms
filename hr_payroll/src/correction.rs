//! Corrections: a run that recalculates chosen people of an approved run, for the same period, and a
//! summary of what changed for each. The engine cannot say "only the difference", so the correction
//! run holds full payslips and the difference is read from the two runs; finance pays that difference.

use aether_sdk::prelude::*;

use crate::common::{id_of, require, require_clerk, require_payroll_staff, text, Record};

#[derive(Deserialize)]
struct Correct {
    run: String,
    #[serde(default)]
    employees: Vec<String>,
    /// Form fields for people one at a time, added to `employees`.
    #[serde(default)]
    employee_1: Option<String>,
    #[serde(default)]
    employee_2: Option<String>,
    #[serde(default)]
    employee_3: Option<String>,
    #[serde(default)]
    employee_4: Option<String>,
    #[serde(default)]
    employee_5: Option<String>,
    reason: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct Summary {
    #[serde(alias = "id")]
    run: String,
    #[serde(default)]
    format: Option<String>,
}

fn create(mut input: Correct) -> Result<Record> {
    require_clerk()?;
    let extra = [input.employee_1.take(), input.employee_2.take(), input.employee_3.take(), input.employee_4.take(), input.employee_5.take()];
    input.employees.extend(extra.into_iter().flatten().filter(|e| !e.trim().is_empty()));
    let original = require("pay_run", &input.run, "run")?;
    if !matches!(text(&original, "status"), Some("approved" | "paid")) {
        return Err(Error::msg("only an approved or paid run is corrected: change a draft run and recalculate it instead"));
    }
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why the run is corrected"));
    }
    if input.employees.is_empty() || input.employees.len() > 100 {
        return Err(Error::msg("a correction names 1 to 100 people"));
    }
    let name = input.name.clone().unwrap_or_else(|| format!("Correction of {}", text(&original, "number").unwrap_or("run")));
    let data = json!({
        "number": aether_sdk::records::next_number("payrun", "PR-", 5)?, "name": name,
        "period_start": original["period_start"], "period_end": original["period_end"],
        "status": "draft", "batch_size": original.get("batch_size").cloned().unwrap_or(json!(20)), "attempt": 0,
        "correction_of": id_of(&original)?, "only_employees": input.employees, "correction_reason": input.reason.trim(),
    });
    db::create("pay_run", &data)
}

fn slips(run: &Record) -> Result<Vec<Record>> {
    let attempt = run.get("attempt").and_then(Value::as_i64).unwrap_or(0);
    db::find::<Record>("payslip").filter("run", id_of(run)?).filter("attempt", attempt).order_by("employee").limit(1000).all()
}

fn net_of(slip: &Record) -> Result<aether_sdk::decimal::Decimal> {
    let raw = slip.get("net").map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).unwrap_or_else(|| "0".into());
    aether_sdk::decimal::Decimal::parse(raw.trim_matches('"'))
}

/// Per person: the net pay in the original run, in the correction, and the difference.
fn difference(run_id: &str) -> Result<Vec<Value>> {
    require_payroll_staff()?;
    let correction = require("pay_run", run_id, "run")?;
    let Some(original_id) = text(&correction, "correction_of") else { return Err(Error::msg("this is not a correction run")) };
    let original = require("pay_run", original_id, "original run")?;
    let before = slips(&original)?;
    let mut rows = Vec::new();
    for slip in slips(&correction)? {
        let who = text(&slip, "employee").unwrap_or_default();
        let old = before.iter().find(|b| text(b, "employee") == Some(who));
        let new_net = net_of(&slip)?;
        let old_net = match old {
            Some(o) => net_of(o)?,
            None => aether_sdk::decimal::Decimal::zero(2),
        };
        let diff = new_net.checked_sub(old_net)?;
        rows.push(json!({
            "employee": who, "employee_name": slip.get("employee_name").cloned().unwrap_or(Value::Null),
            "original_net": old_net.to_string(), "corrected_net": new_net.to_string(), "difference": diff.to_string(),
            "currency": slip.get("currency").cloned().unwrap_or(Value::Null),
        }));
    }
    Ok(rows)
}

/// For a correction run: the sum of what each person is owed (+) or must return (-) against the original.
/// `None` for an ordinary run.
pub(crate) fn net_difference(run_id: &str) -> Result<Option<String>> {
    let run = require("pay_run", run_id, "run")?;
    if text(&run, "correction_of").is_none() {
        return Ok(None);
    }
    let mut sum = aether_sdk::decimal::Decimal::zero(2);
    for row in difference(run_id)? {
        let part = aether_sdk::decimal::Decimal::parse(row["difference"].as_str().unwrap_or("0"))?;
        sum = sum.checked_add(part)?;
    }
    Ok(Some(sum.to_string()))
}

fn export(input: Summary) -> Result<Value> {
    let rows = difference(&input.run)?;
    let format = if input.format.as_deref() == Some("pdf") { "pdf" } else { "xlsx" };
    report::export(&json!({
        "format": format,
        "title": "Payroll correction",
        "columns": [
            { "field": "employee_name", "label": "Employee" },
            { "field": "original_net", "label": "Original net" },
            { "field": "corrected_net", "label": "Corrected net" },
            { "field": "difference", "label": "To pay (+) or recover (-)" },
            { "field": "currency", "label": "Currency" },
        ],
        "rows": rows,
    }))
}

handler! {
    /// A correction run for chosen people of an approved or paid run (same period; closed periods allowed).
    fn create_correction_run(input: Correct) -> Record {
        create(input)
    }

    /// What changed per person between the original run and its correction.
    fn correction_difference(input: Summary) -> Vec<Value> {
        difference(&input.run)
    }

    fn export_correction_difference(input: Summary) -> Value {
        export(input)
    }
}
