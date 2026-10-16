//! The payroll library: salary components, payroll periods, tax slabs, tax exemptions, bulk assignment.
//!
//! A component is a reusable earning/deduction that `compose_structure` turns into a salary structure (still
//! immutable, still hashed); slabs become the constant tables the engine's `bracket()` reads; exemption
//! declarations feed the input `exemptions_annual` for structures that declare it.

use std::collections::BTreeMap;

use aether_sdk::dates::{format_date, parse_date, NaiveDate};
use aether_sdk::db::Filter;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{id_of, require, require_admin, require_payroll_staff, text, today_date, actor, Record};
use crate::setup::{assign, new_structure, Assign, NewStructure};

#[derive(Deserialize)]
struct Id {
    id: String,
}

fn code_ok(code: &str) -> Result<String> {
    let code = code.trim().to_lowercase();
    if code.is_empty() || code.len() > 40 || !code.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        return Err(Error::msg("a code is lower-case letters, digits and `_`, up to 40 characters"));
    }
    Ok(code)
}

// ---- components -------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct NewComponent {
    code: String,
    label: String,
    kind: String,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    formula: Option<String>,
    #[serde(default)]
    amount: Option<String>,
    #[serde(default)]
    input_name: Option<String>,
    #[serde(default)]
    when_expr: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default = "yes")]
    is_taxable: bool,
    #[serde(default)]
    pro_rata: bool,
    #[serde(default)]
    is_statistical: bool,
}

fn yes() -> bool {
    true
}

fn create_component(input: NewComponent) -> Result<Record> {
    require_admin()?;
    let code = code_ok(&input.code)?;
    if !matches!(input.kind.as_str(), "earning" | "deduction" | "employer" | "info") {
        return Err(Error::msg("a component is an earning, a deduction, an employer cost or a working figure (info)"));
    }
    let source = input.source.clone().unwrap_or_else(|| "formula".into());
    let ok = match source.as_str() {
        "formula" => input.formula.as_deref().is_some_and(|f| !f.trim().is_empty()),
        "input" => input.input_name.as_deref().is_some_and(|n| !n.trim().is_empty()),
        "amount" => input.amount.as_deref().is_some_and(|a| Decimal::parse(a).is_ok()),
        _ => return Err(Error::msg("the amount comes from a formula, an input or a fixed amount")),
    };
    if !ok {
        return Err(Error::msg(format!("a component whose amount comes from `{source}` needs that field filled in")));
    }
    if db::count("pay_component", Filter::eq("code", code.as_str()))? > 0 {
        return Err(Error::msg(format!("the component `{code}` exists")));
    }
    db::create(
        "pay_component",
        &json!({
            "code": code, "label": input.label, "kind": input.kind, "source": source, "formula": input.formula, "amount": input.amount,
            "input_name": input.input_name, "when_expr": input.when_expr, "tags": input.tags, "is_taxable": input.is_taxable,
            "pro_rata": input.pro_rata, "is_statistical": input.is_statistical, "is_active": true,
        }),
    )
}

#[derive(Deserialize)]
struct EditComponent {
    id: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    formula: Option<String>,
    #[serde(default)]
    amount: Option<String>,
    #[serde(default)]
    input_name: Option<String>,
    #[serde(default)]
    when_expr: Option<String>,
    #[serde(default)]
    is_taxable: Option<bool>,
    #[serde(default)]
    pro_rata: Option<bool>,
    #[serde(default)]
    is_active: Option<bool>,
}

/// Change a component. Structures already composed keep the text they were composed with; only
/// structures composed afterwards use the new one. The code and the kind do not change.
fn edit_component(input: EditComponent) -> Result<Record> {
    require_admin()?;
    let existing = require("pay_component", &input.id, "component")?;
    let source = input.source.clone().or_else(|| text(&existing, "source").map(str::to_string)).unwrap_or_else(|| "formula".into());
    let formula = input.formula.clone().or_else(|| text(&existing, "formula").map(str::to_string));
    let amount = input.amount.clone().or_else(|| text(&existing, "amount").map(str::to_string));
    let input_name = input.input_name.clone().or_else(|| text(&existing, "input_name").map(str::to_string));
    let ok = match source.as_str() {
        "formula" => formula.as_deref().is_some_and(|f| !f.trim().is_empty()),
        "input" => input_name.as_deref().is_some_and(|n| !n.trim().is_empty()),
        "amount" => amount.as_deref().is_some_and(|a| Decimal::parse(a).is_ok()),
        _ => return Err(Error::msg("the amount comes from a formula, an input or a fixed amount")),
    };
    if !ok {
        return Err(Error::msg(format!("a component whose amount comes from `{source}` needs that field filled in")));
    }
    let mut changes = json!({ "source": source, "formula": formula, "amount": amount, "input_name": input_name });
    if let Some(v) = input.label.filter(|l| !l.trim().is_empty()) {
        changes["label"] = json!(v);
    }
    if let Some(v) = input.when_expr {
        changes["when_expr"] = json!(v);
    }
    for (key, value) in [("is_taxable", input.is_taxable), ("pro_rata", input.pro_rata), ("is_active", input.is_active)] {
        if let Some(v) = value {
            changes[key] = json!(v);
        }
    }
    db::update("pay_component", &input.id, &changes)?.ok_or_else(|| Error::msg("the component is gone"))
}

