//! Asking someone else to approve a run.
//!
//! A run is approved by a person other than the one who made it, so the person looking at a calculated run
//! may be unable to approve it: they made it, or they do not hold the approver role. They can then invite a
//! colleague, who gets a notification with a link to the run.
//!
//! Only people who hold the approver role (or are organization admins, who hold every role) can be asked,
//! `org::members` answers that. Each request is kept on the run (`pay_approval_request`), and the same person is
//! not asked twice about the same calculation of a run.

use aether_sdk::db::Filter;
use aether_sdk::notify::{Audience, Level, Notification};
use aether_sdk::prelude::*;
use aether_sdk::transport;

use crate::common::{actor, id_of, int, require, require_payroll_staff, text, Record, APPROVER_ROLE};

/// The bare key of an id (`users:abc` and `abc` are the same person).
fn key(id: &str) -> &str {
    id.rsplit_once(':').map_or(id, |(_, key)| key)
}

struct Person {
    id: String,
    name: String,
}

/// Everyone who can sign in to this organization; with a role, only those who hold it.
fn members(role: Option<&str>) -> Result<Vec<Person>> {
    let request = match role {
        Some(role) => json!({ "role": role }),
        None => json!({}),
    };
    let reply = transport::call("org::members", &request)?;
    Ok(reply
        .get("data")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let id = row.get("id")?.as_str()?.to_string();
                    let name = row.get("display_name").and_then(Value::as_str).unwrap_or(&id).to_string();
                    Some(Person { id, name })
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Whether the caller made the run.
fn made_by_me(run: &Record) -> Result<bool> {
    let me = actor()?;
    Ok(matches!((me.as_deref(), text(run, "created_by")), (Some(me), Some(maker)) if key(me) == key(maker)))
}

/// What the person looking at a run can do about approving it: `can_approve`, `needs_other` (they cannot,
/// so they may invite someone) or `none` (the run is not waiting for approval).
pub(crate) fn approval_state(run: &Record) -> Result<&'static str> {
    if text(run, "status") != Some("calculated") {
        return Ok("none");
    }
    let holds_role = context::current()?.has_role(APPROVER_ROLE);
    Ok(if holds_role && !made_by_me(run)? { "can_approve" } else { "needs_other" })
}

/// A sentence for the run's page about where it stands for approval.
pub(crate) fn approval_note(run: &Record, state: &str) -> Result<String> {
    Ok(match (text(run, "status").unwrap_or_default(), state) {
        ("draft", _) => "Calculate the run first.".into(),
        ("calculating", _) => "Being calculated.".into(),
        ("failed", _) => "Calculate the run again.".into(),
        ("cancelled", _) => "Cancelled.".into(),
        ("calculated", "can_approve") => "Waiting for your approval.".into(),
        ("calculated", _) => {
            let why = if made_by_me(run)? { "You made this run, so someone else has to approve it.".to_string() } else { format!("Approving needs the role `{APPROVER_ROLE}`, which you do not hold.") };
            let asked: Vec<String> = requests_of(run)?.iter().filter_map(|r| text(r, "requested_from_name").map(str::to_string)).collect();
            if asked.is_empty() { why } else { format!("{why} Asked: {}.", asked.join(", ")) }
        }
        _ => String::new(),
    })
}

/// The requests made about the run's current calculation, oldest first.
fn requests_of(run: &Record) -> Result<Vec<Record>> {
    db::find::<Record>("pay_approval_request")
        .matching(Filter::eq("run", id_of(run)?).and(Filter::eq("attempt", int(run, "attempt"))))
        .order_by("requested_at")
        .limit(100)
        .all()
}

#[derive(Deserialize, Default)]
struct Which {
    #[serde(default)]
    id: Option<String>,
}

#[derive(Deserialize)]
struct Request {
    id: String,
    /// The member asked to approve.
    to: String,
    #[serde(default)]
    note: Option<String>,
}

/// Who could be asked: the holders of the approver role, but not the caller or the person who made the run.
fn candidates(which: Option<Which>) -> Result<Vec<Value>> {
    require_payroll_staff()?;
    let maker = match which.and_then(|w| w.id) {
        Some(id) => text(&require("pay_run", &id, "run")?, "created_by").map(str::to_string),
        None => None,
    };
    let me = actor()?;
    let excluded = |id: &str| [me.as_deref(), maker.as_deref()].into_iter().flatten().any(|other| key(other) == key(id));
    let mut people: Vec<Person> = members(Some(APPROVER_ROLE))?.into_iter().filter(|p| !excluded(&p.id)).collect();
    people.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(people.into_iter().map(|p| json!({ "id": p.id, "display_name": p.name })).collect())
}

fn request(input: Request) -> Result<Value> {
    require_payroll_staff()?;
    let run = require("pay_run", &input.id, "run")?;
    if text(&run, "status") != Some("calculated") {
        return Err(Error::msg("only a calculated run is waiting for approval"));
    }
    let approvers = members(Some(APPROVER_ROLE))?;
    let Some(person) = approvers.iter().find(|p| key(&p.id) == key(&input.to)) else {
        return Err(Error::msg(format!("choose someone who holds the role `{APPROVER_ROLE}`")));
    };
    let me = actor()?;
    if me.as_deref().is_some_and(|me| key(me) == key(&person.id)) {
        return Err(Error::msg("ask someone else: you cannot approve a run you are asking about"));
    }
    if text(&run, "created_by").is_some_and(|maker| key(maker) == key(&person.id)) {
        return Err(Error::msg("that person made the run, so they cannot approve it: ask someone else"));
    }
    let attempt = int(&run, "attempt");
    if requests_of(&run)?.iter().any(|r| text(r, "requested_from").is_some_and(|who| key(who) == key(&person.id))) {
        return Err(Error::msg(format!("{} has already been asked about this calculation of the run", person.name)));
    }
    let everyone = members(None)?;
    let asker = me.as_deref().and_then(|me| everyone.iter().find(|p| key(&p.id) == key(me))).map_or("A colleague", |p| p.name.as_str());
    let number = text(&run, "number").unwrap_or("A pay run");
    let note = input.note.as_deref().map(str::trim).filter(|n| !n.is_empty()).map(|n| n.chars().take(300).collect::<String>());
    let mut body = format!(
        "{asker} asks you to approve pay run {number} ({}), {} to {}.",
        text(&run, "name").unwrap_or_default(),
        text(&run, "period_start").unwrap_or_default(),
        text(&run, "period_end").unwrap_or_default()
    );
    if let Some(note) = &note {
        body.push_str(&format!(" \"{note}\""));
    }
    let saved = db::create::<Record>(
        "pay_approval_request",
        &json!({
            "run": id_of(&run)?, "attempt": attempt, "requested_from": person.id, "requested_from_name": person.name,
            "requested_by": me, "requested_by_name": asker, "note": note, "requested_at": context::current()?.now,
        }),
    )
    .map_err(|e| e.or("this person has already been asked about this calculation of the run"))?;
    let sent = Notification::new(format!("Approval requested: {number}"))
        .body(body.chars().take(900).collect::<String>())
        .link(format!("/hr/payroll/{}", id_of(&run)?))
        .level(Level::Info)
        .to(Audience::Actors(vec![person.id.clone()]))
        .send();
    if let Err(error) = sent {
        // Nothing reached them, so the run must not say they were asked.
        let _ = db::delete::<Record>("pay_approval_request", id_of(&saved)?);
        return Err(error);
    }
    Ok(json!({ "requested_from": person.name }))
}

handler! {
    /// The members who could be asked to approve this run.
    fn list_approval_candidates(input: Option<Which>) -> Vec<Value> {
        candidates(input)
    }

    /// The requests made about a run, newest first.
    fn list_approval_requests(input: Which) -> Vec<Record> {
        require_payroll_staff()?;
        let id = input.id.ok_or_else(|| Error::msg("which run?"))?;
        db::find::<Record>("pay_approval_request").filter("run", id.as_str()).order_by("-requested_at").limit(200).all()
    }

    /// Notify a colleague that a calculated run is waiting for their approval.
    fn request_pay_run_approval(input: Request) -> Value {
        request(input)
    }
}
