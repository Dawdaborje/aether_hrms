//! The HR pending-actions board: what HR has to chase today, from every area, in one place.
//!
//! Each area is asked of the plugin that owns it, as the person looking at the page, so a plugin the caller
//! has no role in simply reports that it could not answer; one failing area never blanks the board. Nothing
//! is stored here and nothing is decided here: every row names its employee and the date that makes it late.

use std::collections::BTreeMap;

use aether_sdk::dates::{self, Duration, NaiveDate};
use aether_sdk::prelude::*;
use aether_sdk::records::{text, Record};

/// Rows shown per area.
const SHOWN: usize = 10;
/// Onboarding and offboarding runs whose tasks are checked.
/// Days ahead for departures and probation endings.
const SOON: i64 = 14;
/// A letter waiting longer than this many days is chased.
const LETTER_DAYS: i64 = 7;

fn key(v: &str) -> &str {
    v.rsplit(':').next().unwrap_or(v)
}

fn label_of(raw: &str) -> String {
    let mut chars: Vec<char> = raw.replace('_', " ").chars().collect();
    if let Some(first) = chars.first_mut() {
        *first = first.to_ascii_uppercase();
    }
    chars.into_iter().collect()
}

fn require_hr_staff() -> Result<()> {
    let me = context::current()?;
    if me.has_role("hr.hr_user") || me.has_role("hr.hr_manager") {
        Ok(())
    } else {
        Err(Error::msg("the pending actions board is for HR staff"))
    }
}

fn date_of(row: &Record, field: &str) -> Option<NaiveDate> {
    text(row, field).and_then(|d| dates::parse_date(d).ok())
}

fn days_between(from: NaiveDate, to: NaiveDate) -> i64 {
    (to - from).num_days()
}

/// Everyone, by id → display name, for rows that carry only an employee id.
fn names() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut offset = 0u32;
    loop {
        let page = plugins::call::<Vec<Record>>("hr", "find_employees", &json!({ "limit": 200, "offset": offset })).unwrap_or_default();
        let count = page.len();
        for p in page {
            if let (Some(id), Some(name)) = (text(&p, "id"), text(&p, "display_name")) {
                out.insert(key(id).to_string(), name.to_string());
            }
        }
        if count < 200 || out.len() >= 5000 {
            break;
        }
        offset += 200;
    }
    out
}

/// One area of the board: its rows, or why it could not be read.
struct Area {
    id: &'static str,
    title: &'static str,
    columns: &'static [(&'static str, &'static str)],
    rows: Result<Vec<Value>>,
}

fn who(names: &BTreeMap<String, String>, row: &Record, field: &str) -> String {
    let id = text(row, field).unwrap_or_default();
    names.get(key(id)).cloned().unwrap_or_else(|| if id.is_empty() { String::new() } else { key(id).to_string() })
}

fn ask_rows(plugin: &str, function: &str, payload: &Value) -> Result<Vec<Record>> {
    plugins::call::<Vec<Record>>(plugin, function, payload)
}

fn documents(names: &BTreeMap<String, String>, today: NaiveDate) -> Result<Vec<Value>> {
    let mut rows: Vec<(NaiveDate, Value)> = ask_rows("hr_documents", "expiring_documents", &json!({}))?
        .iter()
        .filter_map(|d| {
            let expires = date_of(d, "expires_on")?;
            let left = days_between(today, expires);
            let state = if left < 0 { format!("Expired {} days ago", -left) } else if left == 0 { "Expires today".to_string() } else { format!("In {left} days") };
            Some((expires, json!({ "employee": who(names, d, "employee"), "document": label_of(text(d, "kind").unwrap_or_default()),
                "expires": text(d, "expires_on"), "state": state })))
        })
        .collect();
    rows.sort_by_key(|r| r.0);
    Ok(rows.into_iter().map(|r| r.1).collect())
}

fn onboarding_tasks(names: &BTreeMap<String, String>, today: NaiveDate) -> Result<Vec<Value>> {
    // One call for every overdue task of every open run, so the number of runs never hides a late one.
    let mut late: Vec<(i64, Value)> = Vec::new();
    for t in ask_rows("hr_onboarding", "overdue_tasks", &json!({}))? {
        let Some(due) = date_of(&t, "due_end") else { continue };
        if due >= today {
            continue;
        }
        let kind = if text(&t, "run_kind") == Some("offboard") { "Offboarding" } else { "Onboarding" };
        let owner = text(&t, "assignee_note").or_else(|| text(&t, "assignee_role")).unwrap_or_default();
        late.push((days_between(due, today), json!({
            "employee": who(names, &t, "run_employee"), "run": kind, "task": text(&t, "name"), "due": text(&t, "due_end"),
            "late": format!("{} days", days_between(due, today)), "owner": owner,
        })));
    }
    late.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(late.into_iter().map(|r| r.1).collect())
}

