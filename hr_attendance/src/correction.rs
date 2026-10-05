//! Corrections: a missing punch, a wrong time, a day wrongly absent. The person asks and their
//! manager (or an attendance administrator) decides, **never the person**. An approved correction
//! writes new punches that supersede the old ones; the originals stay on record.

use aether_sdk::dates::{format_date, format_datetime, parse_date, parse_datetime, Duration};
use aether_sdk::db::Filter;
use aether_sdk::prelude::*;

use crate::common::{employee, id_of, is_admin, my_employee, next_number, require, text, today_date, Record};
use crate::day::recompute;
use crate::punch::work_date_of;

/// How far back a correction may be asked for.
const MAX_DAYS_BACK: i64 = 60;

#[derive(Deserialize)]
struct Ask {
    #[serde(default)]
    employee: Option<String>,
    work_date: String,
    kind: String,
    #[serde(default)]
    proposed_in: Option<String>,
    #[serde(default)]
    proposed_out: Option<String>,
    /// Ids of punches this replaces (for a wrong time).
    #[serde(default)]
    supersede: Vec<String>,
    reason: String,
}

#[derive(Deserialize)]
struct Decide {
    id: String,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

fn ask(input: Ask) -> Result<Record> {
    let me = my_employee()?;
    let person = match (&input.employee, me) {
        (Some(id), Some(me)) if id_of(&me)? == id => me,
        (Some(id), _) => {
            crate::common::require_admin().map_err(|_| Error::msg("you can only ask for a correction of your own attendance"))?;
            employee(id)?
        }
        (None, Some(me)) => me,
        (None, None) => return Err(Error::msg("you are not an employee")),
    };
    let date = parse_date(&input.work_date)?;
    let today = today_date()?;
    if date > today {
        return Err(Error::msg("that day has not happened yet"));
    }
    if today - date > Duration::days(MAX_DAYS_BACK) {
        return Err(Error::msg(format!("corrections go back at most {MAX_DAYS_BACK} days; ask an administrator")));
    }
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why"));
    }
    let (needs_in, needs_out) = match input.kind.as_str() {
        "missing_in" => (true, false),
        "missing_out" => (false, true),
        "wrong_time" => (input.proposed_in.is_some(), input.proposed_out.is_some()),
        "absent_to_present" => (true, true),
        _ => return Err(Error::msg("kind is missing_in, missing_out, wrong_time or absent_to_present")),
    };
    if (needs_in && input.proposed_in.is_none()) || (needs_out && input.proposed_out.is_none()) {
        return Err(Error::msg("give the time of the punch to add"));
    }
    if input.kind == "wrong_time" && (input.supersede.is_empty() || (input.proposed_in.is_none() && input.proposed_out.is_none())) {
        return Err(Error::msg("a wrong time names the punch to replace and the time it should be"));
    }
    for time in [&input.proposed_in, &input.proposed_out].into_iter().flatten() {
        let at = parse_datetime(time)?;
        if at.date() < date - Duration::days(1) || at.date() > date + Duration::days(2) {
            return Err(Error::msg("the time is not near that day"));
        }
    }
    let person_id = id_of(&person)?.to_string();
    // Punches to replace must be this person's own.
    for punch in &input.supersede {
        let found = require("att_punch", punch, "punch")?;
        if text(&found, "employee") != Some(person_id.as_str()) {
            return Err(Error::msg("a correction can only replace the person's own punches"));
        }
    }
    let open = db::count(
        "att_correction",
        Filter::eq("employee", person_id.as_str()).and(Filter::eq("work_date", format_date(date))).and(Filter::eq("status", "pending")),
    )?;
    if open > 0 {
        return Err(Error::msg("there is already a correction waiting for that day"));
    }
    let mut data = json!({
        "reference": next_number("correction", "AC-", 5)?, "employee": person_id, "work_date": format_date(date),
        "kind": input.kind, "reason": input.reason, "status": "pending", "supersede": input.supersede.join(","),
    });
    if let Some(a) = &input.proposed_in {
        data["proposed_in"] = json!(format_datetime(parse_datetime(a)?));
    }
    if let Some(b) = &input.proposed_out {
        data["proposed_out"] = json!(format_datetime(parse_datetime(b)?));
    }
    if let Some(manager) = text(&person, "manager") {
        data["approver"] = json!(manager);
    }
    db::create("att_correction", &data)
}

/// Whether the caller may decide: an attendance administrator, or the requester's current manager,
/// and never the requester themselves.
fn may_decide(me: Option<&Record>, correction: &Record) -> Result<bool> {
    let requester = text(correction, "employee").unwrap_or_default();
    if me.is_some_and(|m| text(m, "id") == Some(requester)) {
        return Ok(false);
    }
    if is_admin()? {
        return Ok(true);
    }
    let Some(me) = me else { return Ok(false) };
    let manager = employee(requester)?.get("manager").and_then(Value::as_str).map(str::to_string);
    Ok(manager.as_deref() == text(me, "id"))
}

