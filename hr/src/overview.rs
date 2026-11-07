//! The HR overview board: headcount and its movement at a glance, with a period and department filter and
//! a drill-down from every chart to the people behind it.
//!
//! Everything here reads the plugin's own records, so it never asks another plugin. Cards that need leave,
//! attendance or payroll belong to those plugins' own boards.

use std::collections::BTreeMap;

use aether_sdk::dates::{self, Datelike, Duration, NaiveDate};
use aether_sdk::db::Filter;
use aether_sdk::prelude::*;

use crate::common::{text, Record};
use crate::rules::status;

/// At most this many people are read to draw the charts that group people in memory.
const PEOPLE_LIMIT: u32 = 5000;

const TENURE_BANDS: &[&str] = &["Under 1 year", "1 to 2 years", "2 to 5 years", "5 to 10 years", "10+ years"];

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

/// The period the person picked: a named one, a `{from, to}` range, or the last twelve months.
fn resolve_period(value: Option<&Value>, today: NaiveDate) -> (NaiveDate, NaiveDate) {
    let default = (today - Duration::days(364), today);
    let Some(value) = value else { return default };
    if let Some(range) = value.as_object() {
        let from = range.get("from").and_then(Value::as_str).and_then(|d| dates::parse_date(d).ok());
        let to = range.get("to").and_then(Value::as_str).and_then(|d| dates::parse_date(d).ok());
        return match (from, to) {
            (Some(from), Some(to)) if from <= to => (from, to),
            _ => default,
        };
    }
    let first_of = |year: i32, month: u32| NaiveDate::from_ymd_opt(year, month, 1).unwrap_or(today);
    let quarter_start = |d: NaiveDate| first_of(d.year(), (d.month() - 1) / 3 * 3 + 1);
    match value.as_str().unwrap_or_default() {
        "today" => (today, today),
        "last_7_days" => (today - Duration::days(6), today),
        "last_30_days" => (today - Duration::days(29), today),
        "last_90_days" => (today - Duration::days(89), today),
        "this_month" => (first_of(today.year(), today.month()), today),
        "last_month" => {
            let this = first_of(today.year(), today.month());
            let end = this - Duration::days(1);
            (first_of(end.year(), end.month()), end)
        }
        "this_quarter" => (quarter_start(today), today),
        "last_quarter" => {
            let this = quarter_start(today);
            let end = this - Duration::days(1);
            (quarter_start(end), end)
        }
        "this_year" | "year_to_date" => (first_of(today.year(), 1), today),
        "last_year" => (first_of(today.year() - 1, 1), first_of(today.year(), 1) - Duration::days(1)),
        _ => default,
    }
}

/// The chosen departments, if any.
fn chosen_departments(filters: Option<&Value>) -> Vec<String> {
    match filters.and_then(|f| f.get("department")) {
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).filter(|s| !s.is_empty()).map(String::from).collect(),
        Some(Value::String(one)) if !one.is_empty() => vec![one.clone()],
        _ => Vec::new(),
    }
}

fn in_departments(filter: Filter, departments: &[String]) -> Filter {
    if departments.is_empty() {
        filter
    } else {
        filter.and(Filter::one_of("department", departments.iter().map(String::as_str)))
    }
}

/// People on the books on a day: hired by then and not ended before it.
fn in_at(date: NaiveDate, departments: &[String]) -> Result<i64> {
    let day = day(date);
    let filter = Filter::lte("hire_date", day.as_str()).and(Filter::is_set("end_date").not().or(Filter::gte("end_date", day.as_str())));
    Ok(db::count("employee", in_departments(filter, departments))? as i64)
}

fn hired_between(from: NaiveDate, to: NaiveDate, departments: &[String]) -> Result<i64> {
    let filter = Filter::gte("hire_date", day(from)).and(Filter::lte("hire_date", day(to)));
    Ok(db::count("employee", in_departments(filter, departments))? as i64)
}

fn left_between(from: NaiveDate, to: NaiveDate, departments: &[String]) -> Result<i64> {
    let filter = Filter::gte("end_date", day(from)).and(Filter::lte("end_date", day(to)));
    Ok(db::count("employee", in_departments(filter, departments))? as i64)
}