fn letters(names: &BTreeMap<String, String>, today: NaiveDate) -> Result<Vec<Value>> {
    let mut rows: Vec<(i64, Value)> = ask_rows("hr_career", "list_employee_letters", &json!({}))?
        .iter()
        .filter(|l| text(l, "accepted_on").is_none())
        .filter_map(|l| {
            let waiting = days_between(date_of(l, "issued_on")?, today);
            (waiting >= LETTER_DAYS).then(|| (waiting, json!({ "employee": who(names, l, "employee"), "letter": text(l, "reference"),
                "kind": label_of(text(l, "kind").unwrap_or_default()), "issued": text(l, "issued_on"), "waiting": format!("{waiting} days") })))
        })
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(rows.into_iter().map(|r| r.1).collect())
}

fn grievances(today: NaiveDate) -> Result<Vec<Value>> {
    let mut rows: Vec<(i64, Value)> = ask_rows("hr_grievance", "list_grievances", &json!({ "overdue": true }))?
        .iter()
        .filter_map(|c| {
            let late = days_between(date_of(c, "due_date")?, today);
            Some((late, json!({ "case": text(c, "reference"), "severity": label_of(text(c, "severity").unwrap_or_default()),
                "state": label_of(text(c, "state").unwrap_or_default()), "due": text(c, "due_date"), "late": format!("{late} days"),
                "level": c.get("escalation_level") })))
        })
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(rows.into_iter().map(|r| r.1).collect())
}

fn training(names: &BTreeMap<String, String>) -> Result<Vec<Value>> {
    let gaps = plugins::call::<Vec<Value>>("hr_training", "mandatory_training_gaps", &json!({}))?;
    Ok(gaps
        .iter()
        .map(|g| {
            let id = g["employee"].as_str().unwrap_or_default();
            json!({ "employee": names.get(key(id)).cloned().or_else(|| g["name"].as_str().map(String::from)).unwrap_or_default(), "course": g["code"] })
        })
        .collect())
}

fn departures(names: &BTreeMap<String, String>, today: NaiveDate) -> Result<Vec<Value>> {
    let limit = today + Duration::days(SOON);
    let mut rows: Vec<(NaiveDate, Value)> = Vec::new();
    for status in ["notice", "clearance"] {
        for d in ask_rows("hr_onboarding", "list_departures", &json!({ "status": status }))? {
            let Some(last) = date_of(&d, "last_day") else { continue };
            if last > limit {
                continue;
            }
            let left = days_between(today, last);
            let when = if left < 0 { format!("{} days ago", -left) } else if left == 0 { "Today".to_string() } else { format!("In {left} days") };
            rows.push((last, json!({ "employee": who(names, &d, "employee"), "status": label_of(status), "last_day": text(&d, "last_day"), "when": when })));
        }
    }
    rows.sort_by_key(|r| r.0);
    Ok(rows.into_iter().map(|r| r.1).collect())
}

fn probation(names: &BTreeMap<String, String>, today: NaiveDate) -> Result<Vec<Value>> {
    let _ = names;
    let mut rows: Vec<(NaiveDate, Value)> = ask_rows("hr", "probation_ending_soon", &json!({ "days": SOON }))?
        .iter()
        .filter_map(|p| {
            let end = date_of(p, "probation_end")?;
            let left = days_between(today, end);
            Some((end, json!({ "employee": text(p, "display_name"), "ends": text(p, "probation_end"),
                "when": if left == 0 { "Today".to_string() } else { format!("In {left} days") } })))
        })
        .collect();
    rows.sort_by_key(|r| r.0);
    Ok(rows.into_iter().map(|r| r.1).collect())
}

