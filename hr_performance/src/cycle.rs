//! Templates, cycles, appraisals, feedback, calibration and publication.
//!
//! A cycle moves through phases, one step at a time, and each phase allows its own writes. The
//! template is copied onto each appraisal when it is made, so editing it later changes nothing already
//! under way. Scores are computed from what is there under the cycle's missing-data policy; the
//! employee sees nothing between self review and publication.

use std::collections::BTreeMap;

use aether_sdk::dates::{format_date, parse_date};
use aether_sdk::db::Filter;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{
    dec, employee, id_of, int, my_employee, phase_of, require, require_admin, text, today_date, weighted, works_here, Record,
};
use crate::rules::{
    check_scale, check_weights, final_score, goal_component, mean, on_scale, rating_score, reminder_due, rollup, Components, Missing, Phase, Weights,
};

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct NewTemplate {
    name: String,
    kras: Value,
    criteria: Value,
}

#[derive(Deserialize)]
struct NewCycle {
    name: String,
    start_date: String,
    end_date: String,
    template: String,
    #[serde(default)]
    scale_max: Option<i64>,
    #[serde(default)]
    w_goal: Option<i64>,
    #[serde(default)]
    w_self: Option<i64>,
    #[serde(default)]
    w_manager: Option<i64>,
    #[serde(default)]
    w_peer: Option<i64>,
    #[serde(default)]
    missing_policy: Option<String>,
    #[serde(default)]
    min_peers: Option<i64>,
    #[serde(default)]
    deadline_goal: Option<String>,
    #[serde(default)]
    deadline_self: Option<String>,
    #[serde(default)]
    deadline_manager: Option<String>,
    #[serde(default)]
    deadline_calibration: Option<String>,
}

