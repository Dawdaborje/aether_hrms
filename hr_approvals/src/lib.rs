//! One inbox for everything waiting on the caller's decision: leave, loans, expenses, travel, benefit
//! claims, attendance corrections and requests, and timesheets. Each plugin still owns and decides its own
//! requests; this only asks each of them what is waiting and lists the answers together, with the page where
//! the decision is made. A plugin that fails to answer is named in `unavailable`, not hidden.

use aether_sdk::prelude::*;
use aether_sdk::records::{text, Record};

/// (kind label, plugin, function, page where it is decided)
const SOURCES: &[(&str, &str, &str, &str)] = &[
    ("Leave", "hr_leave", "leave_waiting_for_me", "/hr/leave"),
    ("Loan", "hr_loan", "loans_waiting_for_me", "/hr/loans/approvals"),
    ("Expense report", "hr_expense", "reports_waiting_for_me", "/hr/expenses"),
    ("Trip", "hr_travel", "trips_waiting_for_me", "/hr/travel"),
    ("Benefit claim", "hr_benefits", "benefit_claims_waiting_for_me", "/hr/benefits/approvals"),
    ("Attendance correction", "hr_attendance", "corrections_waiting_for_me", "/hr/attendance/approvals/corrections"),
    ("Shift request", "hr_attendance", "shift_requests_waiting_for_me", "/hr/attendance/approvals/shifts"),
    ("Attendance request", "hr_attendance", "attendance_requests_waiting_for_me", "/hr/attendance/approvals/requests"),
    ("Timesheet", "hr_timesheet", "timesheets_waiting_for_me", "/hr/timesheets/approvals"),
];

fn first_text(row: &Record, keys: &[&str]) -> String {
    keys.iter().find_map(|k| text(row, k)).unwrap_or_default().to_string()
}

fn inbox() -> Result<Value> {
    let mut rows = Vec::new();
    let mut unavailable = Vec::new();
    for (kind, plugin, function, page) in SOURCES {
        match plugins::call::<Vec<Record>>(plugin, function, &json!({})) {
            Ok(list) => {
                for item in list {
                    rows.push(json!({
                        "id": item.get("id"), "kind": kind, "plugin": plugin, "route": page,
                        "reference": first_text(&item, &["reference", "number", "name", "code"]),
                        "employee": item.get("employee"),
                        "when": first_text(&item, &["start_date", "work_date", "date", "incurred_on", "created_at", "submitted_on"]),
                        "detail": first_text(&item, &["description", "purpose", "reason", "note", "subject"]),
                    }));
                }
            }
            Err(e) => unavailable.push(json!({ "kind": kind, "error": e.to_string() })),
        }
    }
    Ok(json!({ "rows": rows, "unavailable": unavailable }))
}

/// Ask one plugin; a failure becomes `null` so one missing plugin never blanks the page.
fn ask(plugin: &str, function: &str, payload: &Value) -> Value {
    plugins::call::<Value>(plugin, function, payload).unwrap_or(Value::Null)
}

fn text_of(v: &Value) -> String {
    v.as_str().map(str::to_string).unwrap_or_else(|| if v.is_null() { String::new() } else { v.to_string() })
}

/// The caller's own HR at a glance: balances, the latest payslip, what waits, open tasks, who is away.
fn home() -> Result<Value> {
    let me: Value = ask("hr", "my_employee", &json!({}));
    if me.is_null() {
        return Ok(json!({ "linked": false }));
    }
    let id = text_of(&me["id"]);
    let day = aether_sdk::records::today()?;
    let balances = ask("hr_leave", "leave_balances", &json!({ "employee": id }));
    let balance_rows: Vec<Value> = balances
        .as_array()
        .map(|a| {
            a.iter()
                .map(|b| {
                    let name = ["leave_type_name", "name", "leave_type", "type"].iter().find_map(|k| b.get(*k).and_then(Value::as_str)).unwrap_or("");
                    let avail = ["available", "remaining", "balance"].iter().find_map(|k| b.get(*k)).cloned().unwrap_or(Value::Null);
                    json!({ "name": name, "value": avail })
                })
                .collect()
        })
        .unwrap_or_default();
    let slips = ask("hr_payroll", "my_payslips", &json!({}));
    let latest = slips.as_array().and_then(|a| a.last().cloned()).unwrap_or(Value::Null);
    let counts = inbox()?;
    let waiting = counts["rows"].as_array().map_or(0, Vec::len);
    let tasks = ask("hr_onboarding", "my_tasks", &json!({}));
    let away = ask("hr_leave", "who_is_away_between", &json!({ "from": day, "to": day }));
    Ok(json!({
        "linked": true, "name": me["display_name"], "job": me["job"], "department": me["department"],
        "leave_balances": balance_rows,
        "latest_net": latest["net"], "latest_currency": latest["currency"], "latest_payslip": latest["id"],
        "waiting_on_me": waiting,
        "open_tasks": tasks.as_array().map_or(0, Vec::len),
        "away_today": away.as_array().map_or(0, Vec::len),
        "my_leave_requests": ask("hr_leave", "my_leave_requests", &json!({})).as_array().map_or(0, Vec::len),
    }))
}

handler! {
    /// The caller's own HR at a glance.
    fn my_hr_home(_: Empty) -> Value {
        home()
    }

    /// Everything waiting on the caller, from every plugin that has something.
    fn approvals_inbox(_: Empty) -> Vec<Value> {
        let all = inbox()?;
        Ok(all["rows"].as_array().cloned().unwrap_or_default())
    }

    /// How many items wait, by kind, and which plugins could not be asked.
    fn approvals_count(_: Empty) -> Value {
        let all = inbox()?;
        let rows = all["rows"].as_array().cloned().unwrap_or_default();
        let mut by_kind = serde_json::Map::new();
        for r in &rows {
            let kind = r["kind"].as_str().unwrap_or("").to_string();
            let n = by_kind.get(&kind).and_then(Value::as_i64).unwrap_or(0) + 1;
            by_kind.insert(kind, json!(n));
        }
        Ok(json!({ "total": rows.len(), "by_kind": by_kind, "unavailable": all["unavailable"] }))
    }
}