/// Cheap counts for the landing page: the areas that need one call each, without names or the checklist walk.
pub fn counts(today: NaiveDate) -> Vec<Value> {
    let none = BTreeMap::new();
    let reading = |name: &str, label: &str, rows: Result<Vec<Value>>| {
        rows.ok().map(|r| json!({ "name": name, "label": label, "value": r.len(), "previous": null, "change": null }))
    };
    [
        reading("probation", "Probation ends within 14 days", probation(&none, today)),
        reading("documents", "Documents expiring or expired", documents(&none, today)),
        reading("departures", "Departures to close", departures(&none, today)),
        reading("grievances", "Grievances past due", grievances(today)),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn board() -> Result<Value> {
    require_hr_staff()?;
    let today = dates::parse_date(&aether_sdk::records::today()?)?;
    let names = names();
    let areas = vec![
        Area { id: "probation", title: "Probation ends within 14 days", columns: &[("employee", "Employee"), ("ends", "Ends"), ("when", "When")], rows: probation(&names, today) },
        Area { id: "tasks", title: "Overdue onboarding and offboarding tasks", columns: &[("employee", "Employee"), ("run", "Checklist"), ("task", "Task"), ("due", "Was due"), ("late", "Late by"), ("owner", "Owner")], rows: onboarding_tasks(&names, today) },
        Area { id: "departures", title: "Departures to close within 14 days", columns: &[("employee", "Employee"), ("status", "Stage"), ("last_day", "Last day"), ("when", "When")], rows: departures(&names, today) },
        Area { id: "documents", title: "Identity documents expiring or expired", columns: &[("employee", "Employee"), ("document", "Document"), ("expires", "Expires"), ("state", "State")], rows: documents(&names, today) },
        Area { id: "letters", title: "Letters not yet accepted", columns: &[("employee", "Employee"), ("letter", "Letter"), ("kind", "Kind"), ("issued", "Issued"), ("waiting", "Waiting")], rows: letters(&names, today) },
        Area { id: "grievances", title: "Grievances past their due date", columns: &[("case", "Case"), ("severity", "Severity"), ("state", "State"), ("due", "Was due"), ("late", "Late by"), ("level", "Escalation")], rows: grievances(today) },
        Area { id: "training", title: "Mandatory training missing", columns: &[("employee", "Employee"), ("course", "Course")], rows: training(&names) },
    ];

    let waiting = super::inbox().map(|v| v["rows"].as_array().map_or(0, Vec::len)).ok();
    let mut readings: Vec<Value> = areas
        .iter()
        .filter_map(|a| a.rows.as_ref().ok().map(|r| json!({ "name": a.id, "label": a.title, "value": r.len(), "previous": null, "change": null })))
        .collect();
    if let Some(n) = waiting {
        readings.insert(0, json!({ "name": "waiting", "label": "Waiting on your decision", "value": n, "previous": null, "change": null }));
    }
    let total: u64 = readings.iter().filter(|r| r["name"] != "waiting").filter_map(|r| r["value"].as_u64()).sum();

    let chart_rows: Vec<(&str, f64)> = areas.iter().filter_map(|a| a.rows.as_ref().ok().map(|r| (a.title, r.len() as f64))).collect();
    let mut items = vec![
        json!({ "id": "headline", "width": 12, "card": { "kind": "number", "title": "To chase", "format": "number", "readings": readings,
            "info": "Each figure counts every open item in its area, not only the ones listed below." } }),
        json!({ "id": "by_area", "width": 12, "card": { "kind": "row", "title": "Open items by area",
            "categories": chart_rows.iter().map(|r| r.0).collect::<Vec<_>>(), "keys": chart_rows.iter().map(|r| r.0).collect::<Vec<_>>(),
            "series": [{ "name": "Items", "measure": "count", "values": chart_rows.iter().map(|r| r.1).collect::<Vec<_>>() }], "dimensions": ["area"] } }),
    ];
    for area in &areas {
        match &area.rows {
            Ok(rows) if rows.is_empty() => continue,
            Ok(rows) => {
                let title = if rows.len() > SHOWN { format!("{} (showing {SHOWN} of {})", area.title, rows.len()) } else { format!("{} ({})", area.title, rows.len()) };
                items.push(json!({ "id": area.id, "width": 12, "card": {
                    "kind": "table", "title": title,
                    "columns": area.columns.iter().map(|(k, l)| json!({ "key": k, "label": l, "type": "dimension" })).collect::<Vec<_>>(),
                    "rows": rows.iter().take(SHOWN).map(|r| json!({ "cells": r, "keys": {} })).collect::<Vec<_>>(),
                } }));
            }
            Err(e) => items.push(json!({ "id": area.id, "width": 12, "title": area.title, "error": format!("Could not read this area: {e}") })),
        }
    }
    if total == 0 && items.len() == 2 {
        items.push(json!({ "id": "clear", "width": 12, "card": { "kind": "text", "title": "All clear", "text": "Nothing is waiting on HR right now." } }));
    }
    Ok(json!({ "title": "Pending actions", "description": aether_sdk::dates::format_date(today), "refresh_secs": 600, "filters": [], "items": items }))
}

handler! {
    /// What HR has to chase today: expiring documents, late checklist tasks, unsigned letters, overdue cases.
    fn hr_actions_board(_: Option<Value>) -> Value {
        board()
    }
}