#[derive(Deserialize)]
struct EditPeriod {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    start_date: Option<String>,
    #[serde(default)]
    end_date: Option<String>,
}

/// Rename an open period or move its dates; a closed period is reopened first.
fn edit_period(input: EditPeriod) -> Result<Record> {
    require_admin()?;
    let existing = require("pay_period", &input.id, "period")?;
    if text(&existing, "status") != Some("open") {
        return Err(Error::msg("a closed period is reopened before it is changed"));
    }
    let start = input.start_date.clone().or_else(|| text(&existing, "start_date").map(str::to_string)).unwrap_or_default();
    let end = input.end_date.clone().or_else(|| text(&existing, "end_date").map(str::to_string)).unwrap_or_default();
    let (from, to) = (parse_date(&start)?, parse_date(&end)?);
    if to < from {
        return Err(Error::msg("the period ends after it starts"));
    }
    for other in db::find::<Record>("pay_period").limit(500).all()? {
        if text(&other, "id") == Some(input.id.as_str()) || id_of(&other).is_ok_and(|i| i == input.id) {
            continue;
        }
        let (of, ot) = (parse_date(text(&other, "start_date").unwrap_or_default())?, parse_date(text(&other, "end_date").unwrap_or_default())?);
        if from <= ot && of <= to {
            return Err(Error::msg(format!("it overlaps the period `{}`", text(&other, "name").unwrap_or_default())));
        }
    }
    let mut changes = json!({ "start_date": format_date(from), "end_date": format_date(to) });
    if let Some(name) = input.name.filter(|n| !n.trim().is_empty()) {
        changes["name"] = json!(name);
    }
    db::update("pay_period", &input.id, &changes)?.ok_or_else(|| Error::msg("the period is gone"))
}

/// A stored component as an engine component, plus the inputs and names its text reads.
fn engine_component(row: &Record) -> Result<(Value, Vec<String>)> {
    let code = text(row, "code").unwrap_or_default();
    let mut kind = text(row, "kind").unwrap_or("earning");
    if row.get("is_statistical") == Some(&json!(true)) {
        kind = "info";
    }
    let source = text(row, "source").unwrap_or("formula");
    let mut texts: Vec<String> = Vec::new();
    let base = match source {
        "input" => text(row, "input_name").unwrap_or_default().to_string(),
        "amount" => text(row, "amount").unwrap_or("0").to_string(),
        _ => text(row, "formula").unwrap_or_default().to_string(),
    };
    texts.push(base.clone());
    let mut component = json!({ "id": code, "label": text(row, "label").unwrap_or(code), "kind": kind });
    let mut declared = Vec::new();
    if row.get("pro_rata") == Some(&json!(true)) {
        component["formula"] = json!(format!("({base}) * days_worked / period_days"));
        texts.push("days_worked period_days".into());
    } else {
        match source {
            "input" => {
                component["input"] = json!(base);
                declared.push(base.clone());
            }
            "amount" => component["amount"] = serde_json::from_str(&base).map_err(|e| Error::msg(format!("component {code}: {e}")))?,
            _ => component["formula"] = json!(base),
        }
    }
    if source == "input" && !declared.contains(&base) {
        declared.push(base);
    }
    if let Some(when) = text(row, "when_expr").filter(|w| !w.trim().is_empty()) {
        component["when"] = json!(when);
        texts.push(when.to_string());
    }
    let mut tags: Vec<String> = row["tags"].as_array().map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect()).unwrap_or_default();
    if kind == "earning" && row.get("is_taxable") != Some(&json!(false)) && !tags.iter().any(|t| t == "taxable") {
        tags.push("taxable".into());
    }
    component["tags"] = json!(tags);
    declared.extend(texts);
    Ok((component, declared))
}

#[derive(Deserialize)]
struct SlabUse {
    /// The constant name formulas read: `bracket(taxable * periods_per_year, <constant>)`.
    constant: String,
    /// The slab's code; the edition in force on `on` (default today) is used.
    code: String,
    #[serde(default)]
    on: Option<String>,
}

