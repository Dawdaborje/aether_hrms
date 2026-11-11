//! Positions: seats in the organization, each with a headcount.

use aether_sdk::db::{Filter, Walk};
use aether_sdk::prelude::*;
use crate::rules::status;

use crate::common::{explain, id_of, pick, require, text, Record};

const EDITABLE: &[&str] = &["name", "code", "job", "department", "parent", "headcount", "status", "grade", "notes", "base_wage", "currency"];

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Change {
    id: String,
    /// With a new base wage: also move the people who hold the position onto it, as new terms from today.
    #[serde(default)]
    apply_to_holders: bool,
    #[serde(flatten)]
    fields: Record,
}

#[derive(Deserialize)]
struct Vacancies {
    #[serde(default)]
    department: Option<String>,
}

/// A position as shown to someone who may not see its pay: HR's own code reads the wage, only an HR manager is shown it.
fn without_pay(mut position: Record) -> Record {
    if !context::current().is_ok_and(|who| who.has_role("hr.hr_manager")) {
        if let Some(fields) = position.as_object_mut() {
            fields.remove("base_wage");
            fields.remove("currency");
        }
    }
    position
}

fn bare(id: &str) -> String {
    id.rsplit_once(':').map_or(id, |(_, key)| key).to_string()
}

/// People who hold the position now.
pub fn holders(position: &str) -> Result<u64> {
    let present: Vec<&str> = status::PRESENT.to_vec();
    db::count("employee", Filter::eq("position", position).and(Filter::one_of("status", present)))
}

/// The organization's currency: the one set on its default company during setup.
pub fn org_currency() -> Option<String> {
    let company: Option<Record> = plugins::call("company", "get_default_company", &json!({})).ok().flatten();
    company.and_then(|c| text(&c, "currency").map(str::to_string))
}

/// Keep `status` in step with the seats: open while one is free, filled once all are held. Frozen is HR's call and stays.
pub fn sync_status(position: &str) -> Result<()> {
    let Some(row) = db::get::<Record>("position", position)? else { return Ok(()) };
    let current = text(&row, "status").unwrap_or("open");
    if current == "frozen" {
        return Ok(());
    }
    let headcount = row.get("headcount").and_then(Value::as_u64).unwrap_or(1);
    let wanted = if headcount > 0 && holders(position)? >= headcount { "filled" } else { "open" };
    if current != wanted {
        db::update::<Record>("position", position, &json!({ "status": wanted }))?;
    }
    Ok(())
}

/// Sync the seats of the positions an employee leaves and joins (either may be absent).
pub fn sync_moved(before: Option<&str>, after: Option<&str>) -> Result<()> {
    for id in [before, after].into_iter().flatten() {
        sync_status(id)?;
    }
    Ok(())
}

/// People holding each position now, for every position at once (keyed by the bare id): one grouped count
/// instead of one query per position.
fn holder_counts() -> Result<std::collections::HashMap<String, u64>> {
    let present: Vec<&str> = status::PRESENT.to_vec();
    let rows = db::aggregate(
        "employee",
        Filter::one_of("status", present).and(Filter::is_set("position")),
        &["position"],
        &[("people", aether_sdk::db::Figure::Count)],
    )?;
    Ok(rows
        .iter()
        .filter_map(|row| Some((bare(row.get("position")?.as_str()?), row.get("people")?.as_u64()?)))
        .collect())
}

/// The organization's currency, looked up at most once however many positions need it.
struct DefaultCurrency(Option<Option<String>>);

impl DefaultCurrency {
    fn get(&mut self) -> Option<String> {
        self.0.get_or_insert_with(org_currency).clone()
    }
}

/// Add the live seat count: `held` people in it and `free` seats left.
fn with_seats(mut position: Record) -> Result<Record> {
    let held = holders(id_of(&position)?)?;
    seat_fields(&mut position, held, &mut DefaultCurrency(None));
    Ok(position)
}

fn seat_fields(position: &mut Record, held: u64, currency: &mut DefaultCurrency) {
    let headcount = position.get("headcount").and_then(Value::as_u64).unwrap_or(1);
    position["held"] = json!(held);
    position["free"] = json!(headcount.saturating_sub(held));
    if text(position, "currency").is_none() {
        if let Some(currency) = currency.get() {
            position["currency"] = json!(currency);
        }
    }
}

