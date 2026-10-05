//! Employees: who works here and in what state. Where they work, under whom and on what terms is
//! in `employment` (a dated history); the employee row keeps a copy of the current placement.

use aether_sdk::dates::{age_on, check_order, format_date, parse_date, parse_optional};
use aether_sdk::db::{Filter, Walk};
use aether_sdk::prelude::*;

use crate::common::{explain, id_of, next_number, pick, require, text, today, Record};
use crate::employment;
use crate::rules::{status, MIN_WORKING_AGE};

/// What an HR officer may set on the employee row itself.
const IDENTITY: &[&str] = &["company", "gender", "work_email", "work_phone", "notice_days", "probation_end"];
/// Private details, kept in `employee_private` (readable only by the person and HR).
const PRIVATE: &[&str] = &[
    "legal_name", "date_of_birth", "place_of_birth", "marital_status", "nationality", "national_id", "passport_no",
    "passport_expiry", "visa_no", "work_permit_expiry", "private_email", "private_phone", "private_address",
    "emergency_name", "emergency_relation", "emergency_phone", "education_level", "study_field", "bank_account_no", "bank_name",
];
/// The fields of a first set of terms, taken from the hire form.
const TERMS: &[&str] = &[
    "department", "job", "position", "manager", "employment_type", "work_location", "grade", "wage", "currency",
    "pay_frequency", "contract_start", "contract_end", "fixed_term", "trial_end",
];

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Hire {
    /// The party (person) being hired.
    party: String,
    #[serde(flatten)]
    fields: Record,
}

#[derive(Deserialize)]
struct Change {
    id: String,
    #[serde(flatten)]
    fields: Record,
}

