//! The manager's team board: who is in, who is away, what waits on the manager, and who needs a word soon.
//!
//! Nothing is stored here. Each figure is asked of the plugin that owns it (`hr` for the people, `hr_leave`
//! for absences, `hr_attendance` for who has punched in), as the manager looking at the page, so what a
//! manager can see is decided where the data lives. A plugin that cannot answer leaves its figure out
//! instead of blanking the board.

use std::collections::{BTreeMap, BTreeSet};

use aether_sdk::dates::{self, Datelike, Duration, NaiveDate};
use aether_sdk::prelude::*;
use aether_sdk::records::{text, Record};

/// Most people on one team board.
const TEAM_LIMIT: usize = 500;
/// How far ahead absences and events are listed.
const AWAY_DAYS: i64 = 14;
const EVENT_DAYS: i64 = 30;
const PRESENT: &[&str] = &["active", "probation", "on_leave", "suspended"];

#[derive(Deserialize, Default)]
struct BoardInput {
    #[serde(default)]
    filters: Option<Value>,
}

#[derive(Deserialize, Default)]
struct DrillInput {
    #[serde(default)]
    chart: Option<String>,
    #[serde(default)]
    segment: Option<Value>,
    #[serde(default)]
    filters: Option<Value>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
}

fn key(v: &str) -> &str {
    v.rsplit(':').next().unwrap_or(v)
}

fn day(date: NaiveDate) -> String {
    dates::format_date(date)
}

fn label_of(raw: &str) -> String {
    let mut chars: Vec<char> = raw.replace('_', " ").chars().collect();
    if let Some(first) = chars.first_mut() {
        *first = first.to_ascii_uppercase();
    }
    chars.into_iter().collect()
}

/// Ask another plugin; a failure becomes `null`.
fn ask(plugin: &str, function: &str, payload: &Value) -> Value {
    plugins::call::<Value>(plugin, function, payload).unwrap_or(Value::Null)
}

fn whole_team(filters: Option<&Value>) -> bool {
    match filters.and_then(|f| f.get("scope")) {
        Some(Value::Array(list)) => list.iter().any(|v| v.as_str() == Some("all")),
        Some(Value::String(one)) => one == "all",
        _ => false,
    }
}

/// People who report to `manager` and still work here, at most `TEAM_LIMIT`. Former employees can fill a page of
/// matches, so pages are read until enough present reports are found or the matches run out.
fn reports_of(manager: &str) -> Vec<Record> {
    const PAGE: usize = 500;
    const MAX_PAGES: usize = 20;
    let mut present: Vec<Record> = Vec::new();
    let mut offset = 0;
    for _ in 0..MAX_PAGES {
        let page = plugins::call::<Vec<Record>>("hr", "find_employees", &json!({ "manager": manager, "limit": PAGE, "offset": offset })).unwrap_or_default();
        let fetched = page.len();
        present.extend(page.into_iter().filter(|e| PRESENT.contains(&text(e, "status").unwrap_or("active"))));
        if fetched < PAGE || present.len() >= TEAM_LIMIT {
            break;
        }
        offset += fetched;
    }
    present.truncate(TEAM_LIMIT);
    present
}

/// Does anyone report to the caller (and still work here)? The same answer as `glance().is_some()`, without reading
/// attendance or leave for the figures.
pub fn manages_people() -> bool {
    // `glance` gives nothing when today cannot be read; so does this.
    if aether_sdk::records::today().ok().and_then(|d| dates::parse_date(&d).ok()).is_none() {
        return false;
    }
    let me: Value = ask("hr", "my_employee", &json!({}));
    me.get("id").and_then(Value::as_str).is_some_and(|id| !team(id, false).is_empty())
}

/// The manager's direct reports, or everyone below them when `deep`.
fn team(me: &str, deep: bool) -> Vec<Record> {
    let mut found: Vec<Record> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    seen.insert(key(me).to_string());
    let mut level = vec![me.to_string()];
    for _ in 0..(if deep { 8 } else { 1 }) {
        let mut next = Vec::new();
        for manager in &level {
            for person in reports_of(manager) {
                let Some(id) = text(&person, "id") else { continue };
                if seen.insert(key(id).to_string()) {
                    next.push(id.to_string());
                    found.push(person);
                }
            }
            if found.len() >= TEAM_LIMIT {
                return found;
            }
        }
        if next.is_empty() {
            break;
        }
        level = next;
    }
    found
}

fn reading(name: &str, label: &str, value: f64) -> Value {
    json!({ "name": name, "label": label, "value": value, "previous": null, "change": null })
}

