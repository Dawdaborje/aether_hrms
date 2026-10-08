//! Pay runs: planned in pages, calculated in parallel batches, finalised, approved.
//!
//! One call may spend at most 50 million wasm instructions, so nothing here walks all employees in a
//! single call. `start_run` queues a planning page; a page reads up to 1000 assignments, fans one job out
//! per batch of employees (`aether_sdk::parallel`) and queues the next page; each batch job calculates its
//! people with the engine and writes their payslips; when the last batch is in, `finalize` adds the batch
//! totals up in pages. Every step is safe to repeat: payslips are unique per (run, attempt, employee), batches per
//! (run, attempt, index), and a job that finds its run restarted or cancelled does nothing.

use std::collections::BTreeMap;

use aether_sdk::dates::{format_date, parse_date};
use aether_sdk::db::Filter;
use aether_sdk::parallel::{self, Chunk};
use aether_sdk::prelude::*;
use aether_sdk::scheduler;
use sp_engine::{Compiled, Input as PayInput, Structure};

use crate::common::{
    actor, id_of, int, money, my_employee, require, require_approver, require_clerk, require_finance, require_payroll_staff, text, today_date, Record,
};
use crate::rules::{batch_index, check_period, hours_of, distinct, employed_days, last_key, overlaps_period, Totals};

// A planning page reads this many assignments and queues their batches in one call (700 reads fit the fuel, 1000 did not; each call also has 10 seconds of wall time).
const PAGE: usize = 100;
// One call has 50 million instructions. Measured with the demo structure: 25 people per call work, 50 do not.
const DEFAULT_BATCH: i64 = 20;
const MAX_BATCH: i64 = 40;
const MIN_BATCH: i64 = 5;
/// A run still calculating after this long is declared failed.
const STUCK_MINUTES: i64 = 20;
const QUEUE: &str = "payroll";

