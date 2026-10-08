//! Filing, investigating, deciding, appealing and closing a grievance.
//!
//! * Anyone files for themselves (a case manager can file for someone), never against themselves. The category
//!   fixes the time limit; the due date is copied onto the case so changing a category later does not move it.
//! * A case manager who is a party to a case is recused from it. The investigator is checked for conflicts
//!   (see `rules::conflict`), proposes findings, and a different person decides.
//! * Everything goes into `grv_log` and nothing in it is ever updated or deleted. Entries are `internal` (case
//!   managers and the investigator) or `shared` (the complainant sees them too).
//! * An anonymous complainant is hidden from the investigator; only case managers see who it is.

use aether_sdk::dates::format_date;
use aether_sdk::prelude::*;

use crate::common::{employee, id_of, is_admin, my_employee, next_number, require, require_admin, text, today_date, Record};
use crate::rules::{appeal_open, conflict, due, escalation, next, Action, Person, State};

#[derive(Deserialize)]
struct File {
    subject: String,
    description: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    accused: Option<String>,
    #[serde(default)]
    anonymous: bool,
    #[serde(default)]
    on_behalf_of: Option<String>,
}

#[derive(Deserialize)]
struct Triage {
    id: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    severity: Option<String>,
    #[serde(default)]
    confidential: Option<bool>,
}

#[derive(Deserialize)]
struct Assign {
    id: String,
    investigator: String,
}

#[derive(Deserialize)]
struct Note {
    id: String,
    note: String,
    #[serde(default)]
    shared: bool,
}

#[derive(Deserialize)]
struct Findings {
    id: String,
    findings: String,
    recommended_outcome: String,
}

#[derive(Deserialize)]
struct Decide {
    id: String,
    outcome: String,
    resolution: String,
}

#[derive(Deserialize)]
struct Reason {
    id: String,
    reason: String,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize, Default)]
struct Search {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    overdue: bool,
}

#[derive(Deserialize)]
struct CategoryInput {
    code: String,
    name: String,
    sla_days: i64,
    #[serde(default)]
    default_severity: Option<String>,
    #[serde(default)]
    confidential: bool,
}

fn me() -> Result<Option<String>> {
    Ok(my_employee()?.and_then(|m| text(&m, "id").map(str::to_string)))
}

fn person(id: &str) -> Result<Person> {
    let record = employee(id)?;
    Ok(Person { id: id.to_string(), manager: text(&record, "manager").map(str::to_string) })
}

fn state_of(case: &Record) -> Result<State> {
    State::parse(text(case, "state").unwrap_or_default()).ok_or_else(|| Error::msg("the case has an unknown state"))
}

fn is_party(case: &Record, who: &str) -> bool {
    text(case, "complainant") == Some(who) || text(case, "accused") == Some(who)
}

/// A case manager who is not a party to the case.
fn require_manager(case: &Record) -> Result<()> {
    require_admin()?;
    if let Some(who) = me()? {
        if is_party(case, &who) {
            return Err(Error::msg("you are a party to this case: another case manager must handle it"));
        }
    }
    Ok(())
}

/// The investigator of the case, and only them.
fn require_investigator(case: &Record) -> Result<String> {
    let who = me()?.ok_or_else(|| Error::msg("you have no employee record"))?;
    if text(case, "investigator") != Some(who.as_str()) {
        return Err(Error::msg("only the investigator of this case can do this"));
    }
    Ok(who)
}

fn log(case: &Record, action: &str, detail: &str, shared: bool) -> Result<()> {
    let id = id_of(case)?;
    // Counted on the case, not in the log: what a caller can count is only the part of the log they may see.
    let seq = require("grv_case", id, "case")?.get("log_seq").and_then(Value::as_i64).unwrap_or(0) + 1;
    db::update::<Record>("grv_case", id, &json!({ "log_seq": seq }))?;
    let ctx = context::current()?;
    db::create::<Record>(
        "grv_log",
        &json!({
            "case": id, "seq": seq, "at": ctx.now, "actor": ctx.actor.id.clone().unwrap_or_default(),
            "actor_employee": me()?.unwrap_or_default(), "visibility": if shared { "shared" } else { "internal" },
            "action": action, "detail": detail, "complainant": case.get("complainant"),
            "investigator": case.get("investigator").filter(|v| !v.is_null()),
        }),
    )?;
    Ok(())
}