fn check(data: &Record, current: Option<&Record>) -> Result<()> {
    if let Some(job) = text(data, "job") {
        require("job", job, "job")?;
    }
    if let Some(department) = text(data, "department") {
        let department = require("department", department, "department")?;
        if department.get("is_active") == Some(&json!(false)) {
            return Err(Error::msg("the department is closed"));
        }
    }
    if let Some(parent) = text(data, "parent") {
        if current.and_then(|c| text(c, "id")) == Some(parent) {
            return Err(Error::msg("a position cannot report to itself"));
        }
        let boss = require("position", parent, "position")?;
        if let Some(id) = current.and_then(|c| text(c, "id")) {
            let above: Vec<Record> = db::tree("position", "parent", id_of(&boss)?).walk(Walk::Up).all()?;
            if above.iter().any(|row| text(row, "id").is_some_and(|a| bare(a) == bare(id))) {
                return Err(Error::msg("that position already reports to this one, directly or through others: it would make a loop"));
            }
        }
        // A position reports to one of higher rank (or equal, by the HR default).
        let mut subject = current.cloned().unwrap_or_else(|| json!({}));
        for field in ["grade", "job"] {
            if let Some(value) = data.get(field) {
                subject[field] = value.clone();
            }
        }
        if !crate::rank::allowed(crate::rank::position_rank(&boss)?, crate::rank::position_rank(&subject)?)? {
            return Err(Error::msg(format!("{}: choose a position of higher rank to report to", crate::rank::explain_rule()?)));
        }
    }
    if let Some(headcount) = data.get("headcount") {
        let headcount = headcount.as_i64().ok_or_else(|| Error::msg("headcount is a whole number"))?;
        if headcount < 0 {
            return Err(Error::msg("headcount cannot be negative"));
        }
        if let Some(id) = current.and_then(|c| text(c, "id")) {
            let held = holders(id)?;
            if (headcount as u64) < held {
                return Err(Error::msg(format!("{held} people hold this position: the headcount cannot go below that")));
            }
        }
    }
    if let Some(state) = text(data, "status") {
        if !["open", "filled", "frozen"].contains(&state) {
            return Err(Error::msg("status is open, filled or frozen"));
        }
    }
    Ok(())
}

fn new_position(input: Record) -> Result<Record> {
    if ["base_wage", "currency"].iter().any(|f| input.get(*f).is_some_and(|v| !v.is_null())) {
        crate::common::require_pay_manager()?;
    }
    for required in ["name", "job"] {
        if text(&input, required).is_none() {
            return Err(Error::msg(format!("a position needs its {required}")));
        }
    }
    let mut data = pick(&input, EDITABLE);
    // No department given: take the job's.
    if text(&data, "department").is_none() {
        let job = require("job", text(&data, "job").unwrap_or_default(), "job")?;
        match text(&job, "department") {
            Some(department) => data["department"] = json!(department),
            None => return Err(Error::msg("a position needs its department: this job has none to use")),
        }
    }
    if text(&data, "currency").is_none() {
        if let Some(currency) = org_currency() {
            data["currency"] = json!(currency);
        }
    }
    if text(&data, "parent").is_none() {
        if let (Some(job), Some(department)) = (text(&data, "job"), text(&data, "department")) {
            if let Some(parent) = crate::rank::default_parent(job, department, text(&data, "grade"))? {
                data["parent"] = json!(parent);
            }
        }
    }
    check(&data, None)?;
    explain(db::create("position", &data), "could not create the position")
}

/// Used by Desk demo generation (`demo` module).
pub(crate) fn create_for_demo(input: Record) -> Result<Record> {
    new_position(input)
}

/// Used by Desk demo generation (`demo` module).
pub(crate) fn create_job_for_demo(input: Record) -> Result<Record> {
    if text(&input, "name").is_none() {
        return Err(Error::msg("a job needs a name"));
    }
    explain(
        db::create(
            "job",
            &crate::common::pick(&input, JOB_FIELDS),
        ),
        "could not create the job",
    )
}