#[derive(Deserialize)]
struct NewRun {
    name: String,
    period_start: String,
    period_end: String,
    #[serde(default)]
    batch_size: Option<i64>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Start {
    id: String,
    /// Start over a run that seems stuck while calculating.
    #[serde(default)]
    restart: bool,
    /// A smaller batch, after a run failed because its batches were too big for one call.
    #[serde(default)]
    batch_size: Option<i64>,
}

#[derive(Deserialize, Serialize)]
struct Plan {
    run: String,
    attempt: i64,
    page: i64,
    #[serde(default)]
    after: Option<String>,
}

#[derive(Deserialize)]
struct BatchShared {
    run: String,
    attempt: i64,
    page: i64,
}

#[derive(Deserialize, Serialize)]
struct Finalise {
    run: String,
    attempt: i64,
    after: i64,
    acc: Totals,
}

#[derive(Deserialize)]
struct Payment {
    id: String,
    reference: String,
}

#[derive(Deserialize)]
struct Slips {
    run: String,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

fn dates(run: &Record) -> Result<(aether_sdk::dates::NaiveDate, aether_sdk::dates::NaiveDate)> {
    Ok((parse_date(text(run, "period_start").unwrap_or_default())?, parse_date(text(run, "period_end").unwrap_or_default())?))
}

fn create_run(input: NewRun) -> Result<Record> {
    require_clerk()?;
    let (start, end) = (parse_date(&input.period_start)?, parse_date(&input.period_end)?);
    check_period(start, end)?;
    let batch = input.batch_size.unwrap_or(DEFAULT_BATCH);
    if !(MIN_BATCH..=MAX_BATCH).contains(&batch) {
        return Err(Error::msg(format!("a batch is {MIN_BATCH} to {MAX_BATCH} people: one call has a fixed budget of work")));
    }
    let same = db::count("pay_run", Filter::eq("period_start", format_date(start)).and(Filter::eq("period_end", format_date(end))).and(Filter::ne("status", "cancelled")))?;
    if same > 0 {
        return Err(Error::msg("there is already a run for this period: cancel it first, or change its inputs and recalculate"));
    }
    let mut data = json!({
        "number": aether_sdk::records::next_number("payrun", "PR-", 5)?, "name": input.name, "period_start": format_date(start), "period_end": format_date(end),
        "status": "draft", "batch_size": batch, "attempt": 0,
    });
    if let Some(who) = actor()? {
        data["created_by"] = json!(who);
    }
    db::create("pay_run", &data)
}

fn start_run(input: Start) -> Result<Record> {
    require_clerk()?;
    let run = require("pay_run", &input.id, "run")?;
    let status = text(&run, "status").unwrap_or_default();
    if !(matches!(status, "draft" | "calculated" | "failed") || status == "calculating" && input.restart) {
        return Err(Error::msg(if status == "calculating" { "this run is being calculated: ask to restart it only if it is stuck".to_string() } else { format!("a {status} run cannot be calculated") }));
    }
    let batch_size = input.batch_size.unwrap_or_else(|| int(&run, "batch_size"));
    if !(MIN_BATCH..=MAX_BATCH).contains(&batch_size) {
        return Err(Error::msg(format!("a batch is {MIN_BATCH} to {MAX_BATCH} people")));
    }
    begin_attempt(&input.id, &run, batch_size, 0)
}

/// The next attempt at calculating a run: planning starts from the first page.
fn begin_attempt(run_id: &str, run: &Record, batch_size: i64, retries: i64) -> Result<Record> {
    let attempt = int(run, "attempt") + 1;
    let group = format!("{}-c{attempt}", text(run, "number").unwrap_or("run"));
    let updated = db::update::<Record>(
        "pay_run",
        run_id,
        &json!({
            "status": "calculating", "attempt": attempt, "batch_size": batch_size, "retries": retries, "failure": null, "started": context::current()?.now, "group": group, "planning_done": false, "finalized": false,
            "planned_employees": 0, "batches_total": 0, "error_count": 0, "totals": null,
        }),
    )?
    .ok_or_else(|| Error::msg("the run is gone"))?;
    scheduler::enqueue("plan_page").payload(&Plan { run: run_id.to_string(), attempt, page: 0, after: None }).queue(QUEUE).unique_key(&format!("{group}:plan:0")).send()?;
    Ok(updated)
}

/// Whether a background step still belongs to the run as it is now.
fn current(run_id: &str, attempt: i64) -> Result<Option<Record>> {
    let Some(run) = db::get::<Record>("pay_run", run_id)? else { return Ok(None) };
    Ok((int(&run, "attempt") == attempt && text(&run, "status") == Some("calculating")).then_some(run))
}

fn plan_job(plan: Plan) -> Result<Value> {
    let Some(run) = current(&plan.run, plan.attempt)? else { return Ok(json!({ "stale": true })) };
    let (start, end) = dates(&run)?;
    let mut filter = Filter::lte("valid_from", format_date(end));
    if let Some(after) = &plan.after {
        filter = filter.and(Filter::gt("employee", after.as_str()));
    }
    let rows: Vec<Record> = db::find("pay_assignment").matching(filter).order_by("employee").limit(PAGE as u32).all()?;
    let fetched: Vec<String> = rows.iter().filter_map(|r| text(r, "employee").map(str::to_string)).collect();
    let mut keys = Vec::new();
    for row in &rows {
        let to = text(row, "valid_to").map(parse_date).transpose()?;
        if overlaps_period(parse_date(text(row, "valid_from").unwrap_or_default())?, to, start, end) {
            keys.extend(text(row, "employee").map(str::to_string));
        }
    }
    let ids = distinct(keys);
    let group = text(&run, "group").unwrap_or_default().to_string();
    let batch_size = int(&run, "batch_size").max(MIN_BATCH) as usize;
    let mut batches = 0i64;
    let mut jobs = Value::Null;
    if !ids.is_empty() {
        let launched = parallel::fan_out("calculate_batch")
            .group(&format!("{group}:p{}", plan.page))
            .queue(QUEUE)
            .shared(&json!({ "run": plan.run, "attempt": plan.attempt, "page": plan.page }))
            .chunk_size(batch_size)
            .send(&ids)?;
        batches = launched.jobs.len() as i64;
        jobs = serde_json::to_value(&launched.jobs).map_err(|e| Error::msg(e.to_string()))?;
    }
    // Recorded once per page: a repeated page finds its row and changes nothing.
    let _ = db::create::<Record>("pay_plan", &json!({ "run": plan.run, "attempt": plan.attempt, "page": plan.page, "batches": batches, "employees": ids.len(), "jobs": jobs }));
    if rows.len() == PAGE {
        let next = Plan { run: plan.run.clone(), attempt: plan.attempt, page: plan.page + 1, after: last_key(&fetched).cloned() };
        scheduler::enqueue("plan_page").payload(&next).queue(QUEUE).unique_key(&format!("{group}:plan:{}", plan.page + 1)).send()?;
    } else {
        db::update::<Record>("pay_run", &plan.run, &json!({ "planning_done": true }))?;
        maybe_finalise(&plan.run, plan.attempt)?;
    }
    Ok(json!({ "page": plan.page, "employees": ids.len(), "batches": batches }))
}

/// When planning is over and every batch has reported, queue the finalising.
fn maybe_finalise(run_id: &str, attempt: i64) -> Result<()> {
    let Some(run) = current(run_id, attempt)? else { return Ok(()) };
    if run.get("planning_done") != Some(&json!(true)) {
        return Ok(());
    }
    let plans: Vec<Record> = db::find("pay_plan").filter("run", run_id).filter("attempt", attempt).limit(1000).all()?;
    let expected: i64 = plans.iter().map(|p| int(p, "batches")).sum();
    let done = db::count("pay_batch", Filter::eq("run", run_id).and(Filter::eq("attempt", attempt)))? as i64;
    if done >= expected {
        let group = text(&run, "group").unwrap_or_default();
        scheduler::enqueue("finalise_page")
            .payload(&Finalise { run: run_id.to_string(), attempt, after: -1, acc: Totals::default() })
            .queue(QUEUE)
            .unique_key(&format!("{group}:fin:-1"))
            .send()?;
    }
    Ok(())
}

fn pick_assignment<'a>(rows: &'a [Record], employee: &str, start: aether_sdk::dates::NaiveDate, end: aether_sdk::dates::NaiveDate) -> Result<Option<&'a Record>> {
    let mut best: Option<&Record> = None;
    for row in rows.iter().filter(|r| text(r, "employee") == Some(employee)) {
        let to = text(row, "valid_to").map(parse_date).transpose()?;
        if overlaps_period(parse_date(text(row, "valid_from").unwrap_or_default())?, to, start, end) && best.is_none_or(|b| text(row, "valid_from") > text(b, "valid_from")) {
            best = Some(row);
        }
    }
    Ok(best)
}

