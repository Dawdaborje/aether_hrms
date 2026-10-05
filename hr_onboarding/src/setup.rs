//! Templates, their steps, and departure reasons.

use aether_sdk::db::Filter;
use aether_sdk::prelude::*;

use crate::common::{pick, require, require_manager, text, Record};
use crate::rules::{AssigneeKind, Roll};

const ANCHORS: &[&str] = &["start", "last_day", "notice_date"];
const GATES: &[&str] = &["none", "hire", "access", "settlement"];
const KINDS: &[&str] = &["resignation", "termination", "end_of_contract", "retirement", "other"];

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct NewStep {
    template: String,
    #[serde(flatten)]
    fields: Record,
}

#[derive(Deserialize, Default)]
struct Search {
    #[serde(default)]
    kind: Option<String>,
}

fn new_template(input: Record) -> Result<Record> {
    require_manager()?;
    let kind = text(&input, "kind").unwrap_or_default();
    if !["onboard", "offboard"].contains(&kind) {
        return Err(Error::msg("kind is onboard or offboard"));
    }
    for (field, model) in [("department", "department"), ("job", "job")] {
        if let Some(id) = text(&input, field) {
            let found: Option<Record> = plugins::call("hr", &format!("get_{model}"), &json!({ "id": id }))?;
            if found.is_none() {
                return Err(Error::msg(format!("there is no such {model}")));
            }
        }
    }
    db::create("ob_template", &pick(&input, &["name", "kind", "department", "job"])).map_err(|e| e.or("could not create the template (the name may be taken)"))
}

fn new_step(input: NewStep) -> Result<Record> {
    require_manager()?;
    let template = require("ob_template", &input.template, "template")?;
    let mut data = pick(&input.fields, &["name", "sequence", "anchor", "offset_days", "duration_days", "roll", "assignee_kind", "assignee_user", "assignee_role", "gate", "weight", "description"]);
    data["template"] = json!(input.template);
    check_step(&template, &data)?;
    db::create("ob_step", &data).map_err(|e| e.or("could not add the step"))
}

fn check_step(template: &Record, data: &Record) -> Result<()> {
    let kind = text(template, "kind").unwrap_or_default();
    let anchor = text(data, "anchor").unwrap_or("start");
    if !ANCHORS.contains(&anchor) {
        return Err(Error::msg("anchor is start, last_day or notice_date"));
    }
    if (kind == "onboard") != (anchor == "start") {
        return Err(Error::msg("an onboarding step is anchored on the start day; an offboarding step on the last day or the notice date"));
    }
    if Roll::parse(text(data, "roll").unwrap_or("next")).is_none() {
        return Err(Error::msg("roll is none, next or previous"));
    }
    let who = text(data, "assignee_kind").unwrap_or("owner");
    let Some(who) = AssigneeKind::parse(who) else { return Err(Error::msg("assignee_kind is employee, manager, manager2, user, role or owner")) };
    if who == AssigneeKind::User && text(data, "assignee_user").is_none() {
        return Err(Error::msg("name the user who does this step"));
    }
    if who == AssigneeKind::Role && text(data, "assignee_role").is_none() {
        return Err(Error::msg("name the role that does this step"));
    }
    let gate = text(data, "gate").unwrap_or("none");
    if !GATES.contains(&gate) {
        return Err(Error::msg("gate is none, hire, access or settlement"));
    }
    if gate == "hire" && kind != "onboard" || (gate == "access" || gate == "settlement") && kind != "offboard" {
        return Err(Error::msg("a hire gate belongs to onboarding; access and settlement gates to offboarding"));
    }
    if data.get("duration_days").and_then(Value::as_i64).is_some_and(|d| d < 0) {
        return Err(Error::msg("a step cannot last a negative number of days"));
    }
    if data.get("weight").and_then(Value::as_i64).is_some_and(|w| w < 1) {
        return Err(Error::msg("a step weighs at least 1"));
    }
    Ok(())
}

fn template_with_steps(id: &str) -> Result<Value> {
    let template = require("ob_template", id, "template")?;
    let steps: Vec<Record> = db::find("ob_step").filter("template", id).order_by("sequence").limit(200).all()?;
    Ok(json!({ "template": template, "steps": steps }))
}

fn new_reason(input: Record) -> Result<Record> {
    require_manager()?;
    let kind = text(&input, "kind").unwrap_or("other");
    if !KINDS.contains(&kind) {
        return Err(Error::msg("kind is resignation, termination, end_of_contract, retirement or other"));
    }
    if let Some(status) = text(&input, "status_to") {
        if !["resigned", "terminated", "retired", "deceased"].contains(&status) {
            return Err(Error::msg("an ended employment is resigned, terminated, retired or deceased"));
        }
    }
    db::create("ob_departure_reason", &pick(&input, &["name", "kind", "status_to"])).map_err(|e| e.or("could not create the reason (the name may be taken)"))
}

/// The best template for a person: a template that names a department or job must match it; among
/// those left the more specific wins, then the name decides.
pub fn pick_template(kind: &str, department: Option<&str>, job: Option<&str>) -> Result<Record> {
    let all: Vec<Record> = db::find::<Record>("ob_template")
        .matching(Filter::eq("kind", kind).and(Filter::ne("is_active", false)))
        .order_by("name")
        .limit(200)
        .all()?;
    let mut best: Option<(u8, Record)> = None;
    for template in all {
        let (wants_department, wants_job) = (text(&template, "department"), text(&template, "job"));
        if wants_department.is_some() && wants_department != department || wants_job.is_some() && wants_job != job {
            continue;
        }
        let score = u8::from(wants_department.is_some()) * 2 + u8::from(wants_job.is_some());
        if best.as_ref().is_none_or(|(top, _)| score > *top) {
            best = Some((score, template));
        }
    }
    best.map(|(_, t)| t).ok_or_else(|| Error::msg(format!("there is no active {kind} template for this person: a manager sets one up first")))
}

handler! {
    fn create_template(input: Record) -> Record {
        new_template(input)
    }

    fn add_step(input: NewStep) -> Record {
        new_step(input)
    }

    fn deactivate_template(input: Id) -> Record {
        require_manager()?;
        db::update("ob_template", &input.id, &json!({ "is_active": false }))?.ok_or_else(|| Error::msg("there is no such template"))
    }

    fn get_template(input: Id) -> Value {
        template_with_steps(&input.id)
    }

    fn list_templates(input: Option<Search>) -> Vec<Record> {
        let mut find = db::find::<Record>("ob_template").order_by("name").limit(200);
        if let Some(kind) = input.and_then(|i| i.kind) {
            find = find.filter("kind", kind.as_str());
        }
        find.all()
    }

    fn create_departure_reason(input: Record) -> Record {
        new_reason(input)
    }

    fn list_departure_reasons(_: Empty) -> Vec<Record> {
        db::find("ob_departure_reason").order_by("name").limit(200).all()
    }
}
