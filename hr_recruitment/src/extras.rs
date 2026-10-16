//! Resume text matching, applications from email, interviewer pools, and editing the library records.

use aether_sdk::db::Filter;
use aether_sdk::prelude::*;

use crate::application::{apply_inner, Apply, Person};
use crate::common::{id_of, require, require_manager, require_recruiter, text, Record};

#[derive(Deserialize)]
struct Id {
    id: String,
}

// ---- resume text ------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct Resume {
    application: String,
    text: String,
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '+' || c == '#' || c == '.' || c == '@' || c == '-' || c == '_'))
        .map(|w| w.trim_matches(|c: char| c == '.' || c == '-').to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

fn emails_in(words: &[String]) -> Vec<String> {
    let mut found: Vec<String> = words.iter().filter(|w| w.contains('@') && w.split('@').nth(1).is_some_and(|d| d.contains('.')) && w.len() < 120).map(|w| w.to_lowercase()).collect();
    found.sort();
    found.dedup();
    found
}

fn phones_in(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut run = String::new();
    for c in text.chars().chain(std::iter::once('\n')) {
        if c.is_ascii_digit() || (run.len() > 0 && matches!(c, ' ' | '-' | '(' | ')')) || (run.is_empty() && c == '+') {
            run.push(c);
        } else {
            let digits = run.chars().filter(char::is_ascii_digit).count();
            if (7..=15).contains(&digits) {
                found.push(run.trim().to_string());
            }
            run.clear();
        }
    }
    found.sort();
    found.dedup();
    found.truncate(5);
    found
}

/// What a resume's text says: emails, phones, and which of the organisation's skills it mentions.
fn read_resume(input: Resume) -> Result<Record> {
    require_recruiter()?;
    if input.text.len() > 100_000 {
        return Err(Error::msg("the resume text is at most 100,000 characters"));
    }
    require("rec_application", &input.application, "application")?;
    let tokens = words(&input.text);
    let lower: Vec<String> = tokens.iter().map(|w| w.to_lowercase()).collect();
    let skills: Vec<Record> = plugins::call("hr", "list_skills", &json!({}))?;
    let mut matched: Vec<String> = Vec::new();
    let haystack = format!(" {} ", lower.join(" "));
    for skill in &skills {
        if let Some(name) = text(skill, "name") {
            let needle = format!(" {} ", name.to_lowercase());
            if haystack.contains(&needle) {
                matched.push(name.to_string());
            }
        }
    }
    let parsed = json!({ "emails": emails_in(&tokens), "phones": phones_in(&input.text), "skills": matched });
    db::update("rec_application", &input.application, &json!({ "resume_text": input.text, "parsed": parsed }))?.ok_or_else(|| Error::msg("the application is gone"))
}

// ---- applications by email --------------------------------------------------------------------------------

#[derive(Deserialize)]
struct Mail {
    /// The message's own id: the same message is recorded once.
    message_id: String,
    from_email: String,
    #[serde(default)]
    from_name: Option<String>,
    subject: String,
    #[serde(default)]
    body: Option<String>,
}

/// Turn an email into an application. Called by a mail connector (a recruiter account); the subject must
/// name exactly one open opening by its title.
fn ingest(input: Mail) -> Result<Value> {
    require_recruiter()?;
    let reference: String = format!("mail:{}", input.message_id.chars().take(180).collect::<String>());
    if db::count("rec_application", Filter::eq("source_ref", reference.as_str()))? > 0 {
        return Ok(json!({ "recorded": false, "reason": "already recorded" }));
    }
    let subject = input.subject.to_lowercase();
    let open: Vec<Record> = db::find("rec_opening").filter("status", "open").limit(500).all()?;
    let hits: Vec<&Record> = open.iter().filter(|o| text(o, "title").is_some_and(|t| !t.is_empty() && subject.contains(&t.to_lowercase()))).collect();
    let [opening] = hits.as_slice() else {
        return Ok(json!({ "recorded": false, "reason": if hits.is_empty() { "the subject names no open job" } else { "the subject names more than one open job" } }));
    };
    let name = input.from_name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| input.from_email.split('@').next().unwrap_or("Candidate").to_string());
    let notes = input.body.as_deref().map(|b| b.chars().take(4000).collect::<String>());
    let made = apply_inner(Apply {
        opening: id_of(opening)?.to_string(),
        candidate: None,
        person: Some(Person { name, email: input.from_email, phone: None }),
        source: Some("email".into()),
        referrer: None,
        expected_salary: None,
        notes,
    });
    match made {
        Ok(application) => {
            db::update::<Record>("rec_application", id_of(&application)?, &json!({ "source_ref": reference }))?;
            Ok(json!({ "recorded": true, "application": application["id"] }))
        }
        Err(error) => Ok(json!({ "recorded": false, "reason": error.to_string() })),
    }
}

// ---- interviewer pools ------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct NewPool {
    name: String,
    #[serde(default)]
    members: Vec<String>,
}

#[derive(Deserialize)]
struct PoolMembers {
    id: String,
    members: Vec<String>,
}

fn make_pool(input: NewPool) -> Result<Record> {
    require_manager()?;
    if db::count("rec_pool", Filter::eq("name", input.name.as_str()))? > 0 {
        return Err(Error::msg("there is already a pool with this name"));
    }
    let pool: Record = db::create("rec_pool", &json!({ "name": input.name, "is_active": true }))?;
    if !input.members.is_empty() {
        db::relate::<Record>("rec_pool", "members", id_of(&pool)?, &input.members.iter().map(String::as_str).collect::<Vec<_>>())?;
    }
    Ok(pool)
}