fn number(text: &str) -> Result<Value> {
    serde_json::from_str(text).map_err(|e| Error::msg(format!("`{text}` is not a number: {e}")))
}

fn batch_job(chunk: Chunk<String>) -> Result<Value> {
    let shared: BatchShared = serde_json::from_value(chunk.shared.clone())?;
    let Some(run) = current(&shared.run, shared.attempt)? else { return Ok(json!({ "stale": true })) };
    let batch_no = batch_index(shared.page, chunk.index);
    if db::count("pay_batch", Filter::eq("run", shared.run.as_str()).and(Filter::eq("attempt", shared.attempt)).and(Filter::eq("idx", batch_no)))? > 0 {
        return Ok(json!({ "already": true }));
    }
    let (start, end) = dates(&run)?;
    let ids = chunk.items.clone();

    let assignments: Vec<Record> = db::find("pay_assignment").matching(Filter::one_of("employee", ids.clone()).and(Filter::lte("valid_from", format_date(end)))).limit(1000).all()?;
    let snapshot: Value = plugins::call("hr", "employment_snapshot", &json!({ "employees": ids, "on": format_date(end) }))?;
    let run_inputs: Vec<Record> = db::find("pay_input").filter("run", shared.run.as_str()).matching(Filter::one_of("employee", ids.clone())).limit(1000).all()?;

    let mut structures: BTreeMap<String, (Record, Compiled)> = BTreeMap::new();
    // Which structures are in play decides whether leave and attendance are asked about at all.
    for structure_id in assignments.iter().filter_map(|a| text(a, "structure")).collect::<std::collections::BTreeSet<_>>() {
        let record = require("pay_structure", structure_id, "structure")?;
        let definition: Structure = serde_json::from_value(record["definition"].clone()).map_err(|e| Error::msg(format!("structure {structure_id}: {e}")))?;
        let compiled = Compiled::new(&definition).map_err(|e| Error::msg(format!("structure {structure_id}: {e}")))?;
        structures.insert(structure_id.to_string(), (record, compiled));
    }
    let wants = |name: &str| structures.values().any(|(_, c)| c.structure().inputs.iter().any(|i| i.name == name));
    let leave: Value = if wants("unpaid_leave_days") {
        plugins::call("hr_leave", "payroll_leave_summary", &json!({ "employees": ids, "from": format_date(start), "to": format_date(end) }))?
    } else {
        Value::Null
    };
    let attendance: Value = if wants("overtime_hours") {
        plugins::call("hr_attendance", "payroll_attendance_summary", &json!({ "employees": ids, "from": format_date(start), "to": format_date(end) }))?
    } else {
        Value::Null
    };
    // Extra pay comes from one place: structures that declare inputs named `adj_<code>` get that code's total for the period.
    let adjustments: Value = if structures.values().any(|(_, c)| c.structure().inputs.iter().any(|i| i.name.starts_with("adj_"))) {
        plugins::call("hr_compensation", "payroll_adjustments", &json!({ "employees": ids, "from": format_date(start), "to": format_date(end) }))?
    } else {
        Value::Null
    };
    let mut currencies: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut totals = Totals::default();
    let mut slips: Vec<(String, Value)> = Vec::new();
    let mut errors: Vec<(String, String)> = Vec::new();

    for id in &ids {
        totals.employees += 1;
        let Some(assignment) = pick_assignment(&assignments, id, start, end)? else {
            totals.skipped += 1;
            continue;
        };
        let person = &snapshot[id.as_str()];
        if person.is_null() {
            errors.push((id.clone(), "this employee is not in the directory".into()));
            continue;
        }
        let hired = person["hire_date"].as_str().map(parse_date).transpose()?;
        let left = person["end_date"].as_str().map(parse_date).transpose()?;
        let Some((period_days, days_worked)) = employed_days(start, end, hired, left) else {
            totals.skipped += 1;
            continue;
        };
        let terms = &person["terms"];
        let Some(wage) = terms["wage"].as_str() else {
            errors.push((id.clone(), format!("no pay is set in their employment terms on {end}")));
            continue;
        };
        let structure_id = text(assignment, "structure").unwrap_or_default().to_string();
        if !structures.contains_key(&structure_id) {
            let record = require("pay_structure", &structure_id, "structure")?;
            let definition: Structure = serde_json::from_value(record["definition"].clone()).map_err(|e| Error::msg(format!("structure {structure_id}: {e}")))?;
            let compiled = Compiled::new(&definition).map_err(|e| Error::msg(format!("structure {structure_id}: {e}")))?;
            structures.insert(structure_id.clone(), (record, compiled));
        }
        let (record, compiled) = &structures[&structure_id];
        let code = text(record, "currency").unwrap_or_default().to_string();
        if !currencies.contains_key(&code) {
            let found: Option<Record> = plugins::call("currency", "get_currency_by_code", &json!({ "code": code }))?;
            currencies.insert(code.clone(), found.and_then(|c| text(&c, "id").map(str::to_string)));
        }
        if terms["currency"].as_str() != currencies[&code].as_deref() {
            errors.push((id.clone(), format!("their pay is not in {code}, the currency of the salary structure")));
            continue;
        }

        let declared: Vec<&str> = compiled.structure().inputs.iter().map(|i| i.name.as_str()).collect();
        let mut variables: BTreeMap<String, Value> = BTreeMap::new();
        let mut put = |name: &str, value: Value| {
            if declared.contains(&name) {
                variables.insert(name.to_string(), value);
            }
        };
        put("base_salary", number(wage)?);
        put("period_days", json!(period_days));
        put("days_worked", json!(days_worked));
        put("periods_per_year", json!(12));
        put("unpaid_leave_days", number(leave[id.as_str()]["unpaid_days"].as_str().unwrap_or("0"))?);
        put("overtime_hours", number(&hours_of(attendance[id.as_str()]["overtime_minutes"].as_i64().unwrap_or(0))?.to_string())?);
        for declared_name in declared.iter().filter(|name| name.starts_with("adj_")) {
            // No adjustment of that code for this person this period is zero, never missing.
            let amount = adjustments[id.as_str()][*declared_name].as_str().unwrap_or("0");
            put(declared_name, number(amount)?);
        }
        for source in [assignment["variables"].as_object(), run_inputs.iter().find(|r| text(r, "employee") == Some(id.as_str())).and_then(|r| r["variables"].as_object())].into_iter().flatten() {
            for (name, value) in source {
                variables.insert(name.clone(), value.clone());
            }
        }
        match compiled.run(&PayInput { employee_id: id.clone(), variables }, false) {
            Err(error) => errors.push((id.clone(), error.to_string())),
            Ok(slip) => {
                let (gross, deductions, net, employer, shortfall) = (money(&slip.gross.to_string())?, money(&slip.deductions.to_string())?, money(&slip.net.to_string())?, money(&slip.employer_cost.to_string())?, money(&slip.shortfall.to_string())?);
                totals.calculated += 1;
                totals.gross = totals.gross + gross;
                totals.deductions = totals.deductions + deductions;
                totals.net = totals.net + net;
                totals.employer_cost = totals.employer_cost + employer;
                totals.shortfall = totals.shortfall + shortfall;
                for line in &slip.lines {
                    let kind = match line.kind {
                        sp_engine::Kind::Earning => "earning",
                        sp_engine::Kind::Deduction => "deduction",
                        sp_engine::Kind::Employer => "employer",
                        sp_engine::Kind::Info => continue,
                    };
                    totals.add_line(&line.id, kind, money(&line.amount.to_string())?);
                }
                slips.push((
                    id.clone(),
                    json!({
                        "run": shared.run, "employee": id, "employee_name": person["display_name"], "structure_code": text(record, "code"), "structure_version": text(record, "version"),
                        "structure_hash": text(record, "hash"), "currency": code, "gross": gross, "deductions": deductions, "net": net, "employer_cost": employer, "shortfall": shortfall,
                        "lines": serde_json::to_value(&slip.lines).map_err(|e| Error::msg(e.to_string()))?,
                    }),
                ));
            }
        }
    }
    totals.failed = errors.len() as i64;

    // Every attempt writes its own payslips (an earlier attempt's stay as the record of what was calculated
    // then), so a batch only creates. Its rows and its report go in one transaction: a batch that is run
    // again after a failure finds either all of it written or none.
    let mut tx = db::transaction();
    let mut writes = 0;
    for (_, row) in &mut slips {
        row["attempt"] = json!(shared.attempt);
        tx = tx.create("payslip", row);
        writes += 1;
    }
    for (employee, message) in &errors {
        tx = tx.create("pay_error", &json!({ "run": shared.run, "attempt": shared.attempt, "employee": employee, "message": message.chars().take(390).collect::<String>() }));
        writes += 1;
    }
    let report = json!({
        "run": shared.run, "attempt": shared.attempt, "idx": batch_no, "status": "done", "employees": totals.employees, "calculated": totals.calculated,
        "skipped": totals.skipped, "failed": totals.failed, "totals": serde_json::to_value(&totals).map_err(|e| Error::msg(e.to_string()))?,
    });
    if writes >= 50 {
        return Err(Error::msg("a batch of this size does not fit one transaction"));
    }
    tx.create("pay_batch", &report).run()?;
    maybe_finalise(&shared.run, shared.attempt)?;
    Ok(json!({ "batch": batch_no, "calculated": totals.calculated, "failed": totals.failed, "skipped": totals.skipped }))
}