#[derive(Deserialize)]
struct Open {
    cycle: String,
    #[serde(default)]
    employees: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct SelfReview {
    appraisal: String,
    ratings: BTreeMap<String, i64>,
    #[serde(default)]
    reflection: Option<String>,
}

#[derive(Deserialize)]
struct Assign {
    appraisal: String,
    reviewer: String,
}

#[derive(Deserialize)]
struct Review {
    appraisal: String,
    ratings: BTreeMap<String, i64>,
    #[serde(default)]
    comment: Option<String>,
}

#[derive(Deserialize)]
struct Calibrate {
    appraisal: String,
    score: Decimal,
    reason: String,
}

#[derive(Deserialize, Default)]
struct Search {
    #[serde(default)]
    cycle: Option<String>,
    #[serde(default)]
    employee: Option<String>,
}

fn check_deadline(value: &Option<String>) -> Result<Option<String>> {
    value.as_deref().map(|d| parse_date(d).map(format_date)).transpose()
}

fn new_template(input: NewTemplate) -> Result<Record> {
    require_admin()?;
    check_weights(&weighted(&input.kras, "key result areas")?, "key result areas")?;
    check_weights(&weighted(&input.criteria, "criteria")?, "criteria")?;
    db::create("perf_template", &json!({ "name": input.name, "kras": input.kras, "criteria": input.criteria }))
        .map_err(|e| e.or("could not create the template (the name may be taken)"))
}

fn weights_of(cycle: &Record) -> Weights {
    Weights { goal: int(cycle, "w_goal"), own: int(cycle, "w_self"), manager: int(cycle, "w_manager"), peer: int(cycle, "w_peer") }
}

fn policy_of(cycle: &Record) -> Result<Missing> {
    Missing::parse(text(cycle, "missing_policy").unwrap_or("exclude")).ok_or_else(|| Error::msg("a cycle has an unknown missing-data policy"))
}

fn new_cycle(input: NewCycle) -> Result<Record> {
    require_admin()?;
    let (start, end) = (parse_date(&input.start_date)?, parse_date(&input.end_date)?);
    if end < start {
        return Err(Error::msg("the cycle ends after it starts"));
    }
    require("perf_template", &input.template, "template")?;
    let scale = input.scale_max.unwrap_or(5);
    check_scale(scale)?;
    let weights = Weights { goal: input.w_goal.unwrap_or(40), own: input.w_self.unwrap_or(10), manager: input.w_manager.unwrap_or(40), peer: input.w_peer.unwrap_or(10) };
    weights.check()?;
    let policy = input.missing_policy.as_deref().unwrap_or("exclude");
    if Missing::parse(policy).is_none() {
        return Err(Error::msg("missing_policy is exclude, zero or block"));
    }
    let min_peers = input.min_peers.unwrap_or(0);
    if !(0..=20).contains(&min_peers) {
        return Err(Error::msg("min_peers is 0 to 20"));
    }
    let mut data = json!({
        "name": input.name, "start_date": format_date(start), "end_date": format_date(end), "status": "draft", "scale_max": scale,
        "template": input.template, "w_goal": weights.goal, "w_self": weights.own, "w_manager": weights.manager, "w_peer": weights.peer,
        "missing_policy": policy, "min_peers": min_peers,
    });
    for (field, value) in [
        ("deadline_goal", &input.deadline_goal),
        ("deadline_self", &input.deadline_self),
        ("deadline_manager", &input.deadline_manager),
        ("deadline_calibration", &input.deadline_calibration),
    ] {
        if let Some(day) = check_deadline(value)? {
            data[field] = json!(day);
        }
    }
    db::create("perf_cycle", &data).map_err(|e| e.or("could not create the cycle (the name may be taken)"))
}

fn appraisals_of(cycle_id: &str) -> Result<Vec<Record>> {
    db::find("perf_appraisal").filter("cycle", cycle_id).limit(1000).all()
}

fn open_appraisals(input: Open) -> Result<Value> {
    require_admin()?;
    let cycle = require("perf_cycle", &input.cycle, "cycle")?;
    if !matches!(phase_of(&cycle)?, Phase::Draft | Phase::GoalSetting) {
        return Err(Error::msg("appraisals are opened while the cycle is in draft or goal setting"));
    }
    let template = require("perf_template", text(&cycle, "template").unwrap_or_default(), "template")?;
    let people: Vec<Record> = match &input.employees {
        Some(ids) => ids.iter().map(|id| employee(id)).collect::<Result<_>>()?,
        None => plugins::call("hr", "list_employees", &json!({ "limit": 500 }))?,
    };
    let (mut created, mut skipped) = (0u64, 0u64);
    let mut failed: Vec<String> = Vec::new();
    for person in &people {
        let one = || -> Result<bool> {
            if !works_here(person) {
                return Ok(false);
            }
            let id = id_of(person)?;
            if db::count("perf_appraisal", Filter::eq("cycle", input.cycle.as_str()).and(Filter::eq("employee", id)))? > 0 {
                return Ok(false);
            }
            let mut data = json!({
                "cycle": input.cycle, "employee": id, "kras": template["kras"], "criteria": template["criteria"], "phase": cycle["status"],
            });
            if let Some(manager) = text(person, "manager") {
                data["manager"] = json!(manager);
            }
            db::create::<Record>("perf_appraisal", &data)?;
            Ok(true)
        };
        match one() {
            Ok(true) => created += 1,
            Ok(false) => skipped += 1,
            Err(error) => failed.push(format!("{}: {error}", text(person, "display_name").unwrap_or("?"))),
        }
    }
    Ok(json!({ "created": created, "skipped": skipped, "failed": failed }))
}

fn appraisal_cycle(appraisal: &Record) -> Result<Record> {
    require("perf_cycle", text(appraisal, "cycle").unwrap_or_default(), "cycle")
}

fn submit_self(input: SelfReview) -> Result<Record> {
    let appraisal = require("perf_appraisal", &input.appraisal, "appraisal")?;
    let cycle = appraisal_cycle(&appraisal)?;
    let me = my_employee()?.ok_or_else(|| Error::msg("only an employee does a self review"))?;
    if text(&me, "id") != text(&appraisal, "employee") {
        return Err(Error::msg("a self review is done by the person it is about"));
    }
    if phase_of(&cycle)? != Phase::SelfReview {
        return Err(Error::msg("self reviews are done while the cycle is in self review"));
    }
    let criteria = weighted(&appraisal["criteria"], "criteria")?;
    let score = rating_score(&criteria, &input.ratings, int(&cycle, "scale_max"))?;
    let mut changes = json!({ "self_ratings": input.ratings, "self_score": score, "self_done": true });
    if let Some(reflection) = &input.reflection {
        changes["reflection"] = json!(reflection);
    }
    db::update("perf_appraisal", &input.appraisal, &changes)?.ok_or_else(|| Error::msg("the appraisal is gone"))
}

fn assign_reviewer(input: Assign) -> Result<Record> {
    require_admin()?;
    let appraisal = require("perf_appraisal", &input.appraisal, "appraisal")?;
    let cycle = appraisal_cycle(&appraisal)?;
    if !matches!(phase_of(&cycle)?, Phase::GoalSetting | Phase::SelfReview | Phase::ManagerReview) {
        return Err(Error::msg("reviewers are chosen up to the managers' review"));
    }
    let reviewer = employee(&input.reviewer)?;
    if !works_here(&reviewer) {
        return Err(Error::msg("this person does not work here"));
    }
    if text(&appraisal, "employee") == Some(input.reviewer.as_str()) {
        return Err(Error::msg("nobody reviews themselves as a peer"));
    }
    if text(&appraisal, "manager") == Some(input.reviewer.as_str()) {
        return Err(Error::msg("the manager already reviews: peers are other people"));
    }
    db::create("perf_feedback", &json!({ "appraisal": input.appraisal, "reviewer": input.reviewer, "role": "peer", "status": "requested" }))
        .map_err(|e| e.or("this person is already asked to review"))
}

fn submit_feedback(input: Review) -> Result<Record> {
    let appraisal = require("perf_appraisal", &input.appraisal, "appraisal")?;
    let cycle = appraisal_cycle(&appraisal)?;
    if phase_of(&cycle)? != Phase::ManagerReview {
        return Err(Error::msg("reviews are given while the cycle is in the managers' review"));
    }
    let me = my_employee()?.ok_or_else(|| Error::msg("only an employee gives a review"))?;
    let row = db::find::<Record>("perf_feedback")
        .filter("appraisal", input.appraisal.as_str())
        .filter("reviewer", id_of(&me)?)
        .first()?
        .ok_or_else(|| Error::msg("you were not asked to review this person"))?;
    let criteria = weighted(&appraisal["criteria"], "criteria")?;
    let score = rating_score(&criteria, &input.ratings, int(&cycle, "scale_max"))?;
    let mut changes = json!({ "ratings": input.ratings, "score": score, "status": "submitted" });
    if let Some(comment) = &input.comment {
        changes["comment"] = json!(comment);
    }
    db::update("perf_feedback", id_of(&row)?, &changes)?.ok_or_else(|| Error::msg("the review is gone"))
}

/// Compute an appraisal's component scores and final score from what is there now.
pub fn recompute(appraisal_id: &str) -> Result<Record> {
    let appraisal = require("perf_appraisal", appraisal_id, "appraisal")?;
    let cycle = appraisal_cycle(&appraisal)?;
    let policy = policy_of(&cycle)?;
    let scale = int(&cycle, "scale_max");
    let employee_id = text(&appraisal, "employee").unwrap_or_default();

    // Goals: each key result area's top goals, weighted among themselves.
    let goals: Vec<Record> = db::find::<Record>("perf_goal")
        .filter("employee", employee_id)
        .filter("cycle", text(&cycle, "id").unwrap_or_default())
        .matching(Filter::ne("status", "archived"))
        .limit(1000)
        .all()?;
    let mut areas = Vec::new();
    for (name, weight) in weighted(&appraisal["kras"], "key result areas")? {
        let mut roots = Vec::new();
        for goal in goals.iter().filter(|g| text(g, "kra") == Some(name.as_str()) && g.get("parent").is_none_or(Value::is_null)) {
            roots.push((int(goal, "weight"), dec(goal, "progress")?.unwrap_or(Decimal::zero(2))));
        }
        areas.push((name, weight, if roots.is_empty() { None } else { Some(rollup(&roots)?) }));
    }
    let (goal, uncovered) = goal_component(&areas, policy)?;

    let rows: Vec<Record> = db::find::<Record>("perf_feedback").filter("appraisal", appraisal_id).filter("status", "submitted").limit(200).all()?;
    let mut manager = Vec::new();
    let mut peer = Vec::new();
    for row in &rows {
        if let Some(score) = dec(row, "score")? {
            if text(row, "role") == Some("manager") { manager.push(score) } else { peer.push(score) }
        }
    }
    let min_peers = int(&cycle, "min_peers").max(0) as usize;
    let components = Components {
        goal,
        own: dec(&appraisal, "self_score")?,
        manager: mean(&manager)?,
        peer: if peer.len() >= min_peers.max(1) { mean(&peer)? } else { None },
    };
    let (computed, missing) = final_score(&components, &weights_of(&cycle), policy)?;
    let mut missing: Vec<String> = missing.iter().map(|m| (*m).to_string()).collect();
    missing.extend(uncovered.iter().map(|k| format!("goals:{k}")));

    let mut changes = json!({ "computed_score": computed, "missing": missing });
    for (field, score) in [("goal_score", components.goal), ("manager_score", components.manager), ("peer_score", components.peer)] {
        changes[field] = json!(score);
    }
    if appraisal.get("calibrated") != Some(&json!(true)) {
        changes["final_score"] = json!(computed);
        changes["final_rating"] = json!(on_scale(computed, scale)?);
    }
    db::update("perf_appraisal", appraisal_id, &changes)?.ok_or_else(|| Error::msg("the appraisal is gone"))
}

fn calibrate(input: Calibrate) -> Result<Record> {
    require_admin()?;
    let appraisal = require("perf_appraisal", &input.appraisal, "appraisal")?;
    let cycle = appraisal_cycle(&appraisal)?;
    if phase_of(&cycle)? != Phase::Calibration {
        return Err(Error::msg("scores are calibrated while the cycle is in calibration"));
    }
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why the score is changed"));
    }
    let score = input.score.with_scale(2)?;
    if score < Decimal::zero(2) || score > Decimal::whole(100, 2)? {
        return Err(Error::msg("a score is 0 to 100"));
    }
    db::update(
        "perf_appraisal",
        &input.appraisal,
        &json!({ "final_score": score, "final_rating": on_scale(score, int(&cycle, "scale_max"))?, "calibrated": true, "calibration_reason": input.reason }),
    )?
    .ok_or_else(|| Error::msg("the appraisal is gone"))
}

