//! Numbers about the organization.

use aether_sdk::dates::{self, Datelike, Duration, NaiveDate};
use aether_sdk::db::{Figure, Filter};
use aether_sdk::prelude::*;
use crate::rules::status;

use crate::common::{text, Record};

#[derive(Deserialize, Default)]
struct Scope {
    #[serde(default)]
    department: Option<String>,
}

/// Headcount by department and status, and by employment type.
fn headcount(scope: Scope) -> Result<Value> {
    let mut filter = Filter::all();
    if let Some(department) = scope.department {
        filter = filter.and(Filter::eq("department", department));
    }
    let by_department = db::aggregate("employee", filter.clone(), &["department", "status"], &[("people", Figure::Count)])?;
    let by_type = db::aggregate(
        "employee",
        filter.and(Filter::one_of("status", status::PRESENT.to_vec())),
        &["employment_type"],
        &[("people", Figure::Count)],
    )?;
    let present = db::count("employee", Filter::one_of("status", status::PRESENT.to_vec()))?;
    Ok(json!({ "present": present, "by_department_and_status": by_department, "by_employment_type": by_type }))
}

/// People whose probation ends within the next days.
fn probation_ending(days: u32) -> Result<Vec<Record>> {
    let today = crate::common::today()?;
    let limit = {
        let start = dates::parse_date(&today)?;
        dates::format_date(start + Duration::days(i64::from(days)))
    };
    db::find::<Record>("employee")
        .matching(
            Filter::eq("status", "probation")
                .and(Filter::gte("probation_end", today.as_str()))
                .and(Filter::lte("probation_end", limit)),
        )
        .order_by("probation_end")
        .limit(500)
        .all()
}

#[derive(Deserialize)]
struct Days {
    #[serde(default = "thirty")]
    days: u32,
}

fn thirty() -> u32 {
    30
}

/// Employees whose birthday falls in the next days (by month and day, any year). Birth dates are
/// private, so only those the caller is allowed to read are considered.
fn birthdays(days: u32) -> Result<Vec<Record>> {
    let today = dates::parse_date(&crate::common::today()?)?;
    let private: Vec<Record> = db::find::<Record>("employee_private").matching(Filter::is_set("date_of_birth")).limit(1000).all()?;
    let mut upcoming: Vec<(u32, Record)> = Vec::new();
    for details in private {
        let Some(born) = text(&details, "date_of_birth").and_then(|d| dates::parse_date(d).ok()) else { continue };
        let Some(employee_id) = text(&details, "employee") else { continue };
        let Some(employee) = db::get::<Record>("employee", employee_id)? else { continue };
        if !status::counts_in_headcount(text(&employee, "status").unwrap_or("active")) {
            continue;
        }
        // The next time the day comes round, counting today.
        let this_year = NaiveDate::from_ymd_opt(today.year(), born.month(), born.day()).or_else(|| NaiveDate::from_ymd_opt(today.year(), 3, 1));
        let Some(mut next) = this_year else { continue };
        if next < today {
            next = NaiveDate::from_ymd_opt(today.year() + 1, born.month(), born.day()).unwrap_or(next);
        }
        let away = (next - today).num_days();
        if (0..=i64::from(days)).contains(&away) {
            upcoming.push((away as u32, employee));
        }
    }
    upcoming.sort_by_key(|(away, _)| *away);
    Ok(upcoming.into_iter().map(|(_, employee)| employee).collect())
}


#[derive(Deserialize)]
struct Span {
    from: String,
    to: String,
    #[serde(default)]
    department: Option<String>,
}

