//! Interviews, blind feedback, outcomes, and the periodic tick.
//!
//! Feedback is blind: an interviewer sees only their own until the recruiter evaluates the interview,
//! which reveals everyone's (Frappe shows each interviewer a form of their own and never compares;
//! Odoo has no interview at all). The outcome is computed from the feedback, not typed in.

use std::collections::BTreeMap;

use aether_sdk::dates::{parse_datetime, NaiveDateTime};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{decimal_of, employee, id_of, my_employee, now_utc, require, require_recruiter, text, Record};
use crate::rules::{interview_outcome, weighted_score, Recommendation};
use crate::setup::criteria_of;

#[derive(Deserialize)]
struct Schedule {
    application: String,
    round: String,
    starts_at: String,
    ends_at: String,
    #[serde(default)]
    location: Option<String>,
    panel: Vec<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Feedback {
    interview: String,
    scores: BTreeMap<String, i64>,
    recommendation: String,
    #[serde(default)]
    comment: Option<String>,
}

#[derive(Deserialize)]
struct Outcomes {
    id: String,
    #[serde(default)]
    summary: Option<String>,
}

fn moment(record: &Record, field: &str) -> Result<NaiveDateTime> {
    parse_datetime(text(record, field).ok_or_else(|| Error::msg(format!("{field} is missing")))?)
}

fn overlaps(a: (NaiveDateTime, NaiveDateTime), b: (NaiveDateTime, NaiveDateTime)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

fn schedule(input: Schedule) -> Result<Record> {
    require_recruiter()?;
    let application = require("rec_application", &input.application, "application")?;
    if text(&application, "status") != Some("active") {
        return Err(Error::msg("this application is closed"));
    }
    require("rec_round", &input.round, "round")?;
    let start = parse_datetime(&input.starts_at)?;
    let end = parse_datetime(&input.ends_at)?;
    if end <= start {
        return Err(Error::msg("the interview ends after it starts"));
    }
    if start < now_utc()? {
        return Err(Error::msg("the interview is in the past"));
    }
    let mut panel = input.panel.clone();
    panel.sort();
    panel.dedup();
    if panel.is_empty() {
        return Err(Error::msg("an interview needs at least one interviewer"));
    }
    for id in &panel {
        let person = employee(id)?;
        if !crate::common::works_here(&person) {
            return Err(Error::msg(format!("{} does not work here", text(&person, "display_name").unwrap_or(id))));
        }
        // Nobody sits two interviews at once.
        for other in db::related_reverse::<Record>("rec_interview", "panel", id)? {
            if text(&other, "status") == Some("scheduled") && overlaps((start, end), (moment(&other, "starts_at")?, moment(&other, "ends_at")?)) {
                return Err(Error::msg(format!("{} is already in an interview at that time", text(&person, "display_name").unwrap_or(id))));
            }
        }
    }
    let mut data = json!({ "application": input.application, "round": input.round, "starts_at": input.starts_at, "ends_at": input.ends_at, "status": "scheduled", "outcome": "pending" });
    if let Some(location) = &input.location {
        data["location"] = json!(location);
    }
    let made: Record = db::create("rec_interview", &data).map_err(|e| e.or("could not schedule the interview"))?;
    db::relate::<Record>("rec_interview", "panel", id_of(&made)?, &panel.iter().map(String::as_str).collect::<Vec<_>>())?;
    Ok(made)
}

fn submit(input: Feedback) -> Result<Record> {
    let interview = require("rec_interview", &input.interview, "interview")?;
    let me = my_employee()?.ok_or_else(|| Error::msg("only an employee gives interview feedback"))?;
    let me_id = id_of(&me)?;
    let panel: Vec<Record> = db::related("rec_interview", "panel", &input.interview)?;
    if !panel.iter().any(|p| text(p, "id") == Some(me_id)) {
        return Err(Error::msg("you are not on this interview's panel"));
    }
    if text(&interview, "status") == Some("cancelled") {
        return Err(Error::msg("this interview was cancelled"));
    }
    if text(&interview, "outcome") != Some("pending") {
        return Err(Error::msg("this interview already has its outcome"));
    }
    if moment(&interview, "starts_at")? > now_utc()? {
        return Err(Error::msg("the interview has not started yet"));
    }
    if Recommendation::parse(&input.recommendation).is_none() {
        return Err(Error::msg("recommendation is strong_yes, yes, no or strong_no"));
    }
    let round = require("rec_round", text(&interview, "round").unwrap_or_default(), "round")?;
    let weighted = weighted_score(&criteria_of(&round)?, &input.scores)?;
    let mut data = json!({
        "interview": input.interview, "interviewer": me_id, "scores": input.scores, "weighted": weighted,
        "recommendation": input.recommendation, "revealed": false,
    });
    if let Some(comment) = &input.comment {
        data["comment"] = json!(comment);
    }
    // The unique (interview, interviewer) index refuses a second one.
    db::create("rec_feedback", &data).map_err(|e| e.or("you already gave feedback for this interview"))
}

/// Work out and store an interview's outcome from the feedback in.
fn evaluate_one(interview: &Record, summary: Option<&str>) -> Result<Record> {
    let id = id_of(interview)?;
    let round = require("rec_round", text(interview, "round").unwrap_or_default(), "round")?;
    let panel: Vec<Record> = db::related("rec_interview", "panel", id)?;
    let rows: Vec<Record> = db::find::<Record>("rec_feedback").filter("interview", id).limit(100).all()?;
    let mut feedback = Vec::new();
    for row in &rows {
        let recommendation = Recommendation::parse(text(row, "recommendation").unwrap_or_default()).ok_or_else(|| Error::msg("feedback with an unknown recommendation"))?;
        feedback.push((decimal_of(row, "weighted")?, recommendation));
    }
    let min_average = decimal_of(&round, "min_average")?;
    let min_panelists = round.get("min_panelists").and_then(Value::as_i64).unwrap_or(1).max(1) as usize;
    let outcome = interview_outcome(&feedback, min_average, min_panelists, panel.len())?;
    let mut changes = json!({ "outcome": outcome.as_str(), "status": "held" });
    if !feedback.is_empty() {
        let sum = feedback.iter().fold(Decimal::zero(2), |sum, (score, _)| sum + *score);
        changes["mean_score"] = json!(sum.times_ratio(1, feedback.len() as i64)?.round_to(2));
    }
    if let Some(summary) = summary {
        changes["summary"] = json!(summary);
    }
    let done = db::update::<Record>("rec_interview", id, &changes)?.ok_or_else(|| Error::msg("the interview is gone"))?;
    if outcome.as_str() != "pending" {
        for row in &rows {
            db::update::<Record>("rec_feedback", id_of(row)?, &json!({ "revealed": true }))?;
        }
        events::emit("interview_outcome_ready", &json!({ "interview": id, "application": interview["application"], "outcome": outcome.as_str() }))?;
    }
    Ok(done)
}

fn evaluate(input: Outcomes) -> Result<Record> {
    require_recruiter()?;
    let interview = require("rec_interview", &input.id, "interview")?;
    if text(&interview, "status") == Some("cancelled") {
        return Err(Error::msg("this interview was cancelled"));
    }
    if moment(&interview, "starts_at")? > now_utc()? {
        return Err(Error::msg("the interview has not started yet"));
    }
    evaluate_one(&interview, input.summary.as_deref())
}

fn cancel(input: Id) -> Result<Record> {
    require_recruiter()?;
    let interview = require("rec_interview", &input.id, "interview")?;
    if text(&interview, "status") != Some("scheduled") {
        return Err(Error::msg("only a scheduled interview can be cancelled"));
    }
    db::update::<Record>("rec_interview", &input.id, &json!({ "status": "cancelled" }))?.ok_or_else(|| Error::msg("the interview is gone"))
}

fn no_show(input: Id) -> Result<Record> {
    require_recruiter()?;
    let interview = require("rec_interview", &input.id, "interview")?;
    if text(&interview, "status") != Some("scheduled") || moment(&interview, "starts_at")? > now_utc()? {
        return Err(Error::msg("only an interview that should have happened can be marked a no-show"));
    }
    db::update::<Record>("rec_interview", &input.id, &json!({ "status": "no_show" }))?.ok_or_else(|| Error::msg("the interview is gone"))
}

/// Runs every few minutes: interviews that took place are marked held, ones with enough feedback get
/// their outcome, interviewers are reminded a day ahead, and stale openings and offers close.
fn tick() -> Result<Value> {
    let now = now_utc()?;
    let mut held = 0u64;
    let mut reminded = 0u64;
    let scheduled: Vec<Record> = db::find::<Record>("rec_interview").filter("status", "scheduled").limit(500).all()?;
    for interview in &scheduled {
        let start = moment(interview, "starts_at")?;
        if start <= now {
            db::update::<Record>("rec_interview", id_of(interview)?, &json!({ "status": "held" }))?;
            held += 1;
        } else if interview.get("reminded") != Some(&json!(true)) && start.signed_duration_since(now).num_hours() < 24 {
            let panel: Vec<Record> = db::related("rec_interview", "panel", id_of(interview)?)?;
            let ids: Vec<&str> = panel.iter().filter_map(|p| text(p, "id")).collect();
            events::emit("interview_reminder", &json!({ "interview": interview["id"], "starts_at": interview["starts_at"], "interviewers": ids }))?;
            db::update::<Record>("rec_interview", id_of(interview)?, &json!({ "reminded": true }))?;
            reminded += 1;
        }
    }
    let mut evaluated = 0u64;
    for interview in db::find::<Record>("rec_interview").filter("status", "held").filter("outcome", "pending").limit(500).all()? {
        let done = evaluate_one(&interview, None)?;
        if text(&done, "outcome") != Some("pending") {
            evaluated += 1;
        }
    }
    let closed = crate::opening::close_expired()?;
    let expired = crate::offer::expire_offers()?;
    Ok(json!({ "held": held, "reminded": reminded, "evaluated": evaluated, "openings_closed": closed, "offers_expired": expired }))
}

fn list(application: Option<String>) -> Result<Vec<Record>> {
    let mut find = db::find::<Record>("rec_interview").order_by("starts_at").limit(500);
    if let Some(application) = &application {
        find = find.filter("application", application.as_str());
    }
    find.all()
}

#[derive(Deserialize, Default)]
struct ForApplication {
    #[serde(default)]
    application: Option<String>,
}

fn my_ids(model: &str, openings: bool) -> Result<Vec<String>> {
    let Some(me) = my_employee()? else { return Ok(Vec::new()) };
    let me_id = id_of(&me)?.to_string();
    let mut interviews: Vec<Record> = db::related_reverse("rec_interview", "panel", &me_id)?;
    let mut openings_seen: Vec<Record> = db::related_reverse("rec_opening", "panel", &me_id)?;
    let own: Vec<Record> = db::find::<Record>("rec_opening")
        .matching(aether_sdk::db::Filter::eq("recruiter", me_id.as_str()).or(aether_sdk::db::Filter::eq("requested_by", me_id.as_str())))
        .limit(500)
        .all()?;
    openings_seen.extend(own);
    Ok(match (model, openings) {
        ("interview", _) => interviews.drain(..).filter_map(|i| text(&i, "id").map(str::to_string)).collect(),
        ("application", _) => interviews.drain(..).filter_map(|i| text(&i, "application").map(str::to_string)).collect(),
        _ => openings_seen.iter().filter_map(|o| text(o, "id").map(str::to_string)).collect(),
    })
}

handler! {
    fn schedule_interview(input: Schedule) -> Record {
        schedule(input)
    }

    fn cancel_interview(input: Id) -> Record {
        cancel(input)
    }

    fn mark_no_show(input: Id) -> Record {
        no_show(input)
    }

    /// An interviewer's feedback, blind to the others'.
    fn submit_feedback(input: Feedback) -> Record {
        submit(input)
    }

    /// The recruiter closes the interview: outcome from the feedback in, everyone's feedback revealed.
    fn evaluate_interview(input: Outcomes) -> Record {
        evaluate(input)
    }

    fn get_interview(input: Id) -> Option<Record> {
        db::get("rec_interview", &input.id)
    }

    fn list_interviews(input: Option<ForApplication>) -> Vec<Record> {
        list(input.and_then(|i| i.application))
    }

    fn list_feedback(input: Id) -> Vec<Record> {
        db::find::<Record>("rec_feedback").filter("interview", input.id.as_str()).limit(100).all()
    }

    fn recruitment_tick(_: Empty) -> Value {
        tick()
    }

    fn rule_var_my_openings(_: Empty) -> Vec<String> {
        my_ids("opening", true)
    }

    fn rule_var_my_applications(_: Empty) -> Vec<String> {
        my_ids("application", false)
    }

    fn rule_var_my_interviews(_: Empty) -> Vec<String> {
        my_ids("interview", false)
    }
}
