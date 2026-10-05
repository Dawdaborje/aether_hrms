//! Runs: a template expanded for one person, with a task per step.
//!
//! A run is started once and can be started again without duplicating anything (a task is unique per
//! run and step). Dates follow the working calendar; assignees are resolved when the task is made and
//! the fallback that was used is written on it. A status is computed from the tasks. Cancelling voids
//! tasks, it never deletes them.

use aether_sdk::dates::{format_date, parse_date, Duration, NaiveDate};
use aether_sdk::db::Filter;
use aether_sdk::prelude::*;

use crate::common::{
    caller_user, calendar_for, employee, id_of, is_officer, my_employee, next_number, require, require_manager, require_officer, text, today_date, user_of, Record,
};
use crate::rules::{due_dates, gate_open, resolve_assignee, run_status, walk_chain, AssigneeKind, People, Roll, TaskState};
use crate::setup::pick_template;

/// The days steps are counted from.
#[derive(Debug, Clone, Copy, Default)]
pub struct Anchors {
    pub start: Option<NaiveDate>,
    pub last_day: Option<NaiveDate>,
    pub notice: Option<NaiveDate>,
}

impl Anchors {
    fn for_step(&self, anchor: &str) -> Result<NaiveDate> {
        match anchor {
            "start" => self.start,
            "last_day" => self.last_day,
            _ => self.notice.or(self.last_day),
        }
        .ok_or_else(|| Error::msg(format!("a step is anchored on `{anchor}`, which this run does not have")))
    }
}

pub struct Start {
    pub kind: &'static str,
    pub template: Record,
    pub anchors: Anchors,
    pub primary: NaiveDate,
    pub party: Option<String>,
    pub employee: Option<Record>,
    pub departure: Option<String>,
    pub owner: Option<Record>,
}

fn people_for(employee_record: Option<&Record>, owner: Option<&Record>) -> Result<People> {
    let mut people = People { owner: owner.and_then(user_of), caller: caller_user()?, ..People::default() };
    if let Some(person) = employee_record {
        people.subject = Some(user_of(person));
        let first = text(person, "manager").map(str::to_string);
        let (chain, _looped) = walk_chain(first, |id| Ok(text(&employee(id)?, "manager").map(str::to_string)))?;
        for id in chain {
            people.managers.push(user_of(&employee(&id)?));
        }
    }
    Ok(people)
}

fn steps_of(template_id: &str) -> Result<Vec<Record>> {
    db::find("ob_step").filter("template", template_id).order_by("sequence").limit(200).all()
}

fn int_of(record: &Record, field: &str) -> i64 {
    record.get(field).and_then(Value::as_i64).unwrap_or(0)
}

/// The dates and assignee of one step.
fn plan_task(step: &Record, anchors: &Anchors, calendar: &aether_sdk::dates::Calendar, people: &People) -> Result<Record> {
    let anchor = anchors.for_step(text(step, "anchor").unwrap_or("start"))?;
    let roll = Roll::parse(text(step, "roll").unwrap_or("next")).ok_or_else(|| Error::msg("a step has an unknown roll"))?;
    let (start, end) = due_dates(anchor, int_of(step, "offset_days"), int_of(step, "duration_days"), roll, calendar)?;
    let kind = AssigneeKind::parse(text(step, "assignee_kind").unwrap_or("owner")).ok_or_else(|| Error::msg("a step has an unknown assignee kind"))?;
    let who = resolve_assignee(kind, text(step, "assignee_user"), text(step, "assignee_role"), people);
    Ok(json!({
        "name": step["name"], "due_start": format_date(start), "due_end": format_date(end),
        "assignee": who.user, "assignee_role": who.role, "assignee_note": who.note,
        "gate": step.get("gate").cloned().unwrap_or(json!("none")), "weight": int_of(step, "weight").max(1),
    }))
}

fn span_of(steps: &[Record], anchors: &Anchors) -> Result<(NaiveDate, NaiveDate)> {
    let mut low: Option<NaiveDate> = None;
    let mut high: Option<NaiveDate> = None;
    for step in steps {
        let anchor = anchors.for_step(text(step, "anchor").unwrap_or("start"))?;
        let from = anchor + Duration::days(int_of(step, "offset_days"));
        let to = from + Duration::days(int_of(step, "duration_days") + 40);
        low = Some(low.map_or(from - Duration::days(40), |l| l.min(from - Duration::days(40))));
        high = Some(high.map_or(to, |h| h.max(to)));
    }
    let today = today_date()?;
    Ok((low.unwrap_or(today), high.unwrap_or(today)))
}