fn finalise_job(job: Finalise) -> Result<Value> {
    let Some(run) = current(&job.run, job.attempt)? else { return Ok(json!({ "stale": true })) };
    let rows: Vec<Record> = db::find("pay_batch")
        .matching(Filter::eq("run", job.run.as_str()).and(Filter::eq("attempt", job.attempt)).and(Filter::gt("idx", job.after)))
        .order_by("idx")
        .limit(200)
        .all()?;
    let mut acc = job.acc;
    let mut last = job.after;
    for row in &rows {
        let batch: Totals = serde_json::from_value(row["totals"].clone()).map_err(|e| Error::msg(format!("a batch's totals: {e}")))?;
        acc.merge(&batch);
        last = int(row, "idx");
    }
    let group = text(&run, "group").unwrap_or_default().to_string();
    if rows.len() == 200 {
        scheduler::enqueue("finalise_page")
            .payload(&Finalise { run: job.run, attempt: job.attempt, after: last, acc })
            .queue(QUEUE)
            .unique_key(&format!("{group}:fin:{last}"))
            .send()?;
        return Ok(json!({ "more": true }));
    }
    let plans: Vec<Record> = db::find("pay_plan").filter("run", job.run.as_str()).filter("attempt", job.attempt).limit(1000).all()?;
    let consistent = acc.is_consistent();
    let mut totals = serde_json::to_value(&acc).map_err(|e| Error::msg(e.to_string()))?;
    totals["consistent"] = json!(consistent);
    db::update::<Record>(
        "pay_run",
        &job.run,
        &json!({
            "status": "calculated", "finalized": true, "totals": totals, "error_count": acc.failed,
            "planned_employees": plans.iter().map(|p| int(p, "employees")).sum::<i64>(), "batches_total": plans.iter().map(|p| int(p, "batches")).sum::<i64>(),
        }),
    )?;
    events::emit("payroll_run_calculated", &json!({ "run": job.run, "errors": acc.failed, "consistent": consistent }))?;
    Ok(json!({ "finalised": true, "errors": acc.failed }))
}

