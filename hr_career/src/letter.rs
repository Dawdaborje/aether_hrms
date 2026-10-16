//! Letters from templates. A template is text with `{{placeholders}}`; making a letter fills every one from the
//! change and the person, and fails if any has no value, so a letter never goes out with a hole in it.

use aether_sdk::prelude::*;

use crate::common::{decimal_of, employee, pick, require, require_admin, text, Record};
use crate::rules::render;

#[derive(Deserialize)]
struct Make {
    change: String,
    template: String,
}

fn new_template(input: Record) -> Result<Record> {
    require_admin()?;
    let data = pick(&input, &["code", "name", "kind", "body", "is_active"]);
    let body = text(&data, "body").ok_or_else(|| Error::msg("a template needs a body"))?;
    // A broken placeholder is caught now, not when the first letter is made.
    render(body, &[]).or_else(|e| if e.to_string().contains("not closed") { Err(e) } else { Ok(String::new()) })?;
    db::create("career_letter_template", &data).map_err(|e| e.or("could not create the template (the code may be taken)"))
}

fn name_of(plugin_fn: &str, id: Option<&str>) -> Result<String> {
    let Some(id) = id else { return Ok(String::new()) };
    let found: Option<Record> = plugins::call("hr", plugin_fn, &json!({ "id": id }))?;
    Ok(found.and_then(|r| text(&r, "name").map(str::to_string)).unwrap_or_default())
}

fn make(input: Make) -> Result<Record> {
    require_admin()?;
    let change = require("career_change", &input.change, "career change")?;
    if text(&change, "state") != Some("approved") {
        return Err(Error::msg("a letter is made for an approved change"));
    }
    let template = db::find::<Record>("career_letter_template").filter("code", input.template.as_str()).first()?.ok_or_else(|| Error::msg("there is no such template"))?;
    let kind = text(&change, "kind").unwrap_or_default();
    if !matches!(text(&template, "kind"), Some("any")) && text(&template, "kind") != Some(kind) {
        return Err(Error::msg("this template is for another kind of change"));
    }
    let person = employee(text(&change, "employee").unwrap_or_default())?;
    let money = |field: &str| -> Result<String> { Ok(decimal_of(&change, field)?.map(|d| d.to_string()).unwrap_or_default()) };
    let values = [
        ("employee", text(&person, "display_name").unwrap_or_default().to_string()),
        ("employee_no", text(&person, "employee_no").unwrap_or_default().to_string()),
        ("kind", kind.replace('_', " ")),
        ("reference", text(&change, "reference").unwrap_or_default().to_string()),
        ("effective_date", text(&change, "effective_date").unwrap_or_default().to_string()),
        ("reason", text(&change, "reason").unwrap_or_default().to_string()),
        ("new_grade", text(&change, "grade").unwrap_or_default().to_string()),
        ("old_grade", text(&change, "from_grade").unwrap_or_default().to_string()),
        ("new_wage", money("wage")?),
        ("old_wage", money("from_wage")?),
        ("new_job", name_of("get_job", text(&change, "job"))?),
        ("new_department", name_of("get_department", text(&change, "department"))?),
    ];
    let letter = render(text(&template, "body").unwrap_or_default(), &values)?;
    db::update("career_change", &input.change, &json!({ "letter": letter }))?.ok_or_else(|| Error::msg("the change is gone"))
}

#[derive(Deserialize)]
struct Issue {
    employee: String,
    template: String,
    #[serde(default)]
    issued_on: Option<String>,
    /// Further `{{placeholders}}` the template uses, such as `last_day` on a relieving letter.
    #[serde(default)]
    values: std::collections::BTreeMap<String, String>,
    /// Shortcut for the usual extra value of a relieving or experience letter.
    #[serde(default)]
    last_day: Option<String>,
}