fn hold_key(task_id: &str) -> String {
    task_id.to_string()
}

fn place_hold(run: &Record, task: &Record) -> Result<()> {
    if text(task, "gate") != Some("hire") || text(run, "kind") != Some("onboard") {
        return Ok(());
    }
    let Some(party) = text(run, "party") else { return Ok(()) };
    let _: Value = plugins::call(
        "hr",
        "place_hire_hold",
        &json!({ "party": party, "holder": "hr_onboarding", "key": hold_key(id_of(task)?), "reason": format!("{} is not done", text(task, "name").unwrap_or("a task")) }),
    )?;
    Ok(())
}

fn release_hold(run: &Record, task: &Record) -> Result<()> {
    if text(task, "gate") != Some("hire") {
        return Ok(());
    }
    let Some(party) = text(run, "party") else { return Ok(()) };
    let _: Value = plugins::call("hr", "release_hire_hold", &json!({ "party": party, "holder": "hr_onboarding", "key": hold_key(id_of(task)?) }))?;
    Ok(())
}

fn tasks_of(run_id: &str) -> Result<Vec<Record>> {
    db::find("ob_task").filter("run", run_id).order_by("due_end").limit(500).all()
}

/// Expand the template into tasks; ones that exist are left alone.
fn make_tasks(run: &Record, steps: &[Record], anchors: &Anchors, employee_record: Option<&Record>, owner: Option<&Record>) -> Result<()> {
    let (from, to) = span_of(steps, anchors)?;
    let calendar = calendar_for(employee_record, from, to)?;
    let people = people_for(employee_record, owner)?;
    let run_id = id_of(run)?;
    for step in steps {
        if db::count("ob_task", Filter::eq("run", run_id).and(Filter::eq("step", id_of(step)?)))? > 0 {
            continue;
        }
        let mut data = plan_task(step, anchors, &calendar, &people)?;
        data["run"] = json!(run_id);
        data["step"] = step["id"].clone();
        data["state"] = json!("todo");
        let made: Record = db::create("ob_task", &data).map_err(|e| e.or("could not make a task"))?;
        place_hold(run, &made)?;
    }
    Ok(())
}

pub fn start(start: Start) -> Result<Record> {
    let template_id = id_of(&start.template)?;
    let steps = steps_of(template_id)?;
    if steps.is_empty() {
        return Err(Error::msg("the template has no steps"));
    }
    if text(&start.template, "is_active") == Some("false") || start.template.get("is_active") == Some(&json!(false)) {
        return Err(Error::msg("the template is switched off"));
    }
    let mut data = json!({
        "number": next_number(if start.kind == "onboard" { "onboard" } else { "offboard" }, if start.kind == "onboard" { "ONB-" } else { "OFF-" }, 5)?,
        "kind": start.kind, "template": template_id, "anchor_date": format_date(start.primary), "status": "pending",
    });
    for (field, value) in [("party", &start.party), ("departure", &start.departure)] {
        if let Some(value) = value {
            data[field] = json!(value);
        }
    }
    if let Some(person) = &start.employee {
        data["employee"] = person["id"].clone();
    }
    if let Some(owner) = &start.owner {
        data["owner"] = owner["id"].clone();
    }
    let run: Record = db::create("ob_run", &data).map_err(|e| e.or("could not start the run"))?;
    if let Err(error) = make_tasks(&run, &steps, &start.anchors, start.employee.as_ref(), start.owner.as_ref()) {
        // Undo what was made, holds first, so nothing is left blocking a hire.
        let _ = discard(&run);
        return Err(error);
    }
    events::emit("onboarding_run_started", &json!({ "run": run["id"], "kind": start.kind }))?;
    refresh(id_of(&run)?)
}

fn discard(run: &Record) -> Result<()> {
    for task in tasks_of(id_of(run)?)? {
        let _ = release_hold(run, &task);
        db::delete::<Record>("ob_task", id_of(&task)?)?;
    }
    db::delete::<Record>("ob_run", id_of(run)?)?;
    Ok(())
}

