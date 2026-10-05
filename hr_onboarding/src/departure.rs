//! Departures: a notice, a last day, a checklist, and a close that ends the employment through hr.
//!
//! The close is the one step that cannot be taken back (hr does not reopen an ended employment; a
//! person comes back by being rehired), so every check is made before it: no blocking task open, no
//! item still owed, no settlement line unsettled without a manager's reason. Each departure is moved
//! on its own in the nightly tick, so one failure does not stop the others.

use aether_sdk::dates::{format_date, parse_date};
use aether_sdk::db::Filter;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{employee, id_of, my_employee, require, require_manager, require_officer, text, today_date, works_here, Record};
use crate::rules::{check_close, net_settlement, status_on, Clearance, DepartureStatus};
use crate::run::{anchors_of, gate_clear, rebase, refresh, start, void_run, Anchors, Start};
use crate::setup::pick_template;

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Begin {
    employee: String,
    reason: String,
    last_day: String,
    #[serde(default)]
    notice_date: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct Confirm {
    id: String,
    #[serde(default)]
    template: Option<String>,
}

#[derive(Deserialize)]
struct Change {
    id: String,
    #[serde(default)]
    last_day: Option<String>,
    #[serde(default)]
    notice_date: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct Close {
    id: String,
    #[serde(default)]
    override_reason: Option<String>,
}

#[derive(Deserialize)]
struct Cancel {
    id: String,
    reason: String,
}

#[derive(Deserialize)]
struct NewItem {
    departure: String,
    name: String,
    #[serde(default)]
    cost: Option<Decimal>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct SetItem {
    id: String,
    status: String,
    #[serde(default)]
    cost: Option<Decimal>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct NewLine {
    departure: String,
    kind: String,
    label: String,
    amount: Decimal,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Deserialize)]
struct SettleLine {
    id: String,
    #[serde(default)]
    override_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct Search {
    #[serde(default)]
    status: Option<String>,
}

fn status_of(departure: &Record) -> Result<DepartureStatus> {
    DepartureStatus::parse(text(departure, "status").unwrap_or_default()).ok_or_else(|| Error::msg("a departure has an unknown status"))
}

fn live(id: &str) -> Result<Record> {
    let departure = require("ob_departure", id, "departure")?;
    if !status_of(&departure)?.is_live() {
        return Err(Error::msg(format!("this departure is {}", text(&departure, "status").unwrap_or("over"))));
    }
    Ok(departure)
}

fn run_of(departure_id: &str) -> Result<Option<Record>> {
    db::find("ob_run").filter("departure", departure_id).first()
}

fn begin(input: Begin) -> Result<Record> {
    require_officer()?;
    let person = employee(&input.employee)?;
    if !works_here(&person) {
        return Err(Error::msg("this person does not work here"));
    }
    if db::count("ob_departure", Filter::eq("employee", input.employee.as_str()).and(Filter::one_of("status", ["draft", "notice", "clearance"])))? > 0 {
        return Err(Error::msg("this person already has a departure in progress"));
    }
    require("ob_departure_reason", &input.reason, "reason")?;
    let last = parse_date(&input.last_day)?;
    let notice = input.notice_date.as_deref().map(parse_date).transpose()?;
    if notice.is_some_and(|n| n > last) {
        return Err(Error::msg("the notice is given on or before the last day"));
    }
    if let Some(hired) = text(&person, "hire_date").map(parse_date).transpose()? {
        if last < hired {
            return Err(Error::msg("the last day is before the person was hired"));
        }
    }
    if last < today_date()? && !crate::common::is_manager()? {
        return Err(Error::msg("only a manager records a last day that has passed"));
    }
    let mut data = json!({ "employee": input.employee, "reason": input.reason, "status": "draft", "last_day": format_date(last) });
    if let Some(notice) = notice {
        data["notice_date"] = json!(format_date(notice));
    }
    if let Some(description) = &input.description {
        data["description"] = json!(description);
    }
    db::create("ob_departure", &data).map_err(|e| e.or("could not record the departure"))
}

/// Confirm it: the offboarding checklist is made and the departure is in notice.
fn confirm(input: Confirm) -> Result<Record> {
    require_officer()?;
    let departure = require("ob_departure", &input.id, "departure")?;
    if status_of(&departure)? != DepartureStatus::Draft {
        return Err(Error::msg("only a draft departure is confirmed"));
    }
    let person = employee(text(&departure, "employee").unwrap_or_default())?;
    if !works_here(&person) {
        return Err(Error::msg("this person does not work here any more"));
    }
    let template = match &input.template {
        Some(id) => require("ob_template", id, "template")?,
        None => pick_template("offboard", text(&person, "department"), text(&person, "job"))?,
    };
    if text(&template, "kind") != Some("offboard") {
        return Err(Error::msg("that is not an offboarding template"));
    }
    let last = parse_date(text(&departure, "last_day").unwrap_or_default())?;
    let notice = text(&departure, "notice_date").map(parse_date).transpose()?;
    start(Start {
        kind: "offboard",
        template,
        anchors: Anchors { start: None, last_day: Some(last), notice },
        primary: last,
        party: text(&person, "party").map(str::to_string),
        employee: Some(person),
        departure: Some(input.id.clone()),
        owner: my_employee()?,
    })?;
    let next = status_on(DepartureStatus::Notice, last, today_date()?);
    db::update("ob_departure", &input.id, &json!({ "status": next.as_str() }))?.ok_or_else(|| Error::msg("the departure is gone"))
}

fn change(input: Change) -> Result<Record> {
    require_officer()?;
    let departure = live(&input.id)?;
    let mut changes = json!({});
    let last = match &input.last_day {
        Some(day) => parse_date(day)?,
        None => parse_date(text(&departure, "last_day").unwrap_or_default())?,
    };
    let notice = match &input.notice_date {
        Some(day) => Some(parse_date(day)?),
        None => text(&departure, "notice_date").map(parse_date).transpose()?,
    };
    if notice.is_some_and(|n| n > last) {
        return Err(Error::msg("the notice is given on or before the last day"));
    }
    if input.last_day.is_some() {
        changes["last_day"] = json!(format_date(last));
    }
    if let Some(day) = notice.filter(|_| input.notice_date.is_some()) {
        changes["notice_date"] = json!(format_date(day));
    }
    if let Some(reason) = &input.reason {
        require("ob_departure_reason", reason, "reason")?;
        changes["reason"] = json!(reason);
    }
    if let Some(description) = &input.description {
        changes["description"] = json!(description);
    }
    if changes.as_object().is_some_and(|c| c.is_empty()) {
        return Err(Error::msg("there is nothing to change"));
    }
    // A last day moved into the future takes a departure in clearance back to notice, and the other way.
    let current = status_of(&departure)?;
    if matches!(current, DepartureStatus::Notice | DepartureStatus::Clearance) {
        changes["status"] = json!(status_on(DepartureStatus::Notice, last, today_date()?).as_str());
    }
    let updated = db::update::<Record>("ob_departure", &input.id, &changes)?.ok_or_else(|| Error::msg("the departure is gone"))?;
    if let Some(run) = run_of(&input.id)? {
        rebase(id_of(&run)?, &anchors_of(&run)?)?;
    }
    Ok(updated)
}

fn cancel(input: Cancel) -> Result<Record> {
    require_officer()?;
    live(&input.id)?;
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why the departure is cancelled"));
    }
    if let Some(run) = run_of(&input.id)? {
        if text(&run, "status") != Some("cancelled") {
            void_run(id_of(&run)?, &input.reason)?;
        }
    }
    db::update("ob_departure", &input.id, &json!({ "status": "cancelled", "cancel_reason": input.reason }))?.ok_or_else(|| Error::msg("the departure is gone"))
}

fn new_item(input: NewItem) -> Result<Record> {
    require_officer()?;
    live(&input.departure)?;
    let mut data = json!({ "departure": input.departure, "name": input.name, "status": "owed" });
    if let Some(cost) = input.cost {
        data["cost"] = json!(cost.with_scale(2)?);
    }
    if let Some(note) = &input.note {
        data["note"] = json!(note);
    }
    db::create("ob_return_item", &data).map_err(|e| e.or("could not add the item"))
}

fn set_item(input: SetItem) -> Result<Record> {
    require_officer()?;
    let item = require("ob_return_item", &input.id, "item")?;
    live(text(&item, "departure").unwrap_or_default())?;
    if !["owed", "returned", "recovered", "waived"].contains(&input.status.as_str()) {
        return Err(Error::msg("status is owed, returned, recovered or waived"));
    }
    if input.status == "waived" {
        require_manager()?;
        if input.note.as_deref().map(str::trim).unwrap_or_default().is_empty() {
            return Err(Error::msg("say why the item is waived"));
        }
    }
    let mut changes = json!({ "status": input.status });
    if let Some(cost) = input.cost {
        changes["cost"] = json!(cost.with_scale(2)?);
    }
    if input.status == "recovered" && input.cost.is_none() && item.get("cost").is_none_or(Value::is_null) {
        return Err(Error::msg("say what the item cost to recover"));
    }
    if let Some(note) = &input.note {
        changes["note"] = json!(note);
    }
    db::update("ob_return_item", &input.id, &changes)?.ok_or_else(|| Error::msg("the item is gone"))
}

fn new_line(input: NewLine) -> Result<Record> {
    require_officer()?;
    live(&input.departure)?;
    if !["payable", "receivable"].contains(&input.kind.as_str()) {
        return Err(Error::msg("kind is payable or receivable"));
    }
    let amount = input.amount.with_scale(2)?;
    if amount <= Decimal::zero(2) {
        return Err(Error::msg("an amount is above zero; the kind says which way it goes"));
    }
    let mut data = json!({ "departure": input.departure, "kind": input.kind, "label": input.label, "amount": amount, "status": "open" });
    for (field, value) in [("currency", &input.currency), ("source", &input.source)] {
        if let Some(value) = value {
            data[field] = json!(value);
        }
    }
    db::create("ob_settlement_line", &data).map_err(|e| e.or("could not add the line"))
}

fn settle_line(input: SettleLine) -> Result<Record> {
    require_officer()?;
    let line = require("ob_settlement_line", &input.id, "line")?;
    live(text(&line, "departure").unwrap_or_default())?;
    if text(&line, "status") == Some("settled") {
        return Err(Error::msg("this line is already settled"));
    }
    let mut changes = json!({ "status": "settled" });
    if let Some(reason) = &input.override_reason {
        changes["override_reason"] = json!(reason);
    }
    db::update("ob_settlement_line", &input.id, &changes)?.ok_or_else(|| Error::msg("the line is gone"))
}

fn checklist(departure_id: &str) -> Result<Value> {
    let departure = require("ob_departure", departure_id, "departure")?;
    let items: Vec<Record> = db::find("ob_return_item").filter("departure", departure_id).limit(200).all()?;
    let lines: Vec<Record> = db::find("ob_settlement_line").filter("departure", departure_id).limit(200).all()?;
    let mut amounts = Vec::new();
    for line in &lines {
        let amount: Decimal = serde_json::from_value(line["amount"].clone()).map_err(|e| Error::msg(format!("amount: {e}")))?;
        amounts.push((text(line, "kind") == Some("payable"), amount));
    }
    let run = run_of(departure_id)?;
    let (access, settlement) = match &run {
        Some(run) => (gate_clear(id_of(run)?, "access")?, gate_clear(id_of(run)?, "settlement")?),
        None => (0, 0),
    };
    Ok(json!({
        "departure": departure, "run": run, "items": items, "lines": lines, "net_to_employee": net_settlement(&amounts),
        "open_access_tasks": access, "open_settlement_tasks": settlement,
    }))
}

fn status_for(reason: &Record) -> &'static str {
    match text(reason, "status_to") {
        Some("resigned") => "resigned",
        Some("terminated") => "terminated",
        Some("retired") => "retired",
        Some("deceased") => "deceased",
        _ => match text(reason, "kind") {
            Some("resignation") => "resigned",
            Some("retirement") => "retired",
            _ => "terminated",
        },
    }
}

fn close(input: Close) -> Result<Record> {
    require_manager()?;
    let departure = live(&input.id)?;
    if status_of(&departure)? == DepartureStatus::Draft {
        return Err(Error::msg("confirm the departure first: that makes its checklist"));
    }
    let last = parse_date(text(&departure, "last_day").unwrap_or_default())?;
    let today = today_date()?;
    let clearance = {
        let run_gates = match run_of(&input.id)? {
            Some(run) => gate_clear(id_of(&run)?, "access")? + gate_clear(id_of(&run)?, "settlement")?,
            None => 0,
        };
        Clearance {
            open_gate_tasks: run_gates,
            items_owed: db::count("ob_return_item", Filter::eq("departure", input.id.as_str()).and(Filter::eq("status", "owed")))? as usize,
            lines_open: db::count("ob_settlement_line", Filter::eq("departure", input.id.as_str()).and(Filter::eq("status", "open")))? as usize,
        }
    };
    check_close(clearance, last, today, input.override_reason.as_deref())?;
    let person = employee(text(&departure, "employee").unwrap_or_default())?;
    let reason = require("ob_departure_reason", text(&departure, "reason").unwrap_or_default(), "reason")?;
    let status = status_for(&reason);
    // hr has the last word: it refuses while people still report to this person.
    let _: Value = plugins::call(
        "hr",
        "change_employee_status",
        &json!({ "id": person["id"], "status": status, "date": format_date(last), "reason": text(&reason, "name") }),
    )?;
    let mut changes = json!({ "status": "closed", "closed_on": format_date(today), "prior_status": text(&person, "status") });
    if let Some(why) = &input.override_reason {
        changes["override_reason"] = json!(why);
    }
    let closed = db::update::<Record>("ob_departure", &input.id, &changes)?.ok_or_else(|| Error::msg("the departure is gone"))?;
    if let Some(run) = run_of(&input.id)? {
        refresh(id_of(&run)?)?;
    }
    events::emit("employee_departed", &json!({ "employee": person["id"], "departure": input.id, "status": status, "last_day": format_date(last) }))?;
    Ok(closed)
}

/// Nightly (and on demand): departures whose last day has come move to clearance; open tasks past
/// their due day are announced once. Each item is handled alone, and failures are reported.
fn tick() -> Result<Value> {
    let today = today_date()?;
    let mut moved = 0u64;
    let mut late = 0u64;
    let mut failures: Vec<String> = Vec::new();
    let due: Vec<Record> = db::find::<Record>("ob_departure").filter("status", "notice").limit(500).all()?;
    for departure in &due {
        let step = || -> Result<bool> {
            let last = parse_date(text(departure, "last_day").unwrap_or_default())?;
            if status_on(DepartureStatus::Notice, last, today) == DepartureStatus::Clearance {
                db::update::<Record>("ob_departure", id_of(departure)?, &json!({ "status": "clearance" }))?;
                events::emit("departure_clearance_due", &json!({ "departure": departure["id"], "employee": departure["employee"] }))?;
                return Ok(true);
            }
            Ok(false)
        };
        match step() {
            Ok(true) => moved += 1,
            Ok(false) => {}
            Err(error) => failures.push(format!("{}: {error}", text(departure, "id").unwrap_or("?"))),
        }
    }
    let day = format_date(today);
    let overdue: Vec<Record> = db::find::<Record>("ob_task")
        .matching(Filter::eq("state", "todo").and(Filter::lt("due_end", day.as_str())).and(Filter::ne("overdue_notified", true)))
        .limit(500)
        .all()?;
    for task in &overdue {
        let step = || -> Result<()> {
            events::emit("onboarding_task_overdue", &json!({ "task": task["id"], "run": task["run"], "assignee": task["assignee"], "role": task["assignee_role"], "due": task["due_end"] }))?;
            db::update::<Record>("ob_task", id_of(task)?, &json!({ "overdue_notified": true }))?;
            Ok(())
        };
        match step() {
            Ok(()) => late += 1,
            Err(error) => failures.push(format!("{}: {error}", text(task, "id").unwrap_or("?"))),
        }
    }
    Ok(json!({ "moved_to_clearance": moved, "overdue_announced": late, "failures": failures }))
}

handler! {
    fn begin_departure(input: Begin) -> Record {
        begin(input)
    }

    /// Confirm a departure: the offboarding checklist is made and the notice period starts.
    fn confirm_departure(input: Confirm) -> Record {
        confirm(input)
    }

    fn update_departure(input: Change) -> Record {
        change(input)
    }

    fn cancel_departure(input: Cancel) -> Record {
        cancel(input)
    }

    /// End the employment through hr once the checklist is clear.
    fn close_departure(input: Close) -> Record {
        close(input)
    }

    fn add_return_item(input: NewItem) -> Record {
        new_item(input)
    }

    fn set_return_item(input: SetItem) -> Record {
        set_item(input)
    }

    fn add_settlement_line(input: NewLine) -> Record {
        new_line(input)
    }

    fn settle_line_item(input: SettleLine) -> Record {
        settle_line(input)
    }

    fn departure_checklist(input: Id) -> Value {
        checklist(&input.id)
    }

    fn get_departure(input: Id) -> Option<Record> {
        db::get("ob_departure", &input.id)
    }

    fn list_departures(input: Option<Search>) -> Vec<Record> {
        let mut find = db::find::<Record>("ob_departure").order_by("-last_day").limit(500);
        if let Some(status) = input.and_then(|i| i.status) {
            find = find.filter("status", status.as_str());
        }
        find.all()
    }

    fn onboarding_tick(_: Empty) -> Value {
        tick()
    }
}
