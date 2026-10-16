//! Key result area master list, continuous feedback notes, the team skill matrix, rating distribution, and
//! picking a template for a person by their job or department.

use std::collections::BTreeMap;

use aether_sdk::dates::format_date;
use aether_sdk::db::Filter;
use aether_sdk::prelude::*;

use crate::common::{dec, id_of, int, is_admin, my_employee, require, require_admin, text, today_date, Record};

#[derive(Deserialize)]
struct Id {
    id: String,
}

// ---- key result areas -------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct NewKra {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct ChangeKra {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    is_active: Option<bool>,
}

/// When a master list of key result areas exists, a template may use only its active names.
pub fn check_kras(names: &[String]) -> Result<()> {
    let master: Vec<Record> = db::find("perf_kra").filter("is_active", true).limit(500).all()?;
    if master.is_empty() {
        return Ok(());
    }
    for name in names {
        if !master.iter().any(|k| text(k, "name") == Some(name.as_str())) {
            return Err(Error::msg(format!("`{name}` is not a key result area: add it to the list first")));
        }
    }
    Ok(())
}

fn make_kra(input: NewKra) -> Result<Record> {
    require_admin()?;
    let name = input.name.trim();
    if name.is_empty() {
        return Err(Error::msg("a key result area needs a name"));
    }
    if db::count("perf_kra", Filter::eq("name", name))? > 0 {
        return Err(Error::msg("there is already a key result area with this name"));
    }
    db::create("perf_kra", &json!({ "name": name, "description": input.description, "is_active": true }))
}

fn change_kra(input: ChangeKra) -> Result<Record> {
    require_admin()?;
    require("perf_kra", &input.id, "key result area")?;
    let mut data = serde_json::Map::new();
    if let Some(name) = input.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        let clash = db::find::<Record>("perf_kra").filter("name", name).first()?;
        if clash.is_some_and(|c| text(&c, "id") != Some(input.id.as_str())) {
            return Err(Error::msg("another key result area has this name"));
        }
        data.insert("name".into(), json!(name));
    }
    if let Some(d) = input.description {
        data.insert("description".into(), json!(d));
    }
    if let Some(a) = input.is_active {
        data.insert("is_active".into(), json!(a));
    }
    db::update("perf_kra", &input.id, &Value::Object(data))?.ok_or_else(|| Error::msg("the key result area is gone"))
}

// ---- templates by job or department -----------------------------------------------------------------------

/// The template for a person: one for their job, else one for their department, else the cycle's own.
pub fn template_for(person: &Record, fallback: &Record) -> Result<Record> {
    for (field, key) in [("job", "job"), ("department", "department")] {
        if let Some(value) = text(person, field) {
            if let Some(found) = db::find::<Record>("perf_template").filter(key, value).filter("is_active", true).first()? {
                return Ok(found);
            }
        }
    }
    Ok(fallback.clone())
}

// ---- continuous feedback ----------------------------------------------------------------------------------

#[derive(Deserialize)]
struct NewNote {
    employee: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    rating: Option<i64>,
    comment: String,
    #[serde(default)]
    shared: bool,
}

fn give_note(input: NewNote) -> Result<Record> {
    let me = my_employee()?.ok_or_else(|| Error::msg("only an employee gives feedback"))?;
    let me_id = id_of(&me)?.to_string();
    if input.employee == me_id {
        return Err(Error::msg("feedback is about someone else: use the self review for yourself"));
    }
    crate::common::employee(&input.employee)?;
    let kind = input.kind.as_deref().unwrap_or("general");
    if !matches!(kind, "praise" | "improve" | "general") {
        return Err(Error::msg("kind is praise, improve or general"));
    }
    if input.rating.is_some_and(|r| !(1..=5).contains(&r)) {
        return Err(Error::msg("a rating is 1 to 5"));
    }
    let comment = input.comment.trim();
    if comment.is_empty() || comment.len() > 4000 {
        return Err(Error::msg("write the feedback, up to 4000 characters"));
    }
    db::create(
        "perf_note",
        &json!({ "employee": input.employee, "author": me_id, "kind": kind, "rating": input.rating, "comment": comment, "shared": input.shared, "on_date": format_date(today_date()?) }),
    )
}

/// Notes about a person: all of them for the administrator and the person's manager, only shared ones for the
/// person; nobody else sees any. Authors always see their own.
fn notes_about(employee_id: &str) -> Result<Vec<Record>> {
    let person = crate::common::employee(employee_id)?;
    let me = my_employee()?;
    let me_id = me.as_ref().and_then(|m| text(m, "id")).map(str::to_string);
    let rows: Vec<Record> = db::find("perf_note").filter("employee", employee_id).order_by("-on_date").limit(300).all()?;
    let full = is_admin()? || me_id.is_some() && text(&person, "manager") == me_id.as_deref();
    let is_self = me_id.as_deref() == Some(employee_id);
    if !full && !is_self && me_id.is_none() {
        return Err(Error::msg("you may not see feedback about this person"));
    }
    Ok(rows
        .into_iter()
        .filter(|n| full || text(n, "author") == me_id.as_deref() || (is_self && n.get("shared") == Some(&json!(true))))
        .map(|mut n| {
            // The person is not told who wrote a note unless a manager or the administrator is reading.
            if !full && is_self {
                n["author"] = Value::Null;
            }
            n
        })
        .collect())
}

