//! Policies, trips, legs, costs, approval, advance and report.
//!
//! * A trip is edited only in draft, only by the traveller (or a travel administrator for someone else).
//! * Submitting freezes the figures: the allowance, the legs and costs, lodging against the cap, the estimate.
//!   A person cannot have two live trips on the same day. Lodging over the cap needs a reason.
//! * Approval: the traveller's manager (or a travel administrator) decides, never the traveller. When the
//!   estimate passes the policy's limit a second decision follows, by a travel administrator who is neither the
//!   traveller nor the first approver.
//! * An approved trip may ask for an advance (a share of the estimate, once) and open its expense report
//!   (once, from the first day). Both are made in hr_expense in the traveller's name.
//! * Money is in the policy's currency throughout; nothing is converted here.

use aether_sdk::dates::{format_date, parse_date, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{
    approver_of, currency_places, decimal_of, employee, id_of, is_admin, my_employee, next_number, require, require_admin, text, today_date,
    Record,
};
use crate::rules::{advance_cap, days, legs_fit, lodging_excess, needs_second, nights, overlaps, per_diem, settle_by, Leg};

/// Days after the end of an approved trip before it is reported as unsettled.
const GRACE_DAYS: i64 = 14;
/// States in which a trip holds its dates.
const LIVE: [&str; 4] = ["submitted", "awaiting_second", "approved", "completed"];

#[derive(Deserialize)]
struct PolicyInput {
    code: String,
    name: String,
    destination_class: String,
    currency: String,
    per_diem: Decimal,
    #[serde(default)]
    meal_deduction_percent: Option<i64>,
    #[serde(default)]
    lodging_cap_per_night: Option<Decimal>,
    #[serde(default)]
    approval_limit: Option<Decimal>,
    #[serde(default)]
    advance_percent: Option<i64>,
}

#[derive(Deserialize)]
struct NewTrip {
    purpose: String,
    destination: String,
    policy: String,
    start_date: String,
    end_date: String,
    #[serde(default)]
    funding: Option<String>,
    #[serde(default)]
    sponsor_details: Option<String>,
    #[serde(default)]
    meals_provided_days: Option<i64>,
    #[serde(default)]
    employee: Option<String>,
}

#[derive(Deserialize)]
struct NewLeg {
    trip: String,
    mode: String,
    from_place: String,
    to_place: String,
    depart_on: String,
    arrive_on: String,
    #[serde(default)]
    cost: Option<Decimal>,
    #[serde(default)]
    booking_ref: Option<String>,
}

#[derive(Deserialize)]
struct NewCost {
    trip: String,
    kind: String,
    #[serde(default)]
    description: Option<String>,
    amount: Decimal,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Submit {
    id: String,
    #[serde(default)]
    lodging_reason: Option<String>,
}

#[derive(Deserialize)]
struct Decide {
    id: String,
    approve: bool,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct Cancel {
    id: String,
    reason: String,
}

#[derive(Deserialize)]
struct Complete {
    id: String,
    #[serde(default)]
    without_expenses: bool,
}

#[derive(Deserialize, Default)]
struct Search {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    employee: Option<String>,
}

fn me_id() -> Result<Option<String>> {
    Ok(my_employee()?.and_then(|m| text(&m, "id").map(str::to_string)))
}

fn date_of(record: &Record, field: &str) -> Result<NaiveDate> {
    parse_date(text(record, field).unwrap_or_default())
}

fn money(record: &Record, field: &str, places: u32) -> Result<Decimal> {
    Ok(decimal_of(record, field)?.unwrap_or(Decimal::zero(places)).with_scale(places)?)
}

fn places_of(trip: &Record) -> Result<u32> {
    currency_places(text(trip, "currency").unwrap_or_default())
}

fn change(trip: &Record, data: Value) -> Result<Record> {
    db::update("trv_trip", id_of(trip)?, &data)?.ok_or_else(|| Error::msg("the trip is gone"))
}

fn trip_of(id: &str) -> Result<Record> {
    require("trv_trip", id, "trip")
}

fn is_traveller(trip: &Record) -> Result<bool> {
    Ok(me_id()?.is_some() && me_id()?.as_deref() == text(trip, "employee"))
}

fn require_draft_editor(trip: &Record) -> Result<()> {
    if text(trip, "state") != Some("draft") {
        return Err(Error::msg("a trip can only be changed while it is a draft"));
    }
    if !is_traveller(trip)? && !is_admin()? {
        return Err(Error::msg("only the traveller (or a travel administrator) can change this trip"));
    }
    Ok(())
}

fn policy(input: PolicyInput) -> Result<Record> {
    require_admin()?;
    if !matches!(input.destination_class.as_str(), "domestic" | "regional" | "international") {
        return Err(Error::msg("destination class is domestic, regional or international"));
    }
    let currency = input.currency.to_uppercase();
    let places = currency_places(&currency)?;
    if input.per_diem.is_negative() {
        return Err(Error::msg("a daily allowance cannot be negative"));
    }
    let scale = |value: Option<Decimal>| -> Result<Option<Decimal>> { value.map(|v| v.with_scale(places)).transpose() };
    let percent = |value: Option<i64>, default: i64| -> Result<i64> {
        let v = value.unwrap_or(default);
        if (0..=100).contains(&v) { Ok(v) } else { Err(Error::msg("a share is a percentage between 0 and 100")) }
    };
    db::create(
        "trv_policy",
        &json!({
            "code": input.code, "name": input.name, "destination_class": input.destination_class, "currency": currency,
            "per_diem": input.per_diem.with_scale(places)?, "meal_deduction_percent": percent(input.meal_deduction_percent, 25)?,
            "lodging_cap_per_night": scale(input.lodging_cap_per_night)?, "approval_limit": scale(input.approval_limit)?,
            "advance_percent": percent(input.advance_percent, 70)?, "is_active": true,
        }),
    )
    .map_err(|e| e.or("could not create the policy (the code may be taken)"))
}

fn create(input: NewTrip) -> Result<Record> {
    let me = me_id()?;
    let traveller = match input.employee {
        Some(other) if Some(&other) != me.as_ref() => {
            require_admin()?;
            other
        }
        Some(own) => own,
        None => me.ok_or_else(|| Error::msg("you have no employee record: name the traveller"))?,
    };
    let person = employee(&traveller)?;
    if !matches!(text(&person, "status"), Some("active" | "probation") | None) {
        return Err(Error::msg("only someone who is working can travel"));
    }
    if input.purpose.trim().is_empty() || input.destination.trim().is_empty() {
        return Err(Error::msg("say where the trip goes and why"));
    }
    let (start, end) = (parse_date(&input.start_date)?, parse_date(&input.end_date)?);
    if end < start {
        return Err(Error::msg("the trip ends before it starts"));
    }
    if start < today_date()? {
        return Err(Error::msg("the trip starts in the past"));
    }
    let funding = input.funding.unwrap_or_else(|| "company".into());
    if !matches!(funding.as_str(), "company" | "sponsor" | "self") {
        return Err(Error::msg("funding is company, sponsor or self"));
    }
    if funding == "sponsor" && input.sponsor_details.as_deref().is_none_or(|s| s.trim().is_empty()) {
        return Err(Error::msg("say who sponsors the trip"));
    }
    let chosen = require("trv_policy", &input.policy, "policy")?;
    if chosen.get("is_active") == Some(&json!(false)) {
        return Err(Error::msg("this travel policy is no longer used"));
    }
    let currency = text(&chosen, "currency").unwrap_or_default().to_string();
    let places = currency_places(&currency)?;
    let meal_days = input.meals_provided_days.unwrap_or(0);
    let total_days = days(start, end);
    let rate = money(&chosen, "per_diem", places)?;
    let percent = chosen.get("meal_deduction_percent").and_then(Value::as_i64).unwrap_or(25);
    // A trip paid by a sponsor or by the traveller carries no company allowance.
    let allowance = if funding == "company" { per_diem(rate, total_days, meal_days, percent)? } else { Decimal::zero(places) };
    db::create(
        "trv_trip",
        &json!({
            "reference": next_number("trip", "TRV-", 5)?, "employee": traveller, "purpose": input.purpose, "destination": input.destination,
            "policy": input.policy, "funding": funding, "sponsor_details": input.sponsor_details, "start_date": format_date(start),
            "end_date": format_date(end), "meals_provided_days": meal_days, "state": "draft", "currency": currency,
            "per_diem_rate": rate, "meal_deduction_percent": percent, "lodging_cap_per_night": chosen.get("lodging_cap_per_night").filter(|v| !v.is_null()),
            "approval_limit": chosen.get("approval_limit").filter(|v| !v.is_null()), "advance_percent": chosen.get("advance_percent"),
            "days": total_days, "nights": nights(start, end), "per_diem_total": allowance, "estimate_total": allowance,
        }),
    )
}

fn add_leg(input: NewLeg) -> Result<Record> {
    let trip = trip_of(&input.trip)?;
    require_draft_editor(&trip)?;
    if !matches!(input.mode.as_str(), "flight" | "train" | "bus" | "car" | "ferry" | "other") {
        return Err(Error::msg("mode is flight, train, bus, car, ferry or other"));
    }
    let (depart, arrive) = (parse_date(&input.depart_on)?, parse_date(&input.arrive_on)?);
    let places = places_of(&trip)?;
    let cost = input.cost.unwrap_or(Decimal::zero(places)).with_scale(places)?;
    if cost.is_negative() {
        return Err(Error::msg("a cost cannot be negative"));
    }
    legs_fit(&[Leg { depart, arrive }], date_of(&trip, "start_date")?, date_of(&trip, "end_date")?)?;
    let seq = db::find::<Record>("trv_leg").filter("trip", input.trip.as_str()).limit(500).all()?.len() as i64 + 1;
    db::create(
        "trv_leg",
        &json!({
            "trip": input.trip, "employee": trip["employee"], "seq": seq, "mode": input.mode, "from_place": input.from_place,
            "to_place": input.to_place, "depart_on": format_date(depart), "arrive_on": format_date(arrive), "cost": cost, "booking_ref": input.booking_ref,
        }),
    )
}

fn remove_leg(input: Id) -> Result<Value> {
    let leg = require("trv_leg", &input.id, "leg")?;
    let trip = trip_of(text(&leg, "trip").unwrap_or_default())?;
    require_draft_editor(&trip)?;
    db::delete::<Record>("trv_leg", &input.id)?;
    Ok(json!({ "removed": input.id }))
}

fn add_cost(input: NewCost) -> Result<Record> {
    let trip = trip_of(&input.trip)?;
    require_draft_editor(&trip)?;
    if !matches!(input.kind.as_str(), "lodging" | "registration" | "visa" | "insurance" | "other") {
        return Err(Error::msg("kind is lodging, registration, visa, insurance or other"));
    }
    let amount = input.amount.with_scale(places_of(&trip)?)?;
    if amount.is_negative() || amount.is_zero() {
        return Err(Error::msg("a cost is more than zero"));
    }
    db::create("trv_cost", &json!({ "trip": input.trip, "employee": trip["employee"], "kind": input.kind, "description": input.description, "amount": amount }))
}

fn remove_cost(input: Id) -> Result<Value> {
    let cost = require("trv_cost", &input.id, "cost")?;
    let trip = trip_of(text(&cost, "trip").unwrap_or_default())?;
    require_draft_editor(&trip)?;
    db::delete::<Record>("trv_cost", &input.id)?;
    Ok(json!({ "removed": input.id }))
}

fn legs_of(trip: &str) -> Result<Vec<Record>> {
    let mut legs: Vec<Record> = db::find("trv_leg").filter("trip", trip).limit(500).all()?;
    legs.sort_by_key(|l| l.get("seq").and_then(Value::as_i64).unwrap_or(0));
    Ok(legs)
}

fn costs_of(trip: &str) -> Result<Vec<Record>> {
    db::find("trv_cost").filter("trip", trip).limit(500).all()
}

fn submit(input: Submit) -> Result<Record> {
    let trip = trip_of(&input.id)?;
    require_draft_editor(&trip)?;
    let places = places_of(&trip)?;
    let (start, end) = (date_of(&trip, "start_date")?, date_of(&trip, "end_date")?);
    if start < today_date()? {
        return Err(Error::msg("the trip has already started: ask a travel administrator"));
    }
    let traveller = text(&trip, "employee").unwrap_or_default();

    // No two live trips on one day.
    let others: Vec<Record> = db::find("trv_trip").filter("employee", traveller).limit(500).all()?;
    for other in others {
        if text(&other, "id") == text(&trip, "id") || !LIVE.contains(&text(&other, "state").unwrap_or_default()) {
            continue;
        }
        if overlaps((start, end), (date_of(&other, "start_date")?, date_of(&other, "end_date")?)) {
            return Err(Error::msg(format!("this person is already on trip {} on those days", text(&other, "reference").unwrap_or("?"))));
        }
    }

    let legs = legs_of(&input.id)?;
    let mut spans = Vec::new();
    let mut legs_total = Decimal::zero(places);
    for leg in &legs {
        spans.push(Leg { depart: date_of(leg, "depart_on")?, arrive: date_of(leg, "arrive_on")? });
        legs_total = legs_total + money(leg, "cost", places)?;
    }
    legs_fit(&spans, start, end)?;

    let costs = costs_of(&input.id)?;
    let (mut costs_total, mut lodging) = (Decimal::zero(places), Decimal::zero(places));
    for cost in &costs {
        let amount = money(cost, "amount", places)?;
        costs_total = costs_total + amount;
        if text(cost, "kind") == Some("lodging") {
            lodging = lodging + amount;
        }
    }
    let excess = lodging_excess(decimal_of(&trip, "lodging_cap_per_night")?, nights(start, end), lodging)?;
    let reason = input.lodging_reason.filter(|r| !r.trim().is_empty());
    if excess.is_some() && reason.is_none() {
        return Err(Error::msg(format!("lodging is over the cap by {}: give a reason", excess.unwrap_or(lodging))));
    }
    // A trip the company does not pay for has no company costs to approve beyond the plan itself.
    let per_diem_total = money(&trip, "per_diem_total", places)?;
    let estimate = if text(&trip, "funding") == Some("company") { per_diem_total + legs_total + costs_total } else { Decimal::zero(places) };
    let second = needs_second(estimate, decimal_of(&trip, "approval_limit")?);
    let approver = approver_of(traveller)?;
    let out = change(
        &trip,
        json!({
            "state": "submitted", "legs_total": legs_total, "costs_total": costs_total, "lodging_total": lodging, "lodging_excess": excess,
            "lodging_reason": reason, "estimate_total": estimate, "needs_second": second, "approver": approver, "submitted_on": format_date(today_date()?),
        }),
    )?;
    events::emit("trip_submitted", &json!({ "trip": out["id"], "employee": traveller, "estimate": estimate, "approver": approver }))?;
    Ok(out)
}

fn decide(input: Decide) -> Result<Record> {
    let trip = trip_of(&input.id)?;
    let state = text(&trip, "state").unwrap_or_default().to_string();
    if !matches!(state.as_str(), "submitted" | "awaiting_second") {
        return Err(Error::msg("this trip is not waiting for a decision"));
    }
    let me = me_id()?;
    if me.is_some() && me.as_deref() == text(&trip, "employee") {
        return Err(Error::msg("nobody decides their own trip"));
    }
    if !input.approve && input.note.as_deref().is_none_or(|n| n.trim().is_empty()) {
        return Err(Error::msg("say why the trip is refused"));
    }
    let admin = is_admin()?;
    if state == "submitted" {
        let their_approver = me.is_some() && me.as_deref() == text(&trip, "approver");
        if !their_approver && !admin {
            return Err(Error::msg("only the traveller's approver (or a travel administrator) can decide"));
        }
        if !input.approve {
            return finish(&trip, "rejected", me, input.note);
        }
        if trip.get("needs_second") == Some(&json!(true)) {
            return change(&trip, json!({ "state": "awaiting_second", "first_approver": me, "note": input.note }));
        }
        return finish(&trip, "approved", me, input.note);
    }
    // The second decision: a travel administrator who has not decided already.
    require_admin()?;
    if me.is_some() && me.as_deref() == text(&trip, "first_approver") {
        return Err(Error::msg("the second approval must come from someone other than the first approver"));
    }
    finish(&trip, if input.approve { "approved" } else { "rejected" }, me, input.note)
}

fn finish(trip: &Record, state: &str, by: Option<String>, note: Option<String>) -> Result<Record> {
    let out = change(trip, json!({ "state": state, "decided_by": by, "note": note }))?;
    if state == "approved" {
        events::emit("trip_approved", &json!({ "trip": out["id"], "employee": out["employee"], "estimate": out["estimate_total"], "currency": out["currency"] }))?;
    }
    Ok(out)
}

fn cancel(input: Cancel) -> Result<Record> {
    let trip = trip_of(&input.id)?;
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why the trip is cancelled"));
    }
    if !matches!(text(&trip, "state"), Some("draft" | "submitted" | "awaiting_second" | "approved")) {
        return Err(Error::msg("this trip can no longer be cancelled"));
    }
    if !is_traveller(&trip)? && !is_admin()? {
        return Err(Error::msg("only the traveller (or a travel administrator) can cancel a trip"));
    }
    if date_of(&trip, "start_date")? <= today_date()? && !is_admin()? {
        return Err(Error::msg("the trip has started: ask a travel administrator"));
    }
    // An advance already asked for stays with hr_expense, which settles or returns it.
    change(&trip, json!({ "state": "cancelled", "cancel_reason": input.reason }))
}

fn request_advance(input: Id) -> Result<Record> {
    let trip = trip_of(&input.id)?;
    if !is_traveller(&trip)? {
        return Err(Error::msg("only the traveller asks for the advance"));
    }
    if text(&trip, "state") != Some("approved") {
        return Err(Error::msg("an advance can be asked for once the trip is approved"));
    }
    if text(&trip, "advance").is_some_and(|a| !a.is_empty()) {
        return Err(Error::msg("this trip already has an advance"));
    }
    if text(&trip, "funding") != Some("company") {
        return Err(Error::msg("only a trip the company pays for has an advance"));
    }
    let places = places_of(&trip)?;
    let cap = advance_cap(money(&trip, "estimate_total", places)?, trip.get("advance_percent").and_then(Value::as_i64).unwrap_or(70))?;
    if cap.is_zero() {
        return Err(Error::msg("there is nothing to advance"));
    }
    let made: Record = plugins::call(
        "hr_expense",
        "request_advance",
        &json!({ "purpose": format!("Trip {}: {}", text(&trip, "reference").unwrap_or("?"), text(&trip, "destination").unwrap_or("")), "amount": cap, "currency": trip["currency"], "due_date": trip["start_date"] }),
    )?;
    change(&trip, json!({ "advance": made["id"] }))
}

fn open_report(input: Id) -> Result<Record> {
    let trip = trip_of(&input.id)?;
    if !is_traveller(&trip)? {
        return Err(Error::msg("only the traveller opens the expense report"));
    }
    if text(&trip, "state") != Some("approved") {
        return Err(Error::msg("a report is opened for an approved trip"));
    }
    if text(&trip, "report").is_some_and(|r| !r.is_empty()) {
        return Err(Error::msg("this trip already has an expense report"));
    }
    if date_of(&trip, "start_date")? > today_date()? {
        return Err(Error::msg("the trip has not started"));
    }
    let made: Record = plugins::call(
        "hr_expense",
        "create_report",
        &json!({ "purpose": format!("Trip {}: {}", text(&trip, "reference").unwrap_or("?"), text(&trip, "destination").unwrap_or("")), "settlement_currency": trip["currency"] }),
    )?;
    change(&trip, json!({ "report": made["id"] }))
}

fn complete(input: Complete) -> Result<Record> {
    let trip = trip_of(&input.id)?;
    if !is_traveller(&trip)? && !is_admin()? {
        return Err(Error::msg("only the traveller (or a travel administrator) can complete a trip"));
    }
    if text(&trip, "state") != Some("approved") {
        return Err(Error::msg("only an approved trip is completed"));
    }
    if date_of(&trip, "end_date")? > today_date()? {
        return Err(Error::msg("the trip has not ended"));
    }
    let has_report = text(&trip, "report").is_some_and(|r| !r.is_empty());
    if !has_report && !input.without_expenses && text(&trip, "funding") == Some("company") {
        return Err(Error::msg("open the expense report first, or say the trip had no expenses"));
    }
    change(&trip, json!({ "state": "completed" }))
}

fn get(input: Id) -> Result<Value> {
    let trip = trip_of(&input.id)?;
    Ok(json!({ "trip": trip, "legs": legs_of(&input.id)?, "costs": costs_of(&input.id)? }))
}

fn mine() -> Result<Vec<Record>> {
    let Some(me) = me_id()? else { return Ok(Vec::new()) };
    db::find("trv_trip").filter("employee", me.as_str()).limit(500).all()
}

fn list(input: Search) -> Result<Vec<Record>> {
    let mut rows: Vec<Record> = db::find::<Record>("trv_trip").limit(2000).all()?;
    if let Some(state) = &input.state {
        rows.retain(|r| text(r, "state") == Some(state.as_str()));
    }
    if let Some(who) = &input.employee {
        rows.retain(|r| text(r, "employee") == Some(who.as_str()));
    }
    Ok(rows)
}

fn waiting() -> Result<Vec<Record>> {
    let me = me_id()?;
    let admin = is_admin()?;
    let mut rows: Vec<Record> = db::find::<Record>("trv_trip").limit(2000).all()?;
    rows.retain(|r| match text(r, "state") {
        Some("submitted") => admin || (me.is_some() && me.as_deref() == text(r, "approver")),
        Some("awaiting_second") => admin && me.as_deref() != text(r, "first_approver"),
        _ => false,
    });
    Ok(rows)
}

/// Report approved trips that ended long ago and were never completed, once each.
fn nightly() -> Result<Value> {
    let today = today_date()?;
    let rows: Vec<Record> = db::find::<Record>("trv_trip").filter("state", "approved").limit(5000).all()?;
    let mut flagged = 0;
    for trip in rows {
        if trip.get("unsettled_notified") == Some(&json!(true)) || settle_by(date_of(&trip, "end_date")?, GRACE_DAYS) >= today {
            continue;
        }
        change(&trip, json!({ "unsettled_notified": true }))?;
        events::emit("trip_unsettled", &json!({ "trip": trip["id"], "employee": trip["employee"], "ended": trip["end_date"] }))?;
        flagged += 1;
    }
    Ok(json!({ "flagged": flagged }))
}

handler! {
    fn create_travel_policy(input: PolicyInput) -> Record {
        policy(input)
    }

    fn list_travel_policies(_: Empty) -> Vec<Record> {
        db::find("trv_policy").limit(500).all()
    }

    /// Plan a trip in draft: dates and policy fix the allowance.
    fn create_trip(input: NewTrip) -> Record {
        create(input)
    }

    fn add_trip_leg(input: NewLeg) -> Record {
        add_leg(input)
    }

    fn remove_trip_leg(input: Id) -> Value {
        remove_leg(input)
    }

    fn add_trip_cost(input: NewCost) -> Record {
        add_cost(input)
    }

    fn remove_trip_cost(input: Id) -> Value {
        remove_cost(input)
    }

    /// Check and freeze the trip, and send it for approval.
    fn submit_trip(input: Submit) -> Record {
        submit(input)
    }

    /// The approver's decision; a trip above the policy limit then waits for a travel administrator.
    fn decide_trip(input: Decide) -> Record {
        decide(input)
    }

    fn cancel_trip(input: Cancel) -> Record {
        cancel(input)
    }

    /// Ask hr_expense for an advance of the policy's share of the estimate (once).
    fn request_trip_advance(input: Id) -> Record {
        request_advance(input)
    }

    /// Open the trip's expense report in hr_expense (once, from the first day).
    fn open_trip_report(input: Id) -> Record {
        open_report(input)
    }

    fn complete_trip(input: Complete) -> Record {
        complete(input)
    }

    fn get_trip(input: Id) -> Value {
        get(input)
    }

    fn my_trips(_: Empty) -> Vec<Record> {
        mine()
    }

    fn list_trips(input: Search) -> Vec<Record> {
        list(input)
    }

    fn trips_waiting_for_me(_: Empty) -> Vec<Record> {
        waiting()
    }

    fn travel_nightly(_: Empty) -> Value {
        nightly()
    }
}
