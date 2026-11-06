//! Rank: who may report to whom. A rank comes from a grade (see `grade`): the grade written on the terms,
//! else the grade of the person's position, else the default grade of their job. Without a rank on either
//! side nothing is checked, so data without grades keeps working.

use aether_sdk::db::Walk;
use aether_sdk::prelude::*;

use crate::common::{text, Record};

/// The rank of a grade code.
fn grade_rank(code: &str) -> Result<Option<i64>> {
    Ok(db::find::<Record>("grade").filter("code", code).first()?.and_then(|g| g.get("rank").and_then(Value::as_i64)))
}

fn job_grade(job: &str) -> Result<Option<String>> {
    Ok(db::get::<Record>("job", job)?.and_then(|j| text(&j, "grade").filter(|g| !g.is_empty()).map(str::to_string)))
}

/// The rank of a grade, else of a position's grade, else of a job's default grade.
pub fn rank_of(grade: Option<&str>, position: Option<&str>, job: Option<&str>) -> Result<Option<i64>> {
    let mut grade = grade.filter(|g| !g.is_empty()).map(str::to_string);
    let mut job = job.map(str::to_string);
    if grade.is_none() {
        if let Some(position) = position.filter(|p| !p.is_empty()) {
            if let Some(row) = db::get::<Record>("position", position)? {
                grade = text(&row, "grade").filter(|g| !g.is_empty()).map(str::to_string);
                if job.is_none() {
                    job = text(&row, "job").map(str::to_string);
                }
            }
        }
    }
    if grade.is_none() {
        if let Some(job) = job.as_deref().filter(|j| !j.is_empty()) {
            grade = job_grade(job)?;
        }
    }
    match grade {
        Some(code) => grade_rank(&code),
        None => Ok(None),
    }
}

/// The rank of a position record.
pub fn position_rank(position: &Record) -> Result<Option<i64>> {
    rank_of(text(position, "grade"), None, text(position, "job"))
}

/// The rank of a person now: the grade on their current terms, else their position's, else their job's.
pub fn employee_rank(employee: &Record) -> Result<Option<i64>> {
    let id = crate::common::id_of(employee)?;
    let terms = crate::employment::current_terms(id)?;
    let grade = terms.as_ref().and_then(|t| text(t, "grade")).map(str::to_string);
    rank_of(grade.as_deref(), text(employee, "position"), text(employee, "job"))
}

/// `strict` (the default): a manager ranks above. `equal_ok`: a manager may rank equal.
fn equal_ok() -> Result<bool> {
    Ok(crate::settings::current()?.get("manager_rank_rule").and_then(Value::as_str) == Some("equal_ok"))
}

/// May someone of rank `boss` be over someone of rank `under`? Unknown ranks pass.
pub fn allowed(boss: Option<i64>, under: Option<i64>) -> Result<bool> {
    let (Some(boss), Some(under)) = (boss, under) else { return Ok(true) };
    Ok(if equal_ok()? { boss >= under } else { boss > under })
}

pub fn explain_rule() -> Result<&'static str> {
    Ok(if equal_ok()? { "a manager cannot rank below the people who report to them" } else { "a manager must rank above the people who report to them" })
}

pub fn is_hr_manager() -> bool {
    context::current().is_ok_and(|who| who.has_role("hr.hr_manager"))
}

/// The jobs above `job`, nearest first.
pub fn job_ancestors(job: &str) -> Result<Vec<String>> {
    let rows: Vec<Record> = db::tree("job", "parent", job).walk(Walk::Up).all()?;
    Ok(rows.iter().filter_map(|r| text(r, "id").map(str::to_string)).collect())
}

/// The position a new position of `job` in `department` should report to, when the job reports to another job:
/// a position of the nearest job above, preferring the same department, then the departments above it.
pub fn default_parent(job: &str, department: &str) -> Result<Option<String>> {
    let bare = |id: &str| id.rsplit_once(':').map_or(id, |(_, key)| key).to_string();
    let mut departments = vec![bare(department)];
    let up: Vec<Record> = db::tree("department", "parent", department).walk(Walk::Up).all()?;
    departments.extend(up.iter().filter_map(|d| text(d, "id")).map(bare));
    for boss_job in job_ancestors(job)? {
        let positions: Vec<Record> = db::find::<Record>("position").filter("job", boss_job.as_str()).limit(500).all()?;
        let open: Vec<&Record> = positions.iter().filter(|p| text(p, "status") != Some("frozen")).collect();
        for dep in &departments {
            if let Some(found) = open.iter().find(|p| text(p, "department").is_some_and(|d| bare(d) == *dep)) {
                return Ok(text(found, "id").map(str::to_string));
            }
        }
    }
    Ok(None)
}
