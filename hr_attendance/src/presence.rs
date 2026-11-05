//! Who is in today, and where people work from. Both are read from what is already recorded: the day's
//! first and last punch, and a mode a person (or their administrator) gives for a day.

use aether_sdk::db::Filter;
use aether_sdk::prelude::*;

use crate::common::{employee, id_of, is_admin, my_employee, text, today, Record};

#[derive(Deserialize, Default)]
struct Day {
    #[serde(default)]
    date: Option<String>,
}

#[derive(Deserialize)]
struct SetMode {
    date: String,
    mode: String,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    employee: Option<String>,
}

/// Today's attendance days with a first punch: `in` when there is no last punch yet, `out` otherwise.
fn presence(day: Option<String>) -> Result<Vec<Value>> {
    if !is_admin()? && my_employee()?.is_none() {
        return Ok(Vec::new());
    }
    let date = match day {
        Some(d) => aether_sdk::dates::format_date(aether_sdk::dates::parse_date(&d)?),
        None => today()?,
    };
    let days: Vec<Record> = db::find("att_day").matching(Filter::eq("work_date", date.as_str())).order_by("first_in").limit(1000).all()?;
    let mut out = Vec::new();
    for d in days {
        let Some(first) = d.get("first_in").filter(|v| !v.is_null()) else { continue };
        let who = text(&d, "employee").unwrap_or_default();
        let name = db::get::<Record>("employee", who)?.and_then(|e| text(&e, "display_name").map(str::to_string)).unwrap_or_default();
        let out_at = d.get("last_out").filter(|v| !v.is_null());
        let mode = db::find::<Record>("att_work_mode").filter("employee", who).filter("work_date", date.as_str()).first()?.and_then(|m| text(&m, "mode").map(str::to_string));
        out.push(json!({
            "id": d["id"], "employee": who, "name": name, "state": if out_at.is_some() { "out" } else { "in" },
            "first_in": first, "last_out": out_at, "work_mode": mode, "status": d.get("status"),
        }));
    }
    Ok(out)
}

fn set_mode(input: SetMode) -> Result<Record> {
    if !["office", "home", "remote", "field", "travel"].contains(&input.mode.as_str()) {
        return Err(Error::msg("work mode is office, home, remote, field or travel"));
    }
    let date = aether_sdk::dates::format_date(aether_sdk::dates::parse_date(&input.date)?);
    let me = my_employee()?;
    let target = match (&input.employee, &me) {
        (Some(e), Some(m)) if text(m, "id") == Some(e.as_str()) => e.clone(),
        (Some(e), _) => {
            if !is_admin()? {
                return Err(Error::msg("you set your own work mode, or anyone's if you run attendance"));
            }
            employee(e).and_then(|p| id_of(&p).map(str::to_string))?
        }
        (None, Some(m)) => id_of(m)?.to_string(),
        (None, None) => return Err(Error::msg("you are not an employee: name whom this is for")),
    };
    let data = json!({ "employee": target, "work_date": date, "mode": input.mode, "note": input.note });
    match db::find::<Record>("att_work_mode").filter("employee", target.as_str()).filter("work_date", date.as_str()).first()? {
        Some(existing) => db::update("att_work_mode", id_of(&existing)?, &data)?.ok_or_else(|| Error::msg("the entry is gone")),
        None => db::create("att_work_mode", &data),
    }
}

handler! {
    /// Who has punched in on a day (today by default), with where they work from.
    fn presence_today(input: Option<Day>) -> Vec<Value> {
        presence(input.unwrap_or_default().date)
    }

    /// Say where you work from on a day (or, for administrators, where someone does).
    fn set_work_mode(input: SetMode) -> Record {
        set_mode(input)
    }

    fn my_work_modes(_: Empty) -> Vec<Record> {
        let Some(me) = my_employee()? else { return Ok(Vec::new()) };
        db::find("att_work_mode").filter("employee", id_of(&me)?).order_by("-work_date").limit(120).all()
    }

    fn list_work_modes(input: Option<Day>) -> Vec<Record> {
        require_admin_only()?;
        let mut q = db::find::<Record>("att_work_mode").order_by("-work_date").limit(500);
        if let Some(d) = input.and_then(|i| i.date) {
            q = q.filter("work_date", d);
        }
        q.all()
    }
}

fn require_admin_only() -> Result<()> {
    if is_admin()? { Ok(()) } else { Err(Error::msg("this is for attendance administrators")) }
}