fn set_phase_on_appraisals(cycle_id: &str, phase: Phase) -> Result<Vec<String>> {
    let mut failed = Vec::new();
    for appraisal in appraisals_of(cycle_id)? {
        if let Err(error) = db::update::<Record>("perf_appraisal", id_of(&appraisal)?, &json!({ "phase": phase.as_str() })) {
            failed.push(format!("{}: {error}", text(&appraisal, "id").unwrap_or("?")));
        }
    }
    Ok(failed)
}

fn advance(input: Id) -> Result<Record> {
    require_admin()?;
    let cycle = require("perf_cycle", &input.id, "cycle")?;
    let from = phase_of(&cycle)?;
    let to = from.next().ok_or_else(|| Error::msg("this cycle is closed"))?;
    let appraisals = appraisals_of(&input.id)?;
    match to {
        Phase::GoalSetting if appraisals.is_empty() => return Err(Error::msg("open appraisals before goal setting starts")),
        Phase::ManagerReview => {
            // The manager owes a review for each appraisal; peers were asked while goals were set.
            for appraisal in &appraisals {
                if let Some(manager) = text(appraisal, "manager") {
                    if db::count("perf_feedback", Filter::eq("appraisal", id_of(appraisal)?).and(Filter::eq("reviewer", manager)))? == 0 {
                        db::create::<Record>("perf_feedback", &json!({ "appraisal": appraisal["id"], "reviewer": manager, "role": "manager", "status": "requested" }))?;
                    }
                }
            }
        }
        Phase::Calibration => {
            let mut problems = Vec::new();
            for appraisal in &appraisals {
                if let Err(error) = recompute(id_of(appraisal)?) {
                    problems.push(format!("{}: {error}", text(appraisal, "employee").unwrap_or("?")));
                }
            }
            if !problems.is_empty() {
                return Err(Error::msg(format!("some appraisals cannot be scored yet: {}", problems.join("; "))));
            }
        }
        Phase::Published => {
            let unscored = appraisals.iter().filter(|a| a.get("final_score").is_none_or(Value::is_null)).count();
            if unscored > 0 {
                return Err(Error::msg(format!("{unscored} appraisal(s) have no final score")));
            }
        }
        _ => {}
    }
    let failed = set_phase_on_appraisals(&input.id, to)?;
    if !failed.is_empty() {
        return Err(Error::msg(format!("could not move every appraisal on (run it again): {}", failed.join("; "))));
    }
    if to == Phase::Published {
        let today = format_date(today_date()?);
        for appraisal in &appraisals {
            db::update::<Record>("perf_appraisal", id_of(appraisal)?, &json!({ "published_on": today }))?;
            events::emit("appraisal_published", &json!({ "appraisal": appraisal["id"], "employee": appraisal["employee"], "cycle": input.id }))?;
        }
    }
    let moved = db::update::<Record>("perf_cycle", &input.id, &json!({ "status": to.as_str() }))?.ok_or_else(|| Error::msg("the cycle is gone"))?;
    events::emit("performance_phase_changed", &json!({ "cycle": input.id, "from": from.as_str(), "to": to.as_str() }))?;
    Ok(moved)
}