fn set_members(input: PoolMembers) -> Result<Value> {
    require_manager()?;
    require("rec_pool", &input.id, "pool")?;
    let current: Vec<Record> = db::related("rec_pool", "members", &input.id)?;
    let keep: Vec<&str> = input.members.iter().map(String::as_str).collect();
    let drop: Vec<&str> = current.iter().filter_map(|m| text(m, "id")).filter(|id| !keep.contains(id)).collect();
    if !drop.is_empty() {
        db::unrelate::<Record>("rec_pool", "members", &input.id, &drop)?;
    }
    if !keep.is_empty() {
        db::relate::<Record>("rec_pool", "members", &input.id, &keep)?;
    }
    Ok(json!({ "members": keep.len() }))
}

// ---- editing library records ------------------------------------------------------------------------------

#[derive(Deserialize)]
struct Change {
    id: String,
    #[serde(flatten)]
    fields: Record,
}

fn edit(model: &str, allowed: &[&str], input: Change) -> Result<Record> {
    require_manager()?;
    require(model, &input.id, "record")?;
    let mut data = serde_json::Map::new();
    for field in allowed {
        if let Some(value) = input.fields.get(*field) {
            data.insert((*field).into(), value.clone());
        }
    }
    if data.is_empty() {
        return Err(Error::msg("nothing to change"));
    }
    if let Some(name) = data.get("name").and_then(Value::as_str) {
        if name.trim().is_empty() {
            return Err(Error::msg("a name is needed"));
        }
        let clash = db::find::<Record>(model).filter("name", name).first()?;
        if clash.is_some_and(|c| text(&c, "id") != Some(input.id.as_str())) {
            return Err(Error::msg("another record has this name"));
        }
    }
    db::update(model, &input.id, &Value::Object(data))?.ok_or_else(|| Error::msg("the record is gone"))
}

/// Offer templates name their terms in up to four slots (a form has no multi-select).
#[derive(Deserialize)]
struct SlottedTemplate {
    name: String,
    #[serde(default)]
    intro: Option<String>,
    #[serde(default)]
    term_1: Option<String>,
    #[serde(default)]
    term_2: Option<String>,
    #[serde(default)]
    term_3: Option<String>,
    #[serde(default)]
    term_4: Option<String>,
}

fn slotted_template(input: SlottedTemplate) -> Result<Record> {
    require_manager()?;
    let terms: Vec<String> = [input.term_1, input.term_2, input.term_3, input.term_4].into_iter().flatten().filter(|t| !t.is_empty()).collect();
    for term in &terms {
        require("rec_offer_term", term, "offer term")?;
    }
    if db::count("rec_offer_template", Filter::eq("name", input.name.as_str()))? > 0 {
        return Err(Error::msg("there is already an offer template with this name"));
    }
    db::create("rec_offer_template", &json!({ "name": input.name, "intro": input.intro, "terms": terms, "is_active": true }))
}

handler! {
    /// Read a resume's text for emails, phones and the skills it mentions (recruiters).
    fn read_resume_text(input: Resume) -> Record {
        read_resume(input)
    }

    /// An email that is an application (mail connector with a recruiter account).
    fn ingest_application_email(input: Mail) -> Value {
        ingest(input)
    }

    fn create_interviewer_pool(input: NewPool) -> Record {
        make_pool(input)
    }

    fn set_pool_members(input: PoolMembers) -> Value {
        set_members(input)
    }

    fn list_interviewer_pools(_: Empty) -> Vec<Record> {
        require_recruiter()?;
        db::find("rec_pool").order_by("name").limit(200).all()
    }

    fn retire_interviewer_pool(input: Id) -> Record {
        require_manager()?;
        db::update("rec_pool", &input.id, &json!({ "is_active": false }))?.ok_or_else(|| Error::msg("there is no such pool"))
    }

    fn update_rec_source(input: Change) -> Record {
        edit("rec_source", &["name", "is_active"], input)
    }

    fn get_rec_source(input: Id) -> Option<Record> {
        require_recruiter()?;
        db::get("rec_source", &input.id)
    }

    fn update_opening_template(input: Change) -> Record {
        edit("rec_opening_template", &["name", "title", "description", "seats", "salary_min", "salary_max", "currency", "is_active"], input)
    }

    fn get_opening_template(input: Id) -> Option<Record> {
        require_recruiter()?;
        db::get("rec_opening_template", &input.id)
    }

    fn update_offer_term(input: Change) -> Record {
        edit("rec_offer_term", &["name", "body", "is_active"], input)
    }

    fn get_offer_term(input: Id) -> Option<Record> {
        require_recruiter()?;
        db::get("rec_offer_term", &input.id)
    }

    fn update_offer_template(input: Change) -> Record {
        edit("rec_offer_template", &["name", "intro", "terms", "is_active"], input)
    }

    fn get_offer_template(input: Id) -> Option<Record> {
        require_recruiter()?;
        db::get("rec_offer_template", &input.id)
    }

    fn create_offer_template_form(input: SlottedTemplate) -> Record {
        slotted_template(input)
    }
}