#[derive(Deserialize)]
struct Compose {
    code: String,
    version: String,
    name: String,
    currency: String,
    /// Component codes, in the order their formulas need.
    components: Vec<String>,
    #[serde(default)]
    slabs: Vec<SlabUse>,
    #[serde(default)]
    negative_net: Option<String>,
    #[serde(default)]
    extra_inputs: Vec<String>,
}

const RESERVED_WITH_DEFAULT: &[(&str, i64)] = &[("unpaid_leave_days", 0), ("overtime_hours", 0), ("exemptions_annual", 0), ("other_income_annual", 0)];

fn compose(input: Compose) -> Result<Record> {
    require_admin()?;
    if input.components.is_empty() {
        return Err(Error::msg("pick at least one component"));
    }
    let mut components = Vec::new();
    let mut seen_text = String::new();
    let mut input_names: Vec<String> = Vec::new();
    for code in &input.components {
        let row: Record = db::find("pay_component").filter("code", code.as_str()).first()?.ok_or_else(|| Error::msg(format!("there is no component `{code}`")))?;
        if row.get("is_active") == Some(&json!(false)) {
            return Err(Error::msg(format!("the component `{code}` is switched off")));
        }
        let (component, mentions) = engine_component(&row)?;
        for m in mentions {
            seen_text.push(' ');
            seen_text.push_str(&m);
        }
        if let Some(name) = component["input"].as_str() {
            input_names.push(name.to_string());
        }
        components.push(component);
    }
    let mentions = |name: &str| seen_text.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|w| w == name);
    let mut inputs: Vec<Value> = vec![
        json!({ "name": "base_salary" }),
        json!({ "name": "period_days", "default": 30 }),
        json!({ "name": "days_worked", "default": 30 }),
        json!({ "name": "periods_per_year", "default": 12 }),
    ];
    for (name, default) in RESERVED_WITH_DEFAULT {
        if mentions(name) {
            inputs.push(json!({ "name": name, "default": default }));
        }
    }
    for name in input_names.into_iter().chain(input.extra_inputs.iter().cloned()) {
        if !inputs.iter().any(|i| i["name"] == json!(name)) {
            inputs.push(json!({ "name": name, "default": 0 }));
        }
    }
    let mut constants = serde_json::Map::new();
    let today = today_date()?;
    for slab in &input.slabs {
        let on = slab.on.as_deref().map(parse_date).transpose()?.unwrap_or(today);
        let row = slab_in_force(&slab.code, on)?;
        constants.insert(slab.constant.clone(), row["slabs"].clone());
        if let Some(sd) = text(&row, "standard_deduction").filter(|s| Decimal::parse(s).is_ok_and(|d| !d.is_zero())) {
            constants.insert(format!("{}_standard_deduction", slab.constant), serde_json::from_str(sd).map_err(|e| Error::msg(e.to_string()))?);
        }
    }
    let mut definition = json!({ "id": input.code, "version": input.version, "currency": input.currency, "inputs": inputs, "constants": constants, "components": components });
    if let Some(neg) = input.negative_net {
        definition["negative_net"] = json!(neg);
    }
    new_structure(NewStructure { code: input.code, version: input.version, name: input.name, definition, packs: Vec::new() })
}

fn slab_in_force(code: &str, on: NaiveDate) -> Result<Record> {
    let rows: Vec<Record> = db::find("pay_tax_slab").filter("code", code).filter("is_active", true).order_by("-valid_from").limit(50).all()?;
    for row in rows {
        let from = parse_date(text(&row, "valid_from").unwrap_or_default())?;
        let to = text(&row, "valid_to").map(parse_date).transpose()?;
        if from <= on && to.is_none_or(|t| t >= on) {
            return Ok(row);
        }
    }
    Err(Error::msg(format!("there is no tax slab `{code}` in force on {}", format_date(on))))
}

// ---- tax slabs --------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct NewSlab {
    code: String,
    name: String,
    currency: String,
    #[serde(default)]
    regime: Option<String>,
    valid_from: String,
    #[serde(default)]
    valid_to: Option<String>,
    #[serde(default)]
    standard_deduction: Option<String>,
    /// `[[upper_bound_or_null, rate], ...]`
    slabs: Vec<(Option<f64>, f64)>,
}

