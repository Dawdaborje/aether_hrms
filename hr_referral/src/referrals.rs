//! Submitting, accepting, hiring, releasing, paying and forfeiting a referral.
//!
//! * Anyone working here refers a candidate (never themselves; the email is unique). The policy is not copied yet:
//!   it is copied when a referral administrator accepts the referral, so the bonus is the policy of that day.
//! * Accepting opens one application in hr_recruitment (`source: referral`). Nightly, a hired application moves the
//!   referral to hired (and a rejected one to rejected). The bonus is due after `wait_days` if the hire is still
//!   working; leaving forfeits it. Payment is an hr_compensation adjustment keyed by the referral, once.

use aether_sdk::dates::{format_date, parse_date};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{
    currency_places, decimal_of, employee, id_of, is_admin, my_employee, next_number, require, require_admin, text, today_date, Record,
};
use crate::rules::{bonus_ok, bonus_ready, due_on, next, not_self, Action, State};

#[derive(Deserialize)]
struct PolicyInput {
    code: String,
    name: String,
    currency: String,
    amount: Decimal,
    wait_days: i64,
    pay_code: String,
}

#[derive(Deserialize)]
struct Submit {
    candidate_name: String,
    candidate_email: String,
    #[serde(default)]
    candidate_phone: Option<String>,
    #[serde(default)]
    opening: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct Accept {
    id: String,
    policy: String,
    opening: String,
}

#[derive(Deserialize)]
struct Reason {
    id: String,
    reason: String,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct HireIn {
    id: String,
    hired_employee: String,
    hired_on: String,
}

fn me_id() -> Result<Option<String>> {
    Ok(my_employee()?.and_then(|m| text(&m, "id").map(str::to_string)))
}

fn state_of(row: &Record) -> Result<State> {
    State::parse(text(row, "state").unwrap_or_default()).ok_or_else(|| Error::msg("the referral has an unknown state"))
}

fn change(row: &Record, data: Value) -> Result<Record> {
    db::update("ref_referral", id_of(row)?, &data)?.ok_or_else(|| Error::msg("the referral is gone"))
}

fn policy(input: PolicyInput) -> Result<Record> {
    require_admin()?;
    let currency = input.currency.to_uppercase();
    let places = currency_places(&currency)?;
    let amount = input.amount.with_scale(places)?;
    bonus_ok(amount)?;
    if input.wait_days < 0 {
        return Err(Error::msg("the wait is zero days or more"));
    }
    let _: Record = plugins::call("hr_compensation", "ensure_pay_code", &json!({ "code": input.pay_code, "kind": "earning" }))?;
    db::create(
        "ref_policy",
        &json!({
            "code": input.code, "name": input.name, "currency": currency, "amount": amount, "wait_days": input.wait_days,
            "pay_code": input.pay_code, "is_active": true,
        }),
    )
    .map_err(|e| e.or("could not create the policy (the code may be taken)"))
}

fn submit(input: Submit) -> Result<Record> {
    let me = my_employee()?.ok_or_else(|| Error::msg("you are not an employee"))?;
    if !matches!(text(&me, "status"), Some("active" | "probation") | None) {
        return Err(Error::msg("only someone who is working can refer a candidate"));
    }
    let email = input.candidate_email.trim().to_lowercase();
    not_self(id_of(&me)?, text(&me, "work_email"), &email)?;
    if input.candidate_name.trim().is_empty() {
        return Err(Error::msg("name the candidate"));
    }
    db::create(
        "ref_referral",
        &json!({
            "reference": next_number("referral", "REF-", 5)?, "referrer": id_of(&me)?, "candidate_name": input.candidate_name,
            "candidate_email": email, "candidate_phone": input.candidate_phone, "opening": input.opening, "state": "submitted",
            "note": input.note,
        }),
    )
    .map_err(|e| e.or("this email has already been referred"))
}

fn accept(input: Accept) -> Result<Record> {
    require_admin()?;
    let row = require("ref_referral", &input.id, "referral")?;
    let to = next(state_of(&row)?, Action::Accept).ok_or_else(|| Error::msg("only a new referral is accepted"))?;
    let policy = require("ref_policy", &input.policy, "policy")?;
    if policy.get("is_active") == Some(&json!(false)) {
        return Err(Error::msg("this referral policy is no longer used"));
    }
    let places = currency_places(text(&policy, "currency").unwrap_or_default())?;
    let amount = decimal_of(&policy, "amount")?.unwrap_or(Decimal::zero(places)).with_scale(places)?;
    let wait = policy.get("wait_days").and_then(Value::as_i64).unwrap_or(0);
    // Put the candidate in the directory first (by email). Recruitment's resolve path searches the directory and
    // that search is broken on some hosts; create_person is idempotent enough for a new email, and get-by-id works.
    let candidate: Record = plugins::call(
        "party",
        "create_person",
        &json!({ "name": row["candidate_name"], "email": row["candidate_email"], "phone": row.get("candidate_phone") }),
    )?;
    let application: Record = plugins::call(
        "hr_recruitment",
        "apply_to_opening",
        &json!({
            "opening": input.opening,
            "candidate": candidate["id"],
            "source": "referral",
            "referrer": row["referrer"],
            "notes": format!("Referral {}", text(&row, "reference").unwrap_or("?")),
        }),
    )?;
    change(
        &row,
        json!({
            "state": to.name(), "policy": input.policy, "opening": input.opening, "application": application["id"],
            "bonus_amount": amount, "wait_days": wait, "currency": policy["currency"], "pay_code": policy["pay_code"],
        }),
    )
}

fn withdraw(input: Reason) -> Result<Record> {
    let row = require("ref_referral", &input.id, "referral")?;
    if me_id()?.as_deref() != text(&row, "referrer") && !is_admin()? {
        return Err(Error::msg("only the referrer (or a referral administrator) can withdraw"));
    }
    let to = next(state_of(&row)?, Action::Withdraw).ok_or_else(|| Error::msg("a referral can be withdrawn until recruitment takes it"))?;
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why"));
    }
    change(&row, json!({ "state": to.name(), "note": input.reason }))
}

fn reject(input: Reason) -> Result<Record> {
    require_admin()?;
    let row = require("ref_referral", &input.id, "referral")?;
    let to = next(state_of(&row)?, Action::Reject).ok_or_else(|| Error::msg("this referral is no longer open to refuse"))?;
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why the referral is refused"));
    }
    change(&row, json!({ "state": to.name(), "note": input.reason }))
}

