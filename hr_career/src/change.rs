//! Career changes: a request, a decision, and dated employment written only on approval.
//!
//! Frappe's promotion is a document that rewrites the employee's fields on submit and rewrites them back on
//! cancel, with no approval and no check that a promotion goes up. Here the request is checked against the
//! current terms and the grade ladder, decided by someone who is neither the person nor the requester, and
//! approval adds a *dated employment record* in `hr`: the history stays, and the earlier terms are never edited.
//! A change that is approved but has not started yet can be withdrawn; one that has started is corrected by a
//! new change.

use aether_sdk::dates::{format_date, parse_date};
use aether_sdk::db::Filter;
use aether_sdk::prelude::*;

use crate::common::{
    actor, current_terms, decimal_of, employee, id_of, is_admin, my_employee, next_number, pick, require, require_admin, text,
    today_date, Record,
};
use crate::grade::by_code;
use crate::rules::{check_kind, within_band, Diff, Kind};

/// The terms a change can propose.
const TERMS: &[&str] = &["department", "job", "position", "manager", "work_location", "employment_type", "grade", "wage"];

#[derive(Deserialize)]
struct Ask {
    employee: String,
    kind: String,
    effective_date: String,
    reason: String,
    #[serde(default)]
    band_exception: bool,
    #[serde(flatten)]
    terms: Record,
}

#[derive(Deserialize)]
struct Decide {
    id: String,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize, Default)]
