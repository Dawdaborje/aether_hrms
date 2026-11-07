//! Employment: the terms someone works under, as a history of dated records.
//!
//! Every change of job, department, manager, pay or contract is a new record that starts on a day.
//! The one in force today is `current`; later ones are `planned` and earlier ones `past`. The
//! employee row carries a copy of the current placement (department, job, position, manager,
//! type, location) so lists, the reporting tree and rules read it without a lookup. A nightly job
//! applies records whose day has come.

use aether_sdk::dates::{check_order, format_date, overlaps, parse_date, parse_optional, NaiveDate};
use aether_sdk::db::{Filter, Walk};
use aether_sdk::prelude::*;

use crate::common::{explain, id_of, is_off, pick, require, text, today, Record};
use crate::rules::status;

/// The fields of the terms themselves (not who or when).
const TERMS: &[&str] = &[
    "department", "job", "position", "manager", "employment_type", "work_location", "cost_center", "grade", "wage", "currency",
    "pay_frequency", "wage_source", "contract_start", "contract_end", "fixed_term", "trial_end", "reason", "rank_override_reason",
];
/// The terms that the employee row mirrors while they are current.
const PLACEMENT: &[&str] = &["department", "job", "position", "manager", "employment_type", "work_location", "cost_center"];

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct New {
    employee: String,
    date_from: String,
    #[serde(flatten)]
    terms: Record,
}

#[derive(Deserialize)]
struct Change {
    id: String,
    #[serde(flatten)]
    terms: Record,
}

#[derive(Deserialize)]
struct ForEmployee {
    employee: String,
}

fn date_of(record: &Record, field: &str) -> Result<Option<NaiveDate>> {
    parse_optional(text(record, field))
}

/// What the terms must satisfy, whatever else is true.
/// The bare key of a record id (`employee:abc` and `abc` are the same record).
fn key_of(id: &str) -> &str {
    id.rsplit_once(':').map_or(id, |(_, key)| key)
}

/// Who a person reports to: someone else, who still works here, and not anyone who reports to this person
/// (directly or through others), which would make a loop with no top.
fn check_manager(employee: &Record, manager: &str, terms: &Record, before: Option<&Record>) -> Result<()> {
    let employee_id = id_of(employee)?;
    if key_of(manager) == key_of(employee_id) {
        return Err(Error::msg("a person cannot report to themselves"));
    }
    let boss = require("employee", manager, "manager")?;
    if !status::counts_in_headcount(text(&boss, "status").unwrap_or("active")) {
        return Err(Error::msg("the manager no longer works here"));
    }
    let above: Vec<Record> = db::tree("employee", "manager", id_of(&boss)?).walk(Walk::Up).all()?;
    if above.iter().any(|row| text(row, "id").is_some_and(|id| key_of(id) == key_of(employee_id))) {
        return Err(Error::msg("that person already reports to this one, directly or through others: it would make a loop"));
    }
    check_rank(employee, &boss, terms, before)
}

/// Is `terms` the same reporting line as `before`, with the fields that decide rank unchanged?
fn same_line(employee: &Record, manager: &str, terms: &Record, before: Option<&Record>) -> bool {
    let Some(before) = before else { return false };
    if text(before, "manager").map(key_of) != Some(key_of(manager)) {
        return false;
    }
    ["grade", "position", "job"].iter().all(|field| {
        // Position and job fall back to the employee's own; a grade has no such fallback.
        let held = |record: &Record| -> Option<String> {
            text(record, field)
                .or_else(|| if *field == "grade" { None } else { text(employee, field) })
                .map(str::to_string)
        };
        held(terms) == held(before)
    })
}

/// Only an HR manager sets the rank rule aside, so only an HR manager may change the reason that records it.
/// Leaving it as it was is open to anyone who may edit the terms.
fn guard_override_reason(data: &Record, before: Option<&Record>) -> Result<()> {
    let Some(sent) = data.get("rank_override_reason") else { return Ok(()) };
    let clean = |v: Option<&Value>| v.and_then(Value::as_str).map(str::trim).filter(|r| !r.is_empty()).map(str::to_string);
    if clean(Some(sent)) != clean(before.and_then(|b| b.get("rank_override_reason"))) && !crate::rank::is_hr_manager() {
        return Err(Error::msg("only an HR manager can set the rank rule aside"));
    }
    Ok(())
}