fn still_working(employee_id: &str) -> Result<bool> {
    let person = employee(employee_id)?;
    Ok(matches!(text(&person, "status"), Some("active" | "probation") | None))
}

/// Record that the referred person was hired (when recruitment already did, or the nightly catch-up cannot see it).
fn record_hire(input: HireIn) -> Result<Record> {
    require_admin()?;
    let row = require("ref_referral", &input.id, "referral")?;
    let to = next(state_of(&row)?, Action::Hire).ok_or_else(|| Error::msg("only a referral in process is marked hired"))?;
    let hired_on = parse_date(&input.hired_on)?;
    let wait = row.get("wait_days").and_then(Value::as_i64).unwrap_or(0);
    employee(&input.hired_employee)?;
    let out = change(
        &row,
        json!({
            "state": to.name(), "hired_on": format_date(hired_on), "hired_employee": input.hired_employee,
            "bonus_due_on": format_date(due_on(hired_on, wait)),
        }),
    )?;
    events::emit("referral_hired", &json!({ "referral": out["id"], "employee": input.hired_employee }))?;
    Ok(out)
}

fn release(input: Id) -> Result<Record> {
    require_admin()?;
    let row = require("ref_referral", &input.id, "referral")?;
    let to = next(state_of(&row)?, Action::ReleaseBonus).ok_or_else(|| Error::msg("the bonus is released after the hire has stayed"))?;
    let hired = parse_date(text(&row, "hired_on").unwrap_or_default())?;
    let wait = row.get("wait_days").and_then(Value::as_i64).unwrap_or(0);
    let hire = text(&row, "hired_employee").unwrap_or_default();
    bonus_ready(today_date()?, hired, wait, still_working(hire)?)?;
    let out = change(&row, json!({ "state": to.name() }))?;
    events::emit("referral_bonus_due", &json!({ "referral": out["id"], "referrer": out["referrer"], "amount": out["bonus_amount"] }))?;
    Ok(out)
}

fn pay(input: Id) -> Result<Record> {
    require_admin()?;
    let row = require("ref_referral", &input.id, "referral")?;
    let to = next(state_of(&row)?, Action::Pay).ok_or_else(|| Error::msg("the bonus is paid once it is due"))?;
    let referrer = text(&row, "referrer").unwrap_or_default();
    if referrer == text(&row, "hired_employee").unwrap_or_default() {
        return Err(Error::msg("the bonus is never paid to the referrer for themselves"));
    }
    let adj: Record = plugins::call(
        "hr_compensation",
        "add_adjustment",
        &json!({
            "employee": referrer, "code": row["pay_code"], "amount": row["bonus_amount"], "mode": "one_off",
            "pay_date": format_date(today_date()?), "source_plugin": "hr_referral", "source_ref": input.id,
            "reason": format!("Referral bonus {}", text(&row, "reference").unwrap_or("?")),
        }),
    )?;
    change(&row, json!({ "state": to.name(), "adjustment": adj.get("id") }))
}

fn forfeit(input: Reason) -> Result<Record> {
    require_admin()?;
    let row = require("ref_referral", &input.id, "referral")?;
    let to = next(state_of(&row)?, Action::Forfeit).ok_or_else(|| Error::msg("only a hire whose bonus is not yet paid is forfeited"))?;
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why the bonus is forfeited"));
    }
    change(&row, json!({ "state": to.name(), "note": input.reason }))
}

