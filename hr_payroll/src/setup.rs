//! Salary structures, who is on which, and one-off inputs for a run.

use aether_sdk::dates::{format_date, parse_date};
use aether_sdk::prelude::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use sp_engine::{Compiled, Kind, RulePack, Structure};

use crate::common::{id_of, require, require_admin, require_clerk, require_payroll_staff, text, Record};
use crate::rules::{assignments_overlap, FIXED_FOR_A_RUN, RESERVED_INPUTS};

#[derive(Deserialize)]
struct NewStructure {
    code: String,
    version: String,
    name: String,
    definition: Value,
    /// Rule packs (tax, social security) the structure uses as components.
    #[serde(default)]
    packs: Vec<PackUse>,
}

#[derive(Deserialize)]
struct PackUse {
    /// The pack's id, such as `gm.paye.2025`: one edition, so a payslip can be reproduced.
    pack: String,
    /// The component's id and label in the structure.
    id: String,
    label: String,
    /// `deduction`, `earning` or `employer`.
    kind: String,
    /// For each input of the pack, a formula of the structure that gives it (`taxable`, `total("pensionable")`).
    arguments: BTreeMap<String, String>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct ImportPack {
    description: Value,
    expression: String,
}

#[derive(Deserialize)]
struct Setting {
    allow_unverified_rules: bool,
}

#[derive(Deserialize)]
struct Assign {
    employee: String,
    /// The structure's code; its newest active version, or the one named in `version`.
    structure: String,
    #[serde(default)]
    version: Option<String>,
    valid_from: String,
    #[serde(default)]
    valid_to: Option<String>,
    #[serde(default)]
    variables: Option<serde_json::Map<String, Value>>,
}

#[derive(Deserialize)]
struct EndAssignment {
    id: String,
    valid_to: String,
}

#[derive(Deserialize)]
struct RunInput {
    run: String,
    employee: String,
    variables: serde_json::Map<String, Value>,
}

#[derive(Deserialize)]
struct Code {
    code: String,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize, Default)]
struct ForEmployee {
    #[serde(default)]
    employee: Option<String>,
}

fn sha_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn new_structure(input: NewStructure) -> Result<Record> {
    require_admin()?;
    let mut structure: Structure = serde_json::from_value(input.definition.clone()).map_err(|e| Error::msg(format!("the definition: {e}")))?;
    structure.id = input.code.clone();
    structure.version = input.version.clone();
    let mut used = Vec::new();
    for usage in &input.packs {
        let record = db::find::<Record>("pay_rule_pack").filter("pack_id", usage.pack.as_str()).filter("is_active", true).first()?.ok_or_else(|| Error::msg(format!("there is no rule pack `{}`", usage.pack)))?;
        let pack = parse_stored(&record)?;
        if pack.info.currency != structure.currency {
            return Err(Error::msg(format!("the rule pack `{}` is in {}, the structure in {}", usage.pack, pack.info.currency, structure.currency)));
        }
        let kind = match usage.kind.as_str() {
            "deduction" => Kind::Deduction,
            "earning" => Kind::Earning,
            "employer" => Kind::Employer,
            _ => return Err(Error::msg("a rule pack's component is a deduction, an earning or an employer cost")),
        };
        let (constants, mut component) = pack.as_component(&usage.id, &usage.label, kind, &usage.arguments).map_err(Error::msg)?;
        component.tags = usage.tags.clone();
        structure.constants.extend(constants);
        structure.components.push(component);
        used.push(json!({ "pack": usage.pack, "hash": record["hash"], "verified": record["verified"] }));
    }
    // Payslips keep two digits, so a structure that rounds to anything else cannot be stored faithfully.
    if structure.rounding.digits != 2 || structure.components.iter().any(|c| c.round.is_some_and(|r| r.digits != 2)) {
        return Err(Error::msg("every amount is rounded to two digits: a structure that rounds otherwise cannot be stored on a payslip"));
    }
    Compiled::new(&structure).map_err(|e| Error::msg(e.to_string()))?;
    let definition = serde_json::to_value(&structure).map_err(|e| Error::msg(e.to_string()))?;
    let hash = sha_hex(&definition.to_string());
    db::create("pay_structure", &json!({
        "code": input.code, "version": input.version, "name": input.name, "currency": structure.currency, "definition": definition, "hash": hash, "packs": used, "is_active": true,
    }))
    .map_err(|e| e.or("could not save the structure (this code and version may exist: a change is a new version)"))
}

