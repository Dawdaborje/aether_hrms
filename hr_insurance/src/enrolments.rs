//! Enrolments: an employee on a policy for a window, with the people covered. Many per employee over the years;
//! windows on the same policy never overlap.

use aether_sdk::dates::{format_date, parse_date};
use aether_sdk::prelude::*;

use crate::common::*;
use crate::rules::{members_fit, Candidate, Eligibility, Relation};

#[derive(Deserialize)]
struct MemberIn {
    relationship: String,
    name: String,
    #[serde(default)]
    dependent: Option<String>,
    #[serde(default)]
    date_of_birth: Option<String>,
}

#[derive(Deserialize)]
struct Enrol {
    policy: String,
    #[serde(default)]
    employee: Option<String>,
    start_date: String,
    #[serde(default)]
    end_date: Option<String>,
    #[serde(default)]
    members: Vec<MemberIn>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct EndIn {
    id: String,
    #[serde(default)]
    on: Option<String>,
}

#[derive(Deserialize)]
struct AddMember {
    #[serde(alias = "id")]
    enrolment: String,
    relationship: String,
    name: String,
    #[serde(default)]
    date_of_birth: Option<String>,
}

/// Cover one more person on an active enrolment, from today to the enrolment's end, if the plan allows.
fn add_member(input: AddMember) -> Result<Record> {
    let enrolment = require("ins_enrolment", &input.enrolment, "enrolment")?;
    require_staff()?;
    if text(&enrolment, "state") != Some("active") {
        return Err(Error::msg("people are added to an active enrolment"));
    }
    let policy = require("ins_policy", text(&enrolment, "policy").unwrap_or_default(), "policy")?;
    let plan = plan_of_policy(&policy)?;
    let today = today_date()?;
    let start = today.max(date_of(&enrolment, "start_date")?);
    let end = date_opt(&enrolment, "end_date")?;
    let existing: Vec<Record> = db::find("ins_member").filter("enrolment", input.enrolment.as_str()).filter("is_active", true).limit(50).all()?;
    let mut candidates = Vec::new();
    for m in &existing {
        let relation = Relation::parse(text(m, "relationship").unwrap_or_default()).ok_or_else(|| Error::msg("a member is self, spouse, child, parent or other"))?;
        candidates.push(Candidate { relation, born: date_opt(m, "date_of_birth")? });
    }
    candidates.push(Candidate {
        relation: Relation::parse(&input.relationship).ok_or_else(|| Error::msg("a member is spouse, child, parent or other"))?,
        born: input.date_of_birth.as_deref().map(parse_date).transpose()?,
    });
    members_fit(terms(&plan), &candidates, start)?;
    db::create(
        "ins_member",
        &json!({
            "enrolment": input.enrolment, "employee": text(&enrolment, "employee"), "position": existing.len() as i64 + 1, "relationship": input.relationship, "name": input.name,
            "date_of_birth": input.date_of_birth, "coverage_start": format_date(start), "coverage_end": end.map(format_date), "is_active": true,
        }),
    )
}

pub fn plan_of_policy(policy: &Record) -> Result<Record> {
    require("ins_plan", text(policy, "plan").unwrap_or_default(), "plan")
}

fn terms(plan: &Record) -> Eligibility {
    Eligibility {
        allow_spouse: flag(plan, "allow_spouse"),
        allow_children: flag(plan, "allow_children"),
        max_children: plan.get("max_children").and_then(Value::as_i64),
        child_max_age: plan.get("child_max_age").and_then(Value::as_i64).unwrap_or(25),
    }
}

fn enrol(input: Enrol) -> Result<Record> {
    require_staff()?;
    let policy = require("ins_policy", &input.policy, "policy")?;
    if text(&policy, "state") != Some("active") {
        return Err(Error::msg("people are enrolled on an active policy"));
    }
    let plan = plan_of_policy(&policy)?;
    let who = input.employee.clone().ok_or_else(|| Error::msg("name the employee"))?;
    let person = employee(&who)?;
    if !matches!(text(&person, "status"), Some("active" | "probation") | None) {
        return Err(Error::msg("this person is not working here"));
    }
    let start = parse_date(&input.start_date)?;
    let (pstart, pend) = (date_of(&policy, "start_date")?, date_of(&policy, "end_date")?);
    let end = input.end_date.as_deref().map(parse_date).transpose()?.unwrap_or(pend);
    if end < start {
        return Err(Error::msg("the enrolment ends before it starts"));
    }
    if start < pstart || end > pend {
        return Err(Error::msg("an enrolment lies inside its policy's dates"));
    }
    let mut members = input.members;
    if !members.iter().any(|m| m.relationship == "self") {
        members.insert(0, MemberIn { relationship: "self".into(), name: text(&person, "name").unwrap_or("Employee").to_string(), dependent: None, date_of_birth: None });
    }
    let candidates: Vec<Candidate> = members
        .iter()
        .map(|m| {
            Ok(Candidate {
                relation: Relation::parse(&m.relationship).ok_or_else(|| Error::msg("a member is self, spouse, child, parent or other"))?,
                born: m.date_of_birth.as_deref().map(parse_date).transpose()?,
            })
        })
        .collect::<Result<_>>()?;
    members_fit(terms(&plan), &candidates, start)?;
    let others: Vec<Record> = db::find("ins_enrolment").filter("policy", input.policy.as_str()).filter("employee", who.as_str()).limit(50).all()?;
    for other in &others {
        if matches!(text(other, "state"), Some("active")) {
            let oend = date_opt(other, "end_date")?;
            if aether_sdk::dates::overlaps(start, Some(end), date_of(other, "start_date")?, oend) {
                return Err(Error::msg("this person is already enrolled on this policy on those days"));
            }
        }
    }
    let rows: Vec<Value> = members
        .iter()
        .map(|m| json!({
            "employee": who, "relationship": m.relationship, "name": m.name, "dependent": m.dependent, "date_of_birth": m.date_of_birth,
            "coverage_start": format_date(start), "coverage_end": format_date(end), "is_active": true,
        }))
        .collect();
    db::create(
        "ins_enrolment",
        &json!({
            "reference": next_number("enrolment", "ENR-", 5)?, "policy": input.policy, "employee": who,
            "start_date": format_date(start), "end_date": format_date(end), "state": "active", "members": rows,
        }),
    )
}

fn end(input: EndIn) -> Result<Record> {
    require_staff()?;
    let row = require("ins_enrolment", &input.id, "enrolment")?;
    if text(&row, "state") != Some("active") {
        return Err(Error::msg("this enrolment has already ended"));
    }
    let day = match input.on.as_deref() {
        Some(on) => parse_date(on)?,
        None => today_date()?,
    };
    if day < date_of(&row, "start_date")? {
        return Err(Error::msg("the enrolment cannot end before it starts"));
    }
    let members: Vec<Record> = db::find("ins_member").filter("enrolment", input.id.as_str()).limit(100).all()?;
    for m in members {
        db::update::<Record>("ins_member", text(&m, "id").unwrap_or_default(), &json!({ "coverage_end": format_date(day), "is_active": false }))?;
    }
    db::update("ins_enrolment", &input.id, &json!({ "state": "ended", "end_date": format_date(day) }))?.ok_or_else(|| Error::msg("the enrolment is gone"))
}

handler! {
    fn enrol_in_policy(input: Enrol) -> Record {
        enrol(input)
    }

    fn add_insurance_member(input: AddMember) -> Record {
        add_member(input)
    }

    fn end_insurance_enrolment(input: EndIn) -> Record {
        end(input)
    }

    fn my_insurance_enrolments(_: Empty) -> Vec<Record> {
        let Some(me) = my_employee_id()? else { return Ok(Vec::new()) };
        db::find("ins_enrolment").filter("employee", me.as_str()).limit(100).expand("members").all()
    }

    fn list_insurance_enrolments(_: Empty) -> Vec<Record> {
        require_staff()?;
        db::find("ins_enrolment").limit(5000).all()
    }

    fn get_insurance_enrolment(input: Id) -> Record {
        let row = db::find::<Record>("ins_enrolment").filter("id", input.id.as_str()).expand("members").first()?.ok_or_else(|| Error::msg("there is no such enrolment"))?;
        require_self_or_staff(text(&row, "employee").unwrap_or_default())?;
        Ok(row)
    }
}