fn change_position(input: Change) -> Result<Record> {
    let existing = require("position", &input.id, "position")?;
    let data = pick(&input.fields, EDITABLE);
    if data.as_object().is_some_and(|fields| fields.is_empty()) {
        return Err(Error::msg("there is nothing to change"));
    }
    check(&data, Some(&existing))?;
    let pay_changed = ["base_wage", "currency"].iter().any(|f| data.get(*f).is_some_and(|v| Some(v) != existing.get(*f)));
    if pay_changed || input.apply_to_holders {
        crate::common::require_pay_manager()?;
    }
    if input.apply_to_holders {
        // Applying is asked for on its own too (the wage may already be saved), so it does not depend on the wage changing.
        if crate::employment::pay_mode()? == "individual" {
            return Err(Error::msg(
                "pay is entered for each person: set the pay mode in HR defaults to by position or hybrid to apply a position's wage to the people in it",
            ));
        }
        let has_wage = data.get("base_wage").or_else(|| existing.get("base_wage")).is_some_and(|w| !w.is_null() && w.as_str() != Some(""));
        if !has_wage {
            return Err(Error::msg("this position has no base wage to apply: enter one first"));
        }
    }
    let updated = explain(db::update::<Record>("position", id_of(&existing)?, &data), "could not change the position")?
        .ok_or_else(|| Error::msg("the position is gone"))?;
    let mut updated = updated;
    sync_status(id_of(&updated)?)?;
    updated["status"] = json!(db::get::<Record>("position", id_of(&updated)?)?.and_then(|p| text(&p, "status").map(str::to_string)).unwrap_or_default());
    if input.apply_to_holders {
        let moved = crate::employment::apply_position_pay(&updated)?;
        updated["saved_message"] = json!(match moved {
            0 => "Saved. No one was moved: nobody holds this position, or everyone here has their own wage.".to_string(),
            1 => "Saved. 1 person moved onto the position's wage from today.".to_string(),
            n => format!("Saved. {n} people moved onto the position's wage from today."),
        });
    }
    Ok(updated)
}

/// Positions with free seats, how many are held and how many are free.
fn open_positions(input: Vacancies) -> Result<Vec<Record>> {
    let mut filter = Filter::ne("status", "frozen");
    if let Some(department) = input.department {
        filter = filter.and(Filter::eq("department", department));
    }
    let positions: Vec<Record> = db::find::<Record>("position").matching(filter).order_by("name").limit(1000).all()?;
    let counts = holder_counts()?;
    let mut out = Vec::new();
    for mut position in positions {
        let held = counts.get(&bare(id_of(&position)?)).copied().unwrap_or(0);
        let headcount = position.get("headcount").and_then(Value::as_u64).unwrap_or(1);
        if headcount > held {
            position["held"] = json!(held);
            position["free"] = json!(headcount - held);
            out.push(without_pay(position));
        }
    }
    Ok(out)
}

/// A position's seats and how many are held: for planning hires.
fn seats(input: Id) -> Result<Value> {
    let position = require("position", &input.id, "position")?;
    let headcount = position.get("headcount").and_then(Value::as_u64).unwrap_or(1);
    let held = holders(&input.id)?;
    Ok(json!({ "position": input.id, "headcount": headcount, "held": held, "free": headcount.saturating_sub(held), "frozen": text(&position, "status") == Some("frozen") }))
}

const JOB_FIELDS: &[&str] = &["name", "code", "department", "description", "is_active", "parent", "grade"];

