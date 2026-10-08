//! Encashing leave and compensatory leave.
//!
//! **Encashment** turns unused days into pay. A leave administrator names the days and the daily
//! rate (payroll owns salary, leave does not guess it); the type decides whether it is allowed, how
//! many days must stay and how many may go per year. The days leave the ledger as an `encashment`
//! entry and the money goes to `hr_compensation` as an adjustment keyed by the encashment, so a
//! retried call pays once and a payroll run that already closed the period refuses the payment
//! instead of changing it. Cancelling cancels the adjustment first, then returns the days.
//!
//! **Compensatory leave** is earned by working a day off. A claim names the days worked; the system
//! counts only those that really were days off on the person's calendar (Frappe trusts the
//! claimant's count; Odoo has no such flow), refuses overlapping claims and late claims, and needs
//! a decision from someone other than the claimant. Approval grants days that lapse after the
//! type's `comp_valid_days`.

use aether_sdk::dates::{format_date, parse_date, Duration, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{
    calendar_for, can_take_leave, date_of, decimal_of, employee, id_of, is_admin, is_off, my_employee, next_number, require,
    require_admin, text, today_date, Record,
};
use crate::ledger::{balance_on, post, reverse_key, Post};
use crate::rules::{overlap, year_bounds, SCALE};

// ---- encashment -----------------------------------------------------------------------------

#[derive(Deserialize)]
struct Encash {
    employee: String,
    leave_type: String,
    days: Decimal,
    daily_rate: Decimal,
    pay_date: String,
}

fn encashed_in_year(person: &str, kind: &str, year: i32) -> Result<Decimal> {
    let (first, last) = year_bounds(year).ok_or_else(|| Error::msg("that is not a year"))?;
    let rows: Vec<Record> = db::find("leave_encashment")
        .filter("employee", person)
        .filter("leave_type", kind)
        .filter("state", "done")
        .limit(1000)
        .all()?;
    let mut sum = Decimal::zero(SCALE);
    for row in rows {
        let day = date_of(&row, "date")?;
        if day >= first && day <= last {
            sum = sum + decimal_of(&row, "days")?;
        }
    }
    Ok(sum)
}

fn encash(input: Encash) -> Result<Record> {
    require_admin()?;
    let person = employee(&input.employee)?;
    if !can_take_leave(&person) {
        return Err(Error::msg("this person cannot be paid out for leave now"));
    }
    let kind = require("leave_type", &input.leave_type, "leave type")?;
    if is_off(&kind, "is_active") || kind.get("encashable") != Some(&json!(true)) {
        return Err(Error::msg("this leave type cannot be encashed"));
    }
    input.days.with_scale(SCALE).map_err(|_| Error::msg("days have at most 2 digits after the point"))?;
    if input.days.is_zero() || input.days.is_negative() {
        return Err(Error::msg("encash a positive number of days"));
    }
    if input.daily_rate.is_zero() || input.daily_rate.is_negative() {
        return Err(Error::msg("the daily rate must be above zero"));
    }
    parse_date(&input.pay_date)?;
    let today = today_date()?;
    let available = balance_on(&input.employee, &input.leave_type, today)?.available;
    let keep = decimal_of(&kind, "encash_keep_days")?;
    if available - input.days < keep {
        return Err(Error::msg(format!("{} must stay after encashing and only {available} is available", keep)));
    }
    if kind.get("encash_max_year").is_some_and(|v| !v.is_null()) {
        let max = decimal_of(&kind, "encash_max_year")?;
        let done = encashed_in_year(&input.employee, &input.leave_type, aether_sdk::dates::Datelike::year(&today))?;
        if done + input.days > max {
            return Err(Error::msg(format!("at most {max} days a year can be encashed and {done} already were")));
        }
    }
    let amount = input.days.times(input.daily_rate, 2)?;
    let who = context::current()?.actor.id.unwrap_or_default();
    let number = next_number("encashment", "ENC-", 6)?;
    let key = format!("encash:{number}");
    let entry = post(Post {
        employee: &input.employee,
        leave_type: &input.leave_type,
        kind: "encashment",
        days: -input.days,
        date: today,
        valid_to: None,
        key: Some(key.clone()),
        reverses: None,
        source: "encashment",
        note: Some(&number),
    })?;
    // The money. If it fails, the days go straight back.
    let pay: Result<Record> = (|| {
        let _: Value = plugins::call("hr_compensation", "ensure_pay_code", &json!({ "code": "leave_encashment", "kind": "earning" }))?;
        plugins::call(
            "hr_compensation",
            "add_adjustment",
            &json!({
                "employee": input.employee, "code": "leave_encashment", "amount": amount, "mode": "one_off",
                "pay_date": input.pay_date, "reason": format!("Encashment {number}"),
                "source_plugin": "hr_leave", "source_ref": number,
            }),
        )
    })();
    let adjustment = match pay {
        Ok(adjustment) => adjustment,
        Err(error) => {
            reverse_key(&input.employee, &input.leave_type, &key, "payment refused")?;
            return Err(error);
        }
    };
    let _ = entry;
    db::create(
        "leave_encashment",
        &json!({
            "employee": input.employee, "leave_type": input.leave_type, "days": input.days, "daily_rate": input.daily_rate,
            "amount": amount, "date": format_date(today), "pay_date": input.pay_date, "state": "done", "ledger_key": key,
            "adjustment": adjustment["id"], "decided_by": who,
        }),
    )
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

fn cancel_encashment(input: Id) -> Result<Record> {
    require_admin()?;
    let row = require("leave_encashment", &input.id, "encashment")?;
    if text(&row, "state") != Some("done") {
        return Err(Error::msg("this encashment is already cancelled"));
    }
    if let Some(adjustment) = text(&row, "adjustment") {
        // Payroll refuses this once the period is closed: then the payment stands and so do the days.
        let _: Value = plugins::call("hr_compensation", "cancel_adjustment", &json!({ "id": adjustment }))?;
    }
    reverse_key(
        text(&row, "employee").unwrap_or_default(),
        text(&row, "leave_type").unwrap_or_default(),
        text(&row, "ledger_key").unwrap_or_default(),
        "encashment cancelled",
    )?;
    db::update::<Record>("leave_encashment", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the encashment is gone"))
}

// ---- compensatory leave ---------------------------------------------------------------------

#[derive(Deserialize)]
struct Claim {
    #[serde(default)]
    employee: Option<String>,
    leave_type: String,
    work_from: String,
    #[serde(default)]
    work_to: Option<String>,
    #[serde(default)]
    half_day: bool,
    #[serde(default)]
    reason: Option<String>,
}

/// The days off inside a span that the person's calendar says are not working days.
pub fn days_off(calendar: &aether_sdk::dates::Calendar, from: NaiveDate, to: NaiveDate) -> u32 {
    let mut count = 0;
    let mut day = from;
    while day <= to {
        if !calendar.is_working_day(day) {
            count += 1;
        }
        day += Duration::days(1);
    }
    count
}

fn claim(input: Claim) -> Result<Record> {
    let me = my_employee()?;
    let admin = is_admin()?;
    let person_id = match (&input.employee, &me) {
        (Some(other), Some(me)) if other != id_of(me)? => {
            require_admin()?;
            other.clone()
        }
        (Some(other), None) => {
            require_admin()?;
            other.clone()
        }
        (_, Some(me)) => id_of(me)?.to_string(),
        (None, None) => return Err(Error::msg("name the employee: you have no employee record")),
    };
    let _ = admin;
    let person = employee(&person_id)?;
    if !can_take_leave(&person) {
        return Err(Error::msg("this person cannot claim leave now"));
    }
    let kind = require("leave_type", &input.leave_type, "leave type")?;
    if is_off(&kind, "is_active") || kind.get("is_compensatory") != Some(&json!(true)) {
        return Err(Error::msg("this leave type is not compensatory leave"));
    }
    let from = parse_date(&input.work_from)?;
    let to = input.work_to.as_deref().map(parse_date).transpose()?.unwrap_or(from);
    aether_sdk::dates::check_order(from, Some(to), "claim")?;
    if input.half_day && from != to {
        return Err(Error::msg("half a day can only be claimed for a single day"));
    }
    let today = today_date()?;
    if to > today {
        return Err(Error::msg("the work has not happened yet"));
    }
    if let Some(window) = kind.get("comp_claim_window").and_then(Value::as_i64) {
        if (today - to).num_days() > window {
            return Err(Error::msg(format!("a claim must be made within {window} days of the work")));
        }
    }
    let calendar = calendar_for(&person, from, to)?;
    let off = days_off(&calendar, from, to);
    if off == 0 {
        return Err(Error::msg("there is no day off in that span: only work on a day off earns compensatory leave"));
    }
    let existing: Vec<Record> = db::find("leave_comp_claim").filter("employee", person_id.as_str()).limit(500).all()?;
    for other in existing {
        if matches!(text(&other, "state"), Some("submitted" | "approved")) && overlap((from, to), (date_of(&other, "work_from")?, date_of(&other, "work_to")?)) {
            return Err(Error::msg("another claim already covers some of these days"));
        }
    }
    let days = if input.half_day { Decimal::parse("0.5")?.with_scale(SCALE)? } else { Decimal::whole(i64::from(off), SCALE)? };
    let mut data = json!({
        "employee": person_id, "leave_type": input.leave_type, "work_from": format_date(from), "work_to": format_date(to),
        "half_day": input.half_day, "days": days, "state": "submitted",
    });
    if let Some(reason) = input.reason {
        data["reason"] = json!(reason);
    }
    db::create("leave_comp_claim", &data)
}

#[derive(Deserialize)]
struct Decide {
    id: String,
    #[serde(default)]
    note: Option<String>,
}

fn decide(input: Decide, approve: bool) -> Result<Record> {
    let row = require("leave_comp_claim", &input.id, "claim")?;
    if text(&row, "state") != Some("submitted") {
        return Err(Error::msg("this claim is not waiting for a decision"));
    }
    let me = my_employee()?;
    let claimant = text(&row, "employee").unwrap_or_default();
    if me.as_ref().is_some_and(|m| text(m, "id") == Some(claimant)) {
        return Err(Error::msg("nobody decides their own claim"));
    }
    let manager = employee(claimant)?.get("manager").and_then(Value::as_str).map(str::to_string);
    let is_manager = match (&manager, &me) {
        (Some(manager), Some(me)) => text(me, "id") == Some(manager.as_str()),
        _ => false,
    };
    if !is_manager && !is_admin()? {
        return Err(Error::msg("only the person's manager or a leave administrator can decide this claim"));
    }
    let who = context::current()?.actor.id.unwrap_or_default();
    if !approve {
        return db::update::<Record>("leave_comp_claim", &input.id, &json!({ "state": "rejected", "decided_by": who, "note": input.note }))?
            .ok_or_else(|| Error::msg("the claim is gone"));
    }
    let kind = require("leave_type", text(&row, "leave_type").unwrap_or_default(), "leave type")?;
    let today = today_date()?;
    let valid_to = kind.get("comp_valid_days").and_then(Value::as_i64).map(|n| today + Duration::days(n));
    post(Post {
        employee: claimant,
        leave_type: text(&row, "leave_type").unwrap_or_default(),
        kind: "allocation",
        days: decimal_of(&row, "days")?,
        date: today,
        valid_to,
        key: Some(format!("comp:{}", input.id)),
        reverses: None,
        source: "compensatory",
        note: input.note.as_deref(),
    })?;
    db::update("leave_comp_claim", &input.id, &json!({ "state": "approved", "decided_by": who, "note": input.note }))?.ok_or_else(|| Error::msg("the claim is gone"))
}

fn cancel_claim(input: Id) -> Result<Record> {
    let row = require("leave_comp_claim", &input.id, "claim")?;
    let me = my_employee()?;
    let mine = me.as_ref().is_some_and(|m| text(m, "id") == text(&row, "employee"));
    if !mine && !is_admin()? {
        return Err(Error::msg("only the claimant or a leave administrator can cancel this claim"));
    }
    match text(&row, "state") {
        Some("submitted") => {}
        Some("approved") => {
            // Only while the days are untouched: otherwise leave already taken would be left without cover.
            let person = text(&row, "employee").unwrap_or_default();
            let kind = text(&row, "leave_type").unwrap_or_default();
            let available = balance_on(person, kind, today_date()?)?.available;
            if available < decimal_of(&row, "days")? {
                return Err(Error::msg("some of these days were already used: a leave administrator must adjust the balance"));
            }
            reverse_key(person, kind, &format!("comp:{}", input.id), "claim cancelled")?;
        }
        _ => return Err(Error::msg("only a waiting or approved claim can be cancelled")),
    }
    db::update::<Record>("leave_comp_claim", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the claim is gone"))
}

#[derive(Deserialize, Default)]
struct Mine {
    #[serde(default)]
    employee: Option<String>,
}

handler! {
    /// Pay out unused days (leave administrators): days leave the ledger, the money goes to compensation.
    fn encash_leave(input: Encash) -> Record {
        encash(input)
    }

    /// Cancel an encashment while its pay period is still open.
    fn cancel_leave_encashment(input: Id) -> Record {
        cancel_encashment(input)
    }

    fn list_leave_encashments(input: Mine) -> Vec<Record> {
        let mut query = db::find::<Record>("leave_encashment").order_by("-date").limit(500);
        if let Some(person) = input.employee.as_deref() {
            query = query.filter("employee", person);
        }
        query.all()
    }

    /// Claim compensatory leave for work on a day off.
    fn claim_compensatory_leave(input: Claim) -> Record {
        claim(input)
    }

    fn approve_compensatory_leave(input: Decide) -> Record {
        decide(input, true)
    }

    fn reject_compensatory_leave(input: Decide) -> Record {
        decide(input, false)
    }

    fn cancel_compensatory_claim(input: Id) -> Record {
        cancel_claim(input)
    }

    fn list_compensatory_claims(input: Mine) -> Vec<Record> {
        let mut query = db::find::<Record>("leave_comp_claim").order_by("-work_from").limit(500);
        if let Some(person) = input.employee.as_deref() {
            query = query.filter("employee", person);
        }
        query.all()
    }
}

#[cfg(test)]
mod tests {
    use aether_sdk::dates::{parse_date, Calendar};

    use super::*;

    #[test]
    fn only_days_off_count_toward_a_claim() {
        let mut calendar = Calendar::default();
        calendar.holidays.push(parse_date("2026-10-07").unwrap_or_default());
        // Mon 5 Oct to Sun 11 Oct 2026: Wed 7 is a holiday, Sat 10 and Sun 11 the weekend.
        let from = parse_date("2026-10-05").unwrap_or_default();
        let to = parse_date("2026-10-11").unwrap_or_default();
        assert_eq!(days_off(&calendar, from, to), 3);
        assert_eq!(days_off(&calendar, from, from), 0);
    }
}