fn decide(input: Decide, approve: bool) -> Result<Record> {
    let correction = require("att_correction", &input.id, "correction")?;
    if text(&correction, "status") != Some("pending") {
        return Err(Error::msg("this correction was already decided"));
    }
    let me = my_employee()?;
    if !may_decide(me.as_ref(), &correction)? {
        return Err(Error::msg("only this person's manager (or an attendance administrator) can decide, and never the person themselves"));
    }
    if !approve && input.note.as_deref().is_none_or(|n| n.trim().is_empty()) {
        return Err(Error::msg("say why the correction is rejected"));
    }
    let person_id = text(&correction, "employee").unwrap_or_default().to_string();
    let person = employee(&person_id)?;
    let date = parse_date(text(&correction, "work_date").unwrap_or_default())?;
    if approve {
        let replaced: Vec<String> = text(&correction, "supersede").map(|s| s.split(',').map(str::to_string).collect()).unwrap_or_default();
        let id = text(&correction, "id").unwrap_or_default();
        // Each new punch replaces one old one if any are named, in order; the rest are additions.
        let mut replace = replaced.into_iter();
        for (n, (field, direction)) in [("proposed_in", "in"), ("proposed_out", "out")].into_iter().enumerate() {
            let Some(when) = text(&correction, field) else { continue };
            let mut data = json!({
                "employee": person_id, "ts": when, "direction": direction, "source": "correction",
                "client_ref": format!("corr:{id}:{n}"), "note": format!("correction {}", text(&correction, "reference").unwrap_or("")),
            });
            if let Some(old) = replace.next() {
                data["supersedes"] = json!(old);
            }
            db::create::<Record>("att_punch", &data)?;
        }
        // Any punches named but not matched by a new one are simply replaced by nothing: not
        // possible here, so refuse rather than guess.
        if replace.next().is_some() {
            return Err(Error::msg("more punches are named to replace than times were given"));
        }
    }
    let done = db::update::<Record>(
        "att_correction",
        &input.id,
        &json!({
            "status": if approve { "approved" } else { "rejected" },
            "decided_by": me.as_ref().and_then(|m| text(m, "id")), "decided_on": format_date(today_date()?), "decision_note": input.note,
        }),
    )?
    .ok_or_else(|| Error::msg("the correction is gone"))?;
    if approve {
        // The day may be the one the corrected time falls in; work out both.
        recompute(&person, date)?;
        if let Some(at) = text(&correction, "proposed_in").or(text(&correction, "proposed_out")) {
            let other = work_date_of(&person_id, parse_datetime(at)?)?;
            if other != date {
                recompute(&person, other)?;
            }
        }
    }
    events::emit("correction_decided", &json!({ "correction": input.id, "employee": person_id, "approved": approve }))?;
    Ok(done)
}

fn cancel(input: Id) -> Result<Record> {
    let correction = require("att_correction", &input.id, "correction")?;
    let me = my_employee()?;
    if !me.as_ref().is_some_and(|m| text(m, "id") == text(&correction, "employee")) && !is_admin()? {
        return Err(Error::msg("only the person can withdraw their request"));
    }
    if text(&correction, "status") != Some("pending") {
        return Err(Error::msg("only a waiting correction can be withdrawn"));
    }
    db::update::<Record>("att_correction", &input.id, &json!({ "status": "cancelled" }))?.ok_or_else(|| Error::msg("the correction is gone"))
}

/// Corrections waiting for the caller: their reports', or all of them for an administrator.
fn waiting() -> Result<Vec<Record>> {
    let me = my_employee()?;
    let mut found: Vec<Record> = Vec::new();
    if let Some(me) = &me {
        let reports: Vec<Record> = plugins::call("hr", "list_employees", &json!({ "manager": id_of(me)?, "limit": 500 }))?;
        let ids: Vec<&str> = reports.iter().filter_map(|e| text(e, "id")).collect();
        if !ids.is_empty() {
            found = db::find::<Record>("att_correction").matching(Filter::eq("status", "pending").and(Filter::one_of("employee", ids))).order_by("work_date").limit(500).all()?;
        }
    }
    if is_admin()? {
        for correction in db::find::<Record>("att_correction").filter("status", "pending").order_by("work_date").limit(500).all()? {
            if !found.iter().any(|seen| text(seen, "id") == text(&correction, "id")) {
                found.push(correction);
            }
        }
    }
    if let Some(me) = &me {
        found.retain(|c| text(c, "employee") != text(me, "id"));
    }
    Ok(found)
}

handler! {
    /// Ask for a missing punch, a wrong time to be replaced, or an absent day to be put right.
    fn request_correction(input: Ask) -> Record {
        ask(input)
    }

    fn approve_correction(input: Decide) -> Record {
        decide(input, true)
    }

    fn reject_correction(input: Decide) -> Record {
        decide(input, false)
    }

    fn cancel_correction(input: Id) -> Record {
        cancel(input)
    }

    fn corrections_waiting_for_me(_: Empty) -> Vec<Record> {
        waiting()
    }

    fn my_corrections(_: Empty) -> Vec<Record> {
        let Some(me) = my_employee()? else { return Ok(Vec::new()) };
        db::find("att_correction").filter("employee", id_of(&me)?).order_by("-work_date").limit(200).all()
    }
}