/// A manager ranks above the people who report to them (or equal, by the HR default). An HR manager may go
/// against it by giving a reason, which stays on the terms.
fn check_rank(employee: &Record, boss: &Record, terms: &Record, before: Option<&Record>) -> Result<()> {
    // The same reporting line, with nothing that sets rank changed (grade, position, job): whatever was decided
    // about it before, an approved exception included, stands. Only a new manager or a rank-affecting edit is checked.
    if same_line(employee, text(boss, "id").unwrap_or_default(), terms, before) {
        return Ok(());
    }
    let under = crate::rank::rank_of(
        text(terms, "grade"),
        text(terms, "position").or_else(|| text(employee, "position")),
        text(terms, "job").or_else(|| text(employee, "job")),
    )?;
    if crate::rank::allowed(crate::rank::employee_rank(boss)?, under)? {
        return Ok(());
    }
    if text(terms, "rank_override_reason").is_some_and(|r| !r.trim().is_empty()) {
        if crate::rank::is_hr_manager() {
            return Ok(());
        }
        return Err(Error::msg("only an HR manager can set the rank rule aside"));
    }
    Err(Error::msg(format!("{}; an HR manager can set this aside with a reason", crate::rank::explain_rule()?)))
}

/// A person moving into a position reports to whoever holds the position it reports to, when that is one person
/// and they may be the manager. Only when the terms do not name a manager.
fn auto_manager(data: &mut Record, employee: &Record, explicit: bool, before_position: Option<&str>) {
    let Some(position) = text(data, "position").map(str::to_string) else { return };
    if explicit || text(data, "position") == before_position {
        return;
    }
    let Ok(Some(row)) = db::get::<Record>("position", &position) else { return };
    let Some(parent) = text(&row, "parent") else { return };
    let Ok(holders) = db::find::<Record>("employee")
        .matching(Filter::eq("position", parent).and(Filter::one_of("status", status::PRESENT.to_vec())))
        .limit(2)
        .all()
    else {
        return;
    };
    if let [only] = holders.as_slice() {
        if let Some(id) = text(only, "id") {
            if check_manager(employee, id, data, None).is_ok() {
                data["manager"] = json!(id);
            }
        }
    }
}

fn check_terms(terms: &Record, employee: &Record, date_from: NaiveDate, this: Option<&str>, before: Option<&Record>) -> Result<()> {
    if let Some(joined) = text(employee, "hire_date").map(parse_date).transpose()? {
        if date_from < joined {
            return Err(Error::msg(format!("the terms cannot start before the person joined ({joined})")));
        }
    }
    let (start, end, trial) = (date_of(terms, "contract_start")?, date_of(terms, "contract_end")?, date_of(terms, "trial_end")?);
    if end.is_some() && start.is_none() {
        return Err(Error::msg("a contract end needs a contract start"));
    }
    if let Some(start) = start {
        check_order(start, end, "contract")?;
        if let Some(trial) = trial {
            check_order(start, Some(trial), "trial period")?;
            if end.is_some_and(|end| trial > end) {
                return Err(Error::msg("the trial period ends after the contract"));
            }
        }
    } else if trial.is_some() {
        return Err(Error::msg("a trial period needs a contract start"));
    }
    if terms.get("fixed_term") == Some(&json!(true)) && end.is_none() {
        return Err(Error::msg("a fixed-term contract needs an end date"));
    }
    if let Some(wage) = terms.get("wage").filter(|w| !w.is_null()) {
        let wage: aether_sdk::decimal::Decimal = serde_json::from_value(wage.clone()).map_err(|e| Error::msg(format!("wage: {e}")))?;
        if wage.is_negative() {
            return Err(Error::msg("a wage cannot be negative"));
        }
    }
    for (field, model, what) in [
        ("department", "department", "department"),
        ("job", "job", "job"),
        ("work_location", "work_location", "work location"),
        ("cost_center", "cost_center", "cost centre"),
    ] {
        if let Some(id) = text(terms, field) {
            let found = require(model, id, what)?;
            if is_off(&found, "is_active") {
                return Err(Error::msg(format!("the {what} is closed")));
            }
        }
    }
    if let Some(manager) = text(terms, "manager") {
        check_manager(employee, manager, terms, before)?;
    }
 // Contract periods of one person never overlap. 
    if let Some(start) = start {
        let employee_id = id_of(employee)?;
        let others: Vec<Record> = db::find::<Record>("employment")
            .matching(Filter::eq("employee", employee_id).and(Filter::is_set("contract_start")))
            .limit(500)
            .all()?;
        for other in others.iter().filter(|o| text(o, "id") != this) {
            let (os, oe) = (date_of(other, "contract_start")?, date_of(other, "contract_end")?);
            if let Some(os) = os {
                if overlaps(start, end, os, oe) {
                    return Err(Error::msg(format!(
                        "its contract period overlaps the one that starts {} ({})",
                        format_date(os),
                        text(other, "reason").unwrap_or("earlier terms")
                    )));
                }
            }
        }
    }
    Ok(())
}

