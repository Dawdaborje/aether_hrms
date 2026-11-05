//! Explaining a payslip: for each line, the formula and the values it read. The payslip keeps the inputs the
//! structure was given, and the structure is immutable, so the calculation is repeated with explanations
//! switched on. If the repeated net does not match the stored one, the answer says so instead of explaining
//! something else.

use std::collections::BTreeMap;

use aether_sdk::prelude::*;
use sp_engine::{Compiled, Input as PayInput, Structure};

use crate::common::{money, require, require_payroll_staff, text, Record};

#[derive(Deserialize)]
struct Id {
    id: String,
}

fn explain(input: Id) -> Result<Value> {
    let slip = require("payslip", &input.id, "payslip")?;
    if require_payroll_staff().is_err() {
        let mine = crate::run::my_payslip_ids()?;
        let id = text(&slip, "id").unwrap_or_default();
        if !mine.iter().any(|m| m == id || m.ends_with(id) || id.ends_with(m.as_str())) {
            return Err(Error::msg("this is not your payslip"));
        }
    }
    let Some(stored_inputs) = slip.get("inputs").and_then(Value::as_object).filter(|m| !m.is_empty()) else {
        return Err(Error::msg("this payslip was calculated before explanations were kept: recalculate the run to explain it"));
    };
    let structure = db::find::<Record>("pay_structure")
        .filter("code", text(&slip, "structure_code").unwrap_or_default())
        .filter("version", text(&slip, "structure_version").unwrap_or_default())
        .first()?
        .ok_or_else(|| Error::msg("the structure of this payslip is gone"))?;
    let definition: Structure = serde_json::from_value(structure["definition"].clone()).map_err(|e| Error::msg(format!("structure: {e}")))?;
    let compiled = Compiled::new(&definition).map_err(|e| Error::msg(e.to_string()))?;
    let variables: BTreeMap<String, Value> = stored_inputs.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let again = compiled
        .run(&PayInput { employee_id: text(&slip, "employee").unwrap_or_default().to_string(), variables: variables.clone() }, true)
        .map_err(|e| Error::msg(e.to_string()))?;
    let net_again = money(&again.net.to_string())?.to_string();
    let net_stored = match slip.get("net") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    let consistent = money(&net_stored).map(|m| m.to_string()).ok().as_deref() == Some(net_again.as_str());
    let lines: Vec<Value> = again
        .lines
        .iter()
        .filter(|l| l.applied || l.explain.is_some())
        .map(|l| {
            let kind = match l.kind {
                sp_engine::Kind::Earning => "earning",
                sp_engine::Kind::Deduction => "deduction",
                sp_engine::Kind::Employer => "employer",
                sp_engine::Kind::Info => "info",
            };
            let (formula, reads) = match &l.explain {
                Some(e) => (e.source.clone(), e.reads.clone()),
                None => (String::new(), Vec::new()),
            };
            // The value of each name the line read, where it was an input of this payslip.
            let values: serde_json::Map<String, Value> = reads.iter().filter_map(|r| variables.get(r).map(|v| (r.clone(), v.clone()))).collect();
            json!({
                "id": l.id, "label": l.label, "kind": kind, "amount": l.amount.to_string(), "applied": l.applied,
                "formula": formula, "reads": reads, "input_values": values,
            })
        })
        .collect();
    Ok(json!({
        "employee": slip.get("employee_name"), "structure": slip.get("structure_code"), "version": slip.get("structure_version"),
        "currency": slip.get("currency"), "net": net_stored, "recomputed_net": net_again, "consistent": consistent,
        "inputs": stored_inputs, "lines": lines,
    }))
}

handler! {
    /// The same as rows for a list: one per line, with the formula as text and the names read joined.
    fn explain_payslip_lines(input: Id) -> Vec<Value> {
        let all = explain(input)?;
        let mut rows = Vec::new();
        for l in all["lines"].as_array().cloned().unwrap_or_default() {
            let reads: Vec<String> = l["reads"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
            let shown: Vec<String> = reads
                .iter()
                .map(|r| match l["input_values"].get(r) {
                    Some(v) => format!("{r} = {}", v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())),
                    None => r.clone(),
                })
                .collect();
            rows.push(json!({
                "id": l["id"], "label": l["label"], "kind": l["kind"], "amount": l["amount"],
                "formula": l["formula"], "reads": shown.join("; "),
            }));
        }
        Ok(rows)
    }

    /// Each line of a payslip with the formula that gave it and the values it read.
    fn explain_payslip(input: Id) -> Value {
        explain(input)
    }
}