fn change(case: &Record, data: Value) -> Result<Record> {
    db::update("grv_case", id_of(case)?, &data)?.ok_or_else(|| Error::msg("the case is gone"))
}

/// What a viewer may see of a case: the complainant hidden from the investigator when anonymous, findings
/// only for those who handle the case.
fn present(case: &Record, viewer_is_manager: bool, viewer: Option<&str>) -> Record {
    let mut out = case.clone();
    let investigator = viewer.is_some() && text(case, "investigator") == viewer;
    if investigator && case.get("anonymous") == Some(&json!(true)) {
        strip(&mut out, &["complainant"]);
    }
    if !viewer_is_manager && !investigator {
        strip(&mut out, &["findings", "recommended_outcome", "past_investigators"]);
    }
    out
}

fn strip(record: &mut Record, keys: &[&str]) {
    if let Some(map) = record.as_object_mut() {
        for key in keys {
            map.remove(*key);
        }
    }
}

fn visible_to_me(case: &Record) -> Result<Option<Record>> {
    let who = me()?;
    let manager = is_admin()? && !who.as_deref().is_some_and(|w| is_party(case, w));
    let involved = who.as_deref().is_some_and(|w| text(case, "complainant") == Some(w) || text(case, "investigator") == Some(w));
    if manager || involved { Ok(Some(present(case, manager, who.as_deref()))) } else { Ok(None) }
}

fn file(input: File) -> Result<Record> {
    let mine = me()?;
    let complainant = match &input.on_behalf_of {
        Some(other) => {
            require_admin()?;
            other.clone()
        }
        None => mine.clone().ok_or_else(|| Error::msg("you have no employee record to file under"))?,
    };
    employee(&complainant)?;
    if input.accused.as_deref() == Some(complainant.as_str()) {
        return Err(Error::msg("a grievance cannot be against the person who files it"));
    }
    if let Some(accused) = &input.accused {
        employee(accused)?;
    }
    if input.subject.trim().is_empty() || input.description.trim().is_empty() {
        return Err(Error::msg("say what happened: a subject and a description"));
    }
    let today = today_date()?;
    let (category, sla, severity, confidential) = match &input.category {
        Some(code) => {
            let category = db::find::<Record>("grv_category").filter("code", code.as_str()).first()?.ok_or_else(|| Error::msg(format!("there is no category `{code}`")))?;
            if category.get("is_active") == Some(&json!(false)) {
                return Err(Error::msg("this category is no longer used"));
            }
            let sla = category.get("sla_days").and_then(Value::as_i64).unwrap_or(30);
            (Some(id_of(&category)?.to_string()), sla, text(&category, "default_severity").unwrap_or("medium").to_string(), category.get("confidential") == Some(&json!(true)))
        }
        None => (None, 30, "medium".to_string(), true),
    };
    let case: Record = db::create(
        "grv_case",
        &json!({
            "reference": next_number("grievance", "GRV-", 5)?, "complainant": complainant, "anonymous": input.anonymous,
            "accused": input.accused, "category": category, "severity": severity, "subject": input.subject,
            "description": input.description, "state": "submitted", "confidential": confidential,
            "opened_on": format_date(today), "due_date": format_date(due(today, sla)), "escalation_level": 0, "appeals": 0,
        }),
    )?;
    log(&case, "filed", "The grievance was filed.", true)?;
    events::emit("grievance_filed", &json!({ "case": case["id"], "reference": case["reference"], "severity": case["severity"] }))?;
    Ok(case)
}

fn triage(input: Triage) -> Result<Record> {
    let case = require("grv_case", &input.id, "case")?;
    require_manager(&case)?;
    if !state_of(&case)?.is_open() {
        return Err(Error::msg("this case is no longer open"));
    }
    let mut data = serde_json::Map::new();
    if let Some(severity) = input.severity {
        if !matches!(severity.as_str(), "low" | "medium" | "high") {
            return Err(Error::msg("severity is low, medium or high"));
        }
        data.insert("severity".into(), json!(severity));
    }
    if let Some(flag) = input.confidential {
        data.insert("confidential".into(), json!(flag));
    }
    if let Some(code) = input.category {
        let category = db::find::<Record>("grv_category").filter("code", code.as_str()).first()?.ok_or_else(|| Error::msg(format!("there is no category `{code}`")))?;
        data.insert("category".into(), json!(id_of(&category)?));
    }
    if data.is_empty() {
        return Err(Error::msg("nothing to change"));
    }
    log(&case, "triaged", &Value::Object(data.clone()).to_string(), false)?;
    change(&case, Value::Object(data))
}

