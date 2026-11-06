//! Calculating a batch's payslips: by the kernel when it can, in this call otherwise.
//!
//! The kernel command `payroll::calculate` runs the same engine natively, on every core, keeps the compiled
//! structure between calls and spends none of this call's fuel on it, so it is used whenever the kernel has it
//! and the plugin holds the capability. A person is calculated here instead when one of their figures has more
//! digits than the kernel's JSON keeps exactly (an f64: 15 significant digits), and everyone is when the kernel
//! is older or the capability is missing. Either way the numbers are the engine's, to the last digit.

use std::collections::BTreeMap;

use aether_sdk::prelude::*;
use sp_engine::{Compiled, Decimal, Input as PayInput, Structure};

use crate::common::{text, Record};
use crate::rules::lines_text;

/// A person ready to calculate.
pub struct Pending {
    pub id: String,
    /// The id of their structure (a `pay_structure` record).
    pub structure: String,
    /// Their display name, for the payslip.
    pub name: Value,
    pub variables: BTreeMap<String, Value>,
}

/// What a person's payslip came to: every amount as the engine's exact text.
pub struct Slip {
    pub gross: String,
    pub deductions: String,
    pub net: String,
    pub employer_cost: String,
    pub shortfall: String,
    /// Every component's amount, in the structure's order, as one comma-joined string (see `rules::lines_text`);
    /// `null` when the kernel keeps them for `payroll::write` (see [`Calculated::ticket`]).
    pub lines: Value,
}

pub enum Outcome {
    Done(Slip),
    Failed(String),
}

pub struct Calculated {
    /// Everyone, in the order given, with what became of them.
    pub people: Vec<(Pending, Outcome)>,
    /// By structure id: each component's sum over the people calculated with it.
    pub sums: BTreeMap<String, Vec<Decimal>>,
    /// How many were calculated by the kernel.
    pub by_kernel: usize,
    /// The kernel kept the lines of the people it calculated: write their payslips with `payroll::write` and
    /// this ticket (see [`write_ops`]), so the lines never pass through this call.
    pub ticket: Option<String>,
}

/// Write a batch's rows: `ops` as one transaction, through the kernel when it kept lines (`ticket`). `kept[i]`
/// says whether the i-th op is a payslip whose lines the kernel holds.
pub fn write_ops(ticket: Option<&str>, mut ops: Vec<Value>, kept: &[Option<String>]) -> Result<()> {
    match ticket {
        Some(ticket) => {
            for (op, person) in ops.iter_mut().zip(kept) {
                if let Some(person) = person {
                    op["lines_of"] = json!(person);
                }
            }
            aether_sdk::transport::call("payroll::write", &json!({ "ticket": ticket, "lines_field": "lines", "ops": ops }))?;
            Ok(())
        }
        None => {
            aether_sdk::transport::call("db::transaction", &json!({ "ops": ops, "quiet": true }))?;
            Ok(())
        }
    }
}

/// What a pay run needs of a structure's definition, kept beside it (`outline`) so a batch reads a few kilobytes
/// instead of the whole definition (1.3 MB for 10,000 rules): the names of its inputs, and each component as
/// `id:kind`, comma-joined, in order.
pub fn outline_of(definition: &Value) -> Value {
    let inputs: Vec<&str> = definition["inputs"].as_array().into_iter().flatten().filter_map(|i| i["name"].as_str()).collect();
    let components: Vec<String> = definition["components"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| format!("{}:{}", c["id"].as_str().unwrap_or_default(), c["kind"].as_str().unwrap_or("info")))
        .collect();
    json!({ "inputs": inputs, "components": components.join(",") })
}

/// The names of the inputs a stored structure declares (from its outline, or its definition if it has none).
pub fn input_names(record: &Record) -> Box<dyn Iterator<Item = &str> + '_> {
    match record["outline"]["inputs"].as_array() {
        Some(inputs) => Box::new(inputs.iter().filter_map(Value::as_str)),
        None => Box::new(record["definition"]["inputs"].as_array().into_iter().flatten().filter_map(|i| i["name"].as_str())),
    }
}