#[derive(Deserialize)]
struct Move {
    id: String,
    status: String,
    /// The day it takes effect; today when left out.
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct Search {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    department: Option<String>,
    /// Include the departments below `department`.
    #[serde(default)]
    with_subdepartments: bool,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    manager: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    offset: Option<u32>,
}

#[derive(Deserialize)]
struct Chain {
    id: String,
    #[serde(default)]
    depth: Option<u32>,
    #[serde(default)]
    include_self: bool,
}

#[derive(Deserialize)]
struct LinkUser {
    employee: String,
    user: String,
}

/// Birth and hire dates must make sense together.
fn check_dates(hired: Option<aether_sdk::dates::NaiveDate>, probation_end: Option<aether_sdk::dates::NaiveDate>, born: Option<aether_sdk::dates::NaiveDate>) -> Result<()> {
    if let Some(hired) = hired {
        check_order(hired, probation_end, "probation")?;
        if let Some(born) = born {
            if age_on(born, hired) < MIN_WORKING_AGE {
                return Err(Error::msg(format!("a person must be at least {MIN_WORKING_AGE} when hired")));
            }
        }
    }
    if let Some(born) = born {
        if born > parse_date(&today()?)? {
            return Err(Error::msg("the date of birth is in the future"));
        }
    }
    Ok(())
}

fn hire(input: Hire) -> Result<Record> {
    // The person must exist, and be hired once.
    let party: Record = plugins::call("party", "get_party", &json!({ "id": input.party }))?;
    if party.is_null() {
        return Err(Error::msg("there is no such person in the directory"));
    }
    if text(&party, "kind") == Some("organization") {
        return Err(Error::msg("an organization cannot be an employee"));
    }
    if db::count("employee", Filter::eq("party", input.party.as_str()))? > 0 {
        return Err(Error::msg("this person has been an employee before: use rehire_employee, which keeps their history"));
    }
    check_holds(&input.party)?;
    if let Some(company) = text(&input.fields, "company") {
        require("company", company, "company")?;
    }

    let start = match text(&input.fields, "hire_date") {
        Some(date) => format_date(parse_date(date)?),
        None => today()?,
    };
    let probation_end = parse_optional(text(&input.fields, "probation_end"))?;
    check_dates(Some(parse_date(&start)?), probation_end, parse_optional(text(&input.fields, "date_of_birth"))?)?;

    let mut data = pick(&input.fields, IDENTITY);
    data["hire_date"] = json!(start);
    data["first_hire_date"] = json!(start);
    // New people start in probation when a probation end is given, and active otherwise.
    data["status"] = json!(if probation_end.is_some() { "probation" } else { "active" });
    data["party"] = json!(input.party);
    data["display_name"] = party.get("display_name").or_else(|| party.get("name")).cloned().unwrap_or(Value::Null);
    data["employee_no"] = json!(next_number("employee", "EMP-", 5)?);
    let employee: Record = explain(db::create("employee", &data), "could not hire this person")?;
    let employee_id = id_of(&employee)?.to_string();

    // Their first terms, from the day they join. If these are refused, the hire is undone.
    let terms = pick(&input.fields, TERMS);
    let mut first = employment::create(&employee, &start, &{
        let mut terms = terms;
        terms["reason"] = json!("hire");
        terms
    });
    if first.is_ok() {
        let private = pick(&input.fields, PRIVATE);
        if private.as_object().is_some_and(|fields| !fields.is_empty()) {
            let mut private = private;
            private["employee"] = json!(employee_id);
            if let Err(error) = db::create::<Record>("employee_private", &private) {
                first = Err(error);
            }
        }
    }
    if let Err(error) = first {
        let _ = db::delete::<Record>("employee", &employee_id);
        let _ = db::find::<Record>("employment").filter("employee", employee_id.as_str()).all().map(|rows| {
            rows.iter().for_each(|row| {
                if let Ok(id) = id_of(row) {
                    let _ = db::delete::<Record>("employment", id);
                }
            })
        });
        return Err(match error {
            Error::Message(message) => Error::Message(message),
            other => Error::msg(format!("could not hire this person: {other}")),
        });
    }

    // The directory shows that this person is an employee now.
    let _: Value = plugins::call("party", "update_party", &json!({ "id": input.party, "is_employee": true }))?;
    let hired = require("employee", &employee_id, "employee")?;
    events::emit("employee_hired", &json!({ "employee": employee_id, "party": input.party }))?;
    Ok(hired)
}

fn change(input: Change) -> Result<Record> {
    let existing = require("employee", &input.id, "employee")?;
    if status::is_ended(text(&existing, "status").unwrap_or("active")) {
        return Err(Error::msg("this employment has ended: its record can no longer be changed"));
    }
    let data = pick(&input.fields, IDENTITY);
    if data.as_object().is_some_and(|fields| fields.is_empty()) {
        return Err(Error::msg("there is nothing to change"));
    }
    if let Some(company) = text(&data, "company") {
        require("company", company, "company")?;
    }
    check_dates(
        parse_optional(text(&existing, "hire_date"))?,
        parse_optional(text(&data, "probation_end").or_else(|| text(&existing, "probation_end")))?,
        None,
    )?;
    explain(db::update::<Record>("employee", id_of(&existing)?, &data), "could not change the employee")?
        .ok_or_else(|| Error::msg("the employee is gone"))
}

/// Move an employee to another status. Ending the employment stops their contract and tells the
/// directory they no longer work here.
fn move_status(input: Move) -> Result<Record> {
    let employee = require("employee", &input.id, "employee")?;
    let from = text(&employee, "status").unwrap_or("active").to_string();
    if !status::is_known(&input.status) {
        return Err(Error::msg(format!("`{}` is not a status", input.status)));
    }
    if !status::can_move(&from, &input.status) {
        let allowed = status::next_from(&from).join(", ");
        return Err(Error::msg(if allowed.is_empty() {
            format!("an employment that is {from} cannot change status")
        } else {
            format!("from {from} the status can become: {allowed}")
        }));
    }
    let day = match &input.date {
        Some(date) => parse_date(date)?,
        None => parse_date(&today()?)?,
    };
    let mut changes = json!({ "status": input.status });
    if status::is_ended(&input.status) {
        // Frappe refuses to let someone leave while people still report to them; so do we.
        let reports = db::count("employee", Filter::eq("manager", input.id.as_str()).and(Filter::one_of("status", status::PRESENT.to_vec())))?;
        if reports > 0 {
            return Err(Error::msg(format!("{reports} people still report to this person: give them another manager first")));
        }
        if let Some(hired) = text(&employee, "hire_date") {
            check_order(parse_date(hired)?, Some(day), "employment")?;
        }
        changes["end_date"] = json!(format_date(day));
        changes["is_active"] = json!(false);
    }
    let updated = db::update::<Record>("employee", &input.id, &changes)?.ok_or_else(|| Error::msg("the employee is gone"))?;

    if status::is_ended(&input.status) {
        employment::end_all(&input.id, day)?;
        let party = text(&employee, "party").unwrap_or_default();
        let still_employed = db::count("employee", Filter::eq("party", party).and(Filter::one_of("status", status::PRESENT.to_vec())))?;
        if still_employed == 0 {
            let _: Value = plugins::call("party", "update_party", &json!({ "id": party, "is_employee": false }))?;
        }
    }
    events::emit(
        "employee_status_changed",
        &json!({ "employee": input.id, "from": from, "to": input.status, "date": format_date(day), "reason": input.reason }),
    )?;
    Ok(updated)
}

fn search(input: Search) -> Result<Vec<Record>> {
    let mut filter = Filter::all();
    if let Some(department) = &input.department {
        if input.with_subdepartments {
            let ids: Vec<String> = db::tree("department", "parent", department)
                .walk(Walk::Down)
                .include_self()
                .all::<Record>()?
                .iter()
                .filter_map(|d| text(d, "id").map(str::to_string))
                .collect();
            filter = filter.and(Filter::one_of("department", ids));
        } else {
            filter = filter.and(Filter::eq("department", department.as_str()));
        }
    }
    if let Some(status) = &input.status {
        filter = filter.and(Filter::eq("status", status.as_str()));
    }
    if let Some(manager) = &input.manager {
        filter = filter.and(Filter::eq("manager", manager.as_str()));
    }
    if let Some(words) = input.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        filter = filter.and(Filter::like("display_name", words).or(Filter::like("employee_no", words)));
    }
    let mut find = db::find::<Record>("employee").matching(filter).order_by("display_name").limit(input.limit.unwrap_or(50).min(500));
    if let Some(offset) = input.offset {
        find = find.offset(offset);
    }
    find.all()
}