/// Recompute a run's status from its tasks.
pub fn refresh(run_id: &str) -> Result<Record> {
    let run = require("ob_run", run_id, "run")?;
    if text(&run, "status") == Some("cancelled") {
        return Ok(run);
    }
    let tasks = tasks_of(run_id)?;
    let states: Vec<(TaskState, i64)> = tasks.iter().filter_map(|t| TaskState::parse(text(t, "state")?).map(|s| (s, int_of(t, "weight")))).collect();
    let status = run_status(&states).as_str();
    if text(&run, "status") == Some(status) {
        return Ok(run);
    }
    let done = db::update::<Record>("ob_run", run_id, &json!({ "status": status }))?.ok_or_else(|| Error::msg("the run is gone"))?;
    if status == "done" {
        events::emit("onboarding_run_completed", &json!({ "run": run_id, "kind": run["kind"] }))?;
    }
    Ok(done)
}

/// Whether the tasks that carry `gate` are all finished.
pub fn gate_clear(run_id: &str, gate: &str) -> Result<usize> {
    let tasks = tasks_of(run_id)?;
    let flagged: Vec<(TaskState, bool)> =
        tasks.iter().filter_map(|t| TaskState::parse(text(t, "state")?).map(|s| (s, text(t, "gate") == Some(gate)))).collect();
    Ok(if gate_open(&flagged) { 0 } else { flagged.iter().filter(|(s, on)| *on && *s == TaskState::Todo).count() })
}

/// Recompute the dates of open tasks after an anchor moved.
pub fn rebase(run_id: &str, anchors: &Anchors) -> Result<()> {
    let run = require("ob_run", run_id, "run")?;
    let person = match text(&run, "employee") {
        Some(id) => Some(employee(id)?),
        None => None,
    };
    let template = text(&run, "template").unwrap_or_default();
    let steps = steps_of(template)?;
    let (from, to) = span_of(&steps, anchors)?;
    let calendar = calendar_for(person.as_ref(), from, to)?;
    for task in tasks_of(run_id)? {
        if text(&task, "state") != Some("todo") {
            continue;
        }
        let Some(step) = steps.iter().find(|s| text(s, "id") == text(&task, "step")) else { continue };
        let anchor = anchors.for_step(text(step, "anchor").unwrap_or("start"))?;
        let roll = Roll::parse(text(step, "roll").unwrap_or("next")).ok_or_else(|| Error::msg("a step has an unknown roll"))?;
        let (start, end) = due_dates(anchor, int_of(step, "offset_days"), int_of(step, "duration_days"), roll, &calendar)?;
        db::update::<Record>("ob_task", id_of(&task)?, &json!({ "due_start": format_date(start), "due_end": format_date(end), "overdue_notified": false }))?;
    }
    Ok(())
}