fn create_slab(input: NewSlab) -> Result<Record> {
    require_admin()?;
    let code = code_ok(&input.code)?;
    let (from, to) = (parse_date(&input.valid_from)?, input.valid_to.as_deref().map(parse_date).transpose()?);
    if to.is_some_and(|t| t < from) {
        return Err(Error::msg("the slab ends after it starts"));
    }
    if input.slabs.is_empty() {
        return Err(Error::msg("a tax slab has at least one band"));
    }
    let last = input.slabs.len() - 1;
    let mut previous = f64::MIN;
    for (i, (upper, rate)) in input.slabs.iter().enumerate() {
        if !(0.0..=1.0).contains(rate) {
            return Err(Error::msg("a rate is a fraction between 0 and 1 (10% is 0.10)"));
        }
        match upper {
            Some(u) if i == last => return Err(Error::msg(format!("the last band has no upper bound (got {u})"))),
            Some(u) if *u <= previous => return Err(Error::msg("band upper bounds must ascend")),
            Some(u) => previous = *u,
            None if i != last => return Err(Error::msg("only the last band has no upper bound")),
            None => {}
        }
    }
    if let Some(sd) = &input.standard_deduction {
        Decimal::parse(sd)?;
    }
    for other in db::find::<Record>("pay_tax_slab").filter("code", code.as_str()).filter("is_active", true).limit(100).all()? {
        let (of, ot) = (parse_date(text(&other, "valid_from").unwrap_or_default())?, text(&other, "valid_to").map(parse_date).transpose()?);
        if crate::rules::assignments_overlap(from, to, of, ot) {
            return Err(Error::msg("another edition of this slab is in force in that time: end it first"));
        }
    }
    let table: Vec<Value> = input.slabs.iter().map(|(u, r)| json!([u, r])).collect();
    let mut data = json!({
        "code": code, "name": input.name, "currency": input.currency, "regime": input.regime, "valid_from": format_date(from),
        "standard_deduction": input.standard_deduction.unwrap_or_else(|| "0".into()), "slabs": table, "is_active": true,
    });
    if let Some(to) = to {
        data["valid_to"] = json!(format_date(to));
    }
    db::create("pay_tax_slab", &data)
}

/// Rounding-free tax on a yearly taxable amount: lets a person preview before a run.
#[derive(Deserialize)]
struct Preview {
    code: String,
    on: String,
    yearly_taxable: f64,
}

fn preview_tax(input: Preview) -> Result<Value> {
    require_payroll_staff()?;
    let row = slab_in_force(&input.code, parse_date(&input.on)?)?;
    let sd: f64 = text(&row, "standard_deduction").and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let taxable = (input.yearly_taxable - sd).max(0.0);
    let (mut tax, mut lower) = (0.0, 0.0);
    for band in row["slabs"].as_array().cloned().unwrap_or_default() {
        let upper = band[0].as_f64();
        let rate = band[1].as_f64().unwrap_or(0.0);
        let top = upper.map_or(taxable, |u| u.min(taxable));
        if top > lower {
            tax += (top - lower) * rate;
        }
        match upper {
            Some(u) if taxable > u => lower = u,
            _ => break,
        }
    }
    Ok(json!({ "taxable": taxable, "tax": (tax * 100.0).round() / 100.0 }))
}

// ---- periods ----------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct NewPeriod {
    name: String,
    start_date: String,
    end_date: String,
}

fn create_period(input: NewPeriod) -> Result<Record> {
    require_admin()?;
    let (from, to) = (parse_date(&input.start_date)?, parse_date(&input.end_date)?);
    if to < from {
        return Err(Error::msg("the period ends after it starts"));
    }
    for other in db::find::<Record>("pay_period").limit(500).all()? {
        let (of, ot) = (parse_date(text(&other, "start_date").unwrap_or_default())?, parse_date(text(&other, "end_date").unwrap_or_default())?);
        if from <= ot && of <= to {
            return Err(Error::msg(format!("it overlaps the period `{}`", text(&other, "name").unwrap_or_default())));
        }
    }
    db::create("pay_period", &json!({ "name": input.name, "start_date": format_date(from), "end_date": format_date(to), "status": "open" }))
}

fn set_period_status(id: &str, status: &str) -> Result<Record> {
    require_admin()?;
    require("pay_period", id, "period")?;
    db::update("pay_period", id, &json!({ "status": status }))?.ok_or_else(|| Error::msg("the period is gone"))
}

/// A run must sit inside an open period when the organisation asks for it.
pub fn check_run_period(start: NaiveDate, end: NaiveDate) -> Result<()> {
    let setting: Option<Record> = db::find("pay_setting").first()?;
    if !setting.is_some_and(|s| s.get("require_open_period") == Some(&json!(true))) {
        return Ok(());
    }
    for period in db::find::<Record>("pay_period").filter("status", "open").limit(500).all()? {
        let (from, to) = (parse_date(text(&period, "start_date").unwrap_or_default())?, parse_date(text(&period, "end_date").unwrap_or_default())?);
        if from <= start && end <= to {
            return Ok(());
        }
    }
    Err(Error::msg("payroll periods are required: the run must fall inside an open payroll period"))
}