/// An appointment, relieving or experience letter for a person, from a template of that kind.
fn issue(input: Issue) -> Result<Record> {
    require_admin()?;
    let person = employee(&input.employee)?;
    let by_code = db::find::<Record>("career_letter_template").filter("code", input.template.as_str()).first()?;
    let template = match by_code {
        Some(t) => t,
        None => require("career_letter_template", &input.template, "template")?,
    };
    let kind = text(&template, "kind").unwrap_or_default();
    if !matches!(kind, "appointment" | "relieving" | "experience" | "any") {
        return Err(Error::msg("this template is for a career change; make it from the change"));
    }
    let issued_on = match &input.issued_on {
        Some(day) => aether_sdk::dates::format_date(aether_sdk::dates::parse_date(day)?),
        None => aether_sdk::dates::format_date(crate::common::today_date()?),
    };
    let mut values = vec![
        ("employee", text(&person, "display_name").unwrap_or_default().to_string()),
        ("employee_no", text(&person, "employee_no").unwrap_or_default().to_string()),
        ("hire_date", text(&person, "hire_date").unwrap_or_default().to_string()),
        ("job", name_of("get_job", text(&person, "job"))?),
        ("department", name_of("get_department", text(&person, "department"))?),
        ("issue_date", issued_on.clone()),
    ];
    let mut input_values = input.values;
    if let Some(day) = input.last_day.filter(|d| !d.is_empty()) {
        input_values.insert("last_day".into(), day);
    }
    let extra: Vec<(String, String)> = input_values.into_iter().collect();
    values.extend(extra.iter().map(|(k, v)| (k.as_str(), v.clone())));
    let body = render(text(&template, "body").unwrap_or_default(), &values)?;
    let data = json!({
        "reference": crate::common::next_number("letter", "LTR-", 6)?,
        "employee": input.employee, "template": text(&template, "code").unwrap_or_default(), "kind": kind,
        "issued_on": issued_on, "body": body,
    });
    db::create("career_letter", &data).map_err(|e| e.or("could not keep the letter"))
}

#[derive(Deserialize)]
struct Accept {
    id: String,
}

/// The person confirms they received and accept their letter. Only they can; it is recorded once.
fn accept(input: Accept) -> Result<Record> {
    let letter = require("career_letter", &input.id, "letter")?;
    let me = crate::common::my_employee()?.ok_or_else(|| Error::msg("you are not an employee here"))?;
    if text(&letter, "employee") != text(&me, "id") {
        return Err(Error::msg("a letter is accepted by the person it is addressed to"));
    }
    if text(&letter, "accepted_on").is_some() {
        return Err(Error::msg("this letter is already accepted"));
    }
    let today = aether_sdk::dates::format_date(crate::common::today_date()?);
    let by = crate::common::actor()?;
    db::update("career_letter", &input.id, &json!({ "accepted_on": today, "accepted_by": by }))?.ok_or_else(|| Error::msg("the letter is gone"))
}

handler! {
    /// The addressed person accepts their letter (an acknowledgement, not a cryptographic signature).
    fn accept_employee_letter(input: Accept) -> Record {
        accept(input)
    }

    /// Issue an appointment, relieving or experience letter to a person; every placeholder must have a value.
    fn issue_employee_letter(input: Issue) -> Record {
        issue(input)
    }

    fn my_letters(_: Empty) -> Vec<Record> {
        let Some(me) = crate::common::my_employee()? else { return Ok(Vec::new()) };
        db::find("career_letter").filter("employee", text(&me, "id").unwrap_or_default()).order_by("-issued_on").limit(100).all()
    }

    fn list_employee_letters(input: Option<Record>) -> Vec<Record> {
        let mut q = db::find::<Record>("career_letter");
        if let Some(e) = input.as_ref().and_then(|i| text(i, "employee")) {
            q = q.filter("employee", e);
        }
        q.order_by("-issued_on").limit(200).all()
    }

    fn create_letter_template(input: Record) -> Record {
        new_template(input)
    }

    fn list_letter_templates(_: Empty) -> Vec<Record> {
        db::find("career_letter_template").order_by("code").limit(200).all()
    }

    /// Fill a template from an approved change and keep the text on the change.
    fn make_career_letter(input: Make) -> Record {
        make(input)
    }
}
