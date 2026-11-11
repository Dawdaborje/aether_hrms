//! Employees: who works here and in what state. Where they work, under whom and on what terms is
//! In `employment` (a dated history); the employee row keeps a copy of the current placement.

use aether_sdk::dates::{age_on, check_order, format_date, parse_date, parse_optional};
use aether_sdk::db::{Filter, Walk};
use aether_sdk::prelude::*;

use crate::common::{explain, id_of, pick, require, text, today, Record};
use crate::employment;
use crate::numbering::{self, NumberContext};
use crate::rules::status;

/// What an HR officer may set on the employee row itself.
const IDENTITY: &[&str] = &[
    "company", "gender", "image", "work_email", "work_phone", "work_phone_country", "notice_days",
    "probation_end",
];
/// Private details, kept in `employee_private` (readable only by the person and HR).
const PRIVATE: &[&str] = &[
    "legal_name", "date_of_birth", "place_of_birth", "marital_status", "nationality", "passport_no",
    "passport_expiry", "visa_no", "work_permit_expiry", "private_email", "private_phone", "private_address",
    "emergency_name", "emergency_relation", "emergency_phone", "education_level", "study_field", "bank_account_no", "bank_name",
];
/// The fields of a first set of terms, taken from the hire form.
const TERMS: &[&str] = &[
    "department", "job", "position", "manager", "employment_type", "work_location", "cost_center", "grade", "wage",
    "currency", "pay_frequency", "contract_start", "contract_end", "fixed_term", "trial_end",
];

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Hire {
    /// Person details from the form; the handler creates the directory record itself.
    #[serde(default)]
    first_name: Option<String>,
    #[serde(default)]
    middle_name: Option<String>,
    #[serde(default)]
    last_name: Option<String>,
    /// The work email: kept on the employee as `work_email`, and the directory contact when no `contact_email` is given.
    #[serde(default)]
    email: Option<String>,
    /// The address of the person themselves (an enrollment invitation went here): the directory contact only, never
    /// the work email.
    #[serde(default)]
    contact_email: Option<String>,
    #[serde(default)]
    phone: Option<String>,
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

#[derive(Deserialize, Default, Clone)]
struct Search {
    /// Free-text search. The list page sends this as `q`; `text` is kept for callers.
    #[serde(default, alias = "q")]
    text: Option<String>,
    /// Kernel filter JSON from the faceted search bar (ANDed with other fields).
    #[serde(default)]
    filter: Option<Value>,
    /// Fields `db::search` expands free-text over when present.
    #[serde(default)]
    search_fields: Option<Vec<String>>,
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
    order: Option<String>,
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
struct LinkMe {
    employee: String,
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
            let minimum = crate::settings::minimum_working_age()?;
            if age_on(born, hired) < minimum {
                return Err(Error::msg(format!("a person must be at least {minimum} when hired")));
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

fn trimmed_name(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// Full name from first / middle / last. First and last are required; middle is optional.
fn person_name(input: &Hire) -> Result<String> {
    let first = trimmed_name(&input.first_name).ok_or_else(|| Error::msg("enter the person's first name"))?;
    let last = trimmed_name(&input.last_name).ok_or_else(|| Error::msg("enter the person's last name"))?;
    let mut parts = vec![first];
    if let Some(middle) = trimmed_name(&input.middle_name) {
        parts.push(middle);
    }
    parts.push(last);
    Ok(parts.join(" "))
}

/// Create the directory person from the hire form (name, email, phone). Callers never see this.
fn create_party_for_hire(input: &Hire) -> Result<(String, Record)> {
    let name = person_name(input)?;
    let mut payload = json!({ "name": name });
    if let Some(email) = input.contact_email.as_deref().or(input.email.as_deref()).map(str::trim).filter(|s| !s.is_empty()) {
        payload["email"] = json!(email);
    }
    if let Some(phone) = input.phone.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload["phone"] = json!(phone);
    }
    let party: Record = plugins::call("party", "create_person", &payload)?;
    let id = id_of(&party)?.to_string();
    Ok((id, party))
}

/// Keep avatar data URLs bounded so a hire form cannot dump megabytes into the row.
fn check_image(fields: &Record) -> Result<()> {
    let Some(image) = text(fields, "image").map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    if !(image.starts_with("data:image/") || image.starts_with("http://") || image.starts_with("https://")) {
        return Err(Error::msg("the photo must be an image file or an image URL"));
    }
    if image.len() > 400_000 {
        return Err(Error::msg("the photo is too large; use a smaller image"));
    }
    Ok(())
}

fn hire(input: Hire) -> Result<Record> {
    let (party_id, party) = create_party_for_hire(&input)?;
    if let Some(company) = text(&input.fields, "company") {
        require("company", company, "company")?;
    }
    check_image(&input.fields)?;

    let start = match text(&input.fields, "hire_date") {
        Some(date) => format_date(parse_date(date)?),
        None => today()?,
    };
    // The organisation's default probation applies when the hire form gave none.
    let default_end = if text(&input.fields, "probation_end").is_none() { crate::settings::default_probation_end(&start)? } else { None };
    let probation_end = match &default_end {
        Some(end) => parse_optional(Some(end.as_str()))?,
        None => parse_optional(text(&input.fields, "probation_end"))?,
    };
    check_dates(Some(parse_date(&start)?), probation_end, parse_optional(text(&input.fields, "date_of_birth"))?)?;

    let mut data = pick(&input.fields, IDENTITY);
    if let Some(gender) = text(&input.fields, "gender") {
        data["gender"] = json!(gender);
    }
    if let Some(email) = input.email.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        data["work_email"] = json!(email);
    }
    if let Some(phone) = input.phone.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        data["work_phone"] = json!(phone);
    }
    if let Some(country) = text(&input.fields, "work_phone_country").map(str::trim).filter(|s| !s.is_empty()) {
        let _: Record = plugins::call("country", "get_country", &json!({ "id": country }))
            .map_err(|_| Error::msg("there is no such country"))?;
        data["work_phone_country"] = json!(country);
    }
    if let Some(end) = default_end {
        data["probation_end"] = json!(end);
    }
    if data.get("notice_days").is_none_or(Value::is_null) {
        data["notice_days"] = json!(crate::settings::default_notice_days()?);
    }
    data["hire_date"] = json!(start);
    data["first_hire_date"] = json!(start);
    // New people start in probation when a probation end is given, and active otherwise.
    data["status"] = json!(if probation_end.is_some() { "probation" } else { "active" });
    data["party"] = json!(party_id);
    data["display_name"] = party.get("display_name").or_else(|| party.get("name")).cloned().unwrap_or(Value::Null);
    data["employee_no"] = json!(numbering::allocate(&NumberContext {
        department: text(&input.fields, "department").map(str::to_string),
        employment_type: text(&input.fields, "employment_type").map(str::to_string),
        hire_date: start.clone(),
    })?);
    let employee: Record = match explain(db::create("employee", &data), "could not create this employee") {
        Ok(row) => row,
        Err(error) => {
            let _ = plugins::call::<Value>("party", "archive", &json!({ "id": party_id }));
            return Err(error);
        }
    };
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
        let _ = plugins::call::<Value>("party", "archive", &json!({ "id": party_id }));
        return Err(match error {
            Error::Message(message) => Error::Message(message),
            other => Error::msg(format!("could not create this employee: {other}")),
        });
    }

    let _: Value = plugins::call("party", "update_party", &json!({ "id": party_id, "is_employee": true }))?;
    let hired = require("employee", &employee_id, "employee")?;
    crate::position::sync_moved(None, text(&hired, "position"))?;
    events::emit("employee_hired", &json!({ "employee": employee_id, "party": party_id }))?;
    Ok(hired)
}

/// Used by Desk demo generation (`demo` module).
pub(crate) fn hire_for_demo(fields: Value) -> Result<Record> {
    let input: Hire = serde_json::from_value(fields).map_err(|error| Error::msg(error.to_string()))?;
    hire(input)
}

fn change(input: Change) -> Result<Record> {
    let existing = require("employee", &input.id, "employee")?;
    if status::is_ended(text(&existing, "status").unwrap_or("active")) {
        return Err(Error::msg("this employment has ended: its record can no longer be changed"));
    }
    check_image(&input.fields)?;

    // The form posts everything on it. Three kinds of field, three homes:
    //  - identity: the employee row itself,
    //  - placement (department, job, position, manager, type, location): the employment history,
    //    from today, only where the value actually differs,
    //  - private details: the private record.
    let identity = pick(&input.fields, IDENTITY);
    let mut placement = serde_json::Map::new();
    if let Some(sent) = input.fields.as_object() {
        for field in employment::PLACEMENT_FIELDS {
            let Some(new) = sent.get(*field) else { continue };
            let new_text = new.as_str().map(str::trim).filter(|v| !v.is_empty());
            let old_text = text(&existing, field).filter(|v| !v.is_empty());
            // A value the form left empty clears the field, but only if there is one to clear.
            if new_text != old_text {
                placement.insert((*field).to_string(), new_text.map_or(Value::Null, |v| json!(v)));
            }
        }
    }
    // Setting the rank rule aside travels with the manager change, so it is kept on those terms.
    if placement.contains_key("manager") {
        if let Some(reason) = text(&input.fields, "rank_override_reason").map(str::trim).filter(|r| !r.is_empty()) {
            placement.insert("rank_override_reason".into(), json!(reason));
        }
    }
    // Pay (wage, currency, frequency) lives in the employment terms too, and only an HR manager changes it.
    if let Some(sent) = input.fields.as_object() {
        let current = employment::current_terms(id_of(&existing)?)?;
        for field in ["wage", "currency", "pay_frequency"] {
            let Some(new) = sent.get(field) else { continue };
            let blank = |v: &Value| v.is_null() || v.as_str().is_some_and(|t| t.trim().is_empty());
            let old = current.as_ref().and_then(|c| c.get(field)).cloned().unwrap_or(Value::Null);
            let same = (blank(new) && blank(&old))
                || new == &old
                || (field == "wage" && new.to_string().trim_matches('"').parse::<f64>().ok() == old.to_string().trim_matches('"').parse::<f64>().ok() && !blank(new));
            if !same {
                crate::common::require_pay_manager()?;
                placement.insert(field.to_string(), if blank(new) { Value::Null } else { new.clone() });
            }
        }
    }
    // Switching where the wage comes from: to the position takes its wage and currency; to manual keeps the wage as is.
    if let Some(source) = input.fields.get("wage_source").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        let current = employment::current_terms(id_of(&existing)?)?;
        if current.as_ref().and_then(|c| text(c, "wage_source")) != Some(source) {
            crate::common::require_pay_manager()?;
            match source {
                "position" => {
                    let position = text(&existing, "position").ok_or_else(|| Error::msg("choose a position first: the wage follows it"))?;
                    let position = require("position", position, "position")?;
                    let base = position.get("base_wage").filter(|w| !w.is_null()).cloned().ok_or_else(|| {
                        Error::msg("the position has no base wage to follow: set one on the position first")
                    })?;
                    placement.insert("wage".into(), base);
                    if let Some(currency) = text(&position, "currency") {
                        placement.insert("currency".into(), json!(currency));
                    }
                    placement.insert("wage_source".into(), json!("position"));
                }
                "manual" => {
                    if employment::pay_mode()? == "position" {
                        return Err(Error::msg("pay follows the position in this organization: change the position's wage instead"));
                    }
                    placement.insert("wage_source".into(), json!("manual"));
                }
                _ => return Err(Error::msg("the wage comes from the position or is set manually")),
            }
        }
    }
    let private = crate::people::private_part(&input.fields);
    let nothing = |value: &Value| value.as_object().is_none_or(|fields| fields.is_empty());

    if nothing(&identity) && placement.is_empty() && nothing(&private) {
        return Ok(existing);
    }
    if let Some(company) = text(&identity, "company") {
        require("company", company, "company")?;
    }
    if let Some(country) = text(&identity, "work_phone_country").map(str::trim).filter(|s| !s.is_empty()) {
        let _: Record = plugins::call("country", "get_country", &json!({ "id": country }))
            .map_err(|_| Error::msg("there is no such country"))?;
    }
    check_dates(
        parse_optional(text(&existing, "hire_date"))?,
        parse_optional(text(&identity, "probation_end").or_else(|| text(&existing, "probation_end")))?,
        None,
    )?;

    if !nothing(&identity) {
        explain(db::update::<Record>("employee", id_of(&existing)?, &identity), "could not change the employee")?
            .ok_or_else(|| Error::msg("the employee is gone"))?;
    }
    if !placement.is_empty() {
        employment::apply_from_today(&existing, &Value::Object(placement))?;
    }
    if !nothing(&private) {
        crate::people::save_private(id_of(&existing)?, private)?;
    }
    require("employee", &input.id, "employee")
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
    crate::position::sync_moved(text(&updated, "position"), None)?;

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

fn search(input: Search) -> Result<Value> {
    let mut filter = Filter::all();
    if let Some(extra) = input.filter.clone() {
        let from_ui = Filter::from_json(extra);
        if !from_ui.matches_all() {
            filter = filter.and(from_ui);
        }
    }
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
    if let Some(status) = input.status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        filter = filter.and(Filter::eq("status", status));
    }
    if let Some(manager) = &input.manager {
        filter = filter.and(Filter::eq("manager", manager.as_str()));
    }
    let order = input.order.as_deref().filter(|s| !s.is_empty()).unwrap_or("display_name");
    let limit = input.limit.unwrap_or(50).min(500);
    let q = input.text.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let fields: Vec<&str> = match &input.search_fields {
        Some(list) if !list.is_empty() => list.iter().map(String::as_str).collect(),
        _ => vec!["display_name", "employee_no", "work_email"],
    };
    if q.is_some() {
        return db::search(
            "employee",
            q,
            &fields,
            filter,
            Some(order),
            Some(limit),
            input.offset,
        );
    }
    let total = db::count("employee", filter.clone())?;
    let mut find = db::find::<Record>("employee").matching(filter).order_by(order).limit(limit);
    if let Some(offset) = input.offset {
        find = find.offset(offset);
    }
    Ok(json!({ "rows": find.all()?, "total": total }))
}