/// Each component of a stored structure: its id and kind, in order.
pub fn component_kinds(record: &Record) -> Vec<(String, String)> {
    if let Some(components) = record["outline"]["components"].as_str() {
        return components
            .split(',')
            .filter(|c| !c.is_empty())
            .map(|c| {
                let (id, kind) = c.rsplit_once(':').unwrap_or((c, "info"));
                (id.to_string(), kind.to_string())
            })
            .collect();
    }
    record["definition"]["components"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| (c["id"].as_str().unwrap_or_default().to_string(), c["kind"].as_str().unwrap_or("info").to_string()))
        .collect()
}

/// A stored structure for a batch: without its definition and its components when it has an outline (a batch
/// needs only the names of its inputs; the definition is read only if it is needed, see [`definition_of`]),
/// whole otherwise.
pub fn structure_for_run(id: &str) -> Result<Record> {
    structure_without(id, &["definition", "outline.components"])
}

/// A stored structure with what finalising a run needs: the kind of each component, from its outline.
pub fn structure_kinds(id: &str) -> Result<Record> {
    structure_without(id, &["definition"])
}

fn structure_without(id: &str, omit: &[&str]) -> Result<Record> {
    if let Some(record) = db::get_omitting::<Record>("pay_structure", id, omit)? {
        if record["outline"]["inputs"].is_array() {
            return Ok(record);
        }
    }
    crate::common::require("pay_structure", id, "structure")
}

/// A structure's definition: the one the record holds, or read now.
fn definition_of(id: &str, record: &Record) -> Result<Value> {
    if !record["definition"].is_null() {
        return Ok(record["definition"].clone());
    }
    Ok(crate::common::require("pay_structure", id, "structure")?["definition"].take())
}

/// The kernel's name for a structure: its hash (of the whole definition), so a structure that changed is never
/// confused with what the kernel kept of it, and the one checked when it was made is the one a run finds kept.
fn kernel_key(structure_id: &str, record: &Record) -> String {
    match text(record, "hash") {
        Some(hash) => format!("sha256:{hash}"),
        None => format!("{structure_id}#{}", text(record, "version").unwrap_or_default()),
    }
}

/// Check a new structure in the kernel (it compiles it natively, and keeps it for the runs to come). `Ok(false)`
/// when the kernel cannot (older, or the capability is missing): the caller compiles it itself.
pub fn check_in_kernel(hash: &str, definition: &Value) -> Result<bool> {
    let mut structures = serde_json::Map::new();
    structures.insert(format!("sha256:{hash}"), definition.clone());
    match aether_sdk::transport::call("payroll::calculate", &json!({ "structures": structures, "people": [] })) {
        Ok(_) => Ok(true),
        Err(Error::UnknownCommand(_) | Error::Capability(_)) => Ok(false),
        Err(error) => Err(error),
    }
}

fn decimal(text: &str) -> Option<Decimal> {
    Decimal::from_str_exact(text).or_else(|_| Decimal::from_scientific(text)).ok()
}

/// Whether every number in `value` comes through an f64 (the kernel's JSON) with the same value.
pub fn f64_exact(value: &Value) -> bool {
    match value {
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                return true;
            }
            let written = n.to_string();
            // At most 15 significant digits and a modest exponent always come back through an f64 the same (what
            // DBL_DIG promises): the common case, decided without formatting and parsing the number twice.
            if few_digits(&written) {
                return true;
            }
            match (n.as_f64(), decimal(&written)) {
                (Some(f), Some(exact)) => f.is_finite() && decimal(&format!("{f}")) == Some(exact),
                // An integer serde_json holds as one is exact either way.
                _ => n.is_i64() || n.is_u64(),
            }
        }
        Value::Array(items) => items.iter().all(f64_exact),
        Value::Object(map) => map.values().all(f64_exact),
        _ => true,
    }
}

/// Whether a JSON number's text has at most 15 significant digits and an exponent within ±300.
fn few_digits(text: &str) -> bool {
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(at) => (&text[..at], text[at + 1..].parse::<i32>().ok()),
        None => (text, Some(0)),
    };
    let Some(exponent) = exponent else { return false };
    let digits = mantissa.trim_start_matches('-').bytes().filter(u8::is_ascii_digit).skip_while(|d| *d == b'0').count();
    digits <= 15 && exponent.abs() <= 300 && mantissa.bytes().all(|b| b.is_ascii_digit() || b == b'.' || b == b'-')
}