fn table(id: &str, title: &str, columns: &[(&str, &str)], rows: Vec<Value>, width: u32) -> Value {
    json!({ "id": id, "width": width, "card": {
        "kind": "table", "title": title,
        "columns": columns.iter().map(|(k, l)| json!({ "key": k, "label": l, "type": "dimension" })).collect::<Vec<_>>(),
        "rows": rows,
    }})
}

fn row(cells: Value) -> Value {
    json!({ "cells": cells, "keys": {} })
}

fn chart(id: &str, kind: &str, title: &str, dimension: &str, rows: &[(String, Value, f64)], width: u32) -> Value {
    json!({ "id": id, "chart": id, "width": width, "card": {
        "kind": kind, "title": title,
        "categories": rows.iter().map(|r| r.0.clone()).collect::<Vec<_>>(),
        "keys": rows.iter().map(|r| r.1.clone()).collect::<Vec<_>>(),
        "series": [{ "name": "People", "measure": "count", "values": rows.iter().map(|r| r.2).collect::<Vec<_>>() }],
        "dimensions": [dimension],
    }})
}

/// The next time a yearly date comes round, counting today; Feb 29 moves to 1 March in other years.
fn next_yearly(date: NaiveDate, today: NaiveDate) -> NaiveDate {
    let on = |year: i32| NaiveDate::from_ymd_opt(year, date.month(), date.day()).or_else(|| NaiveDate::from_ymd_opt(year, 3, 1)).unwrap_or(today);
    let this_year = on(today.year());
    if this_year < today { on(today.year() + 1) } else { this_year }
}

struct Today {
    /// Employee key → what they are doing now: `in`, `out`, or absent.
    punched: BTreeMap<String, String>,
    /// Whether the attendance plugin answered at all.
    attendance: bool,
    away: BTreeSet<String>,
    away_rows: Vec<Value>,
}

fn today_state(ids: &BTreeSet<String>, today: NaiveDate) -> Today {
    let presence = ask("hr_attendance", "presence_today", &json!({}));
    let attendance = presence.is_array();
    let punched = presence
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| Some((key(r.get("employee")?.as_str()?).to_string(), r.get("state")?.as_str()?.to_string())))
                .filter(|(who, _)| ids.contains(who))
                .collect()
        })
        .unwrap_or_default();
    let window = ask("hr_leave", "who_is_away_between", &json!({ "from": day(today), "to": day(today + Duration::days(AWAY_DAYS)) }));
    let type_names: BTreeMap<String, String> = ask("hr_leave", "list_leave_types", &json!({}))
        .as_array()
        .map(|rows| rows.iter().filter_map(|t| Some((key(t.get("id")?.as_str()?).to_string(), t.get("name")?.as_str()?.to_string()))).collect())
        .unwrap_or_default();
    let mut away = BTreeSet::new();
    let mut away_rows = Vec::new();
    for r in window.as_array().cloned().unwrap_or_default() {
        let Some(who) = r.get("employee").and_then(Value::as_str).map(key) else { continue };
        if !ids.contains(who) {
            continue;
        }
        let (start, end) = (r["start_date"].as_str().unwrap_or_default(), r["end_date"].as_str().unwrap_or_default());
        if start <= day(today).as_str() && end >= day(today).as_str() {
            away.insert(who.to_string());
        }
        away_rows.push(row(json!({
            "name": r["name"], "type": r["leave_type"].as_str().map(key).and_then(|k| type_names.get(k).cloned()).unwrap_or_default(),
            "from": start, "to": end, "days": r["days"].to_string().trim_matches('"'),
        })));
    }
    Today { punched, attendance, away, away_rows }
}

/// Where a person is right now.
fn where_now(id: &str, t: &Today) -> &'static str {
    if t.away.contains(key(id)) {
        "away"
    } else {
        match t.punched.get(key(id)).map(String::as_str) {
            Some("in") => "in",
            Some(_) => "out",
            None => "not_in",
        }
    }
}