fn parse_stored(record: &Record) -> Result<RulePack> {
    RulePack::parse(&record["description"].to_string(), text(record, "expression").unwrap_or_default()).map_err(Error::msg)
}

fn import_pack(input: ImportPack) -> Result<Record> {
    require_admin()?;
    let description = input.description.to_string();
    let pack = RulePack::parse(&description, &input.expression).map_err(Error::msg)?;
    let results = pack.run_tests();
    if results.is_empty() {
        return Err(Error::msg("a rule pack comes with tests"));
    }
    let failures: Vec<String> = results.iter().filter_map(|(name, r)| r.as_ref().err().map(|e| format!("{name}: {e}"))).collect();
    if !failures.is_empty() {
        return Err(Error::msg(format!("the pack's own tests fail: {}", failures.join("; "))));
    }
    let info = &pack.info;
    db::create(
        "pay_rule_pack",
        &json!({
            "pack_id": info.id, "country": info.country, "kind": info.kind, "currency": info.currency, "valid_from": info.valid_from, "valid_to": info.valid_to,
            "source": info.source, "verified": info.verified, "description": input.description, "expression": input.expression,
            "hash": sha_hex(&format!("{description}\n{}", input.expression)), "tests": results.len(), "is_active": true,
        }),
    )
    .map_err(|e| e.or("could not save the pack (this id may exist: an edition is never changed, a new edition has a new id)"))
}

/// The rule packs a stored structure uses, with whether each was verified.
pub fn packs_of(code: &str, version: &str) -> Result<Vec<Value>> {
    let record = db::find::<Record>("pay_structure").filter("code", code).filter("version", version).first()?.ok_or_else(|| Error::msg(format!("there is no structure `{code}` {version}")))?;
    Ok(record["packs"].as_array().cloned().unwrap_or_default())
}

pub fn unverified_allowed() -> Result<bool> {
    Ok(db::find::<Record>("pay_setting").first()?.is_some_and(|s| s.get("allow_unverified_rules") == Some(&json!(true))))
}

fn set_setting(input: Setting) -> Result<Record> {
    require_admin()?;
    match db::find::<Record>("pay_setting").first()? {
        Some(existing) => db::update("pay_setting", id_of(&existing)?, &json!({ "allow_unverified_rules": input.allow_unverified_rules }))?.ok_or_else(|| Error::msg("the setting is gone")),
        None => db::create("pay_setting", &json!({ "allow_unverified_rules": input.allow_unverified_rules })),
    }
}

/// The newest active version of a structure, or the one asked for.
pub fn find_structure(code: &str, version: Option<&str>) -> Result<Record> {
    let mut find = db::find::<Record>("pay_structure").filter("code", code).filter("is_active", true).order_by("-version").limit(50);
    if let Some(version) = version {
        find = find.filter("version", version);
    }
    find.first()?.ok_or_else(|| Error::msg(format!("there is no active structure `{code}`")))
}

fn assign(input: Assign) -> Result<Record> {
    require_admin()?;
    let found: Option<Record> = plugins::call("hr", "get_employee", &json!({ "id": input.employee }))?;
    if found.is_none() {
        return Err(Error::msg("there is no such employee"));
    }
    let structure = find_structure(&input.structure, input.version.as_deref())?;
    let (from, to) = (parse_date(&input.valid_from)?, input.valid_to.as_deref().map(parse_date).transpose()?);
    if to.is_some_and(|t| t < from) {
        return Err(Error::msg("the assignment ends after it starts"));
    }
    for other in db::find::<Record>("pay_assignment").filter("employee", input.employee.as_str()).limit(200).all()? {
        let (of, ot) = (parse_date(text(&other, "valid_from").unwrap_or_default())?, text(&other, "valid_to").map(parse_date).transpose()?);
        if assignments_overlap(from, to, of, ot) {
            return Err(Error::msg("this person already has a salary structure in that time: end it first"));
        }
    }
    let declared: Vec<String> = structure["definition"]["inputs"].as_array().map(|a| a.iter().filter_map(|i| i["name"].as_str().map(str::to_string)).collect()).unwrap_or_default();
    let variables = input.variables.unwrap_or_default();
    for name in variables.keys() {
        if RESERVED_INPUTS.contains(&name.as_str()) {
            return Err(Error::msg(format!("`{name}` is filled in by payroll, not set on a person")));
        }
        if !declared.contains(name) {
            return Err(Error::msg(format!("`{name}` is not an input of the structure `{}`", input.structure)));
        }
    }
    let mut data = json!({ "employee": input.employee, "structure": id_of(&structure)?, "valid_from": format_date(from), "variables": Value::Object(variables) });
    if let Some(to) = to {
        data["valid_to"] = json!(format_date(to));
    }
    db::create("pay_assignment", &data)
}

