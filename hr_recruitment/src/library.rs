//! Recruitment library: applicant sources, opening templates, offer terms and templates, and the career site.

use aether_sdk::db::Filter;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::application::{apply_inner, Apply, Person};
use crate::common::{id_of, require, require_manager, require_recruiter, text, Record};
use crate::opening::{create as create_opening, New};

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Named {
    name: String,
}

// ---- sources ----------------------------------------------------------------------------------------------

fn named(model: &str, input: &Named, what: &str) -> Result<Record> {
    require_manager()?;
    let name = input.name.trim();
    if name.is_empty() || name.len() > 160 {
        return Err(Error::msg(format!("a {what} needs a name of up to 160 characters")));
    }
    if db::count(model, Filter::eq("name", name))? > 0 {
        return Err(Error::msg(format!("there is already a {what} called `{name}`")));
    }
    db::create(model, &json!({ "name": name, "is_active": true }))
}

// ---- opening templates ------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct NewTemplate {
    name: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    seats: Option<i64>,
    #[serde(default)]
    salary_min: Option<Decimal>,
    #[serde(default)]
    salary_max: Option<Decimal>,
    #[serde(default)]
    currency: Option<String>,
}

fn make_template(input: NewTemplate) -> Result<Record> {
    require_manager()?;
    if db::count("rec_opening_template", Filter::eq("name", input.name.as_str()))? > 0 {
        return Err(Error::msg("there is already a template with this name"));
    }
    if let (Some(min), Some(max)) = (&input.salary_min, &input.salary_max) {
        if min > max {
            return Err(Error::msg("the salary band's minimum is above its maximum"));
        }
    }
    let mut data = json!({ "name": input.name, "title": input.title, "seats": input.seats.unwrap_or(1), "is_active": true });
    for (field, value) in [
        ("description", input.description.map(|v| json!(v))),
        ("currency", input.currency.map(|v| json!(v))),
        ("salary_min", input.salary_min.map(|v| json!(v))),
        ("salary_max", input.salary_max.map(|v| json!(v))),
    ] {
        if let Some(value) = value {
            data[field] = value;
        }
    }
    db::create("rec_opening_template", &data)
}

#[derive(Deserialize)]
struct FromTemplate {
    template: String,
    position: String,
    #[serde(default)]
    department: Option<String>,
    #[serde(default)]
    recruiter: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    seats: Option<i64>,
    #[serde(default)]
    opens_on: Option<String>,
    #[serde(default)]
    closes_on: Option<String>,
}

fn from_template(input: FromTemplate) -> Result<Record> {
    require_recruiter()?;
    let template = require("rec_opening_template", &input.template, "template")?;
    if template.get("is_active") == Some(&json!(false)) {
        return Err(Error::msg("this template is switched off"));
    }
    let mut fields = json!({ "position": input.position, "title": input.title.or_else(|| text(&template, "title").map(str::to_string)), "seats": input.seats.or_else(|| template["seats"].as_i64()).unwrap_or(1) });
    for field in ["description", "salary_min", "salary_max", "currency"] {
        if !template[field].is_null() {
            fields[field] = template[field].clone();
        }
    }
    for (field, value) in [("department", input.department), ("recruiter", input.recruiter), ("opens_on", input.opens_on), ("closes_on", input.closes_on)] {
        if let Some(value) = value {
            fields[field] = json!(value);
        }
    }
    let new: New = serde_json::from_value(fields).map_err(|e| Error::msg(e.to_string()))?;
    create_opening(new)
}

// ---- offer terms and templates ----------------------------------------------------------------------------

#[derive(Deserialize)]
struct NewTerm {
    name: String,
    body: String,
}

#[derive(Deserialize)]
struct NewOfferTemplate {
    name: String,
    #[serde(default)]
    intro: Option<String>,
    #[serde(default)]
    terms: Vec<String>,
}

fn make_offer_template(input: NewOfferTemplate) -> Result<Record> {
    require_manager()?;
    for term in &input.terms {
        require("rec_offer_term", term, "offer term")?;
    }
    if db::count("rec_offer_template", Filter::eq("name", input.name.as_str()))? > 0 {
        return Err(Error::msg("there is already an offer template with this name"));
    }
    db::create("rec_offer_template", &json!({ "name": input.name, "intro": input.intro, "terms": input.terms, "is_active": true }))
}

fn fill(text: &str, vars: &[(&str, String)]) -> String {
    let mut out = text.to_string();
    for (name, value) in vars {
        out = out.replace(&format!("{{{{{name}}}}}"), value);
    }
    out
}

