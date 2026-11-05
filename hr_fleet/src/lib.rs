//! Company vehicles: who has which, what it has done and what has been spent on it.
//!
//! One person holds a vehicle at a time: assigning it closes the previous assignment the same day. Logs
//! (fuel, trips) and services (maintenance, repairs, inspections) are append-only records with an odometer
//! reading; an odometer never goes down, because a lower reading is a typing mistake or a tampered one.

mod common;

use aether_sdk::db::Filter;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use common::{id_of, is_admin, my_id, require, require_admin, text, today, Record};

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Assign {
    vehicle: String,
    employee: String,
    #[serde(default)]
    from: Option<String>,
}

#[derive(Deserialize)]
struct LogIn {
    vehicle: String,
    kind: String,
    #[serde(default)]
    date: Option<String>,
    odometer: i64,
    #[serde(default)]
    litres: Option<String>,
    #[serde(default)]
    cost: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct ServiceIn {
    vehicle: String,
    kind: String,
    #[serde(default)]
    date: Option<String>,
    odometer: i64,
    cost: String,
    #[serde(default)]
    vendor: Option<String>,
    #[serde(default)]
    next_due: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

const VEHICLE: &[&str] = &["plate", "make", "model", "year", "kind", "status", "odometer", "notes"];

fn money(raw: &str, what: &str) -> Result<String> {
    let d = Decimal::parse(raw).map_err(|_| Error::msg(format!("{what} is a number")))?.with_scale(2)?;
    if d.is_negative() {
        return Err(Error::msg(format!("{what} is not negative")));
    }
    Ok(d.to_string())
}

fn day(raw: Option<String>) -> Result<String> {
    match raw.filter(|d| !d.is_empty()) {
        Some(d) => Ok(aether_sdk::dates::format_date(aether_sdk::dates::parse_date(&d)?)),
        None => today(),
    }
}

fn new_vehicle(input: Record) -> Result<Record> {
    require_admin()?;
    let data = common::pick(&input, VEHICLE);
    if text(&data, "plate").is_none_or(str::is_empty) {
        return Err(Error::msg("a vehicle needs its plate"));
    }
    db::create("flt_vehicle", &data).map_err(|e| e.or("could not create the vehicle (the plate may exist)"))
}

fn edit_vehicle(input: Record) -> Result<Record> {
    require_admin()?;
    let id = text(&input, "id").ok_or_else(|| Error::msg("which vehicle?"))?.to_string();
    require("flt_vehicle", &id, "vehicle")?;
    let data = common::pick(&input, VEHICLE);
    db::update("flt_vehicle", &id, &data)?.ok_or_else(|| Error::msg("the vehicle is gone"))
}

/// Give a vehicle to a person from a day, closing whoever had it.
fn assign(input: Assign) -> Result<Record> {
    require_admin()?;
    let vehicle = require("flt_vehicle", &input.vehicle, "vehicle")?;
    if text(&vehicle, "status") == Some("retired") {
        return Err(Error::msg("a retired vehicle cannot be assigned"));
    }
    let _: Value = plugins::call("hr", "get_employee", &json!({ "id": input.employee }))
        .map_err(|_| Error::msg("there is no such employee"))?;
    let from = day(input.from)?;
    for open in db::find::<Record>("flt_assignment").matching(Filter::eq("vehicle", input.vehicle.as_str()).and(Filter::eq("state", "active"))).limit(10).all()? {
        if text(&open, "employee") == Some(input.employee.as_str()) {
            return Err(Error::msg("this person already has the vehicle"));
        }
        db::update::<Record>("flt_assignment", id_of(&open)?, &json!({ "state": "ended", "to_date": from }))?;
    }
    db::create("flt_assignment", &json!({ "vehicle": input.vehicle, "employee": input.employee, "from_date": from, "state": "active" }))
}

fn give_back(input: Id) -> Result<Record> {
    let row = require("flt_assignment", &input.id, "assignment")?;
    if text(&row, "state") != Some("active") {
        return Err(Error::msg("this assignment has ended"));
    }
    let mine = my_id()?;
    let holder = text(&row, "employee").map(str::to_string);
    if !is_admin()? && (mine.is_none() || mine != holder) {
        return Err(Error::msg("you hand back your own vehicle, or anyone's if you run the fleet"));
    }
    db::update("flt_assignment", &input.id, &json!({ "state": "ended", "to_date": today()? }))?.ok_or_else(|| Error::msg("the assignment is gone"))
}

/// The newest odometer reading we hold for a vehicle (its own field, raised by every log and service).
fn odometer_of(vehicle: &Record) -> i64 {
    vehicle.get("odometer").and_then(Value::as_i64).unwrap_or(0)
}

fn raise_odometer(vehicle_id: &str, vehicle: &Record, reading: i64) -> Result<()> {
    if reading < 0 {
        return Err(Error::msg("an odometer reading is not negative"));
    }
    if reading < odometer_of(vehicle) {
        return Err(Error::msg(format!("the odometer reads {}: a lower reading is a mistake", odometer_of(vehicle))));
    }
    db::update::<Record>("flt_vehicle", vehicle_id, &json!({ "odometer": reading }))?;
    Ok(())
}

fn add_log(input: LogIn) -> Result<Record> {
    let vehicle = require("flt_vehicle", &input.vehicle, "vehicle")?;
    if !["fuel", "trip", "toll", "other"].contains(&input.kind.as_str()) {
        return Err(Error::msg("a log is fuel, trip, toll or other"));
    }
    // Whoever holds the vehicle logs for it; the fleet administrator logs for anyone.
    let mine = my_id()?;
    if !is_admin()? {
        let held = db::count("flt_assignment", Filter::eq("vehicle", input.vehicle.as_str()).and(Filter::eq("state", "active")).and(Filter::eq("employee", mine.clone().unwrap_or_default())))?;
        if held == 0 {
            return Err(Error::msg("you log only for a vehicle you hold"));
        }
    }
    raise_odometer(&input.vehicle, &vehicle, input.odometer)?;
    let mut data = json!({ "vehicle": input.vehicle, "kind": input.kind, "log_date": day(input.date)?, "odometer": input.odometer, "note": input.note });
    if let Some(who) = mine {
        data["employee"] = json!(who);
    }
    if let Some(l) = input.litres.filter(|l| !l.is_empty()) {
        data["litres"] = json!(money(&l, "litres")?);
    }
    if let Some(c) = input.cost.filter(|c| !c.is_empty()) {
        data["cost"] = json!(money(&c, "cost")?);
    }
    db::create("flt_log", &data)
}

fn add_service(input: ServiceIn) -> Result<Record> {
    require_admin()?;
    let vehicle = require("flt_vehicle", &input.vehicle, "vehicle")?;
    if !["maintenance", "repair", "inspection", "tyres", "insurance", "other"].contains(&input.kind.as_str()) {
        return Err(Error::msg("a service is maintenance, repair, inspection, tyres, insurance or other"));
    }
    raise_odometer(&input.vehicle, &vehicle, input.odometer)?;
    let mut data = json!({
        "vehicle": input.vehicle, "kind": input.kind, "service_date": day(input.date)?, "odometer": input.odometer,
        "cost": money(&input.cost, "cost")?, "vendor": input.vendor, "note": input.note,
    });
    if let Some(next) = input.next_due.filter(|n| !n.is_empty()) {
        data["next_due"] = json!(aether_sdk::dates::format_date(aether_sdk::dates::parse_date(&next)?));
    }
    db::create("flt_service", &data)
}

/// Services due within the next days (and overdue ones), earliest first.
fn services_due(days: i64) -> Result<Vec<Record>> {
    let limit = aether_sdk::dates::format_date(aether_sdk::dates::parse_date(&today()?)? + aether_sdk::dates::Duration::days(days.clamp(0, 365)));
    db::find("flt_service").matching(Filter::lte("next_due", limit)).order_by("next_due").limit(200).all()
}

#[derive(Deserialize)]
struct Days {
    #[serde(default = "thirty")]
    days: i64,
}

fn thirty() -> i64 {
    30
}

handler! {
    fn create_vehicle(input: Record) -> Record {
        new_vehicle(input)
    }

    fn update_vehicle(input: Record) -> Record {
        edit_vehicle(input)
    }

    fn get_vehicle(input: Id) -> Option<Record> {
        db::get("flt_vehicle", &input.id)
    }

    fn list_vehicles(_: Empty) -> Vec<Record> {
        db::find("flt_vehicle").order_by("plate").limit(500).all()
    }

    fn assign_vehicle(input: Assign) -> Record {
        assign(input)
    }

    fn return_vehicle(input: Id) -> Record {
        give_back(input)
    }

    /// The vehicles the caller holds now.
    fn my_vehicles(_: Empty) -> Vec<Record> {
        let Some(me) = my_id()? else { return Ok(Vec::new()) };
        db::find("flt_assignment").matching(Filter::eq("employee", me).and(Filter::eq("state", "active"))).limit(50).all()
    }

    fn list_assignments(_: Empty) -> Vec<Record> {
        require_admin()?;
        db::find("flt_assignment").order_by("-from_date").limit(500).all()
    }

    fn log_vehicle_use(input: LogIn) -> Record {
        add_log(input)
    }

    fn list_vehicle_logs(input: Option<Id>) -> Vec<Record> {
        let mut q = db::find::<Record>("flt_log").order_by("-log_date").limit(500);
        if let Some(i) = input {
            q = q.filter("vehicle", i.id);
        }
        q.all()
    }

    fn record_service(input: ServiceIn) -> Record {
        add_service(input)
    }

    fn list_services(input: Option<Id>) -> Vec<Record> {
        let mut q = db::find::<Record>("flt_service").order_by("-service_date").limit(500);
        if let Some(i) = input {
            q = q.filter("vehicle", i.id);
        }
        q.all()
    }

    fn services_due_soon(input: Option<Days>) -> Vec<Record> {
        services_due(input.map_or(30, |d| d.days))
    }
}