fn assign(input: Assign) -> Result<Record> {
    let case = require("grv_case", &input.id, "case")?;
    require_manager(&case)?;
    let state = state_of(&case)?;
    let to = next(state, Action::Assign).ok_or_else(|| Error::msg("an investigator is assigned to a new or appealed case"))?;
    let complainant = person(text(&case, "complainant").unwrap_or_default())?;
    let accused = text(&case, "accused").map(person).transpose()?;
    let mut past: Vec<String> = text(&case, "past_investigators").unwrap_or_default().split(',').filter(|s| !s.is_empty()).map(str::to_string).collect();
    // Whoever investigated until now is out for the appeal.
    if let Some(old) = text(&case, "investigator") {
        past.push(old.to_string());
    }
    if let Some(reason) = conflict(&person(&input.investigator)?, &complainant, accused.as_ref(), &past) {
        return Err(Error::msg(reason));
    }
    if !matches!(text(&employee(&input.investigator)?, "status"), Some("active" | "probation") | None) {
        return Err(Error::msg("an investigator has to be working"));
    }
    log(&case, "investigator_assigned", &format!("Investigator: {}", input.investigator), false)?;
    change(&case, json!({ "state": to_str(to), "investigator": input.investigator, "past_investigators": past.join(",") }))
}

fn to_str(state: State) -> &'static str {
    match state {
        State::Submitted => "submitted",
        State::Investigating => "investigating",
        State::FindingsSubmitted => "findings_submitted",
        State::Resolved => "resolved",
        State::Appealed => "appealed",
        State::Closed => "closed",
        State::Withdrawn => "withdrawn",
    }
}

fn note(input: Note) -> Result<Record> {
    let case = require("grv_case", &input.id, "case")?;
    if !state_of(&case)?.is_open() {
        return Err(Error::msg("this case is no longer open"));
    }
    if input.note.trim().is_empty() {
        return Err(Error::msg("the note is empty"));
    }
    if require_investigator(&case).is_err() {
        require_manager(&case).map_err(|_| Error::msg("only the investigator or a case manager can add notes"))?;
    }
    log(&case, "note", &input.note, input.shared)?;
    Ok(case)
}

fn findings_of(input: Findings) -> Result<Record> {
    let case = require("grv_case", &input.id, "case")?;
    require_investigator(&case)?;
    let to = next(state_of(&case)?, Action::SubmitFindings).ok_or_else(|| Error::msg("findings are submitted while investigating"))?;
    if input.findings.trim().is_empty() {
        return Err(Error::msg("write the findings"));
    }
    if !matches!(input.recommended_outcome.as_str(), "upheld" | "partially_upheld" | "not_upheld") {
        return Err(Error::msg("recommend upheld, partially_upheld or not_upheld"));
    }
    log(&case, "findings_submitted", &format!("Recommended: {}", input.recommended_outcome), false)?;
    change(&case, json!({ "state": to_str(to), "findings": input.findings, "recommended_outcome": input.recommended_outcome }))
}

fn decide(input: Decide) -> Result<Record> {
    let case = require("grv_case", &input.id, "case")?;
    require_manager(&case)?;
    let to = next(state_of(&case)?, Action::Decide).ok_or_else(|| Error::msg("a case is decided once the investigator has submitted findings"))?;
    if me()?.is_some() && me()?.as_deref() == text(&case, "investigator") {
        return Err(Error::msg("the investigator cannot decide their own findings"));
    }
    if !matches!(input.outcome.as_str(), "upheld" | "partially_upheld" | "not_upheld") {
        return Err(Error::msg("the outcome is upheld, partially_upheld or not_upheld"));
    }
    if input.resolution.trim().is_empty() {
        return Err(Error::msg("state the resolution"));
    }
    let today = format_date(today_date()?);
    log(&case, "decided", &format!("Outcome: {}", input.outcome), true)?;
    let out = change(&case, json!({
        "state": to_str(to), "outcome": input.outcome, "resolution": input.resolution, "resolved_on": today,
        "decided_by": context::current()?.actor.id.unwrap_or_default(),
    }))?;
    events::emit("grievance_decided", &json!({ "case": out["id"], "outcome": out["outcome"] }))?;
    Ok(out)
}