fn walk_tree(input: &Chain, walk: Walk) -> Result<Vec<Record>> {
    let mut tree = db::tree("employee", "manager", &input.id).walk(walk);
    if let Some(depth) = input.depth {
        tree = tree.depth(depth);
    }
    if input.include_self {
        tree = tree.include_self();
    }
    tree.all()
}

/// Tie a login account to an employee, so what that person does is known to be theirs. One
/// account belongs to one employee.
fn link_account(input: LinkUser) -> Result<Record> {
    let employee = require("employee", &input.employee, "employee")?;
    if input.user.trim().is_empty() {
        return Err(Error::msg("name the login account"));
    }
    let taken = db::find::<Record>("employee").filter("user", input.user.as_str()).first()?;
    if taken.as_ref().is_some_and(|other| text(other, "id") != text(&employee, "id")) {
        return Err(Error::msg("this account already belongs to another employee"));
    }
    db::update::<Record>("employee", id_of(&employee)?, &json!({ "user": input.user }))?
        .ok_or_else(|| Error::msg("the employee is gone"))
}

/// The employment record of the person making the call, if they have one.
pub fn acting_employee() -> Result<Option<Record>> {
    let who = context::current()?;
    let Some(user) = who.actor.id else { return Ok(None) };
    db::find::<Record>("employee")
        .filter("user", user.as_str())
        .matching(Filter::one_of("status", status::PRESENT.to_vec()))
        .first()
}

/// Holds other plugins placed on hiring this person (onboarding tasks that must be done first).
/// The same check runs whoever creates the employee, so it cannot be skipped.
fn check_holds(party: &str) -> Result<()> {
    let holds: Vec<Record> = db::find::<Record>("hire_hold").filter("party", party).filter("released", false).limit(50).all()?;
    if holds.is_empty() {
        return Ok(());
    }
    let reasons: Vec<&str> = holds.iter().filter_map(|h| text(h, "reason")).collect();
    Err(Error::msg(format!("this person cannot be hired yet: {}", reasons.join("; "))))
}

#[derive(Deserialize)]
struct Hold {
    party: String,
    holder: String,
    key: String,
    #[serde(default)]
    reason: Option<String>,
}

/// Place a hold on hiring a person (once per holder and key).
fn place_hold(input: Hold) -> Result<Record> {
    let reason = input.reason.as_deref().filter(|r| !r.trim().is_empty()).ok_or_else(|| Error::msg("say what must happen first"))?;
    if let Some(existing) = db::find::<Record>("hire_hold")
        .filter("party", input.party.as_str())
        .filter("holder", input.holder.as_str())
        .filter("key", input.key.as_str())
        .first()?
    {
        return Ok(existing);
    }
    db::create("hire_hold", &json!({ "party": input.party, "holder": input.holder, "key": input.key, "reason": reason }))
        .map_err(|e| e.or("could not place the hold"))
}

fn release_hold(input: Hold) -> Result<Record> {
    let found = db::find::<Record>("hire_hold")
        .filter("party", input.party.as_str())
        .filter("holder", input.holder.as_str())
        .filter("key", input.key.as_str())
        .first()?
        .ok_or_else(|| Error::msg("there is no such hold"))?;
    if found.get("released") == Some(&json!(true)) {
        return Ok(found);
    }
    db::update::<Record>("hire_hold", id_of(&found)?, &json!({ "released": true, "released_on": format_date(parse_date(&today()?)?) }))?
        .ok_or_else(|| Error::msg("the hold is gone"))
}

#[derive(Deserialize)]
struct Rehire {
    employee: String,
    #[serde(flatten)]
    fields: Record,
}

