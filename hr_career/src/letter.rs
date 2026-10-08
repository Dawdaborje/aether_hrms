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

handler! {
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