/// Whether the kernel calculates for this plugin (a cheap call with nobody in it).
pub fn kernel_calculates() -> bool {
    aether_sdk::transport::call("payroll::calculate", &json!({ "people": [] })).is_ok()
}

/// Calculate everyone in `pending`.
pub fn calculate(structures: &BTreeMap<String, Record>, pending: Vec<Pending>) -> Result<Calculated> {
    let mut people: Vec<(Pending, Option<Outcome>)> = pending.into_iter().map(|p| (p, None)).collect();
    let mut sums: BTreeMap<String, Vec<Decimal>> = BTreeMap::new();
    let mut by_kernel = 0;
    let mut ticket = None;

    // 1. The kernel, for everyone whose figures it keeps exactly.
    let sent: Vec<usize> = (0..people.len()).filter(|&i| people[i].0.variables.values().all(f64_exact)).collect();
    if !sent.is_empty() {
        let mut keys: BTreeMap<String, String> = BTreeMap::new();
        let mut records = serde_json::Map::new();
        let mut request_people = Vec::with_capacity(sent.len());
        for &i in &sent {
            let person = &mut people[i].0;
            let key = kernel_key(&person.structure, &structures[&person.structure]);
            records.insert(key.clone(), json!({ "model": "pay_structure", "id": person.structure, "field": "definition" }));
            keys.insert(key.clone(), person.structure.clone());
            // Moved into the request and back out of it after: a batch's figures are not copied.
            request_people.push(json!({ "id": person.id, "structure": key, "variables": std::mem::take(&mut person.variables) }));
        }
        // The kernel reads a structure it does not hold from its record itself (`structure_records`), and keeps the
        // payslip lines (`keep`) for `payroll::write`: neither a definition nor a line passes through this call.
        let mut request = json!({
            "structures": keys.keys().map(|k| (k.clone(), Value::Null)).collect::<serde_json::Map<_, _>>(),
            "structure_records": records, "people": request_people, "lines": "keep",
        });
        let mut answer = None;
        // The kernel may have forgotten a structure (a restart, or many in use): then it says which, once.
        for _ in 0..2 {
            match aether_sdk::transport::call("payroll::calculate", &request) {
                Ok(mut reply) => {
                    let data = reply["data"].take();
                    if let Some(missing) = data["missing"].as_array() {
                        for key in missing.iter().filter_map(Value::as_str) {
                            let id = keys.get(key).ok_or_else(|| Error::msg(format!("the kernel asked for a structure it was not given: {key}")))?;
                            request["structures"][key] = definition_of(id, &structures[id.as_str()])?;
                        }
                        continue;
                    }
                    answer = Some(data);
                    break;
                }
                // An older kernel, or the capability is not granted: calculated here below.
                Err(Error::UnknownCommand(_) | Error::Capability(_)) => break,
                Err(error) => return Err(error),
            }
        }
        // The figures go back to their people, whatever happened.
        let returned = request["people"].as_array_mut().map(std::mem::take).unwrap_or_default();
        for (&i, mut sent_person) in sent.iter().zip(returned) {
            if let Value::Object(variables) = sent_person["variables"].take() {
                people[i].0.variables = variables.into_iter().collect();
            }
        }
        if let Some(mut data) = answer {
            ticket = data["ticket"].as_str().map(str::to_string);
            let results = data["results"].as_array_mut().map(std::mem::take).unwrap_or_default();
            if results.len() != sent.len() {
                return Err(Error::msg(format!("the kernel calculated {} of {} people", results.len(), sent.len())));
            }
            for (&i, mut result) in sent.iter().zip(results) {
                let outcome = match result["error"].as_str() {
                    Some(message) => Outcome::Failed(message.to_string()),
                    None => {
                        let field = |name: &str| result[name].as_str().unwrap_or("0").to_string();
                        Outcome::Done(Slip {
                            gross: field("gross"),
                            deductions: field("deductions"),
                            net: field("net"),
                            employer_cost: field("employer_cost"),
                            shortfall: field("shortfall"),
                            lines: result["lines"].take(),
                        })
                    }
                };
                people[i].1 = Some(outcome);
                by_kernel += 1;
            }
            for (key, totals) in data["sums"].as_object().into_iter().flatten() {
                let Some(id) = keys.get(key.as_str()) else { continue };
                let parsed: Vec<Decimal> = totals.as_array().into_iter().flatten().map(|t| t.as_str().and_then(decimal).unwrap_or_default()).collect();
                add_into(sums.entry(id.clone()).or_default(), &parsed);
            }
        }
    }

    // 2. Here, for everyone else.
    let mut compiled: BTreeMap<String, Compiled> = BTreeMap::new();
    for (person, outcome) in people.iter_mut().filter(|(_, o)| o.is_none()) {
        if !compiled.contains_key(&person.structure) {
            let definition: Structure = serde_json::from_value(definition_of(&person.structure, &structures[&person.structure])?).map_err(|e| Error::msg(format!("structure {}: {e}", person.structure)))?;
            compiled.insert(person.structure.clone(), Compiled::new(&definition).map_err(|e| Error::msg(format!("structure {}: {e}", person.structure)))?);
        }
        let input = PayInput { employee_id: person.id.clone(), variables: std::mem::take(&mut person.variables) };
        *outcome = Some(match compiled[&person.structure].calculate_amounts(&input) {
            Err(error) => Outcome::Failed(error.to_string()),
            Ok(calc) => {
                add_into(sums.entry(person.structure.clone()).or_default(), &calc.amounts);
                let s = &calc.summary;
                Outcome::Done(Slip {
                    gross: s.gross.to_string(),
                    deductions: s.deductions.to_string(),
                    net: s.net.to_string(),
                    employer_cost: s.employer_cost.to_string(),
                    shortfall: s.shortfall.to_string(),
                    lines: Value::String(lines_text(&calc.amounts)),
                })
            }
        });
        person.variables = input.variables;
    }
    let people = people.into_iter().map(|(p, o)| (p, o.unwrap_or_else(|| Outcome::Failed("not calculated".into())))).collect();
    Ok(Calculated { people, sums, by_kernel, ticket })
}