/// Take back someone whose employment ended: the same employee record (their history stays), a
/// new dated employment, and the same holds apply as for a first hire.
fn rehire(input: Rehire) -> Result<Record> {
    let employee = require("employee", &input.employee, "employee")?;
    if !status::is_ended(text(&employee, "status").unwrap_or("active")) {
        return Err(Error::msg("this person is still employed"));
    }
    let party = text(&employee, "party").unwrap_or_default().to_string();
    check_holds(&party)?;
    let start = match text(&input.fields, "hire_date") {
        Some(date) => format_date(parse_date(date)?),
        None => today()?,
    };
    if let Some(left) = text(&employee, "end_date") {
        if start.as_str() <= left {
            return Err(Error::msg(format!("the new start must be after the employment ended ({left})")));
        }
    }
    let probation_end = parse_optional(text(&input.fields, "probation_end"))?;
    check_order(parse_date(&start)?, probation_end, "probation")?;
    let mut changes = json!({
        "status": if probation_end.is_some() { "probation" } else { "active" }, "hire_date": start, "end_date": null, "is_active": true,
        "probation_end": probation_end.map(format_date),
    });
    if text(&employee, "first_hire_date").is_none() {
        changes["first_hire_date"] = json!(text(&employee, "hire_date"));
    }
    let revived = db::update::<Record>("employee", &input.employee, &changes)?.ok_or_else(|| Error::msg("the employee is gone"))?;
    let mut terms = pick(&input.fields, TERMS);
    terms["reason"] = json!("rehire");
    if let Err(error) = employment::create(&revived, &start, &terms) {
        // Put the record back as it was.
        let _ = db::update::<Record>(
            "employee",
            &input.employee,
            &json!({ "status": text(&employee, "status"), "hire_date": text(&employee, "hire_date"), "end_date": text(&employee, "end_date"), "is_active": false }),
        );
        return Err(error);
    }
    let _: Value = plugins::call("party", "update_party", &json!({ "id": party, "is_employee": true }))?;
    events::emit("employee_rehired", &json!({ "employee": input.employee, "party": party }))?;
    require("employee", &input.employee, "employee")
}

handler! {
    /// Hire a person from the directory, with their first terms.
    fn hire_employee(input: Hire) -> Record {
        hire(input)
    }

    /// The employee's own record (contact, probation). Placement and pay change through employment.
    fn update_employee(input: Change) -> Record {
        change(input)
    }

    /// Change the status: leave, suspension, resignation, termination, retirement.
    fn change_employee_status(input: Move) -> Record {
        move_status(input)
    }

    /// Take back someone whose employment ended.
    fn rehire_employee(input: Rehire) -> Record {
        rehire(input)
    }

    /// Something must happen before this person can be hired (placed by another plugin).
    fn place_hire_hold(input: Hold) -> Record {
        place_hold(input)
    }

    fn release_hire_hold(input: Hold) -> Record {
        release_hold(input)
    }

    fn list_hire_holds(input: Id) -> Vec<Record> {
        db::find("hire_hold").filter("party", input.id.as_str()).limit(100).all()
    }

    /// The employee record of a person in the directory, if they ever were one.
    fn employee_of_party(input: Id) -> Option<Record> {
        db::find("employee").filter("party", input.id.as_str()).first()
    }

    fn get_employee(input: Id) -> Option<Record> {
        db::get("employee", &input.id)
    }

    fn list_employees(input: Search) -> Vec<Record> {
        search(input)
    }

    /// Everyone below an employee in the reporting line.
    fn employee_team(input: Chain) -> Vec<Record> {
        walk_tree(&input, Walk::Down)
    }

    /// The managers above an employee, up to the top.
    fn employee_chain(input: Chain) -> Vec<Record> {
        walk_tree(&input, Walk::Up)
    }

    /// The statuses an employee can move to from where they are.
    fn next_statuses(input: Id) -> Vec<String> {
        let employee = require("employee", &input.id, "employee")?;
        Ok(status::next_from(text(&employee, "status").unwrap_or("active")).into_iter().map(String::from).collect())
    }

    fn link_user(input: LinkUser) -> Record {
        link_account(input)
    }

    /// The caller's own employee record; none when the caller is not an employee.
    fn my_employee(_: Empty) -> Option<Record> {
        acting_employee()
    }

    /// For rules: the id of the caller's employee record, or nothing. Runs without rules.
    fn rule_var_employee(_: Empty) -> Option<String> {
        Ok(acting_employee()?.and_then(|e| text(&e, "id").map(str::to_string)))
    }

    /// For rules: the ids of everyone below the caller in the reporting line (at most a thousand).
    fn rule_var_subordinates(_: Empty) -> Vec<String> {
        let Some(me) = acting_employee()? else { return Ok(Vec::new()) };
        let team: Vec<Record> = db::tree("employee", "manager", id_of(&me)?).walk(Walk::Down).all()?;
        Ok(team.iter().filter_map(|e| text(e, "id").map(str::to_string)).collect())
    }
}
