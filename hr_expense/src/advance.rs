//! Advances: money given before the spending. They are asked for, approved by the person's approver
//! (never themselves), paid out by finance, used on approved reports in their own currency, and the
//! rest is returned. An advance that is overdue is announced, once.

use aether_sdk::dates::{format_date, parse_date, parse_optional};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{approver_of, currency_places, decimal_of, id_of, is_finance, my_employee, require, require_finance, text, today_date, Record};
use crate::rules::{advance_state, advance_unclaimed};

#[derive(Deserialize)]
struct Ask {
    purpose: String,
    amount: Decimal,
    currency: String,
    #[serde(default)]
    due_date: Option<String>,
}

#[derive(Deserialize)]
struct Decide {
    id: String,
    approve: bool,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct Money {
    advance: String,
    amount: Decimal,
    reference: String,
    #[serde(default)]
    paid_on: Option<String>,
}

fn ask(input: Ask) -> Result<Record> {
    let me = my_employee()?.ok_or_else(|| Error::msg("you are not an employee"))?;
    if input.purpose.trim().is_empty() {
        return Err(Error::msg("say what the advance is for"));
    }
    let currency = input.currency.to_uppercase();
    let places = currency_places(&currency)?;
    let amount = input.amount.with_scale(places).map_err(|_| Error::msg(format!("{currency} has {places} digits after the point")))?;
    if amount.is_negative() || amount.is_zero() {
        return Err(Error::msg("an advance is more than zero"));
    }
    let due = parse_optional(input.due_date.as_deref())?;
    if due.is_some_and(|due| due < today_date().unwrap_or(due)) {
        return Err(Error::msg("the due date has passed"));
    }
    let mut data = json!({ "employee": id_of(&me)?, "purpose": input.purpose, "amount": amount, "currency": currency, "state": "requested" });
    if let Some(due) = due {
        data["due_date"] = json!(format_date(due));
    }
    if let Some(approver) = approver_of(id_of(&me)?)? {
        data["approver"] = json!(approver);
    }
    let made: Record = db::create("expense_advance", &data)?;
    events::emit("advance_requested", &json!({ "advance": made["id"], "employee": id_of(&me)? }))?;
    Ok(made)
}

fn decide(input: Decide) -> Result<Record> {
    let advance = require("expense_advance", &input.id, "advance")?;
    if text(&advance, "state") != Some("requested") {
        return Err(Error::msg("this advance is not waiting for a decision"));
    }
    let me = my_employee()?;
    let own = me.as_ref().is_some_and(|m| text(m, "id") == text(&advance, "employee"));
    let allowed = !own && (is_finance()? || me.as_ref().is_some_and(|m| text(m, "id").is_some() && text(m, "id") == text(&advance, "approver")));
    if !allowed {
        return Err(Error::msg("only this person's approver (or finance) can decide, and never the person themselves"));
    }
    if !input.approve && input.note.as_deref().is_none_or(|n| n.trim().is_empty()) {
        return Err(Error::msg("say why the advance is refused"));
    }
    let done = db::update::<Record>(
        "expense_advance",
        &input.id,
        &json!({ "state": if input.approve { "approved" } else { "rejected" }, "decided_by": me.as_ref().and_then(|m| text(m, "id")), "note": input.note }),
    )?
    .ok_or_else(|| Error::msg("the advance is gone"))?;
    if input.approve {
        events::emit("advance_approved", &json!({ "advance": input.id, "employee": text(&advance, "employee"), "amount": advance["amount"], "currency": advance["currency"] }))?;
    }
    Ok(done)
}

/// Money paid out (finance, or the paying system). Each reference is applied once.
fn issued(input: Money) -> Result<Value> {
    require_finance()?;
    let advance = require("expense_advance", &input.advance, "advance")?;
    if let Some(done) = db::find::<Record>("expense_payment").filter("reference", input.reference.as_str()).first()? {
        return Ok(json!({ "payment": done, "repeated": true }));
    }
    if !matches!(text(&advance, "state"), Some("approved" | "issued" | "partially_settled")) {
        return Err(Error::msg("only an approved advance can be paid out"));
    }
    let currency = text(&advance, "currency").unwrap_or("").to_string();
    let amount = input.amount.with_scale(currency_places(&currency)?)?;
    if amount.is_negative() || amount.is_zero() {
        return Err(Error::msg("a payment is more than zero"));
    }
    let already = decimal_of(&advance, "issued_total")?;
    if already + amount > decimal_of(&advance, "amount")? {
        return Err(Error::msg("that is more than the advance that was approved"));
    }
    let now_issued = already + amount;
    let state = advance_state(now_issued, decimal_of(&advance, "settled_total")?, decimal_of(&advance, "returned_total")?);
    let paid_on = match &input.paid_on {
        Some(day) => format_date(parse_date(day)?),
        None => format_date(today_date()?),
    };
    let results = db::transaction()
        .create(
            "expense_payment",
            &json!({ "kind": "advance_issued", "advance": input.advance, "reference": input.reference, "amount": amount, "currency": currency, "paid_on": paid_on }),
        )
        .update("expense_advance", &input.advance, &json!({ "issued_total": now_issued, "state": state }))
        .run()
        .map_err(|e| e.or("could not record the payment"))?;
    Ok(json!({ "payment": results.first(), "repeated": false }))
}

/// Unused money handed back (finance, or the paying system).
fn returned(input: Money) -> Result<Value> {
    require_finance()?;
    let advance = require("expense_advance", &input.advance, "advance")?;
    if let Some(done) = db::find::<Record>("expense_payment").filter("reference", input.reference.as_str()).first()? {
        return Ok(json!({ "payment": done, "repeated": true }));
    }
    let currency = text(&advance, "currency").unwrap_or("").to_string();
    let amount = input.amount.with_scale(currency_places(&currency)?)?;
    if amount.is_negative() || amount.is_zero() {
        return Err(Error::msg("a return is more than zero"));
    }
    let issued = decimal_of(&advance, "issued_total")?;
    let (settled, back) = (decimal_of(&advance, "settled_total")?, decimal_of(&advance, "returned_total")?);
    let left = advance_unclaimed(issued, back, settled);
    if amount > left {
        return Err(Error::msg(format!("only {left} of the advance is still out")));
    }
    let now_back = back + amount;
    let state = advance_state(issued, settled, now_back);
    let paid_on = match &input.paid_on {
        Some(day) => format_date(parse_date(day)?),
        None => format_date(today_date()?),
    };
    let results = db::transaction()
        .create(
            "expense_payment",
            &json!({ "kind": "advance_returned", "advance": input.advance, "reference": input.reference, "amount": amount, "currency": currency, "paid_on": paid_on }),
        )
        .update("expense_advance", &input.advance, &json!({ "returned_total": now_back, "state": state }))
        .run()
        .map_err(|e| e.or("could not record the return"))?;
    Ok(json!({ "payment": results.first(), "repeated": false }))
}

/// Nightly: advances past their due date that are not used up or returned are announced, once.
fn nightly() -> Result<Value> {
    let today = today_date()?;
    let mut out: Vec<Record> = db::find::<Record>("expense_advance").filter("state", "issued").limit(1000).all()?;
    out.extend(db::find::<Record>("expense_advance").filter("state", "partially_settled").limit(1000).all()?);
    let mut announced = 0u64;
    for advance in out {
        let Some(due) = text(&advance, "due_date").map(parse_date).transpose()? else { continue };
        if due >= today || text(&advance, "overdue_notified_on").is_some() {
            continue;
        }
        let left = advance_unclaimed(decimal_of(&advance, "issued_total")?, decimal_of(&advance, "returned_total")?, decimal_of(&advance, "settled_total")?);
        if left.is_zero() {
            continue;
        }
        db::update::<Record>("expense_advance", id_of(&advance)?, &json!({ "overdue_notified_on": format_date(today) }))?;
        events::emit("advance_overdue", &json!({ "advance": advance["id"], "employee": advance["employee"], "still_out": left, "due_date": advance["due_date"] }))?;
        announced += 1;
    }
    Ok(json!({ "overdue_announced": announced }))
}

handler! {
    fn request_advance(input: Ask) -> Record {
        ask(input)
    }

    fn decide_advance(input: Decide) -> Record {
        decide(input)
    }

    fn record_advance_issued(input: Money) -> Value {
        issued(input)
    }

    fn record_advance_returned(input: Money) -> Value {
        returned(input)
    }

    fn my_advances(_: Empty) -> Vec<Record> {
        let Some(me) = my_employee()? else { return Ok(Vec::new()) };
        db::find("expense_advance").filter("employee", id_of(&me)?).order_by("-due_date").limit(200).all()
    }

    fn expense_nightly(_: Empty) -> Value {
        nightly()
    }
}