// ---- reports ----------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct Matrix {
    #[serde(default)]
    department: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

/// People down, skills across: each person's current level of every active skill.
fn skill_matrix(input: Matrix) -> Result<Value> {
    require_admin()?;
    let mut query = json!({ "limit": input.limit.unwrap_or(100).min(200), "with_subdepartments": true });
    if let Some(department) = &input.department {
        query["department"] = json!(department);
    }
    let people: Vec<Record> = plugins::call("hr", "list_employees", &query)?;
    let skills: Vec<Record> = db::find("perf_skill").filter("is_active", true).order_by("name").limit(200).all()?;
    let ids: Vec<&str> = people.iter().filter_map(|p| text(p, "id")).collect();
    let ratings: Vec<Record> = if ids.is_empty() { Vec::new() } else { db::find("perf_skill_rating").matching(Filter::one_of("employee", ids).and(Filter::is_empty("valid_to"))).limit(5000).all()? };
    let mut by_person: BTreeMap<&str, BTreeMap<&str, i64>> = BTreeMap::new();
    for rating in &ratings {
        if let (Some(e), Some(s)) = (text(rating, "employee"), text(rating, "skill")) {
            by_person.entry(e).or_default().insert(s, int(rating, "level"));
        }
    }
    let rows: Vec<Value> = people
        .iter()
        .map(|p| {
            let id = text(p, "id").unwrap_or_default();
            let levels: serde_json::Map<String, Value> = skills
                .iter()
                .filter_map(|s| text(s, "id").map(|sid| (sid.to_string(), json!(by_person.get(id).and_then(|m| m.get(sid)).copied()))))
                .collect();
            json!({ "employee": id, "name": p["display_name"], "levels": levels })
        })
        .collect();
    let columns: Vec<Value> = skills.iter().map(|s| json!({ "skill": s["id"], "name": s["name"], "scale_max": s["scale_max"] })).collect();
    Ok(json!({ "skills": columns, "people": rows }))
}

#[derive(Deserialize)]
struct Cycle {
    cycle: String,
}

/// How a cycle's final scores spread: five 20-point bands, the average, and how many were calibrated.
fn distribution(input: Cycle) -> Result<Value> {
    require_admin()?;
    require("perf_cycle", &input.cycle, "cycle")?;
    let rows: Vec<Record> = db::find("perf_appraisal").filter("cycle", input.cycle.as_str()).limit(2000).all()?;
    let mut bands = [0u64; 5];
    let (mut total, mut counted, mut calibrated, mut without) = (0f64, 0u64, 0u64, 0u64);
    for row in &rows {
        if row.get("calibrated") == Some(&json!(true)) {
            calibrated += 1;
        }
        match dec(row, "final_score")? {
            Some(score) => {
                let value: f64 = score.to_string().parse().unwrap_or(0.0);
                bands[((value / 20.0).floor() as usize).min(4)] += 1;
                total += value;
                counted += 1;
            }
            None => without += 1,
        }
    }
    let labels = ["0-19", "20-39", "40-59", "60-79", "80-100"];
    Ok(json!({
        "appraisals": rows.len(), "scored": counted, "without_score": without, "calibrated": calibrated,
        "average": if counted > 0 { Some((total / counted as f64 * 100.0).round() / 100.0) } else { None },
        "bands": labels.iter().zip(bands).map(|(l, c)| json!({ "band": l, "count": c })).collect::<Vec<_>>(),
    }))
}

handler! {
    /// People to pick from in the feedback form.
    fn list_employees(input: Option<Value>) -> Value {
        plugins::call("hr", "list_employees", &input.unwrap_or_else(|| json!({ "limit": 200 })))
    }

    fn create_kra(input: NewKra) -> Record {
        make_kra(input)
    }

    fn update_kra(input: ChangeKra) -> Record {
        change_kra(input)
    }

    fn get_kra(input: Id) -> Option<Record> {
        db::get("perf_kra", &input.id)
    }

    fn list_kras(_: Empty) -> Vec<Record> {
        db::find("perf_kra").order_by("name").limit(500).all()
    }

    /// Feedback about a colleague at any time, outside a cycle.
    fn give_feedback_note(input: NewNote) -> Record {
        give_note(input)
    }

    fn list_feedback_notes(input: Id) -> Vec<Record> {
        notes_about(&input.id)
    }

    /// Notes about the caller that were shared with them.
    fn my_feedback_notes(_: Empty) -> Vec<Record> {
        let Some(me) = my_employee()? else { return Ok(Vec::new()) };
        notes_about(id_of(&me)?)
    }

    fn team_skill_matrix(input: Matrix) -> Value {
        skill_matrix(input)
    }

    fn rating_distribution(input: Cycle) -> Value {
        distribution(input)
    }
}