fn turnover_percent(left: i64, opening: i64, closing: i64) -> f64 {
    let average = (opening + closing) as f64 / 2.0;
    if average > 0.0 {
        (left as f64 / average * 1000.0).round() / 10.0
    } else {
        0.0
    }
}

/// `(now - previous) / previous`, or nothing without a previous figure.
fn change(now: f64, previous: f64) -> Value {
    if previous == 0.0 {
        Value::Null
    } else {
        json!((now - previous) / previous)
    }
}

/// One reading of a number card. `more_is_good` says which way is good news; `None` means neither.
fn reading(name: &str, label: &str, now: f64, previous: f64, more_is_good: Option<bool>) -> Value {
    let tone = match more_is_good {
        _ if now == previous => "flat",
        Some(true) => if now > previous { "good" } else { "bad" },
        Some(false) => if now < previous { "good" } else { "bad" },
        None => "flat",
    };
    json!({ "name": name, "label": label, "value": now, "previous": previous, "change": change(now, previous), "tone": tone })
}

fn number_card(id: &str, title: &str, readings: Vec<Value>, format: &str, width: u32, info: &str) -> Value {
    json!({ "id": id, "width": width, "card": {
        "kind": "number", "title": title, "format": format, "info": info, "readings": readings, "compared": true,
    }})
}

/// A chart card with one series from `(name, key, value)` rows.
fn series_card(id: &str, kind: &str, title: &str, series_name: &str, dimension: &str, rows: &[(String, Value, f64)], width: u32) -> Value {
    json!({ "id": id, "chart": id, "width": width, "card": {
        "kind": kind, "title": title,
        "categories": rows.iter().map(|r| r.0.clone()).collect::<Vec<_>>(),
        "keys": rows.iter().map(|r| r.1.clone()).collect::<Vec<_>>(),
        "series": [{ "name": series_name, "measure": "count", "values": rows.iter().map(|r| r.2).collect::<Vec<_>>() }],
        "dimensions": [dimension],
    }})
}

fn department_names() -> Result<BTreeMap<String, String>> {
    let rows: Vec<Record> = db::find::<Record>("department").limit(2000).all()?;
    Ok(rows.iter().filter_map(|d| Some((key(text(d, "id")?).to_string(), text(d, "name")?.to_string()))).collect())
}

fn present_people(departments: &[String]) -> Result<Vec<Record>> {
    db::find::<Record>("employee")
        .matching(in_departments(Filter::one_of("status", status::PRESENT.to_vec()), departments))
        .order_by("display_name")
        .limit(PEOPLE_LIMIT)
        .all()
}

fn tenure_band(hired: NaiveDate, today: NaiveDate) -> usize {
    let months = dates::months_between(hired, today);
    match months {
        0..=11 => 0,
        12..=23 => 1,
        24..=59 => 2,
        60..=119 => 3,
        _ => 4,
    }
}

fn label_of(raw: &str) -> String {
    let mut chars = raw.replace('_', " ").chars().collect::<Vec<_>>();
    if let Some(first) = chars.first_mut() {
        *first = first.to_ascii_uppercase();
    }
    chars.into_iter().collect()
}

/// Counts of people grouped by a text field, largest first, with the group's key kept for the drill-down.
fn count_by(people: &[Record], field: &str, names: Option<&BTreeMap<String, String>>) -> Vec<(String, Value, f64)> {
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    for p in people {
        *counts.entry(text(p, field).unwrap_or_default().to_string()).or_default() += 1;
    }
    let mut rows: Vec<(String, Value, f64)> = counts
        .into_iter()
        .map(|(raw, n)| {
            let label = if raw.is_empty() {
                "Not set".to_string()
            } else {
                match names {
                    Some(map) => map.get(key(&raw)).cloned().unwrap_or_else(|| raw.clone()),
                    None => label_of(&raw),
                }
            };
            (label, json!(raw), n as f64)
        })
        .collect();
    rows.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    rows
}

fn month_start(date: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(date.year(), date.month(), 1).unwrap_or(date)
}

fn month_label(date: NaiveDate) -> String {
    const NAMES: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{} {}", NAMES[(date.month() - 1) as usize], date.year() % 100)
}