// ---- exemptions -------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct NewCategory {
    code: String,
    label: String,
    #[serde(default)]
    max_amount: Option<String>,
    /// Code of the parent category, for a sub-category.
    #[serde(default)]
    parent: Option<String>,
}

fn create_category(input: NewCategory) -> Result<Record> {
    require_admin()?;
    let code = code_ok(&input.code)?;
    if let Some(max) = &input.max_amount {
        Decimal::parse(max)?;
    }
    if db::count("pay_exemption_category", Filter::eq("code", code.as_str()))? > 0 {
        return Err(Error::msg(format!("the category `{code}` exists")));
    }
    db::create("pay_exemption_category", &json!({ "code": code, "label": input.label, "max_amount": input.max_amount, "parent": input.parent, "is_active": true }))
}

#[derive(Deserialize)]
struct Declare {
    /// Admin may declare for someone; everyone else only for themselves.
    #[serde(default)]
    employee: Option<String>,
    category: String,
    year: String,
    declared: String,
    #[serde(default)]
    proof_note: Option<String>,
    /// An uploaded file (the media object a file field holds).
    #[serde(default)]
    proof_file: Option<Value>,
}

fn declare(input: Declare) -> Result<Record> {
    let me = crate::common::my_employee()?;
    let my_id = me.as_ref().and_then(|m| text(m, "id")).map(str::to_string);
    let employee = match (&input.employee, &my_id) {
        (Some(e), Some(m)) if e == m => e.clone(),
        (Some(e), _) => {
            require_admin()?;
            e.clone()
        }
        (None, Some(m)) => m.clone(),
        (None, None) => return Err(Error::msg("you are not an employee: name whom this is for")),
    };
    let category = require("pay_exemption_category", &input.category, "category")?;
    let declared = Decimal::parse(&input.declared)?.with_scale(2)?;
    if declared.is_negative() {
        return Err(Error::msg("a declaration is not negative"));
    }
    if input.year.len() != 4 || input.year.parse::<u32>().is_err() {
        return Err(Error::msg("the year is four digits"));
    }
    let _ = category;
    let note = input.proof_note.clone();
    let has_file = input.proof_file.as_ref().is_some_and(|f| !f.is_null());
    let status = if has_file || note.as_deref().is_some_and(|n| !n.trim().is_empty()) { "proof_submitted" } else { "declared" };
    match db::find::<Record>("pay_exemption_declaration").filter("employee", employee.as_str()).filter("category", input.category.as_str()).filter("year", input.year.as_str()).first()? {
        Some(existing) => {
            if text(&existing, "status") == Some("verified") {
                return Err(Error::msg("a verified declaration is final: ask payroll to reopen it"));
            }
            db::update("pay_exemption_declaration", id_of(&existing)?, &json!({ "declared": declared.to_string(), "proof_note": note, "proof_file": input.proof_file, "status": status }))?.ok_or_else(|| Error::msg("the declaration is gone"))
        }
        None => db::create("pay_exemption_declaration", &json!({ "employee": employee, "category": input.category, "year": input.year, "declared": declared.to_string(), "proof_note": note, "proof_file": input.proof_file, "status": status })),
    }
}

#[derive(Deserialize)]
struct Review {
    id: String,
    approve: bool,
    #[serde(default)]
    verified: Option<String>,
}

fn review(input: Review) -> Result<Record> {
    require_admin()?;
    let row = require("pay_exemption_declaration", &input.id, "declaration")?;
    let who = actor()?;
    if input.approve {
        let amount = match &input.verified {
            Some(v) => Decimal::parse(v)?.with_scale(2)?,
            None => Decimal::parse(text(&row, "declared").unwrap_or("0"))?.with_scale(2)?,
        };
        db::update("pay_exemption_declaration", &input.id, &json!({ "status": "verified", "verified": amount.to_string(), "reviewed_by": who }))?.ok_or_else(|| Error::msg("gone"))
    } else {
        db::update("pay_exemption_declaration", &input.id, &json!({ "status": "rejected", "verified": "0.00", "reviewed_by": who }))?.ok_or_else(|| Error::msg("gone"))
    }
}