fn end_assignment(input: EndAssignment) -> Result<Record> {
    require_admin()?;
    let assignment = require("pay_assignment", &input.id, "assignment")?;
    let to = parse_date(&input.valid_to)?;
    if to < parse_date(text(&assignment, "valid_from").unwrap_or_default())? {
        return Err(Error::msg("it cannot end before it starts"));
    }
    db::update("pay_assignment", &input.id, &json!({ "valid_to": format_date(to) }))?.ok_or_else(|| Error::msg("the assignment is gone"))
}

fn set_input(input: RunInput) -> Result<Record> {
    require_clerk()?;
    let run = require("pay_run", &input.run, "run")?;
    if !matches!(text(&run, "status"), Some("draft" | "calculated")) {
        return Err(Error::msg("inputs change while the run is a draft or calculated, not while it is being calculated or after approval"));
    }
    for name in input.variables.keys() {
        if FIXED_FOR_A_RUN.contains(&name.as_str()) {
            return Err(Error::msg(format!("`{name}` is filled in by payroll and cannot be changed for one run")));
        }
    }
    match db::find::<Record>("pay_input").filter("run", input.run.as_str()).filter("employee", input.employee.as_str()).first()? {
        Some(existing) => {
            let mut merged = existing["variables"].as_object().cloned().unwrap_or_default();
            merged.extend(input.variables);
            db::update("pay_input", id_of(&existing)?, &json!({ "variables": Value::Object(merged) }))?.ok_or_else(|| Error::msg("the input is gone"))
        }
        None => db::create("pay_input", &json!({ "run": input.run, "employee": input.employee, "variables": Value::Object(input.variables) })),
    }
}

handler! {
    /// Save a salary structure. A structure is never changed: a new version is a new one.
    fn create_structure(input: NewStructure) -> Record {
        new_structure(input)
    }

    /// Add a rule pack (its description and its expression). Its own tests must pass; an edition is never changed.
    fn import_rule_pack(input: ImportPack) -> Record {
        import_pack(input)
    }

    fn list_rule_packs(_: Empty) -> Vec<Record> {
        require_payroll_staff()?;
        db::find("pay_rule_pack").order_by("pack_id").limit(200).all()
    }

    fn retire_rule_pack(input: Id) -> Record {
        require_admin()?;
        db::update("pay_rule_pack", &input.id, &json!({ "is_active": false }))?.ok_or_else(|| Error::msg("there is no such pack"))
    }

    fn set_payroll_setting(input: Setting) -> Record {
        set_setting(input)
    }

    fn get_payroll_setting(_: Empty) -> Value {
        require_payroll_staff()?;
        Ok(json!({ "allow_unverified_rules": unverified_allowed()? }))
    }

    fn retire_structure(input: Id) -> Record {
        require_admin()?;
        db::update("pay_structure", &input.id, &json!({ "is_active": false }))?.ok_or_else(|| Error::msg("there is no such structure"))
    }

    fn list_structures(_: Empty) -> Vec<Record> {
        require_payroll_staff()?;
        db::find("pay_structure").order_by("code").limit(200).all()
    }

    fn get_structure(input: Code) -> Record {
        require_payroll_staff()?;
        find_structure(&input.code, None)
    }

    fn assign_structure(input: Assign) -> Record {
        assign(input)
    }

    fn end_structure_assignment(input: EndAssignment) -> Record {
        end_assignment(input)
    }

    fn list_assignments(input: Option<ForEmployee>) -> Vec<Record> {
        require_payroll_staff()?;
        let mut find = db::find::<Record>("pay_assignment").order_by("valid_from").limit(500);
        if let Some(employee) = input.and_then(|i| i.employee) {
            find = find.filter("employee", employee.as_str());
        }
        find.all()
    }

    /// One-off figures for one person in one run (overtime, a bonus, unpaid days).
    fn set_run_input(input: RunInput) -> Record {
        set_input(input)
    }
}