/// A job that fails for good leaves no trace in the run, so a minute at a time this looks at the jobs of
/// every run still calculating and fails the run, naming the batches, if any job has failed.
fn watch_runs() -> Result<Value> {
    let running: Vec<Record> = db::find("pay_run").filter("status", "calculating").limit(50).all()?;
    let mut failed_runs = 0u64;
    for run in &running {
        let (id, attempt) = (id_of(run)?, int(run, "attempt"));
        let plans: Vec<Record> = db::find("pay_plan").filter("run", id).filter("attempt", attempt).limit(1000).all()?;
        // A planning page that ran out of budget leaves no job to look at: a run that is far too old is failed.
        let age = match (text(run, "started").map(aether_sdk::dates::parse_datetime).transpose()?, aether_sdk::dates::parse_datetime(&context::current()?.now)) {
            (Some(started), Ok(now)) => (now - started).num_minutes(),
            _ => 0,
        };
        if age >= STUCK_MINUTES {
            db::update::<Record>("pay_run", id, &json!({ "status": "failed", "failure": format!("still calculating after {age} minutes: a step of the planning probably failed. Calculate again.") }))?;
            events::emit("payroll_run_failed", &json!({ "run": id, "stuck_minutes": age }))?;
            failed_runs += 1;
            continue;
        }
        let mut bad: Vec<u32> = Vec::new();
        let mut message = String::new();
        for plan in &plans {
            let Some(list) = plan.get("jobs").filter(|j| !j.is_null()) else { continue };
            let refs: Vec<parallel::JobRef> = serde_json::from_value(list.clone()).map_err(|e| Error::msg(format!("jobs: {e}")))?;
            let progress = parallel::progress(&refs)?;
            if let Some(first) = progress.errors.first() {
                message = first.error.clone();
            }
            bad.extend(progress.failed_indexes().into_iter().map(|i| i + (int(plan, "page") as u32) * 1000));
        }
        if !bad.is_empty() {
            let note = format!("{} batch(es) failed for good ({}). Usually the batch was too big for one call: calculate again with a smaller batch size.", bad.len(), message.chars().take(160).collect::<String>());
            // A batch too big for one call is the usual cause: try again with half as many people, twice at most.
            let size = int(run, "batch_size");
            if size / 2 >= MIN_BATCH && int(run, "retries") < 2 {
                begin_attempt(id, run, size / 2, int(run, "retries") + 1)?;
                events::emit("payroll_run_restarted", &json!({ "run": id, "batch_size": size / 2 }))?;
                continue;
            }
            db::update::<Record>("pay_run", id, &json!({ "status": "failed", "failure": note.chars().take(390).collect::<String>() }))?;
            events::emit("payroll_run_failed", &json!({ "run": id, "failed_batches": bad.len() }))?;
            failed_runs += 1;
        }
    }
    Ok(json!({ "watched": running.len(), "failed": failed_runs }))
}