/// A person moving into a position needs a free seat there and it must not be frozen.
fn check_seat(terms: &Record, employee: &Record) -> Result<()> {
    let Some(wanted) = text(terms, "position") else { return Ok(()) };
    let seat = require("position", wanted, "position")?;
    if text(&seat, "status") == Some("frozen") {
        return Err(Error::msg("the position is frozen"));
    }
    if text(employee, "position") == Some(wanted) {
        return Ok(());
    }
    let headcount = seat.get("headcount").and_then(Value::as_u64).unwrap_or(1);
    if crate::position::holders(wanted)? >= headcount {
        return Err(Error::msg("the position has no free seat"));
    }
    Ok(())
}

/// Work out which record is current today, mark all of them, and copy the current placement to
/// the employee. Safe to run again.
pub fn refresh(employee_id: &str) -> Result<()> {
    let today = parse_date(&today()?)?;
    let rows: Vec<Record> = db::find::<Record>("employment").filter("employee", employee_id).order_by("-date_from").limit(500).all()?;
    let mut current_seen = false;
    let mut current: Option<Record> = None;
    for row in &rows {
        let from = parse_date(text(row, "date_from").ok_or_else(|| Error::msg("an employment record has no start"))?)?;
        let wanted = if from > today {
            "planned"
        } else if !current_seen {
            current_seen = true;
            current = Some(row.clone());
            "current"
        } else {
            "past"
        };
        if text(row, "state") != Some(wanted) {
            db::update::<Record>("employment", id_of(row)?, &json!({ "state": wanted }))?;
        }
    }
    if let Some(current) = current {
        // Copy the placement; a field the terms leave empty is cleared on the employee.
        let mut changes = serde_json::Map::new();
        for field in PLACEMENT {
            changes.insert((*field).to_string(), current.get(*field).cloned().unwrap_or(Value::Null));
        }
        let employee = require("employee", employee_id, "employee")?;
        // Only write what differs, so a day with no change writes nothing.
        changes.retain(|key, value| match (employee.get(key.as_str()), &*value) {
            (None, Value::Null) => false,
            (Some(existing), new) => existing != new,
            (None, _) => true,
        });
        if !changes.is_empty() {
            explain(
                db::update::<Record>("employee", employee_id, &Value::Object(changes)),
                "could not apply the current employment terms to the employee",
            )?;
            crate::position::sync_moved(text(&employee, "position"), current.get("position").and_then(Value::as_str))?;
        }
    }
    Ok(())
}

/// What a new record carries over from the terms in force before it: placement and pay, never the
/// contract period (a new contract is its own thing). Say `null` to clear one.
const CARRIED: &[&str] = &[
    "department", "job", "position", "manager", "employment_type", "work_location", "cost_center", "grade", "wage", "currency", "pay_frequency",
];