fn deadline_of(cycle: &Record) -> Result<Option<(&'static str, aether_sdk::dates::NaiveDate)>> {
    let (key, field) = match phase_of(cycle)? {
        Phase::GoalSetting => ("goal_setting", "deadline_goal"),
        Phase::SelfReview => ("self_review", "deadline_self"),
        Phase::ManagerReview => ("manager_review", "deadline_manager"),
        Phase::Calibration => ("calibration", "deadline_calibration"),
        _ => return Ok(None),
    };
    Ok(text(cycle, field).map(parse_date).transpose()?.map(|d| (key, d)))
}

/// Announce, once per phase, that a deadline is near or past.
fn tick() -> Result<Value> {
    let today = today_date()?;
    let mut announced = 0u64;
    let mut failures: Vec<String> = Vec::new();
    let cycles: Vec<Record> = db::find::<Record>("perf_cycle").limit(200).all()?;
    for cycle in &cycles {
        let one = || -> Result<bool> {
            let Some((key, deadline)) = deadline_of(cycle)? else { return Ok(false) };
            let marker = format!("{}:{key}", text(cycle, "id").unwrap_or_default());
            if text(cycle, "reminded") == Some(marker.as_str()) || !reminder_due(Some(deadline), today, 3) {
                return Ok(false);
            }
            events::emit("performance_deadline", &json!({ "cycle": cycle["id"], "phase": key, "deadline": format_date(deadline), "late": today > deadline }))?;
            db::update::<Record>("perf_cycle", id_of(cycle)?, &json!({ "reminded": marker }))?;
            Ok(true)
        };
        match one() {
            Ok(true) => announced += 1,
            Ok(false) => {}
            Err(error) => failures.push(format!("{}: {error}", text(cycle, "name").unwrap_or("?"))),
        }
    }
    Ok(json!({ "announced": announced, "failures": failures }))
}