pub fn anchors_of(run: &Record) -> Result<Anchors> {
    if text(run, "kind") == Some("onboard") {
        return Ok(Anchors { start: Some(parse_date(text(run, "anchor_date").unwrap_or_default())?), ..Anchors::default() });
    }
    let departure = require("ob_departure", text(run, "departure").unwrap_or_default(), "departure")?;
    let last_day = parse_date(text(&departure, "last_day").unwrap_or_default())?;
    Ok(Anchors { start: None, last_day: Some(last_day), notice: text(&departure, "notice_date").map(parse_date).transpose()? })
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Begin {
    #[serde(default)]
    party: Option<String>,
    #[serde(default)]
    employee: Option<String>,
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    start_date: Option<String>,
    #[serde(default)]
    owner: Option<String>,
}

fn begin_onboarding(input: Begin) -> Result<Record> {
    require_officer()?;
    let person = match (&input.employee, &input.party) {
        (Some(id), _) => Some(employee(id)?),
        (None, Some(party)) => plugins::call::<Option<Record>>("hr", "employee_of_party", &json!({ "id": party }))?,
        (None, None) => return Err(Error::msg("name the person: an employee, or someone in the directory")),
    };
    let party = match (&input.party, &person) {
        (Some(party), _) => Some(party.clone()),
        (None, Some(person)) => text(person, "party").map(str::to_string),
        (None, None) => None,
    };
    let template = match &input.template {
        Some(id) => require("ob_template", id, "template")?,
        None => {
            let placement = person.as_ref().map(|p| (text(p, "department").map(str::to_string), text(p, "job").map(str::to_string)));
            let (department, job) = placement.unwrap_or_default();
            pick_template("onboard", department.as_deref(), job.as_deref())?
        }
    };
    if text(&template, "kind") != Some("onboard") {
        return Err(Error::msg("that is not an onboarding template"));
    }
    if let Some(party) = &party {
        let open = db::count("ob_run", Filter::eq("party", party.as_str()).and(Filter::eq("kind", "onboard")).and(Filter::one_of("status", ["pending", "in_process"])))?;
        if open > 0 {
            return Err(Error::msg("this person already has an onboarding in progress"));
        }
    }
    let day = match &input.start_date {
        Some(date) => parse_date(date)?,
        None => today_date()?,
    };
    let owner = match &input.owner {
        Some(id) => Some(employee(id)?),
        None => my_employee()?,
    };
    start(Start {
        kind: "onboard",
        template,
        anchors: Anchors { start: Some(day), ..Anchors::default() },
        primary: day,
        party,
        employee: person,
        departure: None,
        owner,
    })
}

#[derive(Deserialize)]
struct Complete {
    id: String,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct Reasoned {
    id: String,
    reason: String,
}

#[derive(Deserialize)]
struct Reassign {
    id: String,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    role: Option<String>,
}

#[derive(Deserialize)]
struct Move {
    id: String,
    start_date: String,
}

fn live_task(id: &str) -> Result<(Record, Record)> {
    let task = require("ob_task", id, "task")?;
    let run = require("ob_run", text(&task, "run").unwrap_or_default(), "run")?;
    if text(&run, "status") == Some("cancelled") {
        return Err(Error::msg("this run was cancelled"));
    }
    Ok((run, task))
}

fn may_do(task: &Record) -> Result<bool> {
    if is_officer()? {
        return Ok(true);
    }
    let context = context::current()?;
    Ok(text(task, "assignee").is_some_and(|a| Some(a) == context.actor.id.as_deref()) || text(task, "assignee_role").is_some_and(|r| context.has_role(r)))
}

fn complete(input: Complete) -> Result<Record> {
    let (run, task) = live_task(&input.id)?;
    if text(&task, "state") != Some("todo") {
        return Err(Error::msg(format!("this task is {}", text(&task, "state").unwrap_or("closed"))));
    }
    if !may_do(&task)? {
        return Err(Error::msg("this task is not yours to do"));
    }
    let mut changes = json!({ "state": "done", "done_on": format_date(today_date()?) });
    if let Some(note) = &input.note {
        changes["note"] = json!(note);
    }
    let done = db::update::<Record>("ob_task", &input.id, &changes)?.ok_or_else(|| Error::msg("the task is gone"))?;
    release_hold(&run, &task)?;
    events::emit("onboarding_task_done", &json!({ "task": input.id, "run": run["id"] }))?;
    refresh(id_of(&run)?)?;
    Ok(done)
}

fn waive(input: Reasoned) -> Result<Record> {
    require_manager()?;
    let (run, task) = live_task(&input.id)?;
    if text(&task, "state") != Some("todo") {
        return Err(Error::msg("only an open task can be waived"));
    }
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why the task is waived"));
    }
    let done = db::update::<Record>("ob_task", &input.id, &json!({ "state": "waived", "waive_reason": input.reason, "done_on": format_date(today_date()?) }))?
        .ok_or_else(|| Error::msg("the task is gone"))?;
    release_hold(&run, &task)?;
    refresh(id_of(&run)?)?;
    Ok(done)
}

fn reopen(input: Id) -> Result<Record> {
    require_manager()?;
    let (run, task) = live_task(&input.id)?;
    if !matches!(text(&task, "state"), Some("done" | "waived")) {
        return Err(Error::msg("only a finished task can be reopened"));
    }
    let open = db::update::<Record>("ob_task", &input.id, &json!({ "state": "todo", "done_on": null, "waive_reason": null }))?.ok_or_else(|| Error::msg("the task is gone"))?;
    place_hold(&run, &open)?;
    refresh(id_of(&run)?)?;
    Ok(open)
}

fn reassign(input: Reassign) -> Result<Record> {
    require_officer()?;
    let (_, task) = live_task(&input.id)?;
    if text(&task, "state") != Some("todo") {
        return Err(Error::msg("only an open task can be reassigned"));
    }
    if input.user.is_none() == input.role.is_none() {
        return Err(Error::msg("give a user or a role, not both"));
    }
    db::update::<Record>(
        "ob_task",
        &input.id,
        &json!({ "assignee": input.user, "assignee_role": input.role, "assignee_note": "reassigned by hand" }),
    )?
    .ok_or_else(|| Error::msg("the task is gone"))
}

/// Void what is open in a run and let go of its holds; finished tasks stay as they are.
pub fn void_run(run_id: &str, reason: &str) -> Result<Record> {
    let run = require("ob_run", run_id, "run")?;
    if text(&run, "status") == Some("cancelled") {
        return Err(Error::msg("this run is already cancelled"));
    }
    for task in tasks_of(run_id)? {
        if text(&task, "state") == Some("todo") {
            db::update::<Record>("ob_task", id_of(&task)?, &json!({ "state": "void" }))?;
        }
        release_hold(&run, &task)?;
    }
    db::update::<Record>("ob_run", run_id, &json!({ "status": "cancelled", "cancel_reason": reason }))?.ok_or_else(|| Error::msg("the run is gone"))
}

fn cancel(input: Reasoned) -> Result<Record> {
    require_officer()?;
    let run = require("ob_run", &input.id, "run")?;
    if text(&run, "kind") == Some("offboard") {
        return Err(Error::msg("an offboarding run ends with its departure: cancel the departure"));
    }
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why the run is cancelled"));
    }
    void_run(&input.id, &input.reason)
}