/// The record whose terms were in force on `day`: the latest one starting on or before it.
fn terms_before(employee_id: &str, day: NaiveDate) -> Result<Option<Record>> {
    db::find::<Record>("employment")
        .matching(Filter::eq("employee", employee_id).and(Filter::lte("date_from", format_date(day))))
        .order_by("-date_from")
        .first()
}

fn decimal_of(record: &Record, field: &str) -> Result<Option<aether_sdk::decimal::Decimal>> {
    match record.get(field).filter(|v| !v.is_null()) {
        Some(value) => serde_json::from_value(value.clone()).map(Some).map_err(|e| Error::msg(format!("{field}: {e}"))),
        None => Ok(None),
    }
}

/// Whether `data` sets a wage that differs from the one in `before`: a form posting the unchanged wage sets nothing.
fn wage_given(data: &Record, before: Option<&Record>) -> Result<bool> {
    let Some(new) = decimal_of(data, "wage")? else { return Ok(data.get("wage").is_some_and(Value::is_null) && before.is_some_and(|b| b.get("wage").is_some_and(|w| !w.is_null()))) };
    Ok(before.map(|b| decimal_of(b, "wage")).transpose()?.flatten() != Some(new))
}

/// The organization's pay mode: `individual` (the default), `position` or `hybrid`.
pub fn pay_mode() -> Result<String> {
    Ok(text(&crate::settings::current()?, "pay_mode").unwrap_or("individual").to_string())
}

/// Make the wage in `data` follow the pay mode. By position: the wage is the position's `base_wage`, and a different one
/// is refused. Hybrid: the position's wage unless the person has their own (`wage_source` manual), which stays while they
/// keep the position. Individual: nothing changes. `given` says the caller set a wage that differs from `before`.
fn apply_pay_mode(data: &mut Record, before: Option<&Record>, given: bool) -> Result<()> {
    let mode = pay_mode()?;
    if mode == "individual" {
        return Ok(());
    }
    let position = match text(data, "position") {
        Some(id) => Some(require("position", id, "position")?),
        None => None,
    };
    let base = match &position {
        Some(position) => decimal_of(position, "base_wage")?,
        None => None,
    };
    let (Some(position), Some(base)) = (position, base) else {
        if given {
            data["wage_source"] = json!("manual");
        }
        return Ok(());
    };
    if given {
        if decimal_of(data, "wage")? == Some(base) {
            data["wage_source"] = json!("position");
        } else if mode == "position" {
            return Err(Error::msg(format!(
                "pay follows the position: the wage of {} is {base}; change the position's wage instead",
                text(&position, "name").unwrap_or("this position")
            )));
        } else {
            data["wage_source"] = json!("manual");
        }
        return Ok(());
    }
    let own = mode == "hybrid"
        && before.is_some_and(|b| text(b, "wage_source") == Some("manual") && text(b, "position") == text(data, "position"));
    if own {
        return Ok(());
    }
    data["wage"] = serde_json::to_value(base).map_err(|e| Error::msg(e.to_string()))?;
    data["wage_source"] = json!("position");
    if let Some(currency) = text(&position, "currency").map(str::to_string).or_else(crate::position::org_currency) {
        data["currency"] = json!(currency);
    }
    Ok(())
}

/// Move the people holding `position` onto its base wage, as new terms from today. In hybrid mode people with a wage of
/// their own are left alone. Returns how many were moved.
pub fn apply_position_pay(position: &Record) -> Result<u64> {
    let mode = pay_mode()?;
    let Some(base) = position.get("base_wage").filter(|w| !w.is_null()) else { return Ok(0) };
    if mode == "individual" {
        return Ok(0);
    }
    let holders: Vec<Record> = db::find::<Record>("employee")
        .matching(Filter::eq("position", id_of(position)?).and(Filter::one_of("status", status::PRESENT.to_vec())))
        .limit(1000)
        .all()?;
    let mut moved = 0;
    for employee in &holders {
        let current = current_terms(id_of(employee)?)?;
        if mode == "hybrid" && current.as_ref().is_some_and(|c| text(c, "wage_source") == Some("manual")) {
            continue;
        }
        let mut changes = json!({ "wage": base, "wage_source": "position", "reason": "position pay" });
        if let Some(currency) = text(position, "currency").map(str::to_string).or_else(crate::position::org_currency) {
            changes["currency"] = json!(currency);
        }
        apply_from_today(employee, &changes)?;
        moved += 1;
    }
    Ok(moved)
}