fn my_review_ids() -> Result<Vec<String>> {
    let Some(me) = my_employee()? else { return Ok(Vec::new()) };
    let rows: Vec<Record> = db::find("perf_feedback").filter("reviewer", id_of(&me)?).limit(500).all()?;
    Ok(rows.iter().filter_map(|r| text(r, "appraisal").map(str::to_string)).collect())
}

fn requests() -> Result<Vec<Value>> {
    let Some(me) = my_employee()? else { return Ok(Vec::new()) };
    let rows: Vec<Record> = db::find("perf_feedback").filter("reviewer", id_of(&me)?).limit(500).all()?;
    let mut out = Vec::new();
    for row in rows {
        let Some(appraisal) = db::get::<Record>("perf_appraisal", text(&row, "appraisal").unwrap_or_default())? else { continue };
        if text(&appraisal, "phase") != Some("manager_review") {
            continue;
        }
        out.push(json!({ "feedback": row, "employee": appraisal["employee"], "criteria": appraisal["criteria"] }));
    }
    Ok(out)
}

handler! {
    fn create_template(input: NewTemplate) -> Record {
        new_template(input)
    }

    fn list_templates(_: Empty) -> Vec<Record> {
        db::find("perf_template").order_by("name").limit(200).all()
    }

    fn create_cycle(input: NewCycle) -> Record {
        new_cycle(input)
    }

    fn get_cycle(input: Id) -> Option<Record> {
        db::get("perf_cycle", &input.id)
    }

    fn list_cycles(_: Empty) -> Vec<Record> {
        db::find("perf_cycle").order_by("-start_date").limit(100).all()
    }

    /// Make an appraisal for each employee (or the ones named), copying the template onto it.
    fn open_cycle_appraisals(input: Open) -> Value {
        open_appraisals(input)
    }

    /// Move the cycle to its next phase, checking what that phase needs.
    fn advance_cycle(input: Id) -> Record {
        advance(input)
    }

    fn submit_self_review(input: SelfReview) -> Record {
        submit_self(input)
    }

    fn assign_peer_reviewer(input: Assign) -> Record {
        assign_reviewer(input)
    }

    fn submit_review(input: Review) -> Record {
        submit_feedback(input)
    }

    fn refresh_appraisal(input: Id) -> Record {
        require_admin()?;
        recompute(&input.id)
    }

    fn calibrate_appraisal(input: Calibrate) -> Record {
        calibrate(input)
    }

    fn get_appraisal(input: Id) -> Option<Record> {
        db::get("perf_appraisal", &input.id)
    }

    fn list_appraisals(input: Option<Search>) -> Vec<Record> {
        let input = input.unwrap_or_default();
        let mut find = db::find::<Record>("perf_appraisal").limit(1000);
        for (field, value) in [("cycle", &input.cycle), ("employee", &input.employee)] {
            if let Some(value) = value {
                find = find.filter(field, value.as_str());
            }
        }
        find.all()
    }

    /// The reviews the caller owes in the managers' review.
    fn my_review_requests(_: Empty) -> Vec<Value> {
        requests()
    }

    fn list_feedback(input: Id) -> Vec<Record> {
        require_admin()?;
        db::find("perf_feedback").filter("appraisal", input.id.as_str()).limit(200).all()
    }

    fn performance_tick(_: Empty) -> Value {
        tick()
    }

    fn rule_var_my_review_appraisals(_: Empty) -> Vec<String> {
        my_review_ids()
    }
}
