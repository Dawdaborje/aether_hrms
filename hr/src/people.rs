//! What surrounds an employee: dependents and skills.

use aether_sdk::prelude::*;
use aether_sdk::dates::{parse_date, parse_optional};

use crate::common::{explain, id_of, pick, require, text, Record};

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct ForEmployee {
    employee: String,
}

#[derive(Deserialize)]
struct Skills {
    employee: String,
    #[serde(default)]
    add: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
}

fn new_dependent(input: Record) -> Result<Record> {
    let employee = text(&input, "employee").ok_or_else(|| Error::msg("a dependent belongs to an employee"))?;
    require("employee", employee, "employee")?;
    if text(&input, "name").is_none() {
        return Err(Error::msg("a dependent needs a name"));
    }
    let relationship = text(&input, "relationship").ok_or_else(|| Error::msg("say how the person is related"))?;
    if !["spouse", "child", "parent", "sibling", "other"].contains(&relationship) {
        return Err(Error::msg("the relationship is spouse, child, parent, sibling or other"));
    }
    if let Some(born) = parse_optional(text(&input, "date_of_birth"))? {
        let today = parse_date(&crate::common::today()?)?;
        if born > today {
            return Err(Error::msg("the date of birth is in the future"));
        }
    }
    let data = pick(&input, &["employee", "name", "relationship", "date_of_birth", "phone", "is_beneficiary", "is_emergency_contact"]);
    explain(db::create("dependent", &data), "could not add the dependent")
}

fn set_skills(input: Skills) -> Result<Vec<Record>> {
    let employee = require("employee", &input.employee, "employee")?;
    let id = id_of(&employee)?;
    if !input.add.is_empty() {
        let to: Vec<&str> = input.add.iter().map(String::as_str).collect();
        db::relate::<Record>("employee", "skills", id, &to)?;
    }
    if !input.remove.is_empty() {
        let to: Vec<&str> = input.remove.iter().map(String::as_str).collect();
        db::unrelate::<Record>("employee", "skills", id, &to)?;
    }
    db::related("employee", "skills", id)
}

const PRIVATE: &[&str] = &[
    "legal_name", "date_of_birth", "place_of_birth", "marital_status", "nationality", "national_id", "passport_no",
    "passport_expiry", "visa_no", "work_permit_expiry", "private_email", "private_phone", "private_address",
    "emergency_name", "emergency_relation", "emergency_phone", "education_level", "study_field", "bank_account_no", "bank_name",
];

#[derive(Deserialize)]
struct PrivateChange {
    employee: String,
    #[serde(flatten)]
    fields: Record,
}

/// Change the private details of an employee, creating them the first time. The kernel's rules
/// decide who may: the person themselves and HR.
fn set_private(input: PrivateChange) -> Result<Record> {
    let employee = require("employee", &input.employee, "employee")?;
    let employee_id = crate::common::id_of(&employee)?;
    let data = pick(&input.fields, PRIVATE);
    if data.as_object().is_some_and(|fields| fields.is_empty()) {
        return Err(Error::msg("there is nothing to change"));
    }
    if let Some(born) = parse_optional(text(&data, "date_of_birth"))? {
        if born > parse_date(&crate::common::today()?)? {
            return Err(Error::msg("the date of birth is in the future"));
        }
    }
    match db::find::<Record>("employee_private").filter("employee", employee_id).first()? {
        Some(existing) => db::update::<Record>("employee_private", crate::common::id_of(&existing)?, &data)?
            .ok_or_else(|| Error::msg("the record is gone")),
        None => {
            let mut data = data;
            data["employee"] = json!(employee_id);
            explain(db::create("employee_private", &data), "could not save the private details")
        }
    }
}

handler! {
    /// The private details of an employee (only the person and HR can read them).
    fn get_private_details(input: ForEmployee) -> Option<Record> {
        db::find("employee_private").filter("employee", input.employee.as_str()).first()
    }

    fn update_private_details(input: PrivateChange) -> Record {
        set_private(input)
    }

    fn add_dependent(input: Record) -> Record {
        new_dependent(input)
    }

    fn list_dependents(input: ForEmployee) -> Vec<Record> {
        db::find("dependent").filter("employee", input.employee.as_str()).order_by("name").limit(200).all()
    }

    fn remove_dependent(input: Id) -> Option<Record> {
        db::delete("dependent", &input.id)
    }

    fn create_skill(input: Record) -> Record {
        if text(&input, "name").is_none() {
            return Err(Error::msg("a skill needs a name"));
        }
        explain(db::create("skill", &pick(&input, &["name", "category"])), "could not create the skill (the name may be taken)")
    }

    fn list_skills(_: Empty) -> Vec<Record> {
        db::find("skill").order_by("name").limit(1000).all()
    }

    /// Give or take away skills; answers with the employee's skills afterwards.
    fn update_skills(input: Skills) -> Vec<Record> {
        set_skills(input)
    }

    fn employee_skills(input: ForEmployee) -> Vec<Record> {
        db::related("employee", "skills", &input.employee)
    }

    /// Who has a skill.
    fn people_with_skill(input: Id) -> Vec<Record> {
        db::related_reverse("employee", "skills", &input.id)
    }
}