/// The terms in force today, for showing pay on the employee.
pub fn current_terms(employee_id: &str) -> Result<Option<Record>> {
    terms_before(employee_id, parse_date(&today()?)?)
}

/// Add a record of terms from `date_from`. `employee` is the full employee record.
pub fn create(employee: &Record, date_from: &str, terms: &Record) -> Result<Record> {
    let employee_id = id_of(employee)?;
    if status::is_ended(text(employee, "status").unwrap_or("active")) {
        return Err(Error::msg("this employment has ended: hire the person again first"));
    }
    let from = parse_date(date_from)?;
    let mut data = pick(terms, TERMS);
    let before = terms_before(employee_id, from)?;
    let given = wage_given(&data, before.as_ref())?;
    if let Some(before) = &before {
        for field in CARRIED {
            if data.get(*field).is_none() {
                if let Some(value) = before.get(*field) {
                    data[*field] = value.clone();
                }
            }
        }
        if data.get("wage_source").is_none() {
            if let Some(value) = before.get("wage_source") {
                data["wage_source"] = value.clone();
            }
        }
    }
    guard_override_reason(&data, before.as_ref())?;
    // The same manager on the next terms keeps the reason that let them be the manager.
    if let Some(before) = &before {
        if data.get("rank_override_reason").is_none()
            && text(&data, "manager").or_else(|| text(before, "manager")).map(key_of) == text(before, "manager").map(key_of)
        {
            if let Some(reason) = before.get("rank_override_reason").filter(|r| !r.is_null()) {
                data["rank_override_reason"] = reason.clone();
            }
        }
    }
    let explicit_manager = terms.get("manager").is_some();
    auto_manager(&mut data, employee, explicit_manager, before.as_ref().and_then(|b| text(b, "position")));
    apply_pay_mode(&mut data, before.as_ref(), given)?;
    check_terms(&data, employee, from, None, before.as_ref())?;
    check_seat(&data, employee)?;
    data["employee"] = json!(employee_id);
    data["date_from"] = json!(format_date(from));
    data["state"] = json!("planned");
    let created: Record = explain(db::create("employment", &data), "there may already be terms starting that day")?;
    refresh(employee_id)?;
    Ok(db::get::<Record>("employment", id_of(&created)?)?.unwrap_or(created))
}

/// The placement fields a form edits (department, job, position, manager, type, location).
pub const PLACEMENT_FIELDS: &[&str] = PLACEMENT;

/// Make placement changes (`changes` holds only fields that differ) effective today and keep the
/// history: a record already starting today is corrected, otherwise a new record starts today and
/// carries the rest of the terms over. Unchanged fields are not touched.
pub fn apply_from_today(employee: &Record, changes: &Record) -> Result<()> {
    let employee_id = id_of(employee)?;
    let day = format_date(parse_date(&today()?)?);
    let starting_today: Option<Record> = db::find::<Record>("employment")
        .filter("employee", employee_id)
        .filter("date_from", day.as_str())
        .first()?;
    match starting_today {
        Some(row) => {
            change(Change { id: id_of(&row)?.to_string(), terms: changes.clone() })?;
        }
        None => {
            let mut terms = changes.clone();
            terms["reason"] = json!("change");
            create(employee, &day, &terms)?;
        }
    }
    Ok(())
}

fn add(input: New) -> Result<Record> {
    let employee = require("employee", &input.employee, "employee")?;
    create(&employee, &input.date_from, &input.terms)
}

