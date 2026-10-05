//! Departments: the tree an organization is divided into.

use std::collections::HashMap;

use aether_sdk::db::{Figure, Filter, Walk};
use aether_sdk::prelude::*;
use crate::rules::status;

use crate::common::{explain, id_of, pick, require, text, Record};

const EDITABLE: &[&str] = &["name", "code", "company", "parent", "manager", "cost_center", "description", "is_active"];

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Tree {
    /// The department to start from; the whole organization when left out.
    #[serde(default)]
    root: Option<String>,
}

#[derive(Deserialize)]
struct Change {
    id: String,
    #[serde(flatten)]
    fields: Record,
}

fn new_department(input: Record) -> Result<Record> {
    if text(&input, "name").is_none() {
        return Err(Error::msg("a department needs a name"));
    }
    let data = pick(&input, EDITABLE);
    check_references(&data, None)?;
    explain(db::create("department", &data), "could not create the department")
}

/// The manager must be an employee who works here, and the parent must exist and be active.
fn check_references(data: &Record, current: Option<&str>) -> Result<()> {
    if let Some(company) = text(data, "company") {
        require("company", company, "company")?;
    }
    if let Some(parent) = text(data, "parent") {
        if Some(parent) == current {
            return Err(Error::msg("a department cannot be its own parent"));
        }
        let parent = require("department", parent, "department")?;
        if parent.get("is_active") == Some(&json!(false)) {
            return Err(Error::msg("the parent department is closed"));
        }
    }
    if let Some(manager) = text(data, "manager") {
        let manager = require("employee", manager, "employee")?;
        let status = text(&manager, "status").unwrap_or("active");
        if !status::counts_in_headcount(status) {
            return Err(Error::msg("the manager no longer works here"));
        }
    }
    Ok(())
}

fn change_department(input: Change) -> Result<Record> {
    let existing = require("department", &input.id, "department")?;
    let data = pick(&input.fields, EDITABLE);
    if data.as_object().is_some_and(|fields| fields.is_empty()) {
        return Err(Error::msg("there is nothing to change"));
    }
    check_references(&data, Some(id_of(&existing)?))?;
    explain(
        db::update::<Record>("department", &input.id, &data),
        "could not change the department",
    )?
    .ok_or_else(|| Error::msg("the department is gone"))
}

/// Close a department. It must have no one in it and no open departments below it: close or
/// move them first, so no one is left in a department that no longer exists.
fn shut_department(input: Id) -> Result<Record> {
    let department = require("department", &input.id, "department")?;
    let id = id_of(&department)?;
    let present: Vec<&str> = status::PRESENT.to_vec();
    let staff = db::count("employee", Filter::eq("department", id).and(Filter::one_of("status", present)))?;
    if staff > 0 {
        return Err(Error::msg(format!("{staff} people still work in this department: move them first")));
    }
    let open_below = db::tree("department", "parent", id)
        .depth(1)
        .all::<Record>()?
        .iter()
        .filter(|child| child.get("is_active") != Some(&json!(false)))
        .count();
    if open_below > 0 {
        return Err(Error::msg(format!("{open_below} departments below it are still open")));
    }
    db::update::<Record>("department", id, &json!({ "is_active": false }))?.ok_or_else(|| Error::msg("the department is gone"))
}

/// People working in each department, by department id.
pub fn headcount() -> Result<HashMap<String, u64>> {
    let present: Vec<&str> = status::PRESENT.to_vec();
    let rows = db::aggregate(
        "employee",
        Filter::one_of("status", present),
        &["department"],
        &[("people", Figure::Count)],
    )?;
    Ok(rows
        .iter()
        .filter_map(|row| Some((row.get("department")?.as_str()?.to_string(), row.get("people")?.as_u64()?)))
        .collect())
}

/// The departments as a nested tree with the people in each, from `root` or from every top one.
fn build_tree(input: Tree) -> Result<Vec<Record>> {
    let all: Vec<Record> = match &input.root {
        Some(root) => db::tree("department", "parent", root).walk(Walk::Down).include_self().all()?,
        None => db::find::<Record>("department").order_by("name").limit(1000).all()?,
    };
    let counts = headcount()?;
    let mut children: HashMap<String, Vec<Record>> = HashMap::new();
    let mut tops = Vec::new();
    let ids: Vec<String> = all.iter().filter_map(|d| text(d, "id").map(str::to_string)).collect();
    for department in all {
        let parent = text(&department, "parent").map(str::to_string);
        match parent {
            // A department whose parent is outside what was asked for is a top of this tree.
            Some(parent) if ids.contains(&parent) && Some(&parent) != input.root.as_ref().filter(|r| text(&department, "id") == Some(r.as_str())) => {
                children.entry(parent).or_default().push(department);
            }
            _ => tops.push(department),
        }
    }
    fn build(department: Record, children: &mut HashMap<String, Vec<Record>>, counts: &HashMap<String, u64>) -> Record {
        let id = text(&department, "id").unwrap_or_default().to_string();
        let mut subtree = Vec::new();
        for child in children.remove(&id).unwrap_or_default() {
            subtree.push(build(child, children, counts));
        }
        let mut node = department;
        node["people"] = json!(counts.get(&id).copied().unwrap_or(0));
        node["children"] = Value::Array(subtree);
        node
    }
    Ok(tops.into_iter().map(|top| build(top, &mut children, &counts)).collect())
}

handler! {
    fn create_department(input: Record) -> Record {
        new_department(input)
    }

    fn update_department(input: Change) -> Record {
        change_department(input)
    }

    fn close_department(input: Id) -> Record {
        shut_department(input)
    }

    fn get_department(input: Id) -> Option<Record> {
        db::get("department", &input.id)
    }

    fn list_departments(_: Empty) -> Vec<Record> {
        db::find("department").order_by("name").limit(1000).all()
    }

    /// The departments as a tree, each with the people working in it.
    fn department_tree(input: Tree) -> Vec<Record> {
        build_tree(input)
    }
}
