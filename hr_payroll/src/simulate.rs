//! What a salary package comes to under a structure, without a person or a run: for offers and for asking
//! "what would a wage of X take home". Nothing is stored. Inputs the structure declares and the call does not
//! give are zero, so a result is as complete as the inputs the caller supplies.

use std::collections::BTreeMap;

use aether_sdk::prelude::*;
use sp_engine::{Compiled, Input as PayInput, Structure};

use crate::common::{money, require_payroll_staff};

#[derive(Deserialize)]
struct Simulate {
    structure: String,
    #[serde(default)]
    version: Option<String>,
    /// The wage for one period, as text.
    wage: String,
    /// Further inputs the structure declares (an allowance, hours), by name.
    #[serde(default)]
    inputs: BTreeMap<String, Value>,
    #[serde(default)]
    period_days: Option<i64>,
}

fn simulate(input: Simulate) -> Result<Value> {
    let who = context::current()?;
    if require_payroll_staff().is_err() && !who.has_role("hr_recruitment.recruit_manager") && !who.has_role("hr.hr_manager") {
        return Err(Error::msg("this is for payroll staff, recruitment managers and HR managers"));
    }
    aether_sdk::decimal::Decimal::parse(&input.wage).map_err(|_| Error::msg("the wage is a number"))?;
    let record = crate::setup::find_structure(&input.structure, input.version.as_deref())?;
    let definition: Structure = serde_json::from_value(record["definition"].clone()).map_err(|e| Error::msg(format!("structure: {e}")))?;
    let compiled = Compiled::new(&definition).map_err(|e| Error::msg(e.to_string()))?;
    let days = input.period_days.unwrap_or(30).clamp(1, 366);
    let periods = crate::setup::periods_per_year()?;
    let mut variables: BTreeMap<String, Value> = BTreeMap::new();
    for declared in compiled.structure().inputs.iter().map(|i| i.name.as_str()) {
        let value = match declared {
            "base_salary" => serde_json::from_str(&input.wage).map_err(|_| Error::msg("the wage is a number"))?,
            "period_days" | "days_worked" => json!(days),
            "periods_per_year" => json!(periods),
            other => input.inputs.get(other).cloned().unwrap_or(json!(0)),
        };
        variables.insert(declared.to_string(), value);
    }
    let slip = compiled
        .run(&PayInput { employee_id: "simulation".into(), variables }, false)
        .map_err(|e| Error::msg(e.to_string()))?;
    let lines: Vec<Value> = slip
        .lines
        .iter()
        .map(|l| {
            let kind = match l.kind {
                sp_engine::Kind::Earning => "earning",
                sp_engine::Kind::Deduction => "deduction",
                sp_engine::Kind::Employer => "employer",
                sp_engine::Kind::Info => "info",
            };
            json!({ "id": l.id, "label": l.label, "kind": kind, "amount": l.amount.to_string() })
        })
        .collect();
    Ok(json!({
        "structure": record["code"], "version": record["version"], "currency": record["currency"],
        "gross": money(&slip.gross.to_string())?.to_string(), "deductions": money(&slip.deductions.to_string())?.to_string(),
        "net": money(&slip.net.to_string())?.to_string(), "employer_cost": money(&slip.employer_cost.to_string())?.to_string(),
        "lines": lines, "assumed": { "period_days": days, "periods_per_year": periods },
    }))
}

handler! {
    /// Net pay and employer cost of a wage under a structure; nothing is saved.
    fn simulate_package(input: Simulate) -> Value {
        simulate(input)
    }
}