fn approve(input: Id) -> Result<Record> {
    require_approver()?;
    let run = require("pay_run", &input.id, "run")?;
    if text(&run, "status") != Some("calculated") || run.get("finalized") != Some(&json!(true)) {
        return Err(Error::msg("only a calculated run is approved"));
    }
    if int(&run, "error_count") > 0 {
        return Err(Error::msg(format!("{} employee(s) could not be calculated: fix them and recalculate, or cancel the run", int(&run, "error_count"))));
    }
    if run["totals"]["consistent"] != json!(true) {
        return Err(Error::msg("the totals of this run do not add up: recalculate it"));
    }
    let unverified = unverified_packs(&run)?;
    if !unverified.is_empty() && !crate::setup::unverified_allowed()? {
        return Err(Error::msg(format!(
            "this run uses rule packs nobody has verified against the law ({}): it cannot be approved, except in a test organisation that allows it",
            unverified.join(", ")
        )));
    }
    let who = actor()?;
    if who.is_some() && who.as_deref() == text(&run, "created_by") {
        return Err(Error::msg("someone other than the person who made the run approves it"));
    }
    let totals: Totals = serde_json::from_value(run["totals"].clone()).map_err(|e| Error::msg(format!("totals: {e}")))?;
    let today = format_date(today_date()?);
    let approved = db::update::<Record>("pay_run", &input.id, &json!({ "status": "approved", "approved_by": who, "approved_on": today }))?.ok_or_else(|| Error::msg("the run is gone"))?;
    // The period is closed for extra pay: an adjustment dated inside it can no longer change.
    plugins::call::<Value>("hr_compensation", "lock_through", &json!({ "through": run["period_end"] }))?;
    let components: Vec<Value> = totals.components.iter().map(|(id, c)| json!({ "id": id, "kind": c.kind, "amount": c.amount })).collect();
    // Money amounts travel as text so no digit is lost.
    events::emit(
        "payroll_run_approved",
        &json!({
            "run": input.id, "number": run["number"], "currency": currency_of(&run)?, "period_end": run["period_end"], "employees": totals.calculated,
            "gross": totals.gross, "deductions": totals.deductions, "net": totals.net, "employer_cost": totals.employer_cost, "components": components,
            "unverified_rules": unverified,
        }),
    )?;
    Ok(approved)
}

