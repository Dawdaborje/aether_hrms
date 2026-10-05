//! Goals: a tree per person per cycle, each under a key result area of the cycle's template.
//!
//! Frappe's parent goal is the plain mean of its children and a child carries no weight, target or
//! history. Here a leaf has a target and a current value (progress is their ratio, with a check-in
//! logged each time), a group's progress is the weighted mean of its children, and the roll-up runs
//! up the chain in the same call that changed a child.

use aether_sdk::dates::format_date;
use aether_sdk::db::Filter;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{dec, employee, int, may_act_for, my_employee, phase_of, require, text, today_date, weighted, Record};
use crate::rules::{leaf_progress, rollup};

#[derive(Deserialize)]
struct New {
    employee: String,
    cycle: String,
    kra: String,
    name: String,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    weight: Option<i64>,
    #[serde(default)]
    target: Option<Decimal>,
    #[serde(default)]
    is_group: bool,
}

#[derive(Deserialize)]
struct Change {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    weight: Option<i64>,
    #[serde(default)]
    target: Option<Decimal>,
}

#[derive(Deserialize)]
struct Progress {
    id: String,
    value: Decimal,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Mine {
    employee: String,
    cycle: String,
}

fn cycle_and_person(cycle_id: &str, employee_id: &str) -> Result<(Record, Record)> {
    Ok((require("perf_cycle", cycle_id, "cycle")?, employee(employee_id)?))
}

fn create(input: New) -> Result<Record> {
    let (cycle, person) = cycle_and_person(&input.cycle, &input.employee)?;
    if !may_act_for(&person)? {
        return Err(Error::msg("goals are set by the person or their manager"));
    }
    if !phase_of(&cycle)?.goals_editable() {
        return Err(Error::msg("goals can be set while the cycle is in goal setting or self review"));
    }
    let template = require("perf_template", text(&cycle, "template").unwrap_or_default(), "template")?;
    if !weighted(&template["kras"], "key result areas")?.iter().any(|(name, _)| name == &input.kra) {
        return Err(Error::msg(format!("`{}` is not a key result area of this cycle", input.kra)));
    }
    let weight = input.weight.unwrap_or(1);
    if weight < 1 {
        return Err(Error::msg("a goal weighs at least 1"));
    }
    let mut data = json!({
        "employee": input.employee, "cycle": input.cycle, "kra": input.kra, "name": input.name, "weight": weight, "status": "open",
        "is_group": input.is_group, "progress": Decimal::zero(2),
    });
    if let Some(parent_id) = &input.parent {
        let parent = require("perf_goal", parent_id, "parent goal")?;
        if parent.get("is_group") != Some(&json!(true)) {
            return Err(Error::msg("a goal sits under a group goal"));
        }
        if text(&parent, "employee") != Some(input.employee.as_str()) || text(&parent, "cycle") != Some(input.cycle.as_str()) || text(&parent, "kra") != Some(input.kra.as_str()) {
            return Err(Error::msg("a goal shares its parent's person, cycle and key result area"));
        }
        data["parent"] = json!(parent_id);
    }
    if input.is_group {
        if input.target.is_some() {
            return Err(Error::msg("a group goal has no target of its own: it is made of its children"));
        }
    } else {
        let target = input.target.unwrap_or(Decimal::whole(100, 2)?).with_scale(2)?;
        if target <= Decimal::zero(2) {
            return Err(Error::msg("a goal's target is above zero"));
        }
        data["target"] = json!(target);
        data["current"] = json!(Decimal::zero(2));
    }
    let made: Record = db::create("perf_goal", &data).map_err(|e| e.or("could not create the goal (the parent may be below this goal)"))?;
    if let Some(parent) = &input.parent {
        recompute_up(parent)?;
    }
    Ok(made)
}

fn change(input: Change) -> Result<Record> {
    let goal = require("perf_goal", &input.id, "goal")?;
    let (cycle, person) = cycle_and_person(text(&goal, "cycle").unwrap_or_default(), text(&goal, "employee").unwrap_or_default())?;
    if !may_act_for(&person)? {
        return Err(Error::msg("goals are changed by the person or their manager"));
    }
    if !phase_of(&cycle)?.goals_editable() {
        return Err(Error::msg("goals can be changed while the cycle is in goal setting or self review"));
    }
    let mut changes = json!({});
    if let Some(name) = &input.name {
        changes["name"] = json!(name);
    }
    if let Some(weight) = input.weight {
        if weight < 1 {
            return Err(Error::msg("a goal weighs at least 1"));
        }
        changes["weight"] = json!(weight);
    }
    if let Some(target) = input.target {
        if goal.get("is_group") == Some(&json!(true)) {
            return Err(Error::msg("a group goal has no target"));
        }
        let target = target.with_scale(2)?;
        if target <= Decimal::zero(2) {
            return Err(Error::msg("a goal's target is above zero"));
        }
        changes["target"] = json!(target);
        let current = dec(&goal, "current")?.unwrap_or(Decimal::zero(2));
        changes["progress"] = json!(leaf_progress(current, target)?);
    }
    if changes.as_object().is_some_and(|c| c.is_empty()) {
        return Err(Error::msg("there is nothing to change"));
    }
    let updated = db::update::<Record>("perf_goal", &input.id, &changes)?.ok_or_else(|| Error::msg("the goal is gone"))?;
    if let Some(parent) = text(&goal, "parent") {
        recompute_up(parent)?;
    }
    Ok(updated)
}

/// Recompute a group goal from its children, then its own parent, and so on up the tree.
fn recompute_up(group_id: &str) -> Result<()> {
    let mut current = Some(group_id.to_string());
    let mut steps = 0;
    while let Some(id) = current {
        steps += 1;
        if steps > 20 {
            return Err(Error::msg("the goal tree is too deep"));
        }
        let children: Vec<Record> = db::find::<Record>("perf_goal")
            .filter("parent", id.as_str())
            .matching(Filter::ne("status", "archived"))
            .limit(500)
            .all()?;
        let mut parts = Vec::new();
        for child in &children {
            parts.push((int(child, "weight"), dec(child, "progress")?.unwrap_or(Decimal::zero(2))));
        }
        let progress = rollup(&parts)?;
        let group = db::update::<Record>("perf_goal", &id, &json!({ "progress": progress }))?.ok_or_else(|| Error::msg("a group goal is gone"))?;
        current = text(&group, "parent").map(str::to_string);
    }
    Ok(())
}

fn report(input: Progress) -> Result<Record> {
    let goal = require("perf_goal", &input.id, "goal")?;
    if goal.get("is_group") == Some(&json!(true)) {
        return Err(Error::msg("a group goal's progress comes from its children"));
    }
    if text(&goal, "status") == Some("archived") {
        return Err(Error::msg("this goal is archived"));
    }
    let (cycle, person) = cycle_and_person(text(&goal, "cycle").unwrap_or_default(), text(&goal, "employee").unwrap_or_default())?;
    if !may_act_for(&person)? {
        return Err(Error::msg("progress is reported by the person or their manager"));
    }
    if !phase_of(&cycle)?.progress_reportable() {
        return Err(Error::msg("progress is reported until the managers' review ends"));
    }
    let value = input.value.with_scale(2)?;
    let target = dec(&goal, "target")?.ok_or_else(|| Error::msg("this goal has no target"))?;
    let progress = leaf_progress(value, target)?;
    let status = if progress >= Decimal::whole(100, 2)? { "done" } else { "open" };
    let updated = db::update::<Record>("perf_goal", &input.id, &json!({ "current": value, "progress": progress, "status": status }))?
        .ok_or_else(|| Error::msg("the goal is gone"))?;
    let mut checkin = json!({ "goal": input.id, "value": value, "on_date": format_date(today_date()?) });
    if let Some(note) = &input.note {
        checkin["note"] = json!(note);
    }
    if let Some(me) = my_employee()? {
        checkin["by"] = me["id"].clone();
    }
    db::create::<Record>("perf_checkin", &checkin)?;
    if let Some(parent) = text(&goal, "parent") {
        recompute_up(parent)?;
    }
    Ok(updated)
}

fn archive(input: Id) -> Result<Record> {
    let goal = require("perf_goal", &input.id, "goal")?;
    let (cycle, person) = cycle_and_person(text(&goal, "cycle").unwrap_or_default(), text(&goal, "employee").unwrap_or_default())?;
    if !may_act_for(&person)? {
        return Err(Error::msg("goals are archived by the person or their manager"));
    }
    if !phase_of(&cycle)?.goals_editable() {
        return Err(Error::msg("goals can be archived while the cycle is in goal setting or self review"));
    }
    let archived = db::update::<Record>("perf_goal", &input.id, &json!({ "status": "archived" }))?.ok_or_else(|| Error::msg("the goal is gone"))?;
    if let Some(parent) = text(&goal, "parent") {
        recompute_up(parent)?;
    }
    Ok(archived)
}

handler! {
    fn create_goal(input: New) -> Record {
        create(input)
    }

    fn update_goal(input: Change) -> Record {
        change(input)
    }

    /// Report how far a goal has got; a check-in is logged and the groups above are recomputed.
    fn report_progress(input: Progress) -> Record {
        report(input)
    }

    fn archive_goal(input: Id) -> Record {
        archive(input)
    }

    fn get_goal(input: Id) -> Option<Record> {
        db::get("perf_goal", &input.id)
    }

    fn list_goals(input: Mine) -> Vec<Record> {
        db::find("perf_goal").filter("employee", input.employee.as_str()).filter("cycle", input.cycle.as_str()).order_by("kra").limit(500).all()
    }

    fn goal_checkins(input: Id) -> Vec<Record> {
        db::find("perf_checkin").filter("goal", input.id.as_str()).order_by("on_date").limit(500).all()
    }
}