/// An offer template's intro and terms with the placeholders filled in.
pub fn render_terms(template: &str, vars: &[(&str, String)]) -> Result<String> {
    let row = require("rec_offer_template", template, "offer template")?;
    if row.get("is_active") == Some(&json!(false)) {
        return Err(Error::msg("this offer template is switched off"));
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(intro) = text(&row, "intro").filter(|t| !t.trim().is_empty()) {
        parts.push(fill(intro, vars));
    }
    for (n, id) in row["terms"].as_array().cloned().unwrap_or_default().iter().filter_map(|t| t.as_str()).enumerate() {
        let term = require("rec_offer_term", id, "offer term")?;
        parts.push(format!("{}. {}", n + 1, fill(text(&term, "body").unwrap_or_default(), vars)));
    }
    Ok(parts.join("\n\n"))
}

// ---- career site ------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct CareerApply {
    opening: String,
    name: String,
    email: String,
    #[serde(default)]
    phone: Option<String>,
    #[serde(default)]
    cover_note: Option<String>,
    /// A file already uploaded through the form.
    #[serde(default)]
    resume: Option<Value>,
    /// A field real visitors never see: bots fill it in.
    #[serde(default)]
    website: Option<String>,
}

/// Open jobs for the public: no salary band, no internal people.
fn career_list() -> Result<Vec<Value>> {
    let rows: Vec<Record> = db::find("rec_opening").filter("status", "open").order_by("title").limit(200).all()?;
    Ok(rows
        .iter()
        .filter(|o| o["seats_filled"].as_i64().unwrap_or(0) < o["seats"].as_i64().unwrap_or(1))
        .map(|o| json!({ "id": o["id"], "title": o["title"], "description": o["description"], "closes_on": o["closes_on"] }))
        .collect())
}

fn apply_from_site(input: CareerApply) -> Result<Value> {
    // A bot gets the same answer as a person and nothing is saved.
    if input.website.as_deref().is_some_and(|w| !w.trim().is_empty()) {
        return Ok(json!({ "ok": true }));
    }
    let name = input.name.trim();
    let email = input.email.trim();
    if name.is_empty() || name.len() > 120 {
        return Err(Error::msg("give your name"));
    }
    if email.len() > 160 || !email.contains('@') || email.contains(' ') {
        return Err(Error::msg("give a valid email address"));
    }
    let note = input.cover_note.as_deref().map(str::trim).filter(|n| !n.is_empty());
    if note.is_some_and(|n| n.len() > 4000) {
        return Err(Error::msg("the note is at most 4000 characters"));
    }
    let opening = require("rec_opening", &input.opening, "job")?;
    if text(&opening, "status") != Some("open") {
        return Err(Error::msg("this job is no longer open"));
    }
    let applied = apply_inner(Apply {
        opening: input.opening,
        candidate: None,
        person: Some(Person { name: name.to_string(), email: email.to_string(), phone: input.phone.map(|p| p.chars().take(40).collect()) }),
        source: Some("career_site".into()),
        referrer: None,
        expected_salary: None,
        notes: note.map(str::to_string),
    })?;
    if let Some(resume) = input.resume.filter(|r| !r.is_null() && r.to_string().len() < 2000) {
        db::update::<Record>("rec_application", id_of(&applied)?, &json!({ "resume": resume }))?;
    }
    // Nothing about the application (or whether the person applied before) is given back.
    Ok(json!({ "ok": true }))
}

handler! {
    fn create_rec_source(input: Named) -> Record {
        named("rec_source", &input, "source")
    }

    fn list_rec_sources(_: Empty) -> Vec<Record> {
        require_recruiter()?;
        db::find("rec_source").order_by("name").limit(200).all()
    }

    fn retire_rec_source(input: Id) -> Record {
        require_manager()?;
        db::update("rec_source", &input.id, &json!({ "is_active": false }))?.ok_or_else(|| Error::msg("there is no such source"))
    }

    fn create_opening_template(input: NewTemplate) -> Record {
        make_template(input)
    }

    fn list_opening_templates(_: Empty) -> Vec<Record> {
        require_recruiter()?;
        db::find("rec_opening_template").order_by("name").limit(200).all()
    }

    fn retire_opening_template(input: Id) -> Record {
        require_manager()?;
        db::update("rec_opening_template", &input.id, &json!({ "is_active": false }))?.ok_or_else(|| Error::msg("there is no such template"))
    }

    /// Make an opening from a template; the usual seat and approval rules still apply when it is opened.
    fn open_from_template(input: FromTemplate) -> Record {
        from_template(input)
    }

    fn create_offer_term(input: NewTerm) -> Record {
        require_manager()?;
        if db::count("rec_offer_term", Filter::eq("name", input.name.as_str()))? > 0 {
            return Err(Error::msg("there is already a term with this name"));
        }
        db::create("rec_offer_term", &json!({ "name": input.name, "body": input.body, "is_active": true }))
    }

    fn list_offer_terms(_: Empty) -> Vec<Record> {
        require_recruiter()?;
        db::find("rec_offer_term").order_by("name").limit(200).all()
    }

    fn create_offer_template(input: NewOfferTemplate) -> Record {
        make_offer_template(input)
    }

    fn list_offer_templates(_: Empty) -> Vec<Record> {
        require_recruiter()?;
        db::find("rec_offer_template").order_by("name").limit(200).all()
    }

    /// Open jobs, for the public career site.
    fn career_jobs(_: Empty) -> Vec<Value> {
        career_list()
    }

    /// Apply for an open job from the public career site.
    fn career_apply(input: CareerApply) -> Value {
        apply_from_site(input)
    }
}
