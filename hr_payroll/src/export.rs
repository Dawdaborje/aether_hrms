//! Pay registers and payslips as Excel or PDF. The kernel renders the file (`report::export`);
//! this only decides which rows a person may have. Buttons call these with `format="xlsx"` or `"pdf"`.

use aether_sdk::prelude::*;

use crate::common::{require, require_payroll_staff, text, Record};

#[derive(Deserialize)]
struct RunExport {
    #[serde(alias = "id")]
    run: String,
    #[serde(default)]
    format: Option<String>,
}

#[derive(Deserialize)]
struct SlipExport {
    id: String,
    #[serde(default)]
    format: Option<String>,
}

#[derive(Deserialize, Default)]
struct MineExport {
    #[serde(default)]
    format: Option<String>,
}

fn format_of(raw: &Option<String>) -> &'static str {
    match raw.as_deref() {
        Some("pdf") => "pdf",
        _ => "xlsx",
    }
}

fn money(slip: &Record, field: &str) -> Value {
    slip.get(field).cloned().unwrap_or(Value::Null)
}

fn slip_columns() -> Value {
    json!([
        { "field": "employee_name", "label": "Employee" },
        { "field": "gross", "label": "Gross" },
        { "field": "deductions", "label": "Deductions" },
        { "field": "net", "label": "Net pay" },
        { "field": "currency", "label": "Currency" },
    ])
}

fn slip_row(slip: &Record) -> Value {
    json!({
        "employee_name": slip.get("employee_name").cloned().unwrap_or(Value::Null),
        "gross": money(slip, "gross"),
        "deductions": money(slip, "deductions"),
        "net": money(slip, "net"),
        "currency": slip.get("currency").cloned().unwrap_or(Value::Null),
    })
}

/// The register of a run: one row per payslip of its current attempt (up to the kernel's 1000-row limit).
fn register(input: RunExport) -> Result<Value> {
    require_payroll_staff()?;
    let run = require("pay_run", &input.run, "run")?;
    let attempt = run.get("attempt").and_then(Value::as_i64).unwrap_or(0);
    let slips: Vec<Record> = db::find::<Record>("payslip")
        .filter("run", input.run.as_str())
        .filter("attempt", attempt)
        .order_by("employee_name")
        .limit(1000)
        .all()?;
    let rows: Vec<Value> = slips.iter().map(slip_row).collect();
    let title = format!("Payroll register {}", run.get("number").and_then(Value::as_str).unwrap_or(""));
    report::export(&json!({ "format": format_of(&input.format), "title": title.trim(), "columns": slip_columns(), "rows": rows }))
}

fn mine(input: MineExport) -> Result<Value> {
    let mut rows = Vec::new();
    for id in crate::run::my_payslip_ids()? {
        if let Some(slip) = db::get::<Record>("payslip", &id)? {
            rows.push(slip_row(&slip));
        }
    }
    report::export(&json!({ "format": format_of(&input.format), "title": "My payslips", "columns": slip_columns(), "rows": rows }))
}

/// One payslip, line by line. Staff may export any; everyone else only their own.
fn one(input: SlipExport) -> Result<Value> {
    let slip = require("payslip", &input.id, "payslip")?;
    if require_payroll_staff().is_err() && !crate::run::my_payslip_ids()?.iter().any(|id| id == &input.id || input.id.ends_with(id.as_str()) || id.ends_with(input.id.as_str())) {
        return Err(Error::msg("this is not your payslip"));
    }
    let mut rows = Vec::new();
    // A heading block: whose payslip, for which period, under which structure.
    let run = db::get::<Record>("pay_run", text(&slip, "run").unwrap_or_default())?;
    let period = run.as_ref().map(|r| format!("{} to {}", text(r, "period_start").unwrap_or(""), text(r, "period_end").unwrap_or(""))).unwrap_or_default();
    rows.push(json!({ "label": "Employee", "kind": "", "amount": slip.get("employee_name").cloned().unwrap_or(Value::Null) }));
    rows.push(json!({ "label": "Period", "kind": "", "amount": period }));
    rows.push(json!({ "label": "Structure", "kind": "", "amount": format!("{} {}", text(&slip, "structure_code").unwrap_or(""), text(&slip, "structure_version").unwrap_or("")) }));
    if let Some(lines) = slip.get("lines").and_then(Value::as_array) {
        for line in lines {
            let label = ["label", "name", "code"].iter().find_map(|k| line.get(*k).and_then(Value::as_str)).unwrap_or("");
            rows.push(json!({
                "label": label,
                "kind": line.get("kind").cloned().unwrap_or(Value::Null),
                "amount": line.get("amount").cloned().unwrap_or(Value::Null),
            }));
        }
    }
    for (label, field) in [("Gross", "gross"), ("Deductions", "deductions"), ("Net pay", "net")] {
        rows.push(json!({ "label": label, "kind": "total", "amount": money(&slip, field) }));
    }
    let who = slip.get("employee_name").and_then(Value::as_str).unwrap_or("Payslip");
    report::export(&json!({
        "format": format_of(&input.format),
        "title": format!("Payslip {who}"),
        "columns": [
            { "field": "label", "label": "Component" },
            { "field": "kind", "label": "Kind" },
            { "field": "amount", "label": "Amount" },
        ],
        "rows": rows,
    }))
}

handler! {
    /// The payroll register of a run as Excel or PDF.
    fn export_payroll_register(input: RunExport) -> Value {
        register(input)
    }

    /// Everything the caller has been paid, as Excel or PDF.
    fn export_my_payslips(input: Option<MineExport>) -> Value {
        mine(input.unwrap_or_default())
    }

    /// One payslip as Excel or PDF.
    fn export_payslip(input: SlipExport) -> Value {
        one(input)
    }
}