struct Search {
    #[serde(default)]
    employee: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

/// Terms that really differ from the current ones.
fn differing(current: &Record, proposed: &Record) -> Result<Record> {
    let mut out = json!({});
    for field in TERMS {
        let Some(new) = proposed.get(*field).filter(|v| !v.is_null()) else { continue };
        let same = if *field == "wage" {
            decimal_of(current, "wage")?.zip(decimal_of(proposed, "wage")?).is_some_and(|(a, b)| a == b)
        } else {
            current.get(*field) == Some(new)
        };
        if !same {
            out[*field] = new.clone();
        }
    }
    Ok(out)
}

fn locked_through() -> Result<Option<aether_sdk::dates::NaiveDate>> {
    let lock: Option<Record> = plugins::call("hr_compensation", "get_lock", &json!({}))?;
    Ok(lock.and_then(|l| text(&l, "locked_through").and_then(|d| parse_date(d).ok())))
}

fn check_date(date: aether_sdk::dates::NaiveDate) -> Result<()> {
    if let Some(locked) = locked_through()? {
        if date <= locked {
            return Err(Error::msg(format!("pay is closed through {locked}: a change cannot take effect on or before it")));
        }
    }
    Ok(())
}

fn ask(input: Ask) -> Result<Record> {
    let kind = Kind::parse(&input.kind).ok_or_else(|| Error::msg("kind is promotion, demotion, transfer, grade_change or pay_change"))?;
    if input.reason.trim().len() < 5 {
        return Err(Error::msg("give a reason: a career change without one cannot be judged"));
    }
    let person = employee(&input.employee)?;
    if !matches!(text(&person, "status"), Some("active" | "probation" | "on_leave") | None) {
        return Err(Error::msg("only someone who is employed can have a career change"));
    }
    let me = my_employee()?;
    if me.as_ref().is_some_and(|m| text(m, "id") == Some(input.employee.as_str())) {
        return Err(Error::msg("you cannot ask for a career change for yourself: ask your manager or HR"));
    }
    let their_manager = match (&me, text(&person, "manager")) {
        (Some(me), Some(manager)) => text(me, "id") == Some(manager),
        _ => false,
    };
    if !their_manager && !is_admin()? {
        return Err(Error::msg("only the person's manager or a career administrator can ask for this"));
    }
    let date = parse_date(&input.effective_date)?;
    if date < today_date()? && !is_admin()? {
        return Err(Error::msg("only a career administrator can backdate a change"));
    }
    check_date(date)?;

    let current = current_terms(&input.employee)?;
    let proposed = pick(&input.terms, TERMS);
    let changes = differing(&current, &proposed)?;
    let has = |field: &str| changes.get(field).is_some();
    let diff = Diff {
        grade: has("grade"),
        job_or_position: has("job") || has("position"),
        placement: has("department") || has("manager") || has("work_location") || has("employment_type"),
        wage: has("wage"),
        any: changes.as_object().is_some_and(|c| !c.is_empty()),
    };

    let old_grade = text(&current, "grade").and_then(|c| by_code(c).ok().flatten());
    let new_grade = match text(&changes, "grade") {
        Some(code) => {
            let row = by_code(code)?.ok_or_else(|| Error::msg(format!("there is no grade `{code}`: make it in the ladder first")))?;
            if row.get("is_active") == Some(&json!(false)) {
                return Err(Error::msg("that grade is no longer in use"));
            }
            Some(row)
        }
        None => None,
    };
    let rank = |row: &Option<Record>| row.as_ref().and_then(|r| r.get("rank").and_then(Value::as_i64));
    check_kind(kind, &diff, rank(&old_grade), rank(&new_grade))?;

    // The band of the grade the person will be on, applied to the wage they will have.
    let target_grade = new_grade.clone().or(old_grade.clone());
    let target_wage = decimal_of(&changes, "wage")?.or(decimal_of(&current, "wage")?);
    let mut exception = false;
    if let (Some(grade), Some(wage)) = (&target_grade, target_wage) {
        if (diff.grade || diff.wage) && !within_band(wage, decimal_of(grade, "min_wage")?, decimal_of(grade, "max_wage")?) {
            if !input.band_exception {
                return Err(Error::msg(format!(
                    "the wage {wage} is outside the band of grade {}: change the wage, or say on purpose that it is an exception",
                    text(grade, "code").unwrap_or("?")
                )));
            }
            exception = true;
        }
    }

    let open = db::count(
        "career_change",
        Filter::eq("employee", input.employee.as_str()).and(Filter::eq("kind", input.kind.as_str())).and(Filter::eq("state", "pending")),
    )?;
    if open > 0 {
        return Err(Error::msg("this person already has a waiting change of this kind: decide or withdraw it first"));
    }

    let mut data = changes.clone();
    data["reference"] = json!(next_number("change", "CHG-", 6)?);
    data["employee"] = json!(input.employee);
    data["kind"] = json!(input.kind);
    data["effective_date"] = json!(format_date(date));
    data["state"] = json!("pending");
    data["reason"] = json!(input.reason.trim());
    data["band_exception"] = json!(exception);
    data["requested_by"] = json!(actor()?);
    if let Some(me) = &me {
        data["requested_by_employee"] = json!(id_of(me)?);
    }
    for (from, field) in [("from_grade", "grade"), ("from_wage", "wage"), ("from_job", "job"), ("from_department", "department"), ("from_manager", "manager")] {
        if let Some(value) = current.get(field).filter(|v| !v.is_null()) {
            data[from] = value.clone();
        }
    }
    db::create("career_change", &data)
}

fn decide(input: Decide, approve: bool) -> Result<Record> {
    require_admin()?;
    let change = require("career_change", &input.id, "career change")?;
    if text(&change, "state") != Some("pending") {
        return Err(Error::msg("this change was already decided"));
    }
    let who = actor()?;
    let me = my_employee()?;
    let subject = text(&change, "employee").unwrap_or_default();
    if me.as_ref().is_some_and(|m| text(m, "id") == Some(subject)) {
        return Err(Error::msg("nobody decides their own career change"));
    }
    if text(&change, "requested_by") == Some(who.as_str()) {
        return Err(Error::msg("the person who asked cannot also approve: a second person decides"));
    }
    if !approve {
        if input.note.as_deref().is_none_or(|n| n.trim().is_empty()) {
            return Err(Error::msg("say why it is rejected"));
        }
        return db::update("career_change", &input.id, &json!({ "state": "rejected", "decided_by": who, "decision_note": input.note }))?
            .ok_or_else(|| Error::msg("the change is gone"));
    }

    let date = parse_date(text(&change, "effective_date").unwrap_or_default())?;
    check_date(date)?;
    let person = employee(subject)?;
    if !matches!(text(&person, "status"), Some("active" | "probation" | "on_leave") | None) {
        return Err(Error::msg("this person is no longer employed"));
    }
    // The terms must still be what the request was made against.
    let current = current_terms(subject)?;
    let wage_moved = decimal_of(&current, "wage")? != decimal_of(&change, "from_wage")?;
    if text(&change, "from_grade") != text(&current, "grade") || wage_moved {
        return Err(Error::msg("the person's terms changed after this was asked: withdraw it and ask again"));
    }
    let mut terms = pick(&change, TERMS);
    terms["employee"] = json!(subject);
    terms["date_from"] = json!(format_date(date));
    terms["reason"] = json!(format!("career:{}", text(&change, "reference").unwrap_or_default()));
    let created: Record = plugins::call("hr", "add_employment", &terms)?;
    let approved: Record = db::update(
        "career_change",
        &input.id,
        &json!({ "state": "approved", "decided_by": who, "decision_note": input.note, "employment": created["id"] }),
    )?
    .ok_or_else(|| Error::msg("the change is gone"))?;
    events::emit("career_change_approved", &json!({ "change": input.id, "employee": subject, "kind": change["kind"], "effective_date": change["effective_date"] }))?;
    Ok(approved)
}

fn withdraw(input: Id) -> Result<Record> {
    let change = require("career_change", &input.id, "career change")?;
    let me = my_employee()?;
    let mine = actor()? == text(&change, "requested_by").unwrap_or_default()
        || me.as_ref().is_some_and(|m| text(m, "id") == text(&change, "requested_by_employee"));
    if !mine && !is_admin()? {
        return Err(Error::msg("only who asked, or a career administrator, can withdraw this"));
    }
    match text(&change, "state") {
        Some("pending") => {}
        Some("approved") => {
            require_admin()?;
            let employment = text(&change, "employment").ok_or_else(|| Error::msg("the change has no employment record"))?;
            let history: Vec<Record> = plugins::call("hr", "employment_history", &json!({ "employee": text(&change, "employee").unwrap_or_default() }))?;
            let record = history.iter().find(|r| text(r, "id") == Some(employment));
            match record.and_then(|r| text(r, "state")) {
                Some("planned") => {
                    let _: Value = plugins::call("hr", "remove_employment", &json!({ "id": employment }))?;
                }
                Some(_) => return Err(Error::msg("this change has already taken effect: make a new change to correct it")),
                None => {}
            }
        }
        _ => return Err(Error::msg("only a waiting change, or an approved one that has not started, can be withdrawn")),
    }
    db::update::<Record>("career_change", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the change is gone"))
}

fn list(input: Search) -> Result<Vec<Record>> {
    let mut query = db::find::<Record>("career_change").order_by("-effective_date").limit(500);
    if let Some(person) = input.employee.as_deref() {
        query = query.filter("employee", person);
    }
    if let Some(state) = input.state.as_deref() {
        query = query.filter("state", state);
    }
    query.all()
}

handler! {
    /// Ask for a promotion, demotion, transfer, grade change or pay change.
    fn request_career_change(input: Ask) -> Record {
        ask(input)
    }

    /// Approve (career administrators; never the person, never the one who asked). Writes dated employment.
    fn approve_career_change(input: Decide) -> Record {
        decide(input, true)
    }

    fn reject_career_change(input: Decide) -> Record {
        decide(input, false)
    }

    fn withdraw_career_change(input: Id) -> Record {
        withdraw(input)
    }

    fn list_career_changes(input: Search) -> Vec<Record> {
        list(input)
    }

    fn get_career_change(input: Id) -> Option<Record> {
        db::get("career_change", &input.id)
    }
}