/// The ids of the rule packs, used by the structures of this run's payslips, that nobody has verified.
fn unverified_packs(run: &Record) -> Result<Vec<String>> {
    let filter = Filter::eq("run", id_of(run)?).and(Filter::eq("attempt", int(run, "attempt")));
    let groups = db::find::<Record>("payslip").matching(filter).aggregate(&["structure_code", "structure_version"], &[("people", aether_sdk::db::Figure::sum("gross"))])?;
    let mut out: Vec<String> = Vec::new();
    for group in groups {
        for pack in crate::setup::packs_of(group["structure_code"].as_str().unwrap_or_default(), group["structure_version"].as_str().unwrap_or_default())? {
            if pack["verified"] != json!(true) {
                let id = pack["pack"].as_str().unwrap_or("?").to_string();
                if !out.contains(&id) {
                    out.push(id);
                }
            }
        }
    }
    Ok(out)
}

/// The currency of a run's payslips (one structure currency per run in this version).
fn currency_of(run: &Record) -> Result<Value> {
    let slip: Option<Record> = db::find("payslip").filter("run", text(run, "id").unwrap_or_default()).filter("attempt", int(run, "attempt")).first()?;
    Ok(slip.map_or(Value::Null, |s| s["currency"].clone()))
}

fn cancel(input: Id) -> Result<Record> {
    require_clerk()?;
    let run = require("pay_run", &input.id, "run")?;
    if !matches!(text(&run, "status"), Some("draft" | "calculating" | "calculated" | "failed")) {
        return Err(Error::msg("an approved run is not cancelled: it is corrected by a later run"));
    }
    db::update("pay_run", &input.id, &json!({ "status": "cancelled" }))?.ok_or_else(|| Error::msg("the run is gone"))
}

fn record_payment(input: Payment) -> Result<Record> {
    require_finance()?;
    let run = require("pay_run", &input.id, "run")?;
    if text(&run, "status") != Some("approved") {
        return Err(Error::msg("only an approved run is paid"));
    }
    if input.reference.trim().is_empty() {
        return Err(Error::msg("give the payment's reference"));
    }
    let paid = db::update::<Record>("pay_run", &input.id, &json!({ "status": "paid", "payment_ref": input.reference, "paid_on": format_date(today_date()?) }))?.ok_or_else(|| Error::msg("the run is gone"))?;
    events::emit("payroll_run_paid", &json!({ "run": input.id, "number": run["number"], "reference": input.reference, "net": run["totals"]["net"] }))?;
    Ok(paid)
}

fn progress(input: Id) -> Result<Value> {
    require_payroll_staff()?;
    let run = require("pay_run", &input.id, "run")?;
    let attempt = int(&run, "attempt");
    let plans: Vec<Record> = db::find("pay_plan").filter("run", input.id.as_str()).filter("attempt", attempt).limit(1000).all()?;
    let done = db::count("pay_batch", Filter::eq("run", input.id.as_str()).and(Filter::eq("attempt", attempt)))?;
    let slips = db::count("payslip", Filter::eq("run", input.id.as_str()).and(Filter::eq("attempt", attempt)))?;
    let errors = db::count("pay_error", Filter::eq("run", input.id.as_str()).and(Filter::eq("attempt", attempt)))?;
    Ok(json!({
        "status": run["status"], "attempt": attempt, "planning_done": run["planning_done"], "pages": plans.len(),
        "batches_expected_so_far": plans.iter().map(|p| int(p, "batches")).sum::<i64>(), "batches_done": done, "payslips": slips, "errors": errors,
    }))
}