/// The most months the month-by-month charts draw.
const MAX_MONTHS: usize = 24;

/// The months a period touches, oldest first. A longer period keeps its latest `MAX_MONTHS`, so the charts end where
/// the period ends.
fn months_in(from: NaiveDate, to: NaiveDate) -> Vec<(NaiveDate, NaiveDate)> {
    let mut months = Vec::new();
    let mut start = month_start(from);
    while start <= to {
        let next = dates::add_months(start, 1);
        let end = (next - Duration::days(1)).min(to);
        months.push((start.max(from), end));
        start = next;
    }
    if months.len() > MAX_MONTHS {
        months.drain(..months.len() - MAX_MONTHS);
    }
    months
}

/// Does the period touch more months than the charts draw?
fn months_clipped(from: NaiveDate, to: NaiveDate) -> bool {
    dates::add_months(month_start(from), MAX_MONTHS as u32) <= to
}

fn board(input: BoardInput) -> Result<Value> {
    let today = dates::parse_date(&crate::common::today()?)?;
    let (from, to) = resolve_period(input.filters.as_ref().and_then(|f| f.get("period")), today);
    let departments = chosen_departments(input.filters.as_ref());
    let length = (to - from).num_days() + 1;
    let (prior_from, prior_to) = (from - Duration::days(length), from - Duration::days(1));

    let names = department_names()?;
    let people = present_people(&departments)?;

    // Headline figures against the period just before.
    let (opening, closing) = (in_at(from - Duration::days(1), &departments)?, in_at(to, &departments)?);
    let (prior_opening, _) = (in_at(prior_from - Duration::days(1), &departments)?, ());
    let (hired, left) = (hired_between(from, to, &departments)?, left_between(from, to, &departments)?);
    let (prior_hired, prior_left) = (hired_between(prior_from, prior_to, &departments)?, left_between(prior_from, prior_to, &departments)?);
    let rate = turnover_percent(left, opening, closing);
    let prior_rate = turnover_percent(prior_left, prior_opening, opening);

    let on_probation = people.iter().filter(|p| text(p, "status") == Some("probation")).count() as f64;
    let ending_soon = {
        let limit = day(today + Duration::days(30));
        people
            .iter()
            .filter(|p| text(p, "status") == Some("probation") && text(p, "probation_end").is_some_and(|d| d >= day(today).as_str() && d <= limit.as_str()))
            .count() as f64
    };

    let headline = number_card(
        "headline",
        "People",
        vec![
            reading("headcount", "Headcount", closing as f64, opening as f64, None),
            reading("hired", "Hired", hired as f64, prior_hired as f64, None),
            reading("left", "Left", left as f64, prior_left as f64, Some(false)),
            reading("turnover", "Turnover %", rate, prior_rate, Some(false)),
        ],
        "number",
        12,
        "Headcount compares the end of the period with its start. Hired and left compare with the period of the same length just before. Turnover is leavers as a percentage of the average headcount.",
    );
    let watch = number_card(
        "watch",
        "To watch",
        vec![
            json!({ "name": "probation", "label": "On probation", "value": on_probation, "previous": null, "change": null }),
            json!({ "name": "ending", "label": "Probation ends within 30 days", "value": ending_soon, "previous": null, "change": null }),
        ],
        "number",
        12,
        "Counts today, not for the chosen period.",
    );

    // Headcount and movement, month by month.
    let months = months_in(from, to);
    // Say so when the period is longer than the charts draw; the figures above still cover all of it.
    let clipped = if months_clipped(from, to) { format!(" (latest {MAX_MONTHS} months)") } else { String::new() };
    let mut trend: Vec<(String, Value, f64)> = Vec::new();
    let (mut hires, mut leavers) = (Vec::new(), Vec::new());
    for (start, end) in &months {
        let label = month_label(*start);
        trend.push((label.clone(), json!(day(*start)), in_at(*end, &departments)? as f64));
        hires.push(hired_between(*start, *end, &departments)? as f64);
        leavers.push(left_between(*start, *end, &departments)? as f64);
    }
    let categories: Vec<String> = months.iter().map(|(s, _)| month_label(*s)).collect();
    let keys: Vec<Value> = months.iter().map(|(s, _)| json!(day(*s))).collect();
    let headcount_trend = json!({ "id": "trend", "chart": "trend", "width": 6, "card": {
        "kind": "line", "title": format!("Headcount by month{clipped}"), "categories": categories, "keys": keys,
        "series": [{ "name": "People at month end", "measure": "count", "values": trend.iter().map(|r| r.2).collect::<Vec<_>>() }],
        "dimensions": ["month"],
    }});
    let movement = json!({ "id": "movement", "chart": "movement", "width": 6, "card": {
        "kind": "bar", "title": format!("Hired and left by month{clipped}"), "categories": categories_of(&months), "keys": months.iter().map(|(s, _)| json!(day(*s))).collect::<Vec<_>>(),
        "series": [
            { "name": "Hired", "measure": "count", "values": hires },
            { "name": "Left", "measure": "count", "values": leavers },
        ],
        "dimensions": ["month"],
    }});

    // Who works here now.
    let by_department = count_by(&people, "department", Some(&names));
    let by_type = count_by(&people, "employment_type", None);
    let by_gender = count_by(&people, "gender", None);
    let mut tenure = vec![0.0; TENURE_BANDS.len()];
    for p in &people {
        if let Some(hired_on) = text(p, "hire_date").and_then(|d| dates::parse_date(d).ok()) {
            tenure[tenure_band(hired_on, today)] += 1.0;
        }
    }
    let tenure_rows: Vec<(String, Value, f64)> = TENURE_BANDS.iter().zip(&tenure).map(|(band, n)| (band.to_string(), json!(band), *n)).collect();

    // People to look at: probation ending soonest.
    let mut probation: Vec<&Record> = people
        .iter()
        .filter(|p| text(p, "status") == Some("probation") && text(p, "probation_end").is_some())
        .collect();
    probation.sort_by_key(|p| text(p, "probation_end").unwrap_or_default().to_string());
    let probation_rows: Vec<Value> = probation
        .iter()
        .take(10)
        .map(|p| {
            let department = text(p, "department").and_then(|d| names.get(key(d)).cloned()).unwrap_or_default();
            json!({ "cells": {
                "name": text(p, "display_name").unwrap_or_default(), "department": department,
                "ends": text(p, "probation_end").unwrap_or_default(),
            }, "keys": { "id": text(p, "id") } })
        })
        .collect();
    let probation_card = json!({ "id": "probation", "width": 12, "card": {
        "kind": "table", "title": "Probation ending soonest",
        "columns": [
            { "key": "name", "label": "Employee", "type": "dimension" },
            { "key": "department", "label": "Department", "type": "dimension" },
            { "key": "ends", "label": "Probation ends", "type": "dimension" },
        ],
        "rows": probation_rows,
    }});

    // The stored (full) id is the value: a bare key is not a valid link and the query refuses it.
    let department_rows: Vec<Record> = db::find::<Record>("department").order_by("name").limit(2000).all()?;
    let department_options: Vec<Value> = department_rows
        .iter()
        .filter_map(|d| Some(json!({ "value": text(d, "id")?, "label": text(d, "name")? })))
        .collect();
    let selected_departments = input.filters.as_ref().and_then(|f| f.get("department")).cloned().unwrap_or(json!([]));
    let period_value = input.filters.as_ref().and_then(|f| f.get("period")).cloned().unwrap_or(json!({ "from": day(from), "to": day(to) }));

    let mut items = vec![headline, watch];
    items.push(headcount_trend);
    items.push(movement);
    items.push(series_card("department", "row", "Headcount by department", "People", "department", &by_department, 6));
    items.push(series_card("type", "donut", "Employment type", "People", "employment_type", &by_type, 3));
    items.push(series_card("gender", "donut", "Gender", "People", "gender", &by_gender, 3));
    items.push(series_card("tenure", "bar", "Length of service", "People", "tenure", &tenure_rows, 12));
    items.push(probation_card);

    Ok(json!({
        "title": "HR overview",
        "description": format!("{} to {}", day(from), day(to)),
        "refresh_secs": 300,
        "filters": [
            { "name": "period", "label": "Period", "kind": "period", "value": period_value },
            { "name": "department", "label": "Department", "kind": "select", "value": selected_departments, "options": department_options },
        ],
        "items": items,
    }))
}

