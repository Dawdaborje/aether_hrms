//! Salary withholding: pay held back for whole calendar months, released one month at a time.
//!
//! Frappe stops slip creation for a flagged cycle and releases by creating the slip later; the cycle list is
//! rebuilt when the dates change. Here the cycles are rows written once (no gap, no overlap), payroll is told
//! `withheld` for any employee with an unreleased cycle in the period and holds the payout itself, and a release
//! is a mark on one cycle with who and when.

use aether_sdk::dates::{format_date, parse_date, NaiveDate};
use aether_sdk::db::Filter;
use aether_sdk::prelude::*;

use crate::common::*;
use crate::rules::monthly_cycles;

#[derive(Deserialize)]
struct NewWithholding {
    employee: String,
    date_from: String,
    cycles: u32,
    reason: String,
}

fn start(input: NewWithholding) -> Result<Record> {
    require_admin()?;
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why pay is withheld"));
    }
    if !(1..=24).contains(&input.cycles) {
        return Err(Error::msg("withhold from 1 to 24 months"));
    }
    let from = parse_date(&input.date_from)?;
    employee(&input.employee)?;
    if db::count("comp_withholding", Filter::eq("employee", input.employee.as_str()).and(Filter::eq("status", "active")))? > 0 {
        return Err(Error::msg("this person already has a withholding in force: release or cancel it first"));
    }
    let withholding: Record = db::create(
        "comp_withholding",
        &json!({ "employee": input.employee, "date_from": format_date(from), "cycles": input.cycles, "reason": input.reason, "status": "active", "created_by": actor()? }),
    )?;
    let mut tx = db::transaction();
    for (period_start, period_end) in monthly_cycles(from, input.cycles) {
        tx = tx.create(
            "comp_withholding_cycle",
            &json!({
                "withholding": id_of(&withholding)?, "employee": input.employee, "period_start": format_date(period_start),
                "period_end": format_date(period_end), "released": false,
            }),
        );
    }
    tx.run()?;
    Ok(withholding)
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

fn release(input: Id) -> Result<Record> {
    require_admin()?;
    let cycle = require("comp_withholding_cycle", &input.id, "cycle")?;
    if cycle.get("released") == Some(&json!(true)) {
        return Ok(cycle);
    }
    let withholding_id = text(&cycle, "withholding").unwrap_or_default().to_string();
    let withholding = require("comp_withholding", &withholding_id, "withholding")?;
    if text(&withholding, "status") != Some("active") {
        return Err(Error::msg("this withholding is no longer in force"));
    }
    let released = db::update::<Record>(
        "comp_withholding_cycle",
        &input.id,
        &json!({ "released": true, "released_on": format_date(today_date()?), "released_by": actor()? }),
    )?
    .ok_or_else(|| Error::msg("the cycle is gone"))?;
    if db::count("comp_withholding_cycle", Filter::eq("withholding", withholding_id.as_str()).and(Filter::eq("released", false)))? == 0 {
        db::update::<Record>("comp_withholding", &withholding_id, &json!({ "status": "released" }))?;
    }
    Ok(released)
}

fn cancel(input: Id) -> Result<Record> {
    require_admin()?;
    let withholding = require("comp_withholding", &input.id, "withholding")?;
    if text(&withholding, "status") != Some("active") {
        return Err(Error::msg("this withholding is not in force"));
    }
    db::update("comp_withholding", &input.id, &json!({ "status": "cancelled" }))?.ok_or_else(|| Error::msg("the withholding is gone"))
}

/// The employees (of the ones asked) whose pay is held back for any day of the period.
pub fn withheld_in(employees: &[String], start: NaiveDate, end: NaiveDate) -> Result<Vec<String>> {
    let cycles: Vec<Record> = db::find("comp_withholding_cycle")
        .matching(
            Filter::one_of("employee", employees.to_vec())
                .and(Filter::eq("released", false))
                .and(Filter::lte("period_start", format_date(end)))
                .and(Filter::gte("period_end", format_date(start))),
        )
        .limit(1000)
        .all()?;
    let mut out: Vec<String> = Vec::new();
    for cycle in cycles {
        let parent = text(&cycle, "withholding").unwrap_or_default();
        let active = db::get::<Record>("comp_withholding", parent)?.is_some_and(|w| text(&w, "status") == Some("active"));
        let who = text(&cycle, "employee").unwrap_or_default().to_string();
        if active && !out.contains(&who) {
            out.push(who);
        }
    }
    Ok(out)
}

handler! {
    fn start_withholding(input: NewWithholding) -> Record {
        start(input)
    }

    fn release_withholding_cycle(input: Id) -> Record {
        release(input)
    }

    fn cancel_withholding(input: Id) -> Record {
        cancel(input)
    }

    fn list_withholdings(_: Empty) -> Vec<Record> {
        require_admin()?;
        db::find("comp_withholding").order_by("-date_from").limit(200).all()
    }

    fn list_withholding_cycles(input: Id) -> Vec<Record> {
        require_admin()?;
        db::find("comp_withholding_cycle").filter("withholding", input.id.as_str()).order_by("period_start").limit(50).all()
    }
}
