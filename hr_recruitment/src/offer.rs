//! Offers and the hire.
//!
//! An offer is drafted, approved by a manager who did not draft it, sent (the seats are checked again
//! because time has passed), answered, and finally turned into an employee through the hr plugin, whose
//! hire holds still apply. Salary outside the opening's band needs a written reason.

use aether_sdk::dates::{format_date, parse_date};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{id_of, my_employee, require, require_manager, require_recruiter, text, today_date, Record};
use crate::opening::{free_seats, refresh_filled};
use crate::rules::{check_seats, in_band};

#[derive(Deserialize)]
struct Draft {
    application: String,
    start_date: String,
    salary: Decimal,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    terms: Option<String>,
    #[serde(default)]
    valid_until: Option<String>,
    #[serde(default)]
    override_reason: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Response {
    id: String,
    accepted: bool,
}

fn optional(record: &Record, field: &str) -> Option<Decimal> {
    crate::common::optional_decimal(record, field).ok().flatten()
}

fn draft(input: Draft) -> Result<Record> {
    require_recruiter()?;
    let application = require("rec_application", &input.application, "application")?;
    if text(&application, "status") != Some("active") {
        return Err(Error::msg("this application is closed"));
    }
    let stage = require("rec_stage", text(&application, "stage").unwrap_or_default(), "stage")?;
    if !matches!(text(&stage, "kind"), Some("offer" | "interview" | "screening")) {
        return Err(Error::msg("an offer is made to someone who has been screened or interviewed"));
    }
    let open_offers = db::count(
        "rec_offer",
        aether_sdk::db::Filter::eq("application", input.application.as_str()).and(aether_sdk::db::Filter::one_of("status", ["draft", "approved", "sent"])),
    )?;
    if open_offers > 0 {
        return Err(Error::msg("this application already has an offer in progress"));
    }
    let opening = require("rec_opening", text(&application, "opening").unwrap_or_default(), "opening")?;
    let start = parse_date(&input.start_date)?;
    if start < today_date()? {
        return Err(Error::msg("the start date is in the past"));
    }
    let salary = input.salary.with_scale(2)?;
    if salary <= Decimal::zero(2) {
        return Err(Error::msg("the salary is above zero"));
    }
    let inside = in_band(salary, optional(&opening, "salary_min"), optional(&opening, "salary_max"));
    let reason = input.override_reason.as_deref().map(str::trim).filter(|r| !r.is_empty());
    if !inside && reason.is_none() {
        return Err(Error::msg("the salary is outside the opening's band: give the reason for the exception"));
    }
    let me = my_employee()?;
    let mut data = json!({
        "application": input.application, "status": "draft", "position": opening["position"], "department": opening["department"],
        "start_date": format_date(start), "salary": salary,
        "currency": input.currency.clone().or_else(|| text(&opening, "currency").map(str::to_string)).unwrap_or_default(),
    });
    if let Some(me) = &me {
        data["created_by"] = me["id"].clone();
    }
    if let Some(terms) = &input.terms {
        data["terms"] = json!(terms);
    }
    if let Some(valid) = &input.valid_until {
        data["valid_until"] = json!(format_date(parse_date(valid)?));
    }
    if let (Some(reason), false) = (reason, inside) {
        data["override_reason"] = json!(reason);
    }
    db::create("rec_offer", &data).map_err(|e| e.or("could not draft the offer"))
}

fn approve(input: Id) -> Result<Record> {
    require_manager()?;
    let offer = require("rec_offer", &input.id, "offer")?;
    if text(&offer, "status") != Some("draft") {
        return Err(Error::msg("only a draft offer is approved"));
    }
    let me = my_employee()?;
    if let Some(me) = &me {
        if text(&offer, "created_by") == text(me, "id") {
            return Err(Error::msg("someone other than the person who drafted the offer approves it"));
        }
    }
    db::update::<Record>("rec_offer", &input.id, &json!({ "status": "approved", "approved_by": me.as_ref().map(|m| m["id"].clone()) }))?
        .ok_or_else(|| Error::msg("the offer is gone"))
}

fn send(input: Id) -> Result<Record> {
    require_recruiter()?;
    let offer = require("rec_offer", &input.id, "offer")?;
    if text(&offer, "status") != Some("approved") {
        return Err(Error::msg("an offer is approved before it is sent"));
    }
    let application = require("rec_application", text(&offer, "application").unwrap_or_default(), "application")?;
    let opening = require("rec_opening", text(&application, "opening").unwrap_or_default(), "opening")?;
    if !matches!(text(&opening, "status"), Some("open" | "on_hold")) {
        return Err(Error::msg("the opening is closed"));
    }
    // Time has passed since the opening opened: the position may be full by now. Other offers out for
    // the same opening each hold one of its seats.
    let outstanding = opening.get("seats").and_then(Value::as_i64).unwrap_or(0) - opening.get("seats_filled").and_then(Value::as_i64).unwrap_or(0);
    let sent_for_opening = sent_for(text(&opening, "id").unwrap_or_default())?;
    check_seats(1, outstanding.min(free_seats(&opening)?) - sent_for_opening)?;
    let done = db::update::<Record>("rec_offer", &input.id, &json!({ "status": "sent" }))?.ok_or_else(|| Error::msg("the offer is gone"))?;
    events::emit("offer_sent", &json!({ "offer": input.id, "application": offer["application"], "candidate": application["candidate"] }))?;
    Ok(done)
}

fn sent_for(opening_id: &str) -> Result<i64> {
    let applications: Vec<Record> = db::find::<Record>("rec_application").filter("opening", opening_id).limit(1000).all()?;
    let mut count = 0;
    for application in &applications {
        count += db::count("rec_offer", aether_sdk::db::Filter::eq("application", id_of(application)?).and(aether_sdk::db::Filter::eq("status", "sent")))? as i64;
    }
    Ok(count)
}

fn respond(input: Response) -> Result<Record> {
    require_recruiter()?;
    let offer = require("rec_offer", &input.id, "offer")?;
    if text(&offer, "status") != Some("sent") {
        return Err(Error::msg("only an offer that was sent gets an answer"));
    }
    let status = if input.accepted { "accepted" } else { "declined" };
    let done = db::update::<Record>("rec_offer", &input.id, &json!({ "status": status, "responded_on": format_date(today_date()?) }))?
        .ok_or_else(|| Error::msg("the offer is gone"))?;
    if input.accepted {
        events::emit("offer_accepted", &json!({ "offer": input.id, "application": offer["application"] }))?;
    }
    Ok(done)
}

fn withdraw(input: Id) -> Result<Record> {
    require_recruiter()?;
    let offer = require("rec_offer", &input.id, "offer")?;
    if !matches!(text(&offer, "status"), Some("draft" | "approved" | "sent")) {
        return Err(Error::msg("this offer is already settled"));
    }
    db::update::<Record>("rec_offer", &input.id, &json!({ "status": "withdrawn" }))?.ok_or_else(|| Error::msg("the offer is gone"))
}

#[derive(Deserialize)]
struct Complete {
    id: String,
}

/// Turn an accepted offer into an employee (or take an old one back).
fn complete(input: Complete) -> Result<Record> {
    require_manager()?;
    let offer = require("rec_offer", &input.id, "offer")?;
    if text(&offer, "status") != Some("accepted") {
        return Err(Error::msg("only an accepted offer becomes a hire"));
    }
    let application = require("rec_application", text(&offer, "application").unwrap_or_default(), "application")?;
    let candidate = text(&application, "candidate").unwrap_or_default().to_string();
    let mut terms = json!({
        "hire_date": offer["start_date"], "position": offer["position"], "department": offer["department"], "wage": offer["salary"], "currency": offer["currency"],
    });
    if let Some(code) = text(&offer, "currency").filter(|c| !c.is_empty()) {
        let currency: Option<Record> = plugins::call("currency", "get_currency_by_code", &json!({ "code": code }))?;
        terms["currency"] = currency.ok_or_else(|| Error::msg(format!("there is no currency `{code}`")))?["id"].clone();
    } else {
        terms.as_object_mut().map(|t| t.remove("currency"));
    }
    let position: Option<Record> = plugins::call("hr", "get_position", &json!({ "id": offer["position"] }))?;
    if let Some(job) = position.as_ref().and_then(|p| p.get("job")) {
        terms["job"] = job.clone();
    }
    let existing: Option<Record> = plugins::call("hr", "employee_of_party", &json!({ "id": candidate }))?;
    // hr refuses with its own reason (a hold, someone still employed...), which reaches the caller.
    let hired: Record = match existing {
        Some(old) => {
            terms["employee"] = old["id"].clone();
            plugins::call("hr", "rehire_employee", &terms)?
        }
        None => {
            terms["party"] = json!(candidate);
            plugins::call("hr", "hire_employee", &terms)?
        }
    };
    db::update::<Record>("rec_application", text(&application, "id").unwrap_or_default(), &json!({ "status": "hired", "hired_on": format_date(today_date()?) }))?;
    db::update::<Record>("rec_offer", &input.id, &json!({ "status": "accepted" }))?;
    refresh_filled(text(&application, "opening").unwrap_or_default())?;
    events::emit("candidate_hired", &json!({ "application": application["id"], "employee": hired["id"], "offer": input.id }))?;
    Ok(hired)
}

/// Offers past their validity lapse.
pub fn expire_offers() -> Result<u64> {
    let today = format_date(today_date()?);
    let due: Vec<Record> = db::find::<Record>("rec_offer")
        .matching(aether_sdk::db::Filter::eq("status", "sent").and(aether_sdk::db::Filter::lt("valid_until", today.as_str())))
        .limit(500)
        .all()?;
    for offer in &due {
        db::update::<Record>("rec_offer", id_of(offer)?, &json!({ "status": "expired" }))?;
    }
    Ok(due.len() as u64)
}

handler! {
    fn create_offer(input: Draft) -> Record {
        draft(input)
    }

    fn approve_offer(input: Id) -> Record {
        approve(input)
    }

    fn send_offer(input: Id) -> Record {
        send(input)
    }

    fn record_offer_response(input: Response) -> Record {
        respond(input)
    }

    fn withdraw_offer(input: Id) -> Record {
        withdraw(input)
    }

    /// Hire the person: a new employee, or the old record taken back.
    fn complete_hire(input: Complete) -> Record {
        complete(input)
    }

    fn get_offer(input: Id) -> Option<Record> {
        db::get("rec_offer", &input.id)
    }
}