#[derive(Deserialize)]
struct ManagerSearch {
    /// The employee whose "Reports to" this is (the page's record).
    #[serde(default)]
    id: Option<String>,
    #[serde(flatten)]
    search: Search,
}

/// Who the "Reports to" picker offers: not the person themselves, and not anyone below them, since either would
/// make a loop. A new employee has no one to leave out.
fn possible_managers(input: ManagerSearch) -> Result<Value> {
    let (want_limit, want_offset) = (input.search.limit.unwrap_or(50).min(500) as usize, input.search.offset.unwrap_or(0) as usize);
    let bare = |id: &str| id.rsplit_once(':').map_or(id, |(_, key)| key).to_string();
    let mut left_out: Vec<String> = Vec::new();
    if let Some(id) = input.id.as_deref().filter(|id| !id.trim().is_empty()) {
        left_out.push(bare(id));
        let below: Vec<Record> = db::tree("employee", "manager", id).walk(Walk::Down).all()?;
        left_out.extend(below.iter().filter_map(|row| text(row, "id")).map(bare));
    }
    // Who may be offered: those holding the position this person's position reports to (when someone does),
    // and, but for HR managers who may set the rule aside, those who rank high enough.
    let mut only: Option<Vec<String>> = None;
    let mut under = None;
    if let Some(subject) = input.id.as_deref().filter(|id| !id.trim().is_empty()).map(|id| require("employee", id, "employee")).transpose()? {
        under = crate::rank::employee_rank(&subject)?;
        if let Some(parent) = text(&subject, "position")
            .and_then(|p| db::get::<Record>("position", p).ok().flatten())
            .and_then(|p| text(&p, "parent").map(str::to_string))
        {
            let holders: Vec<Record> = db::find::<Record>("employee")
                .matching(Filter::eq("position", parent.as_str()).and(Filter::one_of("status", status::PRESENT.to_vec())))
                .limit(500)
                .all()?;
            if !holders.is_empty() {
                only = Some(holders.iter().filter_map(|h| text(h, "id")).map(bare).collect());
            }
        }
    }
    let check_rank = under.is_some() && !crate::rank::is_hr_manager();
    // Eligibility is applied to every page of matches before the page asked for is cut, so someone eligible is
    // never lost behind 500 who are not, and `total` counts everyone eligible.
    const PAGE: u32 = 500;
    const MAX_PAGES: u32 = 40;
    let mut rows: Vec<Value> = Vec::new();
    for page in 0..MAX_PAGES {
        let mut ask = input.search.clone();
        ask.limit = Some(PAGE);
        ask.offset = Some(page * PAGE);
        let mut found = search(ask)?;
        let mut batch: Vec<Value> = found.get_mut("rows").and_then(Value::as_array_mut).map(std::mem::take).unwrap_or_default();
        let fetched = batch.len();
        batch.retain(|row| text(row, "id").map_or(true, |id| !left_out.contains(&bare(id))));
        if let Some(only) = &only {
            batch.retain(|row| text(row, "id").is_some_and(|id| only.contains(&bare(id))));
        }
        if check_rank {
            batch.retain(|row| crate::rank::employee_rank(row).and_then(|boss| crate::rank::allowed(boss, under)).unwrap_or(true));
        }
        rows.extend(batch);
        if fetched < PAGE as usize {
            break;
        }
    }
    let total = rows.len();
    let rows: Vec<Value> = rows.into_iter().skip(want_offset).take(want_limit).collect();
    Ok(json!({ "rows": rows, "total": total }))
}

