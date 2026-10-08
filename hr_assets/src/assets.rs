//! Registering, assigning, returning, maintaining and retiring assets, and the ledger behind them.
//!
//! * Each action writes one ledger entry (`asset_event`) and moves the asset with one update; the entry's number
//!   is kept on the asset, because a caller can only count the entries they may read.
//! * An asset has one holder at most, who must be working. The holder acknowledges receipt; an assignment nobody
//!   acknowledged within a week is reported once.
//! * A return states the condition: good frees the asset, damaged sends it to maintenance, lost retires it.
//! * A vehicle's odometer comes from readings by its holder or an administrator and never goes backwards.
//! * `outstanding_assets` is what a leaver still has to hand back (categories marked for return on departure).

use aether_sdk::dates::{format_date, parse_date};
use aether_sdk::prelude::*;

use crate::common::{employee, id_of, is_admin, my_employee, next_number, require, require_admin, text, today_date, Record};
use crate::rules::{distance, next, odometer_ok, Condition, Move, State};

const ACK_DAYS: i64 = 7;

#[derive(Deserialize)]
struct CategoryInput {
    code: String,
    name: String,
    kind: String,
    #[serde(default = "yes")]
    return_on_departure: bool,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct NewAsset {
    name: String,
    category: String,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    serial: Option<String>,
    #[serde(default)]
    odometer: Option<i64>,
    #[serde(default)]
    purchase_date: Option<String>,
    #[serde(default)]
    purchase_cost: Option<Value>,
}

#[derive(Deserialize)]
struct Assign {
    asset: String,
    employee: String,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct Return {
    asset: String,
    condition: String,
    #[serde(default)]
    odometer: Option<i64>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct Note {
    asset: String,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct Reading {
    asset: String,
    odometer: i64,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Who {
    employee: String,
}

#[derive(Deserialize, Default)]
struct Search {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    category: Option<String>,
}

fn me_id() -> Result<Option<String>> {
    Ok(my_employee()?.and_then(|m| text(&m, "id").map(str::to_string)))
}

fn state_of(asset: &Record) -> Result<State> {
    State::parse(text(asset, "state").unwrap_or_default()).ok_or_else(|| Error::msg("the asset has an unknown state"))
}

fn is_vehicle(asset: &Record) -> Result<bool> {
    let category = require("asset_category", text(asset, "category").unwrap_or_default(), "category")?;
    Ok(text(&category, "kind") == Some("vehicle"))
}

/// Write one ledger entry and update the asset in the same step. `changes` are the asset's new field values.
fn record(asset_id: &str, kind: &str, who: Option<&str>, condition: Option<&str>, odometer: Option<i64>, note: Option<&str>, mut changes: Value) -> Result<Record> {
    let fresh = require("asset", asset_id, "asset")?;
    let seq = fresh.get("event_seq").and_then(Value::as_i64).unwrap_or(0) + 1;
    db::create::<Record>(
        "asset_event",
        &json!({
            "asset": asset_id, "seq": seq, "on_date": format_date(today_date()?), "kind": kind, "employee": who.filter(|w| !w.is_empty()),
            "condition": condition, "odometer": odometer, "note": note, "actor": context::current()?.actor.id.unwrap_or_default(),
        }),
    )?;
    changes["event_seq"] = json!(seq);
    db::update("asset", asset_id, &changes)?.ok_or_else(|| Error::msg("the asset is gone"))
}

fn category(input: CategoryInput) -> Result<Record> {
    require_admin()?;
    if !matches!(input.kind.as_str(), "equipment" | "vehicle") {
        return Err(Error::msg("kind is equipment or vehicle"));
    }
    db::create("asset_category", &json!({ "code": input.code, "name": input.name, "kind": input.kind, "return_on_departure": input.return_on_departure, "is_active": true }))
        .map_err(|e| e.or("could not create the category (the code may be taken)"))
}

fn register(input: NewAsset) -> Result<Record> {
    require_admin()?;
    let cat = require("asset_category", &input.category, "category")?;
    if cat.get("is_active") == Some(&json!(false)) {
        return Err(Error::msg("this category is no longer used"));
    }
    if input.name.trim().is_empty() {
        return Err(Error::msg("name the asset"));
    }
    let vehicle = text(&cat, "kind") == Some("vehicle");
    if !vehicle && input.odometer.is_some() {
        return Err(Error::msg("only a vehicle has an odometer"));
    }
    if let Some(reading) = input.odometer {
        odometer_ok(None, reading).map_err(Error::msg)?;
    }
    let tag = match input.tag {
        Some(tag) => tag,
        None => next_number("asset", "AST-", 5)?,
    };
    let made: Record = db::create(
        "asset",
        &json!({
            "tag": tag, "name": input.name, "category": input.category, "serial": input.serial, "state": "available", "odometer": input.odometer,
            "purchase_date": input.purchase_date.map(|d| parse_date(&d).map(format_date)).transpose()?, "purchase_cost": input.purchase_cost, "event_seq": 0,
        }),
    )
    .map_err(|e| e.or("could not register the asset (the tag may be taken)"))?;
    record(id_of(&made)?, "created", None, None, input.odometer, None, json!({}))
}

fn assign(input: Assign) -> Result<Record> {
    require_admin()?;
    let asset = require("asset", &input.asset, "asset")?;
    let to = next(state_of(&asset)?, Move::Assign).ok_or_else(|| Error::msg("only an available asset can be assigned (it is held, in maintenance or retired)"))?;
    let person = employee(&input.employee)?;
    if !matches!(text(&person, "status"), Some("active" | "probation") | None) {
        return Err(Error::msg("only someone who is working can hold an asset"));
    }
    let out = record(
        &input.asset, "assigned", Some(&input.employee), None, None, input.note.as_deref(),
        json!({ "state": to.name(), "holder": input.employee, "assigned_on": format_date(today_date()?), "acknowledged": false, "unacknowledged_notified": false }),
    )?;
    events::emit("asset_assigned", &json!({ "asset": out["id"], "employee": input.employee }))?;
    Ok(out)
}

fn acknowledge(input: Note) -> Result<Record> {
    let asset = require("asset", &input.asset, "asset")?;
    let me = me_id()?;
    if state_of(&asset)? != State::Assigned || me.is_none() || me.as_deref() != text(&asset, "holder") {
        return Err(Error::msg("only the holder of an assigned asset can acknowledge receipt"));
    }
    if asset.get("acknowledged") == Some(&json!(true)) {
        return Err(Error::msg("receipt is already acknowledged"));
    }
    record(&input.asset, "acknowledged", me.as_deref(), None, None, input.note.as_deref(), json!({ "acknowledged": true }))
}

fn give_back(input: Return) -> Result<Record> {
    require_admin()?;
    let asset = require("asset", &input.asset, "asset")?;
    let condition = Condition::parse(&input.condition).ok_or_else(|| Error::msg("condition is good, damaged or lost"))?;
    let to = next(state_of(&asset)?, Move::Return(condition)).ok_or_else(|| Error::msg("only an assigned asset can be returned"))?;
    if condition != Condition::Good && input.note.as_deref().is_none_or(|n| n.trim().is_empty()) {
        return Err(Error::msg("say what happened to a damaged or lost asset"));
    }
    let mut changes = json!({ "state": to.name(), "holder": null, "assigned_on": null, "acknowledged": false });
    if let Some(reading) = input.odometer {
        if !is_vehicle(&asset)? {
            return Err(Error::msg("only a vehicle has an odometer"));
        }
        odometer_ok(asset.get("odometer").and_then(Value::as_i64), reading).map_err(Error::msg)?;
        changes["odometer"] = json!(reading);
    }
    let holder = text(&asset, "holder").map(str::to_string);
    let out = record(&input.asset, "returned", holder.as_deref(), Some(&input.condition), input.odometer, input.note.as_deref(), changes)?;
    events::emit("asset_returned", &json!({ "asset": out["id"], "employee": holder, "condition": input.condition }))?;
    Ok(out)
}

fn move_maintenance(input: Note, mv: Move, kind: &str) -> Result<Record> {
    require_admin()?;
    let asset = require("asset", &input.asset, "asset")?;
    let to = next(state_of(&asset)?, mv).ok_or_else(|| Error::msg(format!("this asset cannot do that from {}", text(&asset, "state").unwrap_or("its state"))))?;
    record(&input.asset, kind, None, None, None, input.note.as_deref(), json!({ "state": to.name() }))
}

fn retire(input: Note) -> Result<Record> {
    require_admin()?;
    let asset = require("asset", &input.asset, "asset")?;
    let to = next(state_of(&asset)?, Move::Retire).ok_or_else(|| Error::msg("an asset in custody is returned before it is retired"))?;
    if input.note.as_deref().is_none_or(|n| n.trim().is_empty()) {
        return Err(Error::msg("say why the asset is retired"));
    }
    record(&input.asset, "retired", None, None, None, input.note.as_deref(), json!({ "state": to.name() }))
}

fn read_odometer(input: Reading) -> Result<Record> {
    let asset = require("asset", &input.asset, "asset")?;
    if !is_vehicle(&asset)? {
        return Err(Error::msg("only a vehicle has an odometer"));
    }
    let me = me_id()?;
    let holder = me.is_some() && me.as_deref() == text(&asset, "holder");
    if !holder && !is_admin()? {
        return Err(Error::msg("only the holder (or an asset administrator) can record a reading"));
    }
    if state_of(&asset)? == State::Retired {
        return Err(Error::msg("this vehicle is retired"));
    }
    odometer_ok(asset.get("odometer").and_then(Value::as_i64), input.odometer).map_err(Error::msg)?;
    record(&input.asset, "odometer", me.as_deref(), None, Some(input.odometer), None, json!({ "odometer": input.odometer }))
}

fn history(input: Id) -> Result<Value> {
    let asset = require("asset", &input.id, "asset")?;
    let me = me_id()?;
    let holder = me.is_some() && me.as_deref() == text(&asset, "holder");
    if !holder && !is_admin()? {
        return Err(Error::msg("you can see the history of assets you hold, or all if you run assets"));
    }
    let mut rows: Vec<Record> = db::find::<Record>("asset_event").filter("asset", input.id.as_str()).limit(5000).all()?;
    rows.sort_by_key(|r| r.get("seq").and_then(Value::as_i64).unwrap_or(0));
    // Kilometres driven under the current holder, from the odometer at assignment.
    let mut at_assignment: Option<i64> = None;
    let mut last = None;
    for row in &rows {
        let reading = row.get("odometer").and_then(Value::as_i64);
        match text(row, "kind") {
            Some("assigned") => at_assignment = last,
            Some("returned") => at_assignment = None,
            _ => {}
        }
        if reading.is_some() {
            last = reading;
        }
    }
    let driven = match (at_assignment, asset.get("odometer").and_then(Value::as_i64)) {
        (Some(from), Some(to)) => Some(distance(from, to)),
        _ => None,
    };
    Ok(json!({ "asset": asset, "events": rows, "driven_under_current_holder": driven }))
}

fn outstanding(input: Who) -> Result<Vec<Record>> {
    let me = me_id()?;
    if me.as_deref() != Some(input.employee.as_str()) && !is_admin()? {
        return Err(Error::msg("you can list your own assets, or anyone's if you run assets"));
    }
    let held: Vec<Record> = db::find("asset").filter("holder", input.employee.as_str()).limit(500).all()?;
    let mut out = Vec::new();
    for asset in held {
        let cat = require("asset_category", text(&asset, "category").unwrap_or_default(), "category")?;
        if cat.get("return_on_departure") != Some(&json!(false)) {
            out.push(asset);
        }
    }
    Ok(out)
}

fn list(input: Search) -> Result<Vec<Record>> {
    require_admin()?;
    let mut rows: Vec<Record> = db::find::<Record>("asset").limit(5000).all()?;
    if let Some(state) = &input.state {
        rows.retain(|r| text(r, "state") == Some(state.as_str()));
    }
    if let Some(cat) = &input.category {
        rows.retain(|r| text(r, "category") == Some(cat.as_str()));
    }
    Ok(rows)
}

fn mine() -> Result<Vec<Record>> {
    let Some(me) = me_id()? else { return Ok(Vec::new()) };
    db::find("asset").filter("holder", me.as_str()).limit(500).all()
}

/// Report an assignment nobody acknowledged in a week, once.
fn nightly() -> Result<Value> {
    let today = today_date()?;
    let rows: Vec<Record> = db::find::<Record>("asset").filter("state", "assigned").limit(5000).all()?;
    let mut flagged = 0;
    for asset in rows {
        if asset.get("acknowledged") == Some(&json!(true)) || asset.get("unacknowledged_notified") == Some(&json!(true)) {
            continue;
        }
        let Some(since) = text(&asset, "assigned_on").map(parse_date).transpose()? else { continue };
        if (today - since).num_days() >= ACK_DAYS {
            db::update::<Record>("asset", id_of(&asset)?, &json!({ "unacknowledged_notified": true }))?;
            events::emit("asset_unacknowledged", &json!({ "asset": asset["id"], "employee": asset["holder"], "since": asset["assigned_on"] }))?;
            flagged += 1;
        }
    }
    Ok(json!({ "flagged": flagged }))
}

handler! {
    fn create_asset_category(input: CategoryInput) -> Record {
        category(input)
    }

    fn list_asset_categories(_: Empty) -> Vec<Record> {
        db::find("asset_category").limit(500).all()
    }

    fn create_asset(input: NewAsset) -> Record {
        register(input)
    }

    fn assign_asset(input: Assign) -> Record {
        assign(input)
    }

    /// The holder confirms they have the asset.
    fn acknowledge_asset(input: Note) -> Record {
        acknowledge(input)
    }

    fn return_asset(input: Return) -> Record {
        give_back(input)
    }

    fn start_asset_maintenance(input: Note) -> Record {
        move_maintenance(input, Move::StartMaintenance, "maintenance_started")
    }

    fn finish_asset_maintenance(input: Note) -> Record {
        move_maintenance(input, Move::FinishMaintenance, "maintenance_finished")
    }

    fn retire_asset(input: Note) -> Record {
        retire(input)
    }

    /// Record a vehicle's odometer (the holder or an administrator); it never goes backwards.
    fn record_odometer(input: Reading) -> Record {
        read_odometer(input)
    }

    fn asset_history(input: Id) -> Value {
        history(input)
    }

    /// What a person holds that must be handed back when they leave.
    fn outstanding_assets(input: Who) -> Vec<Record> {
        outstanding(input)
    }

    fn my_assets(_: Empty) -> Vec<Record> {
        mine()
    }

    fn list_assets(input: Search) -> Vec<Record> {
        list(input)
    }

    fn assets_nightly(_: Empty) -> Value {
        nightly()
    }
}
