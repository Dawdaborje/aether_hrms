//! The role-aware landing page: everyone sees their own HR; a manager also sees their team; HR staff also see
//! the organisation. Which sections appear is decided here from who is asking, never from a setting, and each
//! figure is asked of the plugin that owns it, so a role missing there leaves the figure out.

use aether_sdk::dates;
use aether_sdk::prelude::*;

fn is_hr_staff() -> bool {
    context::current().map(|me| me.has_role("hr.hr_user") || me.has_role("hr.hr_manager")).unwrap_or(false)
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn reading(name: &str, label: &str, value: f64) -> Value {
    json!({ "name": name, "label": label, "value": value, "previous": null, "change": null })
}

fn card(id: &str, title: &str, readings: Vec<Value>, format: &str, info: &str) -> Value {
    json!({ "id": id, "width": 12, "card": { "kind": "number", "title": title, "format": format, "info": info, "readings": readings } })
}

fn text_card(id: &str, title: &str, body: &str) -> Value {
    json!({ "id": id, "width": 12, "card": { "kind": "text", "title": title, "text": body } })
}

/// The greeting and a line saying which hats the person wears.
fn header() -> Value {
    let me: Value = plugins::call("hr", "my_employee", &json!({})).unwrap_or(Value::Null);
    let name = me.get("display_name").and_then(Value::as_str);
    let mut hats = vec!["Employee"];
    // Only whether anyone reports to them: the figures behind the team card are not read for the greeting.
    if super::team::manages_people() {
        hats.push("Manager");
    }
    if is_hr_staff() {
        hats.push("HR");
    }
    json!({
        "title": name.map_or("Welcome".to_string(), |n| format!("Welcome, {n}")),
        "subtitle": if name.is_some() { hats.join(" · ") } else { "Your account is not linked to an employee record".to_string() },
    })
}

fn board() -> Result<Value> {
    let today = dates::parse_date(&aether_sdk::records::today()?)?;
    let mut items: Vec<Value> = Vec::new();

    // Everyone: their own HR.
    let mine = super::home()?;
    if mine["linked"] == json!(true) {
        let mut readings = vec![
            reading("waiting", "Waiting on you", mine["waiting_on_me"].as_f64().unwrap_or(0.0)),
            reading("tasks", "Your open tasks", mine["open_tasks"].as_f64().unwrap_or(0.0)),
            reading("requests", "Your leave requests", mine["my_leave_requests"].as_f64().unwrap_or(0.0)),
            reading("away", "Away today", mine["away_today"].as_f64().unwrap_or(0.0)),
        ];
        if let Some(net) = number(&mine["latest_net"]) {
            readings.push(reading("net", "Latest net pay", net));
        }
        items.push(card("me", "You", readings, "number", "Away today counts everyone with approved leave covering today."));
        let balances: Vec<(String, f64)> = mine["leave_balances"]
            .as_array()
            .map(|rows| rows.iter().filter_map(|b| Some((b["name"].as_str()?.to_string(), number(&b["value"])?))).collect())
            .unwrap_or_default();
        if !balances.is_empty() {
            items.push(json!({ "id": "balances", "width": 12, "card": {
                "kind": "bar", "title": "Leave available (days)",
                "categories": balances.iter().map(|b| b.0.clone()).collect::<Vec<_>>(), "keys": balances.iter().map(|b| b.0.clone()).collect::<Vec<_>>(),
                "series": [{ "name": "Days", "measure": "sum", "values": balances.iter().map(|b| b.1).collect::<Vec<_>>() }], "dimensions": ["leave_type"] } }));
        }
    } else {
        items.push(text_card("unlinked", "Link your account", "Your sign-in is not linked to an employee record, so personal figures cannot be shown. Ask HR to link it, or open My profile to link it yourself."));
    }

    // Managers: their team.
    if let Some(readings) = super::team::glance() {
        items.push(card("team", "Your team", readings, "number", "Direct reports only. Open My team for the whole line, who is away and what is coming up."));
    }

    // HR staff: the organisation.
    if is_hr_staff() {
        let mut readings = Vec::new();
        if let Ok(h) = plugins::call::<Value>("hr", "headcount_report", &json!({})) {
            if let Some(n) = h["present"].as_f64() {
                readings.push(reading("headcount", "Headcount", n));
            }
        }
        readings.extend(super::actions::counts(today));
        if !readings.is_empty() {
            items.push(card("hr", "Across the organisation", readings, "number", "Open Pending actions for the rows behind these counts and the checklist and training items."));
        }
    }
    Ok(json!({ "title": null, "filters": [], "refresh_secs": 300, "items": items }))
}

handler! {
    /// Greeting and roles for the landing page.
    fn landing_header(_: Option<Value>) -> Value {
        Ok(header())
    }

    /// The landing page's figures: yours, your team's if you manage people, the organisation's if you work in HR.
    fn landing_board(_: Option<Value>) -> Value {
        board()
    }
}
