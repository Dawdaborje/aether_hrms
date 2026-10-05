//! Applications: a person in the directory applying to an opening.
//!
//! The candidate is one party record (Frappe uses the email as the applicant's key, Odoo copies
//! identity onto each application). Stages are entered in order; hired is reached only by hiring and
//! rejected only by rejecting with a reason.

use aether_sdk::dates::format_date;
use aether_sdk::decimal::Decimal;
use aether_sdk::db::Filter;
use aether_sdk::prelude::*;

use crate::common::{id_of, is_manager, kind_of, require, require_recruiter, sequence_of, stage_of_kind, text, today_date, Record};
use crate::rules::{check_move, normalise_email};

#[derive(Deserialize)]
struct Person {
    name: String,
    email: String,
    #[serde(default)]
    phone: Option<String>,
}

#[derive(Deserialize)]
struct Apply {
    opening: String,
    /// A person already in the directory...
    #[serde(default)]
    candidate: Option<String>,
    /// ...or the details to find them by email, or add them.
    #[serde(default)]
    person: Option<Person>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    referrer: Option<String>,
    #[serde(default)]
    expected_salary: Option<Decimal>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Deserialize)]
struct Move {
    id: String,
    stage: String,
}

#[derive(Deserialize)]
struct Reject {
    id: String,
    reason: String,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Kanban {
    id: String,
    state: String,
}

#[derive(Deserialize, Default)]
struct Search {
    #[serde(default)]
    opening: Option<String>,
    #[serde(default)]
    stage: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

/// The person in the directory for an applicant: the one with that email, or a new one. Matching on
/// the email keeps one record per human instead of one per application.
fn resolve_candidate(input: &Apply) -> Result<String> {
    if let Some(id) = &input.candidate {
        let party: Record = plugins::call("party", "get_party", &json!({ "id": id }))?;
        if party.is_null() {
            return Err(Error::msg("there is no such person in the directory"));
        }
        if text(&party, "kind") == Some("organization") {
            return Err(Error::msg("an organization cannot apply for a job"));
        }
        return Ok(id.clone());
    }
    let person = input.person.as_ref().ok_or_else(|| Error::msg("name the candidate: a directory id, or a name and an email"))?;
    let email = normalise_email(&person.email);
    if !email.contains('@') {
        return Err(Error::msg("the candidate needs an email address"));
    }
    let found: Vec<Record> = plugins::call("party", "search", &json!({ "text": email, "kind": "person", "limit": 20 }))?;
    if let Some(same) = found.iter().find(|p| text(p, "email").map(normalise_email).as_deref() == Some(email.as_str())) {
        return Ok(id_of(same)?.to_string());
    }
    let created: Record = plugins::call("party", "create_person", &json!({ "name": person.name, "email": email, "phone": person.phone }))?;
    Ok(id_of(&created)?.to_string())
}

fn apply(input: Apply) -> Result<Record> {
    require_recruiter()?;
    let opening = require("rec_opening", &input.opening, "opening")?;
    if text(&opening, "status") != Some("open") {
        return Err(Error::msg("this opening is not open for applications"));
    }
    let candidate = resolve_candidate(&input)?;
    if let Some(earlier) = db::find::<Record>("rec_application").filter("candidate", candidate.as_str()).filter("opening", input.opening.as_str()).first()? {
        return Err(Error::msg(format!(
            "this person already applied to this opening (it is {}); a recruitment manager can reopen it",
            text(&earlier, "status").unwrap_or("on file")
        )));
    }
    if let Some(referrer) = &input.referrer {
        let _: Value = plugins::call("hr", "get_employee", &json!({ "id": referrer }))?;
    }
    let first = stage_of_kind("sourcing")?;
    let mut data = json!({
        "candidate": candidate, "opening": input.opening, "stage": id_of(&first)?, "status": "active", "stage_changed_on": format_date(today_date()?),
    });
    for (field, value) in [("source", input.source.as_ref().map(|s| json!(s))), ("referrer", input.referrer.as_ref().map(|s| json!(s))), ("notes", input.notes.as_ref().map(|s| json!(s)))] {
        if let Some(value) = value {
            data[field] = value;
        }
    }
    if let Some(salary) = input.expected_salary {
        data["expected_salary"] = json!(salary.with_scale(2)?);
    }
    db::create("rec_application", &data).map_err(|e| e.or("could not record the application"))
}

fn live(id: &str) -> Result<Record> {
    let application = require("rec_application", id, "application")?;
    if text(&application, "status") != Some("active") {
        return Err(Error::msg(format!("this application is {}", text(&application, "status").unwrap_or("closed"))));
    }
    Ok(application)
}

fn move_stage(input: Move) -> Result<Record> {
    require_recruiter()?;
    let application = live(&input.id)?;
    let from = require("rec_stage", text(&application, "stage").unwrap_or_default(), "stage")?;
    let to = require("rec_stage", &input.stage, "stage")?;
    check_move(sequence_of(&from), sequence_of(&to), kind_of(&to)?, is_manager()?)?;
    let moved = db::update::<Record>("rec_application", &input.id, &json!({ "stage": input.stage, "stage_changed_on": format_date(today_date()?), "kanban_state": "ok" }))?
        .ok_or_else(|| Error::msg("the application is gone"))?;
    events::emit("application_stage_changed", &json!({ "application": input.id, "from": from["id"], "to": to["id"] }))?;
    Ok(moved)
}

/// Whatever else was in motion for an application ends with it.
fn wind_down(application_id: &str) -> Result<()> {
    for offer in db::find::<Record>("rec_offer").filter("application", application_id).limit(20).all()? {
        if matches!(text(&offer, "status"), Some("draft" | "approved" | "sent")) {
            db::update::<Record>("rec_offer", id_of(&offer)?, &json!({ "status": "withdrawn" }))?;
        }
    }
    for interview in db::find::<Record>("rec_interview").filter("application", application_id).filter("status", "scheduled").limit(50).all()? {
        db::update::<Record>("rec_interview", id_of(&interview)?, &json!({ "status": "cancelled" }))?;
    }
    Ok(())
}

fn reject(input: Reject) -> Result<Record> {
    require_recruiter()?;
    let application = live(&input.id)?;
    let reason = require("rec_reject_reason", &input.reason, "reason")?;
    let rejected_stage = stage_of_kind("rejected")?;
    wind_down(&input.id)?;
    let mut changes = json!({
        "status": "rejected", "stage": id_of(&rejected_stage)?, "reject_reason": id_of(&reason)?, "stage_changed_on": format_date(today_date()?),
    });
    if let Some(note) = &input.note {
        changes["notes"] = json!(note);
    }
    let done = db::update::<Record>("rec_application", &input.id, &changes)?.ok_or_else(|| Error::msg("the application is gone"))?;
    events::emit("application_stage_changed", &json!({ "application": input.id, "from": application["stage"], "to": rejected_stage["id"], "rejected": true }))?;
    Ok(done)
}

fn withdraw(input: Id) -> Result<Record> {
    require_recruiter()?;
    live(&input.id)?;
    wind_down(&input.id)?;
    db::update::<Record>("rec_application", &input.id, &json!({ "status": "withdrawn" }))?.ok_or_else(|| Error::msg("the application is gone"))
}

/// A recruitment manager brings back a rejected or withdrawn application, at the first stage.
fn reopen(input: Id) -> Result<Record> {
    crate::common::require_manager()?;
    let application = require("rec_application", &input.id, "application")?;
    if !matches!(text(&application, "status"), Some("rejected" | "withdrawn")) {
        return Err(Error::msg("only a rejected or withdrawn application can be reopened"));
    }
    let opening = require("rec_opening", text(&application, "opening").unwrap_or_default(), "opening")?;
    if text(&opening, "status") != Some("open") {
        return Err(Error::msg("the opening is not open"));
    }
    let first = stage_of_kind("sourcing")?;
    db::update::<Record>(
        "rec_application",
        &input.id,
        &json!({ "status": "active", "stage": id_of(&first)?, "reject_reason": null, "stage_changed_on": format_date(today_date()?) }),
    )?
    .ok_or_else(|| Error::msg("the application is gone"))
}

fn kanban(input: Kanban) -> Result<Record> {
    require_recruiter()?;
    live(&input.id)?;
    if !["ok", "waiting", "blocked"].contains(&input.state.as_str()) {
        return Err(Error::msg("state is ok, waiting or blocked"));
    }
    db::update::<Record>("rec_application", &input.id, &json!({ "kanban_state": input.state }))?.ok_or_else(|| Error::msg("the application is gone"))
}

fn list(input: Search) -> Result<Vec<Record>> {
    let mut filter = Filter::all();
    for (field, value) in [("opening", &input.opening), ("stage", &input.stage), ("status", &input.status)] {
        if let Some(value) = value {
            filter = filter.and(Filter::eq(field, value.as_str()));
        }
    }
    db::find::<Record>("rec_application").matching(filter).order_by("-stage_changed_on").limit(500).all()
}

handler! {
    /// Add an application: the person from the directory, or found by email, or added.
    fn apply_to_opening(input: Apply) -> Record {
        apply(input)
    }

    fn move_application(input: Move) -> Record {
        move_stage(input)
    }

    fn reject_application(input: Reject) -> Record {
        reject(input)
    }

    fn withdraw_application(input: Id) -> Record {
        withdraw(input)
    }

    fn reopen_application(input: Id) -> Record {
        reopen(input)
    }

    fn set_application_state(input: Kanban) -> Record {
        kanban(input)
    }

    fn get_application(input: Id) -> Option<Record> {
        db::get("rec_application", &input.id)
    }

    fn list_applications(input: Option<Search>) -> Vec<Record> {
        list(input.unwrap_or_default())
    }
}