fn appeal(input: Reason) -> Result<Record> {
    let case = require("grv_case", &input.id, "case")?;
    if me()?.as_deref() != text(&case, "complainant") {
        return Err(Error::msg("only the complainant can appeal"));
    }
    let to = next(state_of(&case)?, Action::Appeal).ok_or_else(|| Error::msg("only a decided case can be appealed"))?;
    if case.get("appeals").and_then(Value::as_i64).unwrap_or(0) >= 1 {
        return Err(Error::msg("a case can be appealed once"));
    }
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why you appeal"));
    }
    let resolved = aether_sdk::dates::parse_date(text(&case, "resolved_on").unwrap_or_default())?;
    if !appeal_open(today_date()?, resolved) {
        return Err(Error::msg("the appeal window of 14 days has passed"));
    }
    log(&case, "appealed", &input.reason, true)?;
    change(&case, json!({ "state": to_str(to), "appeals": 1, "outcome": null, "resolution": null, "findings": null, "recommended_outcome": null, "resolved_on": null, "decided_by": null }))
}

fn withdraw(input: Reason) -> Result<Record> {
    let case = require("grv_case", &input.id, "case")?;
    if me()?.as_deref() != text(&case, "complainant") {
        return Err(Error::msg("only the complainant can withdraw"));
    }
    let to = next(state_of(&case)?, Action::Withdraw).ok_or_else(|| Error::msg("a case can be withdrawn until findings are in"))?;
    log(&case, "withdrawn", &input.reason, true)?;
    change(&case, json!({ "state": to_str(to), "closed_on": format_date(today_date()?) }))
}

/// A case manager turns away a case that was just filed, with the reason shown to the complainant.
fn dismiss(input: Reason) -> Result<Record> {
    let case = require("grv_case", &input.id, "case")?;
    require_manager(&case)?;
    let to = next(state_of(&case)?, Action::Dismiss).ok_or_else(|| Error::msg("only a case nobody has started on can be dismissed"))?;
    if input.reason.trim().is_empty() {
        return Err(Error::msg("give the reason"));
    }
    let today = format_date(today_date()?);
    log(&case, "dismissed", &input.reason, true)?;
    change(&case, json!({ "state": to_str(to), "outcome": "dismissed", "resolution": input.reason, "resolved_on": today, "closed_on": today }))
}

fn close(input: Id) -> Result<Record> {
    let case = require("grv_case", &input.id, "case")?;
    require_manager(&case)?;
    let to = next(state_of(&case)?, Action::Close).ok_or_else(|| Error::msg("only a decided case can be closed"))?;
    log(&case, "closed", "Closed by a case manager.", true)?;
    change(&case, json!({ "state": to_str(to), "closed_on": format_date(today_date()?) }))
}

fn get(input: Id) -> Result<Record> {
    let case = require("grv_case", &input.id, "case")?;
    visible_to_me(&case)?.ok_or_else(|| Error::msg("you cannot see this case"))
}

fn list(input: Search) -> Result<Vec<Record>> {
    let today = today_date()?;
    let mut rows: Vec<Record> = db::find::<Record>("grv_case").limit(5000).all()?;
    if let Some(state) = &input.state {
        rows.retain(|r| text(r, "state") == Some(state.as_str()));
    }
    if input.overdue {
        rows.retain(|r| {
            State::parse(text(r, "state").unwrap_or_default()).is_some_and(State::is_open)
                && text(r, "due_date").and_then(|d| aether_sdk::dates::parse_date(d).ok()).is_some_and(|d| d < today)
        });
    }
    let mut out = Vec::new();
    for row in rows {
        if let Some(seen) = visible_to_me(&row)? {
            out.push(seen);
        }
    }
    Ok(out)
}

/// The log of a case as the caller may read it: everything for a case manager or the investigator, only the
/// shared entries for the complainant. An anonymous complainant's name and entries' actors are withheld from
/// the investigator.
fn case_log(input: Id) -> Result<Vec<Record>> {
    let case = require("grv_case", &input.id, "case")?;
    let who = me()?;
    let manager = is_admin()? && !who.as_deref().is_some_and(|w| is_party(&case, w));
    let investigator = who.is_some() && who.as_deref() == text(&case, "investigator");
    let complainant = who.is_some() && who.as_deref() == text(&case, "complainant");
    if !manager && !investigator && !complainant {
        return Err(Error::msg("you cannot see this case"));
    }
    let mut rows: Vec<Record> = db::find::<Record>("grv_log").filter("case", input.id.as_str()).limit(5000).all()?;
    rows.sort_by_key(|r| r.get("seq").and_then(Value::as_i64).unwrap_or(0));
    let hide_complainant = investigator && case.get("anonymous") == Some(&json!(true));
    let mut out = Vec::new();
    for mut row in rows {
        if !manager && !investigator && text(&row, "visibility") != Some("shared") {
            continue;
        }
        if hide_complainant || !manager {
            strip(&mut row, &["complainant"]);
        }
        if hide_complainant {
            if row.get("actor_employee").and_then(Value::as_str) == text(&case, "complainant") {
                strip(&mut row, &["actor", "actor_employee"]);
            }
        }
        out.push(row);
    }
    Ok(out)
}