fn check_job(data: &Record, current: Option<&str>) -> Result<()> {
    if let Some(parent) = text(data, "parent") {
        if current.is_some_and(|id| bare(id) == bare(parent)) {
            return Err(Error::msg("a job cannot report to itself"));
        }
        let boss = require("job", parent, "parent job")?;
        if let Some(id) = current {
            let above: Vec<Record> = db::tree("job", "parent", id_of(&boss)?).walk(Walk::Up).all()?;
            if above.iter().any(|row| text(row, "id").is_some_and(|a| bare(a) == bare(id))) {
                return Err(Error::msg("that job already reports to this one, directly or through others: it would make a loop"));
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct ParentSearch {
    /// The position being edited.
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
    /// The job and grade as typed on the form, which may differ from the saved ones.
    #[serde(default)]
    job: Option<String>,
    #[serde(default)]
    grade: Option<String>,
}

/// What the "Reports to" picker of a position offers: not itself nor anything below it, positions of the job
/// above its job when it has one, and only those of high enough rank.
fn possible_parents(input: ParentSearch) -> Result<Value> {
    let mut left_out: Vec<String> = Vec::new();
    let mut subject: Option<Record> = None;
    if let Some(id) = input.id.as_deref().filter(|id| !id.trim().is_empty()) {
        left_out.push(bare(id));
        let below: Vec<Record> = db::tree("position", "parent", id).walk(Walk::Down).all()?;
        left_out.extend(below.iter().filter_map(|r| text(r, "id")).map(bare));
        subject = db::get::<Record>("position", id)?;
    }
    // What is on the form wins over what is saved: a job or grade being changed decides who may be the boss.
    for (field, sent) in [("job", &input.job), ("grade", &input.grade)] {
        if let Some(sent) = sent {
            let subject = subject.get_or_insert_with(|| json!({}));
            subject[field] = if sent.trim().is_empty() { Value::Null } else { json!(sent.trim()) };
        }
    }
    let jobs: Vec<String> = match subject.as_ref().and_then(|s| text(s, "job")) {
        Some(job) => crate::rank::job_ancestors(job)?.iter().map(|j| bare(j)).collect(),
        None => Vec::new(),
    };
    let under = match &subject {
        Some(s) => crate::rank::position_rank(s)?,
        None => None,
    };
    let q = input.text.as_deref().map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty());
    let mut rows: Vec<Record> = db::find::<Record>("position").order_by("name").limit(1000).all()?;
    rows.retain(|row| text(row, "id").is_none_or(|id| !left_out.contains(&bare(id))));
    if let Some(q) = &q {
        rows.retain(|row| ["name", "code"].iter().any(|f| text(row, f).is_some_and(|v| v.to_lowercase().contains(q.as_str()))));
    }
    // Positions of the job above, when there are any; otherwise every position.
    let by_job: Vec<Record> = rows.iter().filter(|r| text(r, "job").is_some_and(|j| jobs.contains(&bare(j)))).cloned().collect();
    if !jobs.is_empty() && !by_job.is_empty() {
        rows = by_job;
    }
    rows.retain(|row| crate::rank::position_rank(row).and_then(|boss| crate::rank::allowed(boss, under)).unwrap_or(true));
    let total = rows.len();
    let rows: Vec<Record> = rows.into_iter().skip(input.offset.unwrap_or(0)).take(input.limit.unwrap_or(50).min(500)).map(without_pay).collect();
    Ok(json!({ "rows": rows, "total": total }))
}

handler! {
    fn list_possible_parents(input: ParentSearch) -> Value {
        possible_parents(input)
    }

    /// The seats of a position and how many are held.
    fn position_seats(input: Id) -> Value {
        seats(input)
    }

    fn create_position(input: Record) -> Record {
        new_position(input)
    }

    fn update_position(input: Change) -> Record {
        change_position(input)
    }

    fn get_position(input: Id) -> Option<Record> {
        db::get::<Record>("position", &input.id)?.map(|p| with_seats(p).map(without_pay)).transpose()
    }

    fn list_positions(_: Empty) -> Vec<Record> {
        let counts = holder_counts()?;
        let mut currency = DefaultCurrency(None);
        let mut out = Vec::new();
        for mut position in db::find::<Record>("position").order_by("name").limit(1000).all()? {
            let held = counts.get(&bare(id_of(&position)?)).copied().unwrap_or(0);
            seat_fields(&mut position, held, &mut currency);
            out.push(without_pay(position));
        }
        Ok(out)
    }

    /// Positions that still have a free seat.
    fn vacancies(input: Vacancies) -> Vec<Record> {
        open_positions(input)
    }

    fn create_job(input: Record) -> Record {
        if text(&input, "name").is_none() {
            return Err(Error::msg("a job needs a name"));
        }
        let data = crate::common::pick(&input, JOB_FIELDS);
        check_job(&data, None)?;
        explain(db::create("job", &data), "could not create the job")
    }

    fn update_job(input: Change) -> Record {
        let data = crate::common::pick(&input.fields, JOB_FIELDS);
        check_job(&data, Some(&input.id))?;
        // A new default grade must not break a reporting line that holds today.
        if let Some(grade) = data.get("grade") {
            let before = db::get::<Record>("job", &input.id)?;
            let old = before.as_ref().and_then(|j| text(j, "grade")).unwrap_or_default();
            let new = grade.as_str().unwrap_or_default();
            if old != new {
                crate::rank::check_job_grade(&input.id, Some(new))?;
            }
        }
        if data.get("name").is_some() && text(&data, "name").is_none() {
            return Err(Error::msg("a job needs a name"));
        }
        explain(db::update::<Record>("job", &input.id, &data), "could not change the job")?.ok_or_else(|| Error::msg("there is no such job"))
    }

    fn get_job(input: Id) -> Option<Record> {
        db::get("job", &input.id)
    }

    fn list_jobs(_: Empty) -> Vec<Record> {
        db::find("job").order_by("name").limit(1000).all()
    }
}