fn get(input: Id) -> Result<Record> {
    let row = require("ref_referral", &input.id, "referral")?;
    let me = me_id()?;
    if me.as_deref() != text(&row, "referrer") && !is_admin()? {
        return Err(Error::msg("you can see your own referrals, or all if you run referrals"));
    }
    Ok(row)
}

/// Catch up from recruitment: a hired application moves the referral to hired; a rejected one is refused here too.
fn catch_up(row: &Record) -> Result<Option<Record>> {
    let Some(app_id) = text(row, "application") else { return Ok(None) };
    let application: Option<Record> = plugins::call("hr_recruitment", "get_application", &json!({ "id": app_id }))?;
    let Some(application) = application else { return Ok(None) };
    match (state_of(row)?, text(&application, "status")) {
        (State::InProcess, Some("hired")) => {
            let hired_on = text(&application, "hired_on").unwrap_or(&today_date().map(format_date)?).to_string();
            let hired_date = parse_date(&hired_on)?;
            let wait = row.get("wait_days").and_then(Value::as_i64).unwrap_or(0);
            let employee: Option<Record> = plugins::call("hr", "employee_of_party", &json!({ "id": application["candidate"] }))?;
            let hired_employee = employee.as_ref().and_then(|e| text(e, "id")).map(str::to_string);
            let out = change(
                row,
                json!({
                    "state": State::Hired.name(), "hired_on": hired_on, "hired_employee": hired_employee,
                    "bonus_due_on": format_date(due_on(hired_date, wait)),
                }),
            )?;
            events::emit("referral_hired", &json!({ "referral": out["id"], "employee": hired_employee }))?;
            Ok(Some(out))
        }
        (State::InProcess, Some("rejected" | "withdrawn")) => Ok(Some(change(row, json!({ "state": State::Rejected.name() }))?)),
        _ => Ok(None),
    }
}

fn nightly() -> Result<Value> {
    let today = today_date()?;
    let rows: Vec<Record> = db::find::<Record>("ref_referral").limit(10_000).all()?;
    let (mut caught, mut released, mut forfeited) = (0, 0, 0);
    for row in rows {
        if let Some(_) = catch_up(&row)? {
            caught += 1;
            continue;
        }
        if state_of(&row)? != State::Hired {
            continue;
        }
        let Some(hired) = text(&row, "hired_on").map(parse_date).transpose()? else { continue };
        let wait = row.get("wait_days").and_then(Value::as_i64).unwrap_or(0);
        let hire = text(&row, "hired_employee").unwrap_or_default();
        if hire.is_empty() {
            continue;
        }
        if !still_working(hire)? {
            change(&row, json!({ "state": State::Forfeited.name(), "note": "the hire has left" }))?;
            forfeited += 1;
            continue;
        }
        if bonus_ready(today, hired, wait, true).is_ok() {
            change(&row, json!({ "state": State::BonusDue.name() }))?;
            events::emit("referral_bonus_due", &json!({ "referral": row["id"], "referrer": row["referrer"], "amount": row["bonus_amount"] }))?;
            released += 1;
        }
    }
    Ok(json!({ "caught": caught, "released": released, "forfeited": forfeited }))
}

handler! {
    fn create_referral_policy(input: PolicyInput) -> Record {
        policy(input)
    }

    fn list_referral_policies(_: Empty) -> Vec<Record> {
        db::find("ref_policy").limit(200).all()
    }

    /// Refer a candidate (never yourself; the email is unique).
    fn submit_referral(input: Submit) -> Record {
        submit(input)
    }

    /// Accept: copy the policy, open the application in recruitment.
    fn accept_referral(input: Accept) -> Record {
        accept(input)
    }

    fn withdraw_referral(input: Reason) -> Record {
        withdraw(input)
    }

    fn reject_referral(input: Reason) -> Record {
        reject(input)
    }

    /// Mark the referral hired (hire date and employee). The nightly job does this from the application when it can.
    fn record_referral_hire(input: HireIn) -> Record {
        record_hire(input)
    }

    /// Mark the bonus due (hire has stayed, still working). The nightly job does this too.
    fn release_referral_bonus(input: Id) -> Record {
        release(input)
    }

    fn pay_referral_bonus(input: Id) -> Record {
        pay(input)
    }

    fn forfeit_referral_bonus(input: Reason) -> Record {
        forfeit(input)
    }

    fn get_referral(input: Id) -> Record {
        get(input)
    }

    fn my_referrals(_: Empty) -> Vec<Record> {
        let Some(me) = me_id()? else { return Ok(Vec::new()) };
        db::find("ref_referral").filter("referrer", me.as_str()).limit(500).all()
    }

    fn list_referrals(_: Empty) -> Vec<Record> {
        require_admin()?;
        db::find("ref_referral").limit(2000).all()
    }

    fn referral_nightly(_: Empty) -> Value {
        nightly()
    }
}