fn category(input: CategoryInput) -> Result<Record> {
    require_admin()?;
    if input.sla_days < 1 {
        return Err(Error::msg("a time limit is at least one day"));
    }
    db::create(
        "grv_category",
        &json!({ "code": input.code, "name": input.name, "sla_days": input.sla_days, "default_severity": input.default_severity.unwrap_or_else(|| "medium".into()), "confidential": input.confidential, "is_active": true }),
    )
    .map_err(|e| e.or("could not create the category (the code may be taken)"))
}

/// Escalate overdue cases a level at a time (each level once), and close decided cases whose appeal window passed.
fn nightly() -> Result<Value> {
    let today = today_date()?;
    let rows: Vec<Record> = db::find::<Record>("grv_case").limit(10_000).all()?;
    let (mut escalated, mut closed) = (0, 0);
    for case in rows {
        let state = state_of(&case)?;
        if state.is_open() {
            let (Some(opened), Some(due_on)) = (
                text(&case, "opened_on").map(aether_sdk::dates::parse_date).transpose()?,
                text(&case, "due_date").map(aether_sdk::dates::parse_date).transpose()?,
            ) else { continue };
            let level = escalation(today, opened, due_on);
            let have = case.get("escalation_level").and_then(Value::as_i64).unwrap_or(0);
            if level > have {
                log(&case, "escalated", &format!("Overdue: escalated to level {level}."), false)?;
                change(&case, json!({ "escalation_level": level }))?;
                events::emit("grievance_escalated", &json!({ "case": case["id"], "reference": case["reference"], "level": level }))?;
                escalated += 1;
            }
        } else if state == State::Resolved {
            if let Some(resolved) = text(&case, "resolved_on").map(aether_sdk::dates::parse_date).transpose()? {
                if !appeal_open(today, resolved) {
                    log(&case, "closed", "Closed: the appeal window passed.", true)?;
                    change(&case, json!({ "state": "closed", "closed_on": format_date(today) }))?;
                    closed += 1;
                }
            }
        }
    }
    Ok(json!({ "escalated": escalated, "closed": closed }))
}

handler! {
    /// File a grievance (for yourself; a case manager may file for someone). It may be anonymous.
    fn file_grievance(input: File) -> Record {
        file(input)
    }

    fn create_grievance_category(input: CategoryInput) -> Record {
        category(input)
    }

    fn list_grievance_categories(_: Empty) -> Vec<Record> {
        db::find("grv_category").limit(500).all()
    }

    /// Set the category, severity or confidentiality (case managers).
    fn triage_grievance(input: Triage) -> Record {
        triage(input)
    }

    /// Assign an investigator, checked for conflicts (case managers).
    fn assign_investigator(input: Assign) -> Record {
        assign(input)
    }

    /// Add to the log: the investigator and case managers; `shared` lets the complainant see it.
    fn add_case_note(input: Note) -> Record {
        note(input)
    }

    fn submit_findings(input: Findings) -> Record {
        findings_of(input)
    }

    /// A case manager other than the investigator decides the case.
    fn decide_grievance(input: Decide) -> Record {
        decide(input)
    }

    fn appeal_grievance(input: Reason) -> Record {
        appeal(input)
    }

    fn withdraw_grievance(input: Reason) -> Record {
        withdraw(input)
    }

    fn dismiss_grievance(input: Reason) -> Record {
        dismiss(input)
    }

    fn close_grievance(input: Id) -> Record {
        close(input)
    }

    fn get_grievance(input: Id) -> Record {
        get(input)
    }

    /// The cases the caller may see, optionally by state or only the overdue ones.
    fn list_grievances(input: Search) -> Vec<Record> {
        list(input)
    }

    fn grievance_log(input: Id) -> Vec<Record> {
        case_log(input)
    }

    fn grievance_nightly(_: Empty) -> Value {
        nightly()
    }
}