fn board(input: BoardInput) -> Result<Value> {
    let today = dates::parse_date(&aether_sdk::records::today()?)?;
    let scope = if whole_team(input.filters.as_ref()) { "all" } else { "direct" };
    let filters = json!([{ "name": "scope", "label": "Team", "kind": "select", "value": [scope],
        "options": [{ "value": "direct", "label": "Direct reports" }, { "value": "all", "label": "Everyone below me" }] }]);
    let me: Value = ask("hr", "my_employee", &json!({}));
    let Some(my_id) = me.get("id").and_then(Value::as_str) else {
        return Ok(json!({ "title": "My team", "filters": [], "items": [{ "id": "none", "width": 12, "card": {
            "kind": "text", "title": "No team", "text": "Your account is not linked to an employee record, so there is no team to show." } }] }));
    };
    let people = team(my_id, scope == "all");
    if people.is_empty() {
        return Ok(json!({ "title": "My team", "filters": filters, "items": [{ "id": "none", "width": 12, "card": {
            "kind": "text", "title": "No team", "text": "Nobody reports to you." } }] }));
    }
    let ids: BTreeSet<String> = people.iter().filter_map(|p| text(p, "id")).map(|i| key(i).to_string()).collect();
    let names: BTreeMap<String, String> = people.iter().filter_map(|p| Some((key(text(p, "id")?).to_string(), text(p, "display_name")?.to_string()))).collect();
    let t = today_state(&ids, today);
    let waiting = super::inbox().map(|v| v["rows"].as_array().cloned().unwrap_or_default()).unwrap_or_default();

    let count_where = |state: &str| people.iter().filter(|p| where_now(text(p, "id").unwrap_or_default(), &t) == state).count() as f64;
    let mut readings = vec![reading("team", "People", people.len() as f64), reading("away", "Away today", t.away.len() as f64)];
    if t.attendance {
        readings.push(reading("in", "In now", count_where("in")));
    }
    readings.push(reading("probation", "On probation", people.iter().filter(|p| text(p, "status") == Some("probation")).count() as f64));
    readings.push(reading("waiting", "Waiting on you", waiting.len() as f64));
    let headline = json!({ "id": "headline", "width": 12, "card": { "kind": "number", "title": "Today", "format": "number", "readings": readings,
        "info": "Away counts approved leave that covers today. In now counts people who have punched in and not out; it is left out when attendance is not set up." } });

    let mut today_rows = vec![("In".to_string(), json!("in"), count_where("in")), ("Away".to_string(), json!("away"), count_where("away")),
        ("Out for the day".to_string(), json!("out"), count_where("out")), ("Not in yet".to_string(), json!("not_in"), count_where("not_in"))];
    if !t.attendance {
        today_rows = vec![("Away".to_string(), json!("away"), count_where("away")), ("Working".to_string(), json!("not_in"), people.len() as f64 - t.away.len() as f64)];
    }
    let mut by_status: BTreeMap<String, f64> = BTreeMap::new();
    for p in &people {
        *by_status.entry(text(p, "status").unwrap_or("active").to_string()).or_default() += 1.0;
    }
    let status_rows: Vec<(String, Value, f64)> = by_status.into_iter().map(|(s, n)| (label_of(&s), json!(s), n)).collect();

    // The inbox is the caller's, not the team's: a finance user can be asked to approve someone outside the team. Those
    // names are looked up, so a raw id is shown only when HR will not say who it is.
    let mut outside: BTreeMap<String, String> = BTreeMap::new();
    for r in waiting.iter().take(10) {
        let Some(who) = r["employee"].as_str().map(key) else { continue };
        if names.contains_key(who) || outside.contains_key(who) {
            continue;
        }
        let found = ask("hr", "get_employee", &json!({ "id": who }));
        if let Some(name) = found.get("display_name").and_then(Value::as_str) {
            outside.insert(who.to_string(), name.to_string());
        }
    }
    let inbox_rows: Vec<Value> = waiting
        .iter()
        .take(10)
        .map(|r| {
            let who = r["employee"].as_str().map(key).unwrap_or_default();
            let name = names.get(who).or_else(|| outside.get(who)).cloned().unwrap_or_else(|| who.to_string());
            row(json!({ "kind": r["kind"], "employee": name, "when": r["when"], "detail": r["detail"] }))
        })
        .collect();

    // Probation endings and yearly milestones in the next days, soonest first.
    let limit = today + Duration::days(EVENT_DAYS);
    let mut events: Vec<(NaiveDate, String, String)> = Vec::new();
    for p in &people {
        let name = text(p, "display_name").unwrap_or_default().to_string();
        if let Some(end) = text(p, "probation_end").and_then(|d| dates::parse_date(d).ok()) {
            if text(p, "status") == Some("probation") && end >= today && end <= limit {
                events.push((end, name.clone(), "Probation ends".into()));
            }
        }
        if let Some(hired) = text(p, "hire_date").and_then(|d| dates::parse_date(d).ok()) {
            let next = next_yearly(hired, today);
            let years = next.year() - hired.year();
            if years >= 1 && next <= limit {
                events.push((next, name, format!("{years} {} with us", if years == 1 { "year" } else { "years" })));
            }
        }
    }
    events.sort();
    let event_rows: Vec<Value> = events.iter().take(15).map(|(d, n, what)| row(json!({ "name": n, "what": what, "date": day(*d) }))).collect();

    Ok(json!({
        "title": "My team",
        "description": format!("{} people · {}", people.len(), day(today)),
        "refresh_secs": 300,
        "filters": filters,
        "items": [
            headline,
            chart("today", "donut", "Where the team is today", "today", &today_rows, 4),
            chart("status", "bar", "By status", "status", &status_rows, 4),
            table("waiting", "Waiting on you", &[("kind", "Kind"), ("employee", "Employee"), ("when", "Date"), ("detail", "Detail")], inbox_rows, 12),
            table("away", &format!("Away in the next {AWAY_DAYS} days"), &[("name", "Employee"), ("type", "Leave"), ("from", "From"), ("to", "To"), ("days", "Days")], t.away_rows.clone(), 6),
            table("events", &format!("Coming up in {EVENT_DAYS} days"), &[("name", "Employee"), ("what", "What"), ("date", "Date")], event_rows, 6),
        ],
    }))
}