fn change(input: Change) -> Result<Record> {
    let existing = require("employment", &input.id, "employment record")?;
    let employee_id = text(&existing, "employee").ok_or_else(|| Error::msg("the record has no employee"))?;
    let employee = require("employee", employee_id, "employee")?;
    let mut data = pick(&input.terms, TERMS);
    if let Some(new_date) = text(&input.terms, "date_from") {
        data["date_from"] = json!(format_date(parse_date(new_date)?));
    }
    if data.as_object().is_some_and(|fields| fields.is_empty()) {
        return Err(Error::msg("there is nothing to change"));
    }
    // Pay follows the pay mode when the wage is set, or when the position changes.
    let given = wage_given(&data, Some(&existing))?;
    let moved = data.get("position").is_some() && text(&data, "position") != text(&existing, "position");
    if given || moved {
        let mut view = data.clone();
        if view.get("position").is_none() {
            view["position"] = existing.get("position").cloned().unwrap_or(Value::Null);
        }
        apply_pay_mode(&mut view, Some(&existing), given)?;
        for field in ["wage", "currency", "wage_source"] {
            if let Some(value) = view.get(field) {
                data[field] = value.clone();
            }
        }
    }
    if data.get("position").is_some() && data.get("manager").is_none() {
        let mut view = existing.clone();
        view["position"] = data["position"].clone();
        auto_manager(&mut view, &employee, false, text(&existing, "position"));
        if text(&view, "manager") != text(&existing, "manager") {
            data["manager"] = view["manager"].clone();
        }
    }
    guard_override_reason(&data, Some(&existing))?;
    let mut merged = existing.clone();
    for (key, value) in data.as_object().cloned().unwrap_or_default() {
        merged[key] = value;
    }
    let from = parse_date(text(&merged, "date_from").unwrap_or_default())?;
    check_terms(&merged, &employee, from, Some(&input.id), Some(&existing))?;
    check_seat(&data, &employee)?;
    let updated = explain(db::update::<Record>("employment", &input.id, &data), "could not change the terms")?
        .ok_or_else(|| Error::msg("the record is gone"))?;
    refresh(employee_id)?;
    Ok(db::get::<Record>("employment", id_of(&updated)?)?.unwrap_or(updated))
}

/// Remove a record. The last one cannot go: a person always has terms.
fn remove(input: Id) -> Result<Record> {
    let existing = require("employment", &input.id, "employment record")?;
    let employee_id = text(&existing, "employee").unwrap_or_default().to_string();
    if db::count("employment", Filter::eq("employee", employee_id.as_str()))? <= 1 {
        return Err(Error::msg("the last employment record cannot be removed: a person always has terms"));
    }
    let gone = db::delete::<Record>("employment", &input.id)?.ok_or_else(|| Error::msg("the record is gone"))?;
    refresh(&employee_id)?;
    Ok(gone)
}

/// An employee's terms, newest first.
fn history(input: ForEmployee) -> Result<Vec<Record>> {
    let mut rows: Vec<Record> = db::find("employment").filter("employee", input.employee.as_str()).order_by("-date_from").limit(200).all()?;
    // This function may read pay for its own use; only an HR manager is shown it.
    if !context::current()?.has_role("hr.hr_manager") {
        for row in rows.iter_mut().filter_map(Value::as_object_mut) {
            for field in ["wage", "currency", "pay_frequency", "wage_source"] {
                row.remove(field);
            }
        }
    }
    Ok(rows)
}

/// The employment ends on `day`: the contract in force stops then and later records are dropped.
pub fn end_all(employee_id: &str, day: NaiveDate) -> Result<()> {
    let rows: Vec<Record> = db::find::<Record>("employment").filter("employee", employee_id).limit(500).all()?;
    for row in &rows {
        let from = parse_date(text(row, "date_from").unwrap_or_default())?;
        if from > day {
            // Not started yet, and now never will: the last record cannot be removed, so keep one.
            if rows.len() > 1 {
                db::delete::<Record>("employment", id_of(row)?)?;
            }
            continue;
        }
        if let Some(start) = date_of(row, "contract_start")? {
            let end = date_of(row, "contract_end")?;
            if end.is_none_or(|end| end > day) && start <= day {
                db::update::<Record>("employment", id_of(row)?, &json!({ "contract_end": format_date(day) }))?;
            }
        }
    }
    Ok(())
}

