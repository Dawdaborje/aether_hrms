//! Preauthorisations: a reviewer approves an amount valid until a date; a claim that needs one consumes it, and a
//! claim over what is left is refused.

use aether_sdk::dates::{format_date, parse_date, NaiveDate};
use aether_sdk::db::Transaction;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::*;
use crate::rules::covered_on;

#[derive(Deserialize)]
struct Ask {
    enrolment: String,
    #[serde(default)]
    member: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    procedure: String,
    estimated_amount: Decimal,
    #[serde(default)]
    planned_date: Option<String>,
}

#[derive(Deserialize)]
struct Decide {
    id: String,
    approve: bool,
    #[serde(default)]
    approved_amount: Option<Decimal>,
    #[serde(default)]
    valid_until: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

fn ask(input: Ask) -> Result<Record> {
    let enrolment = require("ins_enrolment", &input.enrolment, "enrolment")?;
    let who = text(&enrolment, "employee").unwrap_or_default().to_string();
    require_self_or_staff(&who)?;
    if text(&enrolment, "state") != Some("active") {
        return Err(Error::msg("preauthorisation is asked on an active enrolment"));
    }
    if input.procedure.trim().is_empty() {
        return Err(Error::msg("say what the procedure is"));
    }
    if input.estimated_amount.is_negative() || input.estimated_amount.is_zero() {
        return Err(Error::msg("an estimate is more than zero"));
    }
    if let Some(m) = &input.member {
        let member = require("ins_member", m, "member")?;
        if text(&member, "enrolment") != Some(input.enrolment.as_str()) {
            return Err(Error::msg("that person is not covered by this enrolment"));
        }
    }
    db::create(
        "ins_preauth",
        &json!({
            "reference": next_number("preauth", "PRE-", 5)?, "enrolment": input.enrolment, "employee": who, "member": input.member,
            "provider": input.provider, "procedure": input.procedure, "estimated_amount": input.estimated_amount, "planned_date": input.planned_date,
            "consumed_amount": 0, "state": "submitted",
        }),
    )
}

fn decide(input: Decide) -> Result<Record> {
    require_staff()?;
    let row = require("ins_preauth", &input.id, "preauthorisation")?;
    if text(&row, "state") != Some("submitted") {
        return Err(Error::msg("this request is not waiting for a decision"));
    }
    if my_employee_id()?.as_deref() == text(&row, "employee") {
        return Err(Error::msg("nobody decides their own request"));
    }
    if !input.approve {
        if input.note.as_deref().is_none_or(|n| n.trim().is_empty()) {
            return Err(Error::msg("say why it is refused"));
        }
        return db::update("ins_preauth", &input.id, &json!({ "state": "rejected", "decided_by": actor()?, "note": input.note }))?
            .ok_or_else(|| Error::msg("the request is gone"));
    }
    let until = input.valid_until.as_deref().ok_or_else(|| Error::msg("an approval is valid until a date"))?;
    if parse_date(until)? < today_date()? {
        return Err(Error::msg("an approval cannot already be over"));
    }
    let amount = input.approved_amount.ok_or_else(|| Error::msg("name the amount approved"))?;
    if amount.is_negative() || amount.is_zero() {
        return Err(Error::msg("an approved amount is more than zero"));
    }
    db::update("ins_preauth", &input.id, &json!({ "state": "approved", "approved_amount": amount, "valid_until": until, "decided_by": actor()?, "note": input.note }))?
        .ok_or_else(|| Error::msg("the request is gone"))
}

/// Refuses a claim that cannot lean on this preauthorisation.
pub fn check_usable(id: &str, enrolment: &str, member: &str, service: NaiveDate, total: Decimal, places: u32) -> Result<()> {
    let row = require("ins_preauth", id, "preauthorisation")?;
    if text(&row, "state") != Some("approved") {
        return Err(Error::msg("this preauthorisation is not approved (or is used up)"));
    }
    if text(&row, "enrolment") != Some(enrolment) {
        return Err(Error::msg("this preauthorisation is for another enrolment"));
    }
    if let Some(m) = text(&row, "member") {
        if m != member {
            return Err(Error::msg("this preauthorisation is for another person"));
        }
    }
    if let Some(until) = date_opt(&row, "valid_until")? {
        if !covered_on(service.min(until), Some(until), service) {
            return Err(Error::msg("this preauthorisation had expired by the service date"));
        }
    }
    let left = decimal_of(&row, "approved_amount", places)? - decimal_of(&row, "consumed_amount", places)?;
    if total > left {
        return Err(Error::msg("the claim is more than what the preauthorisation has left"));
    }
    Ok(())
}

/// Adds the claim's approved amount to what the preauthorisation has consumed, closing it when used up.
pub fn consume(tx: Transaction, id: &str, amount: Decimal, places: u32) -> Result<Transaction> {
    let row = require("ins_preauth", id, "preauthorisation")?;
    let used = decimal_of(&row, "consumed_amount", places)? + amount;
    let state = if used >= decimal_of(&row, "approved_amount", places)? { "used" } else { "approved" };
    Ok(tx.update("ins_preauth", id, &json!({ "consumed_amount": used, "state": state })))
}

/// Daily: approved preauthorisations past their date become `expired`.
pub fn expire_due() -> Result<i64> {
    let today = format_date(today_date()?);
    let rows: Vec<Record> = db::find("ins_preauth").filter("state", "approved").limit(5000).all()?;
    let mut n = 0;
    for r in rows {
        if text(&r, "valid_until").is_some_and(|v| v < today.as_str()) {
            db::update::<Record>("ins_preauth", text(&r, "id").unwrap_or_default(), &json!({ "state": "expired" }))?;
            n += 1;
        }
    }
    Ok(n)
}

handler! {
    fn request_preauthorisation(input: Ask) -> Record {
        ask(input)
    }

    fn decide_preauthorisation(input: Decide) -> Record {
        decide(input)
    }

    fn cancel_preauthorisation(input: Id) -> Record {
        let row = require("ins_preauth", &input.id, "preauthorisation")?;
        require_self_or_staff(text(&row, "employee").unwrap_or_default())?;
        if !matches!(text(&row, "state"), Some("submitted" | "approved")) {
            return Err(Error::msg("this request is already over"));
        }
        db::update("ins_preauth", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the request is gone"))
    }

    fn list_preauthorisations(_: Empty) -> Vec<Record> {
        if is_staff()? {
            return db::find("ins_preauth").limit(2000).all();
        }
        let Some(me) = my_employee_id()? else { return Ok(Vec::new()) };
        db::find("ins_preauth").filter("employee", me.as_str()).limit(500).all()
    }
}