/// The team members behind a slice or bar.
fn drill(input: DrillInput) -> Result<Value> {
    let today = dates::parse_date(&aether_sdk::records::today()?)?;
    let me: Value = ask("hr", "my_employee", &json!({}));
    let Some(my_id) = me.get("id").and_then(Value::as_str) else { return Ok(json!({ "columns": [], "rows": [], "total": 0, "limit": 25, "offset": 0 })) };
    let mut people = team(my_id, whole_team(input.filters.as_ref()));
    let ids: BTreeSet<String> = people.iter().filter_map(|p| text(p, "id")).map(|i| key(i).to_string()).collect();
    let t = today_state(&ids, today);
    let wanted = |field: &str| input.segment.as_ref().and_then(|s| s.get(field)).and_then(Value::as_str).map(String::from);
    match input.chart.as_deref() {
        Some("status") => {
            if let Some(s) = wanted("status") {
                people.retain(|p| text(p, "status") == Some(s.as_str()));
            }
        }
        Some("today") => {
            if let Some(s) = wanted("today") {
                people.retain(|p| where_now(text(p, "id").unwrap_or_default(), &t) == s);
            }
        }
        _ => {}
    }
    let total = people.len();
    let (limit, offset) = (input.limit.unwrap_or(25).clamp(1, 200), input.offset.unwrap_or(0));
    let rows: Vec<Value> = people
        .iter()
        .skip(offset)
        .take(limit)
        .map(|p| json!({ "id": text(p, "id"), "cells": {
            "name": text(p, "display_name").unwrap_or_default(), "status": label_of(text(p, "status").unwrap_or_default()),
            "now": label_of(where_now(text(p, "id").unwrap_or_default(), &t)), "email": text(p, "work_email").unwrap_or_default(),
        }}))
        .collect();
    Ok(json!({
        "columns": [{ "key": "name", "label": "Employee" }, { "key": "status", "label": "Status" }, { "key": "now", "label": "Today" }, { "key": "email", "label": "Email" }],
        "rows": rows, "total": total, "limit": limit, "offset": offset,
    }))
}

/// Headline readings for the caller's direct reports, or nothing when nobody reports to them.
pub fn glance() -> Option<Vec<Value>> {
    let today = dates::parse_date(&aether_sdk::records::today().ok()?).ok()?;
    let me: Value = ask("hr", "my_employee", &json!({}));
    let people = team(me.get("id")?.as_str()?, false);
    if people.is_empty() {
        return None;
    }
    let ids: BTreeSet<String> = people.iter().filter_map(|p| text(p, "id")).map(|i| key(i).to_string()).collect();
    let t = today_state(&ids, today);
    let mut out = vec![reading("team", "People", people.len() as f64), reading("away", "Away today", t.away.len() as f64)];
    if t.attendance {
        out.push(reading("in", "In now", people.iter().filter(|p| where_now(text(p, "id").unwrap_or_default(), &t) == "in").count() as f64));
    }
    out.push(reading("probation", "On probation", people.iter().filter(|p| text(p, "status") == Some("probation")).count() as f64));
    Some(out)
}

handler! {
    /// The caller's team at a glance: who is in or away, what waits, what is coming up.
    fn team_board(input: Option<BoardInput>) -> Value {
        board(input.unwrap_or_default())
    }

    /// The team members behind one slice or bar of the team board.
    fn team_board_drill(input: Option<DrillInput>) -> Value {
        drill(input.unwrap_or_default())
    }
}
