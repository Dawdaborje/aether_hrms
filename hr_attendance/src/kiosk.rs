//! Kiosk punching: a shared device (a tablet at the door) punches people in by employee number and PIN.
//!
//! The device account holds the role `att_kiosk`; it can punch but reads nothing else. A PIN is kept as a hash
//! tied to the person; five wrong tries lock it until an attendance administrator unlocks it.

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;
use sha2::{Digest, Sha256};

use crate::common::{id_of, is_admin, my_employee, require_admin, text, Record};
use crate::punch::{press_for, Press};

const KIOSK_ROLE: &str = "hr_attendance.att_kiosk";
const MAX_FAILURES: i64 = 5;

#[derive(Deserialize)]
struct SetPin {
    #[serde(default)]
    employee: Option<String>,
    pin: String,
}

#[derive(Deserialize)]
struct Unlock {
    employee: String,
}

#[derive(Deserialize)]
struct KioskPunch {
    employee_no: String,
    pin: String,
    #[serde(default)]
    direction: Option<String>,
    #[serde(default)]
    latitude: Option<Decimal>,
    #[serde(default)]
    longitude: Option<Decimal>,
    #[serde(default)]
    client_ref: Option<String>,
}

fn hash(employee: &str, pin: &str) -> String {
    Sha256::digest(format!("{employee}:{pin}").as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn set_pin(input: SetPin) -> Result<Record> {
    let me = my_employee()?;
    let target = match (&input.employee, &me) {
        (Some(e), Some(m)) if text(m, "id") == Some(e.as_str()) => e.clone(),
        (Some(e), _) => {
            require_admin()?;
            e.clone()
        }
        (None, Some(m)) => id_of(m)?.to_string(),
        (None, None) => return Err(Error::msg("you are not an employee")),
    };
    if !(4..=8).contains(&input.pin.len()) || !input.pin.chars().all(|c| c.is_ascii_digit()) {
        return Err(Error::msg("a PIN is 4 to 8 digits"));
    }
    let data = json!({ "employee": target, "pin_hash": hash(&target, &input.pin), "failures": 0, "locked": false });
    match db::find::<Record>("att_kiosk_pin").filter("employee", target.as_str()).first()? {
        Some(existing) => {
            // Someone locked out sets a new PIN only through an administrator.
            if existing.get("locked") == Some(&json!(true)) && !is_admin()? {
                return Err(Error::msg("this PIN is locked: ask an attendance administrator"));
            }
            db::update("att_kiosk_pin", id_of(&existing)?, &data)?.ok_or_else(|| Error::msg("the PIN is gone"))
        }
        None => db::create("att_kiosk_pin", &data),
    }
}

/// A refusal that is an answer, not an error: an error would roll back the failure count with it.
fn refused(why: &str) -> Result<Value> {
    Ok(json!({ "ok": false, "error": why }))
}

fn kiosk(input: KioskPunch) -> Result<Value> {
    let context = context::current()?;
    if !context.has_role(KIOSK_ROLE) && !context.has_role(crate::common::ADMIN_ROLE) {
        return Err(Error::msg(format!("only a kiosk account (role `{KIOSK_ROLE}`) punches people in this way")));
    }
    let found: Vec<Record> = plugins::call("hr", "list_employees", &json!({ "text": input.employee_no, "limit": 20 }))?;
    // The same message for an unknown number and a wrong PIN: the device says nothing about who exists.
    const WRONG: &str = "employee number or PIN is wrong";
    let Some(person) = found.into_iter().find(|p| text(p, "employee_no") == Some(input.employee_no.as_str())) else { return refused(WRONG) };
    let person_id = id_of(&person)?.to_string();
    let Some(row) = db::find::<Record>("att_kiosk_pin").filter("employee", person_id.as_str()).first()? else { return refused(WRONG) };
    if row.get("locked") == Some(&json!(true)) {
        return refused("this PIN is locked: ask an attendance administrator");
    }
    if text(&row, "pin_hash") != Some(hash(&person_id, &input.pin).as_str()) {
        let failures = row.get("failures").and_then(Value::as_i64).unwrap_or(0) + 1;
        db::update::<Record>("att_kiosk_pin", id_of(&row)?, &json!({ "failures": failures, "locked": failures >= MAX_FAILURES }))?;
        return refused(WRONG);
    }
    if row.get("failures").and_then(Value::as_i64).unwrap_or(0) > 0 {
        db::update::<Record>("att_kiosk_pin", id_of(&row)?, &json!({ "failures": 0 }))?;
    }
    press_for(
        person,
        Press { direction: input.direction, latitude: input.latitude, longitude: input.longitude, client_ref: input.client_ref, source: Some("device".into()) },
    )
}

handler! {
    /// Set a PIN for punching at a kiosk: your own, or anyone's (attendance administrators).
    fn set_kiosk_pin(input: SetPin) -> Value {
        set_pin(input).map(|_| json!({ "ok": true }))
    }

    /// Clear a lock after wrong tries (attendance administrators).
    fn unlock_kiosk_pin(input: Unlock) -> Value {
        require_admin()?;
        let row = db::find::<Record>("att_kiosk_pin").filter("employee", input.employee.as_str()).first()?.ok_or_else(|| Error::msg("this person has no PIN"))?;
        db::update::<Record>("att_kiosk_pin", id_of(&row)?, &json!({ "failures": 0, "locked": false }))?;
        Ok(json!({ "ok": true }))
    }

    /// Punch a person in or out from a shared device, by employee number and PIN.
    fn kiosk_punch(input: KioskPunch) -> Value {
        kiosk(input)
    }
}
