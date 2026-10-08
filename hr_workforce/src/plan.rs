//! Staffing plans: how many hires into which positions over a period, and at what budget.
//!
//! Frappe checks overlap against other submitted plans but keeps the numbers (current count, open jobs) frozen on
//! the plan row at save time. Here the plan holds only intent (planned hires, cost per hire); what has happened
//! since is always read live from the requisitions that drew on it. A plan is edited only while it is a draft, is
//! approved by someone other than its maker, and two approved plans never cover one position on the same day.

use aether_sdk::dates::{check_order, format_date, parse_date, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{actor, decimal_of, id_of, require, require_admin, text, Record};
use crate::rules::{periods_overlap, plan_room};

#[derive(Deserialize)]
struct NewPlan {
    name: String,
    period_from: String,
    period_to: String,
    #[serde(default)]
    department: Option<String>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    lines: Vec<Record>,
}

#[derive(Deserialize)]
struct Lines {
    id: String,
    lines: Vec<Record>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

fn check_lines(lines: &[Record]) -> Result<()> {
    if lines.is_empty() {
        return Err(Error::msg("a plan needs at least one line"));
    }
    let mut seen: Vec<&str> = Vec::new();
    for line in lines {
        let position = text(line, "position").ok_or_else(|| Error::msg("every line names a position"))?;
        if seen.contains(&position) {
            return Err(Error::msg("a position appears once in a plan: add the numbers together"));
        }
        seen.push(position);
        let hires = line.get("planned_hires").and_then(Value::as_i64).unwrap_or(0);
        if !(1..=10_000).contains(&hires) {
            return Err(Error::msg("planned hires are 1 to 10000 a line"));
        }
        if decimal_of(line, "cost_per_hire")?.is_some_and(|c| c.is_negative()) {
            return Err(Error::msg("a cost cannot be negative"));
        }
        let found: Option<Record> = plugins::call("hr", "get_position", &json!({ "id": position }))?;
        if found.is_none() {
            return Err(Error::msg(format!("there is no position `{position}`")));
        }
    }
    Ok(())
}

fn write_lines(plan: &str, lines: &[Record]) -> Result<()> {
    for line in lines {
        let mut data = crate::common::pick(line, &["position", "planned_hires", "cost_per_hire", "notes"]);
        data["plan"] = json!(plan);
        db::create::<Record>("wf_plan_line", &data)?;
    }
    Ok(())
}

fn make(input: NewPlan) -> Result<Record> {
    require_admin()?;
    let (from, to) = (parse_date(&input.period_from)?, parse_date(&input.period_to)?);
    check_order(from, Some(to), "plan")?;
    check_lines(&input.lines)?;
    let mut data = json!({ "name": input.name, "period_from": format_date(from), "period_to": format_date(to), "state": "draft", "created_by": actor()? });
    for (key, value) in [("department", &input.department), ("currency", &input.currency), ("notes", &input.notes)] {
        if let Some(value) = value {
            data[key] = json!(value);
        }
    }
    let plan: Record = db::create("wf_plan", &data).map_err(|e| e.or("could not create the plan (the name may be taken)"))?;
    write_lines(id_of(&plan)?, &input.lines)?;
    Ok(plan)
}

fn lines_of(plan: &str) -> Result<Vec<Record>> {
    db::find("wf_plan_line").filter("plan", plan).order_by("position").limit(500).all()
}

fn replace_lines(input: Lines) -> Result<Record> {
    require_admin()?;
    let plan = require("wf_plan", &input.id, "plan")?;
    if text(&plan, "state") != Some("draft") {
        return Err(Error::msg("only a draft plan can be edited: close it and make a new one"));
    }
    check_lines(&input.lines)?;
    for old in lines_of(&input.id)? {
        db::delete::<Record>("wf_plan_line", id_of(&old)?)?;
    }
    write_lines(&input.id, &input.lines)?;
    Ok(plan)
}

fn period_of(plan: &Record) -> Result<(NaiveDate, NaiveDate)> {
    Ok((parse_date(text(plan, "period_from").unwrap_or_default())?, parse_date(text(plan, "period_to").unwrap_or_default())?))
}

fn approve(input: Id) -> Result<Record> {
    require_admin()?;
    let plan = require("wf_plan", &input.id, "plan")?;
    if text(&plan, "state") != Some("draft") {
        return Err(Error::msg("only a draft plan can be approved"));
    }
    let who = actor()?;
    if text(&plan, "created_by") == Some(who.as_str()) {
        return Err(Error::msg("the person who made the plan cannot approve it: a second planner decides"));
    }
    let mine = period_of(&plan)?;
    let lines = lines_of(&input.id)?;
    let others: Vec<Record> = db::find::<Record>("wf_plan").filter("state", "approved").limit(500).all()?;
    for other in &others {
        if !periods_overlap(mine, period_of(other)?) {
            continue;
        }
        for line in lines_of(id_of(other)?)? {
            if lines.iter().any(|l| text(l, "position") == text(&line, "position")) {
                return Err(Error::msg(format!(
                    "plan `{}` already covers position `{}` in those dates: close it first or change the period",
                    text(other, "name").unwrap_or("?"),
                    text(&line, "position").unwrap_or("?")
                )));
            }
        }
    }
    let mut budget = Decimal::zero(2);
    for line in &lines {
        if let Some(cost) = decimal_of(line, "cost_per_hire")? {
            let hires = line.get("planned_hires").and_then(Value::as_i64).unwrap_or(0);
            budget = budget + cost.times(Decimal::whole(hires, 2)?, 2)?;
        }
    }
    db::update("wf_plan", &input.id, &json!({ "state": "approved", "approved_by": who, "total_budget": budget }))?.ok_or_else(|| Error::msg("the plan is gone"))
}

fn close(input: Id) -> Result<Record> {
    require_admin()?;
    let plan = require("wf_plan", &input.id, "plan")?;
    if text(&plan, "state") == Some("closed") {
        return Err(Error::msg("this plan is already closed"));
    }
    db::update("wf_plan", &input.id, &json!({ "state": "closed" }))?.ok_or_else(|| Error::msg("the plan is gone"))
}

/// The approved plan line covering a position on a day, with its plan.
pub fn line_for(position: &str, day: NaiveDate) -> Result<Option<(Record, Record)>> {
    let lines: Vec<Record> = db::find("wf_plan_line").filter("position", position).limit(200).all()?;
    for line in lines {
        let plan = require("wf_plan", text(&line, "plan").unwrap_or_default(), "plan")?;
        if text(&plan, "state") != Some("approved") {
            continue;
        }
        let (from, to) = period_of(&plan)?;
        if day >= from && day <= to {
            return Ok(Some((plan, line)));
        }
    }
    Ok(None)
}

/// Seats that live requisitions (not rejected or cancelled) have used on a plan line.
pub fn seats_used(line: &str) -> Result<(u64, u64)> {
    let rows: Vec<Record> = db::find::<Record>("wf_requisition").filter("plan_line", line).limit(2000).all()?;
    let (mut asked, mut filled) = (0u64, 0u64);
    for row in rows.iter().filter(|r| matches!(text(r, "state"), Some("pending" | "approved" | "filled"))) {
        asked += row.get("seats").and_then(Value::as_u64).unwrap_or(0);
        filled += row.get("seats_filled").and_then(Value::as_u64).unwrap_or(0);
    }
    Ok((asked, filled))
}

fn status(input: Id) -> Result<Value> {
    let plan = require("wf_plan", &input.id, "plan")?;
    let mut out = Vec::new();
    let mut committed = Decimal::zero(2);
    for line in lines_of(&input.id)? {
        let planned = line.get("planned_hires").and_then(Value::as_u64).unwrap_or(0);
        let (asked, filled) = seats_used(id_of(&line)?)?;
        if let Some(cost) = decimal_of(&line, "cost_per_hire")? {
            committed = committed + cost.times(Decimal::whole(i64::try_from(asked).unwrap_or(0), 2)?, 2)?;
        }
        out.push(json!({ "position": line["position"], "planned": planned, "requested": asked, "filled": filled, "room": plan_room(planned, asked) }));
    }
    Ok(json!({ "plan": plan, "lines": out, "budget": plan["total_budget"], "committed": committed }))
}

handler! {
    /// A draft staffing plan with its lines (workforce planners).
    fn create_staffing_plan(input: NewPlan) -> Record {
        make(input)
    }

    /// Replace the lines of a draft plan.
    fn set_plan_lines(input: Lines) -> Record {
        replace_lines(input)
    }

    /// Approve a plan: a second planner, and no other approved plan covering the same position and dates.
    fn approve_staffing_plan(input: Id) -> Record {
        approve(input)
    }

    fn close_staffing_plan(input: Id) -> Record {
        close(input)
    }

    /// Planned, requested and filled seats of each line, live.
    fn staffing_plan_status(input: Id) -> Value {
        status(input)
    }

    fn list_staffing_plans(_: Empty) -> Vec<Record> {
        db::find("wf_plan").order_by("-period_from").limit(200).all()
    }

    fn get_staffing_plan(input: Id) -> Option<Value> {
        let Some(plan) = db::get::<Record>("wf_plan", &input.id)? else { return Ok(None) };
        let lines = lines_of(&input.id)?;
        Ok(Some(json!({ "plan": plan, "lines": lines })))
    }
}