/// Yearly exemption amount per employee for `year`: verified amounts, plus declared ones not yet checked when
/// the setting allows; each category capped at its maximum.
pub fn exemptions_for(employees: &[String], year: &str) -> Result<BTreeMap<String, Decimal>> {
    let mut out: BTreeMap<String, Decimal> = BTreeMap::new();
    let mut grouped: BTreeMap<(String, String), Decimal> = BTreeMap::new();
    if employees.is_empty() {
        return Ok(out);
    }
    let pending = db::find::<Record>("pay_setting").first()?.is_none_or(|s| s.get("count_pending_declarations") != Some(&json!(false)));
    let rows: Vec<Record> = db::find("pay_exemption_declaration").matching(Filter::one_of("employee", employees.to_vec()).and(Filter::eq("year", year))).limit(2000).all()?;
    let mut caps: BTreeMap<String, Option<Decimal>> = BTreeMap::new();
    for row in rows {
        let amount = match text(&row, "status") {
            Some("verified") => text(&row, "verified"),
            Some("declared" | "proof_submitted") if pending => text(&row, "declared"),
            _ => None,
        };
        let Some(amount) = amount else { continue };
        let mut amount = Decimal::parse(amount)?.with_scale(2)?;
        let category = text(&row, "category").unwrap_or_default().to_string();
        if !caps.contains_key(&category) {
            let cap = match db::get::<Record>("pay_exemption_category", &category)? {
                Some(c) => text(&c, "max_amount").map(|m| Decimal::parse(m).and_then(|d| d.with_scale(2))).transpose()?,
                None => None,
            };
            caps.insert(category.clone(), cap);
        }
        if let Some(Some(cap)) = caps.get(&category) {
            if amount > *cap {
                amount = *cap;
            }
        }
        let employee = text(&row, "employee").unwrap_or_default().to_string();
        // A sub-category counts towards its parent's maximum as well: keep each person's sum per parent.
        let parent = match db::get::<Record>("pay_exemption_category", &category)? {
            Some(c) => text(&c, "parent").filter(|p| !p.is_empty()).map(str::to_string),
            None => None,
        };
        match parent {
            Some(parent) => {
                let slot = grouped.entry((employee.clone(), parent)).or_insert_with(|| Decimal::zero(2));
                *slot = *slot + amount;
            }
            None => {
                let total = out.entry(employee).or_insert_with(|| Decimal::zero(2));
                *total = *total + amount;
            }
        }
    }
    for ((employee, parent), sum) in grouped {
        let cap = match db::find::<Record>("pay_exemption_category").filter("code", parent.as_str()).first()? {
            Some(c) => text(&c, "max_amount").map(|m| Decimal::parse(m).and_then(|d| d.with_scale(2))).transpose()?,
            None => None,
        };
        let counted = match cap {
            Some(cap) if sum > cap => cap,
            _ => sum,
        };
        let total = out.entry(employee).or_insert_with(|| Decimal::zero(2));
        *total = *total + counted;
    }
    Ok(out)
}

// ---- other income ----------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct OtherIncome {
    #[serde(default)]
    employee: Option<String>,
    year: String,
    label: String,
    amount: String,
}

#[derive(Deserialize)]
struct ReviewIncome {
    id: String,
    approve: bool,
}

/// Income from another source (a second job, prior-employer pay in the year) that raises the tax base.
fn declare_income(input: OtherIncome) -> Result<Record> {
    let me = crate::common::my_employee()?;
    let my_id = me.as_ref().and_then(|m| text(m, "id")).map(str::to_string);
    let employee = match (&input.employee, &my_id) {
        (Some(e), Some(m)) if e == m => e.clone(),
        (Some(e), _) => {
            require_admin()?;
            e.clone()
        }
        (None, Some(m)) => m.clone(),
        (None, None) => return Err(Error::msg("you are not an employee: name whom this is for")),
    };
    if input.year.len() != 4 || input.year.parse::<u32>().is_err() {
        return Err(Error::msg("the year is four digits"));
    }
    let amount = Decimal::parse(&input.amount)?.with_scale(2)?;
    if amount.is_negative() || amount.is_zero() {
        return Err(Error::msg("an amount of other income is above zero"));
    }
    if input.label.trim().is_empty() {
        return Err(Error::msg("say what the income is"));
    }
    db::create("pay_other_income", &json!({ "employee": employee, "year": input.year, "label": input.label.trim(), "amount": amount.to_string(), "status": "declared" }))
}

fn review_income(input: ReviewIncome) -> Result<Record> {
    require_admin()?;
    let row = require("pay_other_income", &input.id, "declaration")?;
    let _ = row;
    let status = if input.approve { "verified" } else { "rejected" };
    db::update("pay_other_income", &input.id, &json!({ "status": status, "reviewed_by": crate::common::actor()? }))?.ok_or_else(|| Error::msg("the declaration is gone"))
}

