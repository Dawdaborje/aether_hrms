//! Punching in and out. A punch is a raw fact: it is written once and never edited. Where it was
//! made is checked against the person's shift location, and the day it belongs to is worked out
//! at once.

use aether_sdk::dates::{format_datetime, parse_datetime, Duration, NaiveDate, NaiveDateTime};
use aether_sdk::db::Filter;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{around, employee, id_of, my_employee, next_number, now_utc, require, require_admin, text, Record};
use crate::day::recompute;
use crate::rules::{distance_m, Direction};
use crate::setup::{assignment_on, shift_of};

/// A press of the button this soon after the last is the same press.
const REPEAT_SECS: i64 = 20;

#[derive(Deserialize)]
struct Press {
    #[serde(default)]
    direction: Option<String>,
    #[serde(default)]
    latitude: Option<Decimal>,
    #[serde(default)]
    longitude: Option<Decimal>,
    /// Make a retry of the same press harmless: the same reference is recorded once.
    #[serde(default)]
    client_ref: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Deserialize)]
struct Imported {
    ts: String,
    #[serde(default)]
    direction: Option<String>,
    client_ref: String,
}

#[derive(Deserialize)]
struct Import {
    employee: String,
    punches: Vec<Imported>,
}

#[derive(Deserialize)]
struct Mine {
    #[serde(default)]
    limit: Option<u32>,
}

fn degrees(value: &Decimal) -> f64 {
    value.to_string().parse().unwrap_or(0.0)
}

/// The work date a punch belongs to: the day whose shift window holds it, else its own date.
pub fn work_date_of(employee_id: &str, at: NaiveDateTime) -> Result<NaiveDate> {
    for day in around(at.date()) {
        if let Some((_, record)) = assignment_on(employee_id, day)? {
            let window = shift_of(&record)?.window(day);
            if at >= window.from && at <= window.to {
                return Ok(day);
            }
        }
    }
    Ok(at.date())
}

fn press(input: Press) -> Result<Value> {
    let person = my_employee()?.ok_or_else(|| Error::msg("you are not an employee: only an employee can punch"))?;
    let person_id = id_of(&person)?.to_string();
    if !matches!(text(&person, "status"), Some("active" | "probation") | None) {
        return Err(Error::msg("this person cannot punch right now"));
    }
    let now = now_utc()?;
    let direction = input.direction.as_deref().unwrap_or("auto");
    if Direction::parse(direction).is_none() {
        return Err(Error::msg("direction is in, out or auto"));
    }
    if matches!(input.source.as_deref(), Some("import" | "correction")) {
        return Err(Error::msg("that source is for administrators"));
    }
    // The same press sent twice, or pressed twice.
    if let Some(reference) = &input.client_ref {
        if let Some(existing) = db::find::<Record>("att_punch").filter("employee", person_id.as_str()).filter("client_ref", reference.as_str()).first()? {
            return Ok(json!({ "punch": existing, "repeated": true }));
        }
    }
    let recent = db::find::<Record>("att_punch")
        .matching(Filter::eq("employee", person_id.as_str()).and(Filter::gte("ts", format_datetime(now - Duration::seconds(REPEAT_SECS)))))
        .first()?;
    if recent.is_some() {
        return Err(Error::msg("already recorded a moment ago"));
    }

    // Where: the location of the shift they are on now.
    let work_date = work_date_of(&person_id, now)?;
    let location = match assignment_on(&person_id, work_date)? {
        Some((assignment, _)) => match text(&assignment, "location") {
            Some(id) => Some(require("att_location", id, "location")?),
            None => None,
        },
        None => None,
    };
    let (mut distance, mut inside) = (None, None);
    if let Some(location) = &location {
        let mode = text(location, "geo_mode").unwrap_or("record");
        let radius = location.get("radius_m").and_then(Value::as_i64).unwrap_or(0);
        if mode != "off" && radius > 0 {
            let (lat0, lon0) = (
                location.get("latitude").and_then(|v| serde_json::from_value::<Decimal>(v.clone()).ok()),
                location.get("longitude").and_then(|v| serde_json::from_value::<Decimal>(v.clone()).ok()),
            );
            match (&input.latitude, &input.longitude, lat0, lon0) {
                (Some(lat), Some(lon), Some(lat0), Some(lon0)) => {
                    let metres = distance_m(degrees(lat), degrees(lon), degrees(&lat0), degrees(&lon0)).round() as i64;
                    distance = Some(metres);
                    inside = Some(metres <= radius);
                    if mode == "enforce" && metres > radius {
                        return Err(Error::msg(format!("you are {metres} m from {}: you must be within {radius} m to punch", text(location, "name").unwrap_or("the workplace"))));
                    }
                }
                _ if mode == "enforce" => return Err(Error::msg("your location is needed to punch here")),
                _ => inside = None,
            }
        }
    }

    let reference = match input.client_ref {
        Some(reference) => reference,
        None => next_number("punch", "P-", 9)?,
    };
    let mut data = json!({
        "employee": person_id, "ts": format_datetime(now), "direction": direction, "source": input.source.as_deref().unwrap_or("web"),
        "client_ref": reference,
    });
    if let Some(lat) = &input.latitude {
        data["latitude"] = json!(lat);
    }
    if let Some(lon) = &input.longitude {
        data["longitude"] = json!(lon);
    }
    if let Some(metres) = distance {
        data["distance_m"] = json!(metres);
    }
    if let Some(ok) = inside {
        data["geo_ok"] = json!(ok);
    }
    let recorded: Record = db::create("att_punch", &data)?;
    events::emit("punch_recorded", &json!({ "employee": person_id, "punch": recorded["id"] }))?;
    // The day it belongs to, as it stands now.
    let day = recompute(&person, work_date)?;
    Ok(json!({ "punch": recorded, "day": day, "repeated": false }))
}