/// Hires, leavers and turnover over a period: people in at the start, hired, left, in at the end, and
/// leavers as a percentage of the average headcount. Left means an end date inside the period.
fn turnover(span: Span) -> Result<Value> {
    let from = dates::parse_date(&span.from)?;
    let to = dates::parse_date(&span.to)?;
    if to < from {
        return Err(Error::msg("the period ends before it starts"));
    }
    let (from_text, to_text) = (dates::format_date(from), dates::format_date(to));
    let day_before = dates::format_date(from - Duration::days(1));
    let scope = |f: Filter| match &span.department {
        Some(d) => f.and(Filter::eq("department", d.as_str())),
        None => f,
    };
    let in_at = |day: &str| -> Result<i64> {
        // Hired on or before the day, and not ended before it.
        let hired = Filter::lte("hire_date", day);
        let still = Filter::is_set("end_date").not().or(Filter::gte("end_date", day));
        Ok(db::count("employee", scope(hired.and(still)))? as i64)
    };
    let opening = in_at(&day_before)?;
    let closing = in_at(&to_text)?;
    let hired = db::count("employee", scope(Filter::gte("hire_date", from_text.as_str()).and(Filter::lte("hire_date", to_text.as_str()))))? as i64;
    let left = db::count("employee", scope(Filter::gte("end_date", from_text.as_str()).and(Filter::lte("end_date", to_text.as_str()))))? as i64;
    let leavers_by_status = db::aggregate(
        "employee",
        scope(Filter::gte("end_date", from_text.as_str()).and(Filter::lte("end_date", to_text.as_str()))),
        &["status"],
        &[("people", Figure::Count)],
    )?;
    let average = (opening + closing) as f64 / 2.0;
    let rate = if average > 0.0 { (left as f64 / average * 10000.0).round() / 100.0 } else { 0.0 };
    Ok(json!({
        "from": from_text, "to": to_text, "opening": opening, "hired": hired, "left": left,
        "closing": closing, "average_headcount": average, "turnover_percent": rate,
        "leavers_by_status": leavers_by_status,
    }))
}

#[derive(Deserialize, Default)]
struct TurnoverExport {
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    department: Option<String>,
    #[serde(default)]
    format: Option<String>,
}

/// The turnover figures as Excel or PDF; the last twelve months when no period is given.
fn export_turnover_table(input: TurnoverExport) -> Result<Value> {
    let today = dates::parse_date(&crate::common::today()?)?;
    let to = input.to.clone().unwrap_or_else(|| dates::format_date(today));
    let from = input.from.clone().unwrap_or_else(|| dates::format_date(today - Duration::days(365)));
    let figures = turnover(Span { from: from.clone(), to: to.clone(), department: input.department })?;
    let row = |label: &str, key: &str| json!({ "metric": label, "value": figures.get(key).cloned().unwrap_or(Value::Null) });
    let format = if input.format.as_deref() == Some("pdf") { "pdf" } else { "xlsx" };
    report::export(&json!({
        "format": format,
        "title": format!("Turnover {from} to {to}"),
        "columns": [{ "field": "metric", "label": "Measure" }, { "field": "value", "label": "Value" }],
        "rows": [
            row("People at the start", "opening"), row("Hired", "hired"), row("Left", "left"),
            row("People at the end", "closing"), row("Average headcount", "average_headcount"),
            row("Turnover %", "turnover_percent"),
        ],
    }))
}

/// Turnover for a dashboard page: stat figures, the movement of people and leavers by status.
/// The period is the last twelve months unless `from` and `to` are in the page's parameters.
fn turnover_dashboard_data(input: TurnoverExport) -> Result<Value> {
    let today = dates::parse_date(&crate::common::today()?)?;
    let to = input.to.filter(|v| !v.is_empty()).unwrap_or_else(|| dates::format_date(today));
    let from = input.from.filter(|v| !v.is_empty()).unwrap_or_else(|| dates::format_date(today - Duration::days(365)));
    let department = input.department.filter(|v| !v.is_empty());
    let mut figures = turnover(Span { from: from.clone(), to: to.clone(), department })?;
    let number = |key: &str| figures.get(key).cloned().unwrap_or(json!(0));
    let movement = json!([
        { "name": "At start", "value": number("opening") },
        { "name": "Hired", "value": number("hired") },
        { "name": "Left", "value": number("left") },
        { "name": "At end", "value": number("closing") },
    ]);
    let leavers: Vec<Value> = figures
        .get("leavers_by_status")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|r| json!({ "name": r.get("status").cloned().unwrap_or(Value::Null), "value": r.get("people").cloned().unwrap_or(json!(0)) }))
                .collect()
        })
        .unwrap_or_default();
    figures["movement"] = movement;
    figures["leavers"] = json!(leavers);
    figures["period"] = json!(format!("{from} to {to}"));
    Ok(figures)
}