/// Yearly other income per employee for `year`: verified ones, plus declared ones when the setting allows.
pub fn other_income_for(employees: &[String], year: &str) -> Result<BTreeMap<String, Decimal>> {
    let mut out: BTreeMap<String, Decimal> = BTreeMap::new();
    if employees.is_empty() {
        return Ok(out);
    }
    let pending = db::find::<Record>("pay_setting").first()?.is_none_or(|s| s.get("count_pending_declarations") != Some(&json!(false)));
    let rows: Vec<Record> = db::find("pay_other_income").matching(Filter::one_of("employee", employees.to_vec()).and(Filter::eq("year", year))).limit(2000).all()?;
    for row in rows {
        let counts = match text(&row, "status") {
            Some("verified") => true,
            Some("declared") => pending,
            _ => false,
        };
        if !counts {
            continue;
        }
        let amount = Decimal::parse(text(&row, "amount").unwrap_or("0"))?.with_scale(2)?;
        let total = out.entry(text(&row, "employee").unwrap_or_default().to_string()).or_insert_with(|| Decimal::zero(2));
        *total = *total + amount;
    }
    Ok(out)
}

// ---- bulk assignment --------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct Bulk {
    employees: Vec<String>,
    structure: String,
    #[serde(default)]
    version: Option<String>,
    valid_from: String,
    #[serde(default)]
    valid_to: Option<String>,
    #[serde(default)]
    variables: Option<serde_json::Map<String, Value>>,
}

fn bulk_assign(input: Bulk) -> Result<Value> {
    require_admin()?;
    if input.employees.len() > 100 {
        return Err(Error::msg("at most 100 people per call: one call has a fixed budget of work"));
    }
    let (mut done, mut failed) = (Vec::new(), Vec::new());
    for employee in input.employees {
        let one = Assign {
            employee: employee.clone(),
            structure: input.structure.clone(),
            version: input.version.clone(),
            valid_from: input.valid_from.clone(),
            valid_to: input.valid_to.clone(),
            variables: input.variables.clone(),
        };
        match assign(one) {
            Ok(row) => done.push(row["id"].clone()),
            Err(e) => failed.push(json!({ "employee": employee, "error": e.to_string() })),
        }
    }
    Ok(json!({ "assigned": done, "failed": failed }))
}


/// Ids of the people in a department (and below) who work here, as `hr` lists them. At most 100.
fn department_people(department: &str) -> Result<Vec<String>> {
    let found: Value = plugins::call(
        "hr",
        "list_employees",
        &json!({ "department": department, "with_subdepartments": true, "limit": 100 }),
    )?;
    let rows = found.get("rows").and_then(Value::as_array).cloned().or_else(|| found.as_array().cloned()).unwrap_or_default();
    Ok(rows
        .iter()
        .filter(|r| r.get("status").and_then(Value::as_str).is_none_or(|s| !matches!(s, "resigned" | "terminated" | "retired" | "deceased")))
        .filter_map(|r| r.get("id").and_then(Value::as_str).map(str::to_string))
        .collect())
}

#[derive(Deserialize)]
struct ByDepartment {
    department: String,
    structure: String,
    valid_from: String,
    #[serde(default)]
    valid_to: Option<String>,
}

#[derive(Deserialize)]
struct SimpleSlab {
    code: String,
    name: String,
    currency: String,
    #[serde(default)]
    regime: Option<String>,
    valid_from: String,
    #[serde(default)]
    valid_to: Option<String>,
    #[serde(default)]
    standard_deduction: Option<String>,
    #[serde(default)]
    upper_1: Option<String>,
    #[serde(default)]
    rate_1: Option<String>,
    #[serde(default)]
    upper_2: Option<String>,
    #[serde(default)]
    rate_2: Option<String>,
    #[serde(default)]
    upper_3: Option<String>,
    #[serde(default)]
    rate_3: Option<String>,
    #[serde(default)]
    upper_4: Option<String>,
    #[serde(default)]
    rate_4: Option<String>,
    #[serde(default)]
    upper_5: Option<String>,
    #[serde(default)]
    rate_5: Option<String>,
}

/// A slab from a form: up to five bands, each an upper bound (empty on the last) and a rate in percent.
fn simple_slab(input: SimpleSlab) -> Result<Record> {
    let pairs = [
        (&input.upper_1, &input.rate_1),
        (&input.upper_2, &input.rate_2),
        (&input.upper_3, &input.rate_3),
        (&input.upper_4, &input.rate_4),
        (&input.upper_5, &input.rate_5),
    ];
    let mut slabs = Vec::new();
    for (upper, rate) in pairs {
        let Some(rate) = rate.as_deref().map(str::trim).filter(|r| !r.is_empty()) else { continue };
        let percent: f64 = rate.parse().map_err(|_| Error::msg("a rate is a number in percent, such as 10"))?;
        let upper = match upper.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
            Some(u) => Some(u.parse::<f64>().map_err(|_| Error::msg("an upper bound is a number"))?),
            None => None,
        };
        slabs.push((upper, percent / 100.0));
    }
    create_slab(NewSlab {
        code: input.code,
        name: input.name,
        currency: input.currency,
        regime: input.regime,
        valid_from: input.valid_from,
        valid_to: input.valid_to,
        standard_deduction: input.standard_deduction,
        slabs,
    })
}