fn my_payslip_ids() -> Result<Vec<String>> {
    let Some(me) = my_employee()? else { return Ok(Vec::new()) };
    let runs: Vec<Record> = db::find("pay_run").matching(Filter::one_of("status", ["approved", "paid"])).limit(500).all()?;
    let run_ids: Vec<String> = runs.iter().filter_map(|r| text(r, "id").map(str::to_string)).collect();
    if run_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<Record> = db::find("payslip").filter("employee", id_of(&me)?).matching(Filter::one_of("run", run_ids)).limit(500).all()?;
    // Only the attempt the run ended with is the payslip.
    let attempt_of: BTreeMap<&str, i64> = runs.iter().filter_map(|r| Some((text(r, "id")?, int(r, "attempt")))).collect();
    Ok(rows.iter().filter(|r| attempt_of.get(text(r, "run").unwrap_or_default()) == Some(&int(r, "attempt"))).filter_map(|r| text(r, "id").map(str::to_string)).collect())
}

handler! {
    fn create_pay_run(input: NewRun) -> Record {
        create_run(input)
    }

    /// Calculate (or recalculate) a run: queues the planning, which queues the batches.
    fn start_pay_run(input: Start) -> Record {
        start_run(input)
    }

    fn plan_page(input: Plan) -> Value {
        plan_job(input)
    }

    fn calculate_batch(input: Chunk<String>) -> Value {
        batch_job(input)
    }

    fn finalise_page(input: Finalise) -> Value {
        finalise_job(input)
    }

    fn watch_pay_runs(_: Empty) -> Value {
        watch_runs()
    }

    fn approve_pay_run(input: Id) -> Record {
        approve(input)
    }

    fn cancel_pay_run(input: Id) -> Record {
        cancel(input)
    }

    fn record_pay_run_payment(input: Payment) -> Record {
        record_payment(input)
    }

    fn get_pay_run(input: Id) -> Option<Record> {
        require_payroll_staff()?;
        db::get("pay_run", &input.id)
    }

    fn list_pay_runs(_: Empty) -> Vec<Record> {
        require_payroll_staff()?;
        db::find("pay_run").order_by("-period_end").limit(200).all()
    }

    fn pay_run_progress(input: Id) -> Value {
        progress(input)
    }

    fn list_payslips(input: Slips) -> Vec<Record> {
        require_payroll_staff()?;
        let run = require("pay_run", &input.run, "run")?;
        let mut filter = Filter::eq("run", input.run.as_str()).and(Filter::eq("attempt", int(&run, "attempt")));
        if let Some(after) = &input.after {
            filter = filter.and(Filter::gt("employee", after.as_str()));
        }
        db::find::<Record>("payslip").matching(filter).order_by("employee").limit(input.limit.unwrap_or(50).min(200)).all()
    }

    fn list_pay_errors(input: Id) -> Vec<Record> {
        require_payroll_staff()?;
        let run = require("pay_run", &input.id, "run")?;
        db::find("pay_error").filter("run", input.id.as_str()).filter("attempt", int(&run, "attempt")).limit(200).all()
    }

    /// The caller's own payslips of approved runs.
    fn my_payslips(_: Empty) -> Vec<Record> {
        let ids = my_payslip_ids()?;
        let mut out = Vec::new();
        for id in ids {
            if let Some(slip) = db::get::<Record>("payslip", &id)? {
                out.push(slip);
            }
        }
        Ok(out)
    }

    fn rule_var_my_payslips(_: Empty) -> Vec<String> {
        my_payslip_ids()
    }
}

// Only for measuring what one call can afford: `cargo build --features probe`. Never in the published plugin.
#[cfg(feature = "probe")]
handler! {
    fn probe_assignments(input: Slips) -> Value {
        let rows: Vec<Record> = db::find("pay_assignment").order_by("employee").limit(input.limit.unwrap_or(10)).all()?;
        Ok(json!({ "rows": rows.len() }))
    }
}