fn categories_of(months: &[(NaiveDate, NaiveDate)]) -> Vec<String> {
    months.iter().map(|(s, _)| month_label(*s)).collect()
}

/// The people behind a bar, slice or row, as a table with paging.
fn drill(input: DrillInput) -> Result<Value> {
    let today = dates::parse_date(&crate::common::today()?)?;
    let departments = chosen_departments(input.filters.as_ref());
    let names = department_names()?;
    let segment = input.segment.unwrap_or(Value::Null);
    let value_of = |field: &str| segment.get(field).and_then(Value::as_str).map(String::from);

    let mut filter = Filter::one_of("status", status::PRESENT.to_vec());
    let mut band: Option<String> = None;
    let mut month: Option<NaiveDate> = None;
    match input.chart.as_deref().unwrap_or_default() {
        "department" => {
            if let Some(v) = value_of("department") {
                filter = if v.is_empty() { filter.and(Filter::is_set("department").not()) } else { filter.and(Filter::eq("department", v)) };
            }
        }
        "type" => {
            if let Some(v) = value_of("employment_type") {
                filter = filter.and(Filter::eq("employment_type", v));
            }
        }
        "gender" => {
            if let Some(v) = value_of("gender") {
                filter = if v.is_empty() { filter.and(Filter::is_set("gender").not()) } else { filter.and(Filter::eq("gender", v)) };
            }
        }
        "tenure" => band = value_of("tenure"),
        "trend" | "movement" => month = value_of("month").and_then(|d| dates::parse_date(&d).ok()),
        _ => {}
    }
    if let Some(start) = month {
        // The people hired or leaving in that month; the headcount line shows who was there at its end.
        let end = dates::add_months(start, 1) - Duration::days(1);
        let hired = Filter::gte("hire_date", day(start)).and(Filter::lte("hire_date", day(end)));
        let left = Filter::gte("end_date", day(start)).and(Filter::lte("end_date", day(end)));
        filter = if input.chart.as_deref() == Some("trend") {
            Filter::lte("hire_date", day(end)).and(Filter::is_set("end_date").not().or(Filter::gte("end_date", day(end))))
        } else {
            hired.or(left)
        };
    }
    let mut rows: Vec<Record> = db::find::<Record>("employee").matching(in_departments(filter, &departments)).order_by("display_name").limit(PEOPLE_LIMIT).all()?;
    if let Some(band) = band {
        rows.retain(|p| {
            text(p, "hire_date")
                .and_then(|d| dates::parse_date(d).ok())
                .is_some_and(|d| TENURE_BANDS[tenure_band(d, today)] == band)
        });
    }
    let total = rows.len();
    let (limit, offset) = (input.limit.unwrap_or(25).clamp(1, 200), input.offset.unwrap_or(0));
    let page: Vec<Value> = rows
        .iter()
        .skip(offset)
        .take(limit)
        .map(|p| {
            let department = text(p, "department").and_then(|d| names.get(key(d)).cloned()).unwrap_or_default();
            json!({ "id": text(p, "id"), "cells": {
                "employee_no": text(p, "employee_no").unwrap_or_default(),
                "name": text(p, "display_name").unwrap_or_default(), "department": department,
                "status": label_of(text(p, "status").unwrap_or_default()),
                "hired": text(p, "hire_date").unwrap_or_default(),
            }})
        })
        .collect();
    Ok(json!({
        "columns": [
            { "key": "employee_no", "label": "No." }, { "key": "name", "label": "Employee" },
            { "key": "department", "label": "Department" }, { "key": "status", "label": "Status" },
            { "key": "hired", "label": "Hired" },
        ],
        "rows": page, "total": total, "limit": limit, "offset": offset,
    }))
}

handler! {
    /// The HR overview board: headcount and movement for a period and departments.
    fn hr_overview_board(input: Option<BoardInput>) -> Value {
        crate::common::require_hr_staff()?;
        board(input.unwrap_or_default())
    }

    /// The people behind one bar, slice or row of the HR overview.
    fn hr_overview_drill(input: Option<DrillInput>) -> Value {
        crate::common::require_hr_staff()?;
        drill(input.unwrap_or_default())
    }
}