handler! {
    /// Declare income from another source for a tax year.
    fn declare_other_income(input: OtherIncome) -> Record {
        declare_income(input)
    }

    fn review_other_income(input: ReviewIncome) -> Record {
        review_income(input)
    }

    fn my_other_income(_: Empty) -> Vec<Record> {
        let Some(me) = crate::common::my_employee()? else { return Ok(Vec::new()) };
        db::find("pay_other_income").filter("employee", id_of(&me)?).limit(200).all()
    }

    fn list_other_income(_: Empty) -> Vec<Record> {
        require_payroll_staff()?;
        db::find("pay_other_income").order_by("-year").limit(500).all()
    }

    fn update_pay_component(input: EditComponent) -> Record {
        edit_component(input)
    }

    fn get_pay_component(input: Id) -> Option<Record> {
        require_admin()?;
        db::get("pay_component", &input.id)
    }

    fn update_pay_period(input: EditPeriod) -> Record {
        edit_period(input)
    }

    fn get_pay_period(input: Id) -> Option<Record> {
        require_admin()?;
        db::get("pay_period", &input.id)
    }

    /// Put everyone in a department (up to 100) on a pay structure.
    fn bulk_assign_structure_by_department(input: ByDepartment) -> Value {
        let employees = department_people(&input.department)?;
        bulk_assign(Bulk {
            employees,
            structure: input.structure,
            version: None,
            valid_from: input.valid_from,
            valid_to: input.valid_to,
            variables: None,
        })
    }

    /// Create a tax slab from a form: up to five bands with rates in percent.
    fn create_tax_slab_simple(input: SimpleSlab) -> Record {
        simple_slab(input)
    }

    fn create_pay_component(input: NewComponent) -> Record {
        create_component(input)
    }

    fn list_pay_components(_: Empty) -> Vec<Record> {
        require_payroll_staff()?;
        db::find("pay_component").order_by("code").limit(500).all()
    }

    fn retire_pay_component(input: Id) -> Record {
        require_admin()?;
        db::update("pay_component", &input.id, &json!({ "is_active": false }))?.ok_or_else(|| Error::msg("there is no such component"))
    }

    /// Build and save a salary structure from library components and tax slabs.
    fn compose_structure(input: Compose) -> Record {
        compose(input)
    }

    fn create_tax_slab(input: NewSlab) -> Record {
        create_slab(input)
    }

    fn list_tax_slabs(_: Empty) -> Vec<Record> {
        require_payroll_staff()?;
        db::find("pay_tax_slab").order_by("code").limit(200).all()
    }

    fn retire_tax_slab(input: Id) -> Record {
        require_admin()?;
        db::update("pay_tax_slab", &input.id, &json!({ "is_active": false }))?.ok_or_else(|| Error::msg("there is no such slab"))
    }

    fn preview_tax_slab(input: Preview) -> Value {
        preview_tax(input)
    }

    fn create_pay_period(input: NewPeriod) -> Record {
        create_period(input)
    }

    fn list_pay_periods(_: Empty) -> Vec<Record> {
        require_payroll_staff()?;
        db::find("pay_period").order_by("-start_date").limit(200).all()
    }

    fn close_pay_period(input: Id) -> Record {
        set_period_status(&input.id, "closed")
    }

    fn reopen_pay_period(input: Id) -> Record {
        set_period_status(&input.id, "open")
    }

    fn create_exemption_category(input: NewCategory) -> Record {
        create_category(input)
    }

    fn list_exemption_categories(_: Empty) -> Vec<Record> {
        db::find("pay_exemption_category").filter("is_active", true).order_by("code").limit(200).all()
    }

    fn declare_exemption(input: Declare) -> Record {
        declare(input)
    }

    fn review_exemption(input: Review) -> Record {
        review(input)
    }

    fn list_exemption_declarations(_: Empty) -> Vec<Record> {
        require_payroll_staff()?;
        db::find("pay_exemption_declaration").order_by("-year").limit(500).all()
    }

    /// The caller's own declarations.
    fn my_exemptions(_: Empty) -> Vec<Record> {
        let Some(me) = crate::common::my_employee()? else { return Ok(Vec::new()) };
        db::find("pay_exemption_declaration").filter("employee", id_of(&me)?).order_by("-year").limit(200).all()
    }

    fn bulk_assign_structure(input: Bulk) -> Value {
        bulk_assign(input)
    }
}