/// Nightly: apply records whose day has come, for every employee that has one waiting.
fn apply_due() -> Result<u64> {
    let today = today()?;
    let due: Vec<Record> = db::find::<Record>("employment")
        .matching(Filter::eq("state", "planned").and(Filter::lte("date_from", today.as_str())))
        .limit(500)
        .all()?;
    let mut people: Vec<String> = due.iter().filter_map(|r| text(r, "employee").map(str::to_string)).collect();
    people.sort();
    people.dedup();
    for person in &people {
        refresh(person)?;
    }
    Ok(people.len() as u64)
}

/// Employees whose contract ends within the next days.
fn ending_soon(days: u32) -> Result<Vec<Record>> {
    let today = parse_date(&today()?)?;
    let limit = today + aether_sdk::dates::Duration::days(i64::from(days));
    db::find::<Record>("employment")
        .matching(
            Filter::eq("state", "current")
                .and(Filter::gte("contract_end", format_date(today)))
                .and(Filter::lte("contract_end", format_date(limit))),
        )
        .order_by("contract_end")
        .limit(500)
        .all()
}

#[derive(Deserialize)]
struct Days {
    #[serde(default = "thirty")]
    days: u32,
}

fn thirty() -> u32 {
    30
}

#[derive(Deserialize)]
struct Snapshot {
    employees: Vec<String>,
    on: String,
}

/// For many people at once: who they are and the terms in force on a day, for something that works
/// through a whole payroll in batches (one call, not one per person). At most 300 people.
fn snapshot(input: Snapshot) -> Result<Value> {
    if input.employees.is_empty() || input.employees.len() > 300 {
        return Err(Error::msg("ask for 1 to 300 people at a time"));
    }
    let on = format_date(parse_date(&input.on)?);
    let people: Vec<Record> = db::find("employee").matching(Filter::one_of("id", input.employees.clone())).limit(300).all()?;
    let terms: Vec<Record> = db::find::<Record>("employment")
        .matching(Filter::one_of("employee", input.employees.clone()).and(Filter::lte("date_from", on.as_str())))
        .order_by("-date_from")
        .limit(1000)
        .all()?;
    if terms.len() >= 1000 {
        return Err(Error::msg("too much history for one call: ask for fewer people"));
    }
    let mut out = serde_json::Map::new();
    for person in &people {
        let id = id_of(person)?;
        // Newest first: the first record of this person is the terms in force.
        let current = terms.iter().find(|t| text(t, "employee") == Some(id));
        out.insert(
            id.to_string(),
            json!({
                "display_name": person["display_name"], "status": person["status"], "hire_date": person["hire_date"], "end_date": person["end_date"],
                "department": person["department"], "job": person["job"], "user": person["user"],
                "terms": current.map(|t| json!({ "date_from": t["date_from"], "wage": t["wage"], "currency": t["currency"], "pay_frequency": t["pay_frequency"], "employment_type": t["employment_type"] })),
            }),
        );
    }
    Ok(Value::Object(out))
}

handler! {
    /// The people and their terms on a day, for a batch.
    fn employment_snapshot(input: Snapshot) -> Value {
        snapshot(input)
    }

    /// New terms for an employee from a day: promotion, transfer, pay change, renewal.
    fn add_employment(input: New) -> Record {
        add(input)
    }

    fn update_employment(input: Change) -> Record {
        change(input)
    }

    fn remove_employment(input: Id) -> Record {
        remove(input)
    }

    fn employment_history(input: ForEmployee) -> Vec<Record> {
        history(input)
    }

    /// Nightly: terms whose first day has come become current.
    fn apply_due_employments(_: Empty) -> u64 {
        apply_due()
    }

    fn contracts_ending_soon(input: Days) -> Vec<Record> {
        ending_soon(input.days)
    }
}