#[derive(Deserialize, Default)]
struct ChartRoot {
    /// Start from this person's team; the whole organisation (top-level managers) when left out.
    #[serde(default)]
    root: Option<String>,
    #[serde(default)]
    depth: Option<usize>,
}

/// The reporting structure as an indented list, depth first: each row has `depth`, and its `name` carries
/// the indent so a plain list reads as a chart. Only people still working here; at most 2000.
fn org_chart_rows(input: ChartRoot) -> Result<Vec<Value>> {
    let people: Vec<Record> = db::find::<Record>("employee")
        .matching(Filter::one_of("status", status::PRESENT.to_vec()))
        .order_by("display_name")
        .limit(2000)
        .all()?;
    let key = |v: &str| v.rsplit(':').next().unwrap_or(v).to_string();
    let mut children: std::collections::BTreeMap<String, Vec<usize>> = std::collections::BTreeMap::new();
    let mut tops = Vec::new();
    let present: std::collections::BTreeSet<String> = people.iter().filter_map(|p| text(p, "id").map(key)).collect();
    for (i, p) in people.iter().enumerate() {
        match text(p, "manager").map(key).filter(|m| present.contains(m)) {
            Some(m) => children.entry(m).or_default().push(i),
            None => tops.push(i),
        }
    }
    let max_depth = input.depth.unwrap_or(12).min(12);
    let mut out = Vec::new();
    let mut stack: Vec<(usize, usize)> = match &input.root {
        Some(root) => people.iter().position(|p| text(p, "id").map(key) == Some(key(root))).map(|i| vec![(i, 0)]).unwrap_or_default(),
        None => tops.iter().rev().map(|&i| (i, 0)).collect(),
    };
    while let Some((i, depth)) = stack.pop() {
        let p = &people[i];
        let name = text(p, "display_name").unwrap_or_default();
        let id = text(p, "id").unwrap_or_default();
        let reports = children.get(&key(id)).map_or(0, Vec::len);
        out.push(json!({
            "id": id, "depth": depth, "name": format!("{}{}", "\u{2003}".repeat(depth), name), "employee_no": p.get("employee_no"),
            "department": p.get("department"), "job": p.get("job"), "manager": p.get("manager"), "direct_reports": reports,
        }));
        if depth < max_depth {
            if let Some(kids) = children.get(&key(id)) {
                for &k in kids.iter().rev() {
                    stack.push((k, depth + 1));
                }
            }
        }
    }
    Ok(out)
}

handler! {
    fn org_chart(input: Option<ChartRoot>) -> Vec<Value> {
        org_chart_rows(input.unwrap_or_default())
    }

    fn turnover_dashboard(input: Option<TurnoverExport>) -> Value {
        turnover_dashboard_data(input.unwrap_or_default())
    }

    fn export_turnover(input: Option<TurnoverExport>) -> Value {
        export_turnover_table(input.unwrap_or_default())
    }

    fn turnover_report(input: Span) -> Value {
        turnover(input)
    }

    fn headcount_report(input: Scope) -> Value {
        headcount(input)
    }

    fn probation_ending_soon(input: Days) -> Vec<Record> {
        probation_ending(input.days)
    }

    fn upcoming_birthdays(input: Days) -> Vec<Record> {
        birthdays(input.days)
    }
}