fn move_start(input: Move) -> Result<Record> {
    require_officer()?;
    let run = require("ob_run", &input.id, "run")?;
    if text(&run, "kind") != Some("onboard") || text(&run, "status") == Some("cancelled") {
        return Err(Error::msg("only an onboarding in progress has a start day to move"));
    }
    let day = parse_date(&input.start_date)?;
    let updated = db::update::<Record>("ob_run", &input.id, &json!({ "anchor_date": format_date(day) }))?.ok_or_else(|| Error::msg("the run is gone"))?;
    rebase(&input.id, &anchors_of(&updated)?)?;
    Ok(updated)
}

/// Ids of the tasks the caller may see and do: theirs, and those for a role they hold.
fn my_task_ids() -> Result<Vec<String>> {
    let context = context::current()?;
    let mut ids = Vec::new();
    if let Some(user) = &context.actor.id {
        let mine: Vec<Record> = db::find::<Record>("ob_task").filter("assignee", user.as_str()).limit(500).all()?;
        ids.extend(mine.iter().filter_map(|t| text(t, "id").map(str::to_string)));
    }
    for role in context.roles.iter().filter(|r| r.as_str() != "org_admin") {
        let held: Vec<Record> = db::find::<Record>("ob_task").filter("assignee_role", role.as_str()).limit(500).all()?;
        ids.extend(held.iter().filter_map(|t| text(t, "id").map(str::to_string)));
    }
    ids.truncate(500);
    Ok(ids)
}

#[derive(Deserialize, Default)]
struct Search {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

fn tasks_for_me(include_done: bool) -> Result<Vec<Record>> {
    let ids = my_task_ids()?;
    let mut out = Vec::new();
    for id in ids {
        if let Some(task) = db::get::<Record>("ob_task", &id)? {
            if include_done || text(&task, "state") == Some("todo") {
                out.push(task);
            }
        }
    }
    out.sort_by(|a, b| text(a, "due_end").cmp(&text(b, "due_end")));
    Ok(out)
}

#[derive(Deserialize, Default)]
struct Mine {
    #[serde(default)]
    include_done: bool,
}

handler! {
    /// Start onboarding for a person: the best template unless one is named.
    fn start_onboarding(input: Begin) -> Record {
        begin_onboarding(input)
    }

    fn complete_task(input: Complete) -> Record {
        complete(input)
    }

    fn waive_task(input: Reasoned) -> Record {
        waive(input)
    }

    fn reopen_task(input: Id) -> Record {
        reopen(input)
    }

    fn reassign_task(input: Reassign) -> Record {
        reassign(input)
    }

    fn cancel_run(input: Reasoned) -> Record {
        cancel(input)
    }

    fn move_start_day(input: Move) -> Record {
        move_start(input)
    }

    fn get_run(input: Id) -> Option<Record> {
        db::get("ob_run", &input.id)
    }

    fn list_runs(input: Option<Search>) -> Vec<Record> {
        let input = input.unwrap_or_default();
        let mut find = db::find::<Record>("ob_run").order_by("-anchor_date").limit(500);
        for (field, value) in [("kind", &input.kind), ("status", &input.status)] {
            if let Some(value) = value {
                find = find.filter(field, value.as_str());
            }
        }
        find.all()
    }

    fn list_run_tasks(input: Id) -> Vec<Record> {
        tasks_of(&input.id)
    }

    /// The caller's own open tasks, and those for a role they hold.
    fn my_tasks(input: Option<Mine>) -> Vec<Record> {
        tasks_for_me(input.is_some_and(|i| i.include_done))
    }

    fn rule_var_my_tasks(_: Empty) -> Vec<String> {
        my_task_ids()
    }
}