fn add_into(total: &mut Vec<Decimal>, amounts: &[Decimal]) {
    if total.len() < amounts.len() {
        total.resize(amounts.len(), Decimal::ZERO);
    }
    for (sum, amount) in total.iter_mut().zip(amounts) {
        *sum += *amount;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn figures_an_f64_keeps_go_to_the_kernel_and_others_do_not() {
        for ok in ["0", "1", "4200.50", "0.075", "12345678901.23", "-3.5", "1e3", "100000000000000000000"] {
            assert!(f64_exact(&n(ok)), "{ok}");
        }
        assert!(few_digits("4200.50") && few_digits("-0.000123") && few_digits("1.5e10") && !few_digits("1234567890123456.7"));
        assert!(f64_exact(&n("123456789012345.6")) && f64_exact(&n("0.000000000000000000123")));
        for not in ["1234567890123456.78", "0.1000000000000000055511151231257827", "9007199254740993.5"] {
            assert!(!f64_exact(&n(not)), "{not}");
        }
        assert!(f64_exact(&json!({ "a": [1, "x", null], "b": { "c": 2.5 } })));
        assert!(!f64_exact(&json!({ "a": [n("1234567890123456.78")] })));
    }

    #[test]
    fn a_stored_structure_names_its_inputs_and_components() {
        let record = json!({ "definition": { "inputs": [{ "name": "base_salary" }, { "name": "adj_bonus" }], "components": [{ "id": "basic", "kind": "earning" }, { "id": "paye", "kind": "deduction" }] } });
        assert_eq!(input_names(&record).collect::<Vec<_>>(), ["base_salary", "adj_bonus"]);
        assert_eq!(component_kinds(&record), [("basic".to_string(), "earning".to_string()), ("paye".to_string(), "deduction".to_string())]);
        assert_eq!(kernel_key("pay_structure:a", &json!({ "hash": "h1", "version": "2" })), "sha256:h1");
        assert_eq!(kernel_key("pay_structure:a", &json!({ "version": "2" })), "pay_structure:a#2");
        // An outline says the same, without the definition.
        let outline = json!({ "outline": outline_of(&record["definition"]) });
        assert_eq!(outline["outline"]["components"], "basic:earning,paye:deduction");
        assert_eq!(input_names(&outline).collect::<Vec<_>>(), ["base_salary", "adj_bonus"]);
        assert_eq!(component_kinds(&outline), component_kinds(&record));
    }
}
