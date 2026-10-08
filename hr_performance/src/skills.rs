//! Skills: a level per person and skill, kept as a history, compared with what a job asks.
//!
//! Odoo keeps `valid_from` / `valid_to` rows and expires a removed skill rather than deleting it;
//! its levels are asserted by HR alone and never compared with a job. Here a rating records who gave
//! it and why, a level never disappears, and the gap to a job's targets is computed.

use aether_sdk::dates::{format_date, Duration};
use aether_sdk::db::Filter;
use aether_sdk::prelude::*;

use crate::common::{employee, id_of, int, is_admin, may_act_for, pick, require, require_admin, text, today_date, Record, my_employee};
use crate::rules::skill_gap;

#[derive(Deserialize)]
struct Rate {
    employee: String,
    skill: String,
    level: i64,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
struct Requirement {
    job: String,
    skill: String,
    target_level: i64,
    #[serde(default)]
    weight: Option<i64>,
}

#[derive(Deserialize)]
struct Who {
    employee: String,
}

#[derive(Deserialize)]
struct History {
    employee: String,
    skill: String,
}

#[derive(Deserialize)]
struct Job {
    job: String,
}

fn new_skill(input: Record) -> Result<Record> {
    require_admin()?;
    let scale = input.get("scale_max").and_then(Value::as_i64).unwrap_or(5);
    crate::rules::check_scale(scale)?;
    db::create("perf_skill", &pick(&input, &["name", "category", "scale_max"])).map_err(|e| e.or("could not create the skill (the name may be taken)"))
}

fn rate(input: Rate) -> Result<Record> {
    let person = employee(&input.employee)?;
    if !may_act_for(&person)? {
        return Err(Error::msg("only a manager, or the people who run reviews, rate someone's skills"));
    }
    // Rating yourself is allowed only for the people who run reviews: a level is asserted by someone else.
    if !is_admin()? && my_employee()?.is_some_and(|me| text(&me, "id") == Some(input.employee.as_str())) {
        return Err(Error::msg("your manager rates your skills"));
    }
    let skill = require("perf_skill", &input.skill, "skill")?;
    let max = int(&skill, "scale_max");
    if !(1..=max).contains(&input.level) {
        return Err(Error::msg(format!("a level of this skill is 1 to {max}")));
    }
    let today = today_date()?;
    let current: Option<Record> = db::find::<Record>("perf_skill_rating")
        .filter("employee", input.employee.as_str())
        .filter("skill", input.skill.as_str())
        .matching(Filter::all())
        .order_by("-valid_from")
        .limit(20)
        .all()?
        .into_iter()
        .find(|r| r.get("valid_to").is_none_or(Value::is_null));
    let me = my_employee()?;
    let mut data = json!({
        "employee": input.employee, "skill": input.skill, "level": input.level, "scale_max": max, "valid_from": format_date(today),
    });
    if let Some(reason) = &input.reason {
        data["reason"] = json!(reason);
    }
    if let Some(me) = &me {
        data["rated_by"] = me["id"].clone();
    }
    if let Some(current) = current {
        if int(&current, "level") == input.level {
            return Err(Error::msg("that is already their level"));
        }
        if text(&current, "valid_from") == Some(format_date(today).as_str()) {
            // Corrected on the day it was given: the same entry, not a new step in the history.
            return db::update::<Record>("perf_skill_rating", id_of(&current)?, &pick(&data, &["level", "reason", "rated_by"]))?
                .ok_or_else(|| Error::msg("the rating is gone"));
        }
        db::update::<Record>("perf_skill_rating", id_of(&current)?, &json!({ "valid_to": format_date(today - Duration::days(1)) }))?;
    }
    db::create("perf_skill_rating", &data).map_err(|e| e.or("could not record the level"))
}

#[derive(Deserialize)]
struct Credit {
    employee: String,
    skill: String,
    level: i64,
    source: String,
}

/// Raise a level because of something that happened (a course passed). Only the people who run reviews or
/// training may do it; it never lowers a level and is a no-op when the person is already there.
fn credit(input: Credit) -> Result<Value> {
    let ctx = context::current()?;
    if !ctx.has_role(crate::common::ADMIN_ROLE) && !ctx.has_role("hr_training.training_admin") {
        return Err(Error::msg("only the people who run reviews or training can credit a skill"));
    }
    employee(&input.employee)?;
    let skill = require("perf_skill", &input.skill, "skill")?;
    let max = int(&skill, "scale_max");
    if !(1..=max).contains(&input.level) {
        return Err(Error::msg(format!("a level of this skill is 1 to {max}")));
    }
    let current = current_levels(&input.employee)?.into_iter().find(|r| text(r, "skill") == Some(input.skill.as_str()));
    if current.as_ref().is_some_and(|c| int(c, "level") >= input.level) {
        return Ok(json!({ "credited": false, "level": current.map(|c| int(&c, "level")) }));
    }
    let today = today_date()?;
    if let Some(current) = current {
        let closed = if text(&current, "valid_from") == Some(format_date(today).as_str()) { today } else { today - Duration::days(1) };
        db::update::<Record>("perf_skill_rating", id_of(&current)?, &json!({ "valid_to": format_date(closed) }))?;
    }
    let made: Record = db::create(
        "perf_skill_rating",
        &json!({ "employee": input.employee, "skill": input.skill, "level": input.level, "scale_max": max, "valid_from": format_date(today), "reason": input.source }),
    )?;
    Ok(json!({ "credited": true, "level": input.level, "rating": made["id"] }))
}

fn current_levels(employee_id: &str) -> Result<Vec<Record>> {
    let rows: Vec<Record> = db::find("perf_skill_rating").filter("employee", employee_id).order_by("-valid_from").limit(500).all()?;
    Ok(rows.into_iter().filter(|r| r.get("valid_to").is_none_or(Value::is_null)).collect())
}

fn set_requirement(input: Requirement) -> Result<Record> {
    require_admin()?;
    let skill = require("perf_skill", &input.skill, "skill")?;
    let found: Option<Record> = plugins::call("hr", "get_job", &json!({ "id": input.job }))?;
    if found.is_none() {
        return Err(Error::msg("there is no such job"));
    }
    if !(1..=int(&skill, "scale_max")).contains(&input.target_level) {
        return Err(Error::msg("the target is a level of the skill"));
    }
    let weight = input.weight.unwrap_or(1).max(1);
    match db::find::<Record>("perf_skill_requirement").filter("job", input.job.as_str()).filter("skill", input.skill.as_str()).first()? {
        Some(existing) => db::update("perf_skill_requirement", id_of(&existing)?, &json!({ "target_level": input.target_level, "weight": weight }))?
            .ok_or_else(|| Error::msg("the requirement is gone")),
        None => db::create("perf_skill_requirement", &json!({ "job": input.job, "skill": input.skill, "target_level": input.target_level, "weight": weight })),
    }
}

/// What a person lacks for their job: each requirement with their level and the shortfall.
fn gaps(employee_id: &str) -> Result<Value> {
    let person = employee(employee_id)?;
    let job = text(&person, "job").ok_or_else(|| Error::msg("this person has no job, so nothing is asked of them"))?;
    let requirements: Vec<Record> = db::find("perf_skill_requirement").filter("job", job).limit(200).all()?;
    let levels = current_levels(employee_id)?;
    let mut rows = Vec::new();
    let mut shortfall = 0i64;
    for requirement in &requirements {
        let have = levels.iter().find(|l| text(l, "skill") == text(requirement, "skill")).map(|l| int(l, "level"));
        let gap = skill_gap(int(requirement, "target_level"), have);
        shortfall += gap * int(requirement, "weight").max(1);
        rows.push(json!({ "skill": requirement["skill"], "target": requirement["target_level"], "level": have, "gap": gap, "weight": requirement["weight"] }));
    }
    Ok(json!({ "employee": employee_id, "job": job, "rows": rows, "weighted_shortfall": shortfall }))
}

handler! {
    fn create_skill(input: Record) -> Record {
        new_skill(input)
    }

    fn list_skills(_: Empty) -> Vec<Record> {
        db::find("perf_skill").order_by("name").limit(500).all()
    }

    /// Give someone a level in a skill; the old one stays in the history.
    /// Raise a person's level for something they did (a course): never lowers.
    fn credit_skill(input: Credit) -> Value {
        credit(input)
    }

    fn rate_skill(input: Rate) -> Record {
        rate(input)
    }

    fn current_skills(input: Who) -> Vec<Record> {
        current_levels(&input.employee)
    }

    fn skill_history(input: History) -> Vec<Record> {
        db::find("perf_skill_rating").filter("employee", input.employee.as_str()).filter("skill", input.skill.as_str()).order_by("valid_from").limit(200).all()
    }

    fn set_skill_requirement(input: Requirement) -> Record {
        set_requirement(input)
    }

    fn list_skill_requirements(input: Job) -> Vec<Record> {
        db::find("perf_skill_requirement").filter("job", input.job.as_str()).limit(200).all()
    }

    fn skill_gaps(input: Who) -> Value {
        gaps(&input.employee)
    }
}