/// Load punches from a device or another system (administrators). Each is recorded once, by its
/// reference, so loading the same file again changes nothing.
fn import(input: Import) -> Result<Value> {
    require_admin()?;
    let person = employee(&input.employee)?;
    let person_id = id_of(&person)?.to_string();
    let (mut added, mut skipped) = (0u64, 0u64);
    let mut dates: Vec<NaiveDate> = Vec::new();
    for item in &input.punches {
        let at = parse_datetime(&item.ts)?;
        let direction = item.direction.as_deref().unwrap_or("auto");
        if Direction::parse(direction).is_none() {
            return Err(Error::msg("direction is in, out or auto"));
        }
        let exists = db::count("att_punch", Filter::eq("employee", person_id.as_str()).and(Filter::eq("client_ref", item.client_ref.as_str())))?;
        if exists > 0 {
            skipped += 1;
            continue;
        }
        db::create::<Record>(
            "att_punch",
            &json!({ "employee": person_id, "ts": format_datetime(at), "direction": direction, "source": "import", "client_ref": item.client_ref }),
        )?;
        added += 1;
        let date = work_date_of(&person_id, at)?;
        if !dates.contains(&date) {
            dates.push(date);
        }
    }
    for date in dates {
        recompute(&person, date)?;
    }
    Ok(json!({ "added": added, "already_had": skipped }))
}

handler! {
    /// Punch in or out as the caller. Place and time are taken from the request, never from the client.
    fn punch(input: Option<Press>) -> Value {
        press(input.unwrap_or(Press { direction: None, latitude: None, longitude: None, client_ref: None, source: None }))
    }

    /// Load punches from a device or file (attendance administrators).
    fn import_punches(input: Import) -> Value {
        import(input)
    }

    /// The caller's latest punches.
    fn my_punches(input: Option<Mine>) -> Vec<Record> {
        let Some(me) = my_employee()? else { return Ok(Vec::new()) };
        let limit = input.and_then(|m| m.limit).unwrap_or(50).min(500);
        db::find("att_punch").filter("employee", id_of(&me)?).order_by("-ts").limit(limit).all()
    }
}