/// The rows of `search`, without the total.
fn find(input: Search) -> Result<Vec<Record>> {
    let mut found = search(input)?;
    Ok(serde_json::from_value(found["rows"].take())?)
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
    let rehired = require("employee", &input.employee, "employee")?;
    crate::position::sync_moved(None, text(&rehired, "position"))?;
    Ok(rehired)
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
        let Some(mut row) = db::get::<Record>("employee", &input.id)? else { return Ok(None) };
        // The page passes the bare key; links and filters want the full record id.
        let id = id_of(&row)?.to_string();
        // Pay is shown to HR managers only, from the terms in force today.
        if context::current()?.has_role("hr.hr_manager") {
            if let (Some(row), Some(terms)) = (row.as_object_mut(), employment::current_terms(&id)?) {
                for field in ["wage", "currency", "pay_frequency", "wage_source"] {
                    if let Some(value) = terms.get(field) {
                        row.insert(field.to_string(), value.clone());
                    }
                }
                // The form shows or locks the pay fields by the organization's pay mode.
                row.insert("pay_mode".to_string(), json!(employment::pay_mode()?));
            }
        }
        // Private details live in their own record; the form edits them with the rest. A person
        // who may not read them (the rules decide) simply gets the row without them.
        if let Ok(Some(private)) = db::find::<Record>("employee_private").filter("employee", id.as_str()).first() {
            if let (Some(row), Some(private)) = (row.as_object_mut(), crate::people::private_part(&private).as_object()) {
                for (key, value) in private {
                    row.insert(key.clone(), value.clone());
                }
            }
        }
        Ok(Some(row))
    }

    fn list_employees(input: Search) -> Value {
        search(input)
    }

    /// The people an employee could report to (the "Reports to" picker).
    fn list_possible_managers(input: ManagerSearch) -> Value {
        possible_managers(input)
    }

    /// The same search as `list_employees`, as a plain list of employees: for other plugins, which page with
    /// `limit` and `offset` themselves and need no total.
    fn find_employees(input: Search) -> Vec<Record> {
        find(input)
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

    /// Link the caller's own login to an employee record. HR staff only: anyone else is told to
    /// ask HR, so nobody can claim someone else's employment record.
    fn link_me(input: LinkMe) -> Record {
        if acting_employee()?.is_some() {
            return Err(Error::msg("your account is already linked to an employee"));
        }
        let who = context::current()?;
        let Some(user) = who.actor.id.clone() else {
            return Err(Error::msg("sign in to link your account"));
        };
        if !(who.has_role("hr.hr_user") || who.has_role("hr.hr_manager")) {
            return Err(Error::msg(
                "only HR staff can link an account to an employee; ask HR to link your account",
            ));
        }
        link_account(LinkUser { employee: input.employee, user })
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
