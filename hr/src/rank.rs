//! Rank: who may report to whom. A rank comes from a grade (see `grade`): the grade written on the terms,
//! else the grade of the person's position, else the default grade of their job. Without a rank on either
//! side nothing is checked, so data without grades keeps working.

use aether_sdk::db::{Filter, Walk};
use aether_sdk::prelude::*;

use crate::common::{text, Record};

/// The rank of a grade code.
fn grade_rank(code: &str) -> Result<Option<i64>> {
    Ok(db::find::<Record>("grade").filter("code", code).first()?.and_then(|g| g.get("rank").and_then(Value::as_i64)))
}

/// A grade a job would have, before it is saved: `(job id, grade code)`.
pub type JobGrade<'a> = (&'a str, &'a str);

fn same_record(a: &str, b: &str) -> bool {
    a.rsplit_once(':').map_or(a, |(_, key)| key) == b.rsplit_once(':').map_or(b, |(_, key)| key)
}

fn job_grade(job: &str, pending: Option<JobGrade>) -> Result<Option<String>> {
    if let Some((id, grade)) = pending {
        if same_record(id, job) {
            return Ok(Some(grade).filter(|g| !g.is_empty()).map(str::to_string));
        }
    }
    Ok(db::get::<Record>("job", job)?.and_then(|j| text(&j, "grade").filter(|g| !g.is_empty()).map(str::to_string)))
}

/// The rank of a grade, else of a position's grade, else of a job's default grade.
pub fn rank_of(grade: Option<&str>, position: Option<&str>, job: Option<&str>) -> Result<Option<i64>> {
    rank_of_pending(grade, position, job, None)
}

/// As [`rank_of`], reading one job as if its default grade were already `pending`.
pub fn rank_of_pending(grade: Option<&str>, position: Option<&str>, job: Option<&str>, pending: Option<JobGrade>) -> Result<Option<i64>> {
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
            grade = job_grade(job, pending)?;
        }
    }
    match grade {
        Some(code) => grade_rank(&code),
        None => Ok(None),
    }
}

/// The rank of a position record.
pub fn position_rank(position: &Record) -> Result<Option<i64>> {
    position_rank_pending(position, None)
}

pub fn position_rank_pending(position: &Record, pending: Option<JobGrade>) -> Result<Option<i64>> {
    rank_of_pending(text(position, "grade"), None, text(position, "job"), pending)
}

/// The rank of a person now: the grade on their current terms, else their position's, else their job's.
pub fn employee_rank(employee: &Record) -> Result<Option<i64>> {
    employee_rank_pending(employee, None)
}

pub fn employee_rank_pending(employee: &Record, pending: Option<JobGrade>) -> Result<Option<i64>> {
    let id = crate::common::id_of(employee)?;
    let terms = crate::employment::current_terms(id)?;
    let grade = terms.as_ref().and_then(|t| text(t, "grade")).map(str::to_string);
    rank_of_pending(grade.as_deref(), text(employee, "position"), text(employee, "job"), pending)
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

/// The position a new position of `job` (and `grade`) in `department` should report to, when the job reports to another
/// job: a position of the nearest job above, preferring the same department, then the departments above it. A candidate
/// whose rank the rank rule refuses is passed over for the next one, so a usable boss is never hidden behind a bad one.
pub fn default_parent(job: &str, department: &str, grade: Option<&str>) -> Result<Option<String>> {
    let bare = |id: &str| id.rsplit_once(':').map_or(id, |(_, key)| key).to_string();
    let mut departments = vec![bare(department)];
    let up: Vec<Record> = db::tree("department", "parent", department).walk(Walk::Up).all()?;
    departments.extend(up.iter().filter_map(|d| text(d, "id")).map(bare));
    let under = rank_of(grade, None, Some(job))?;
    for boss_job in job_ancestors(job)? {
        let positions: Vec<Record> = db::find::<Record>("position").filter("job", boss_job.as_str()).limit(500).all()?;
        let open: Vec<&Record> = positions.iter().filter(|p| text(p, "status") != Some("frozen")).collect();
        let fits = |candidate: &Record| -> Result<bool> { allowed(position_rank(candidate)?, under) };
        for dep in &departments {
            for candidate in open.iter().filter(|p| text(p, "department").is_some_and(|d| bare(d) == *dep)) {
                if fits(candidate)? {
                    return Ok(text(candidate, "id").map(str::to_string));
                }
            }
        }
    }
    Ok(None)
}

/// Would giving `job` the default grade `grade` break a reporting line that holds today? Looks at the people in the job
/// (and the positions of it) against their managers and the people who report to them, with the new grade in place. A
/// line that already carries an approved exception, and any pair whose rank is unknown, is left alone.
pub fn check_job_grade(job: &str, grade: Option<&str>) -> Result<()> {
    let pending: JobGrade = (job, grade.unwrap_or_default());
    let rule = explain_rule()?;
    let present = crate::rules::status::PRESENT.to_vec();

    let people: Vec<Record> = db::find::<Record>("employee")
        .matching(Filter::eq("job", job).and(Filter::one_of("status", present.clone())))
        .limit(1000)
        .all()?;
    let has_exception = |employee: &Record| -> Result<bool> {
        let terms = crate::employment::current_terms(crate::common::id_of(employee)?)?;
        Ok(terms.is_some_and(|t| text(&t, "rank_override_reason").is_some_and(|r| !r.trim().is_empty())))
    };
    for person in &people {
        let id = crate::common::id_of(person)?;
        let mine = employee_rank_pending(person, Some(pending))?;
        if let Some(manager) = text(person, "manager").filter(|m| !m.is_empty()) {
            if let Some(boss) = db::get::<Record>("employee", manager)? {
                if !allowed(employee_rank_pending(&boss, Some(pending))?, mine)? && !has_exception(person)? {
                    return Err(Error::msg(format!(
                        "{rule}: that grade would put {} above their manager {}; change the manager or set the rank rule aside on their terms first",
                        text(person, "display_name").unwrap_or("someone"),
                        text(&boss, "display_name").unwrap_or("someone"),
                    )));
                }
            }
        }
        let reports: Vec<Record> = db::find::<Record>("employee")
            .matching(Filter::eq("manager", id).and(Filter::one_of("status", present.clone())))
            .limit(1000)
            .all()?;
        for report in &reports {
            if !allowed(mine, employee_rank_pending(report, Some(pending))?)? && !has_exception(report)? {
                return Err(Error::msg(format!(
                    "{rule}: that grade would put {} below their report {}; change the grade or the reporting line first",
                    text(person, "display_name").unwrap_or("someone"),
                    text(report, "display_name").unwrap_or("someone"),
                )));
            }
        }
    }

    // Positions of this job, and the positions that report to them.
    let positions: Vec<Record> = db::find::<Record>("position").filter("job", job).limit(1000).all()?;
    for position in &positions {
        let mine = position_rank_pending(position, Some(pending))?;
        if let Some(parent) = text(position, "parent").filter(|p| !p.is_empty()) {
            if let Some(boss) = db::get::<Record>("position", parent)? {
                if !allowed(position_rank_pending(&boss, Some(pending))?, mine)? {
                    return Err(Error::msg(format!(
                        "{rule}: that grade would put the position {} above the position it reports to ({})",
                        text(position, "name").unwrap_or("?"),
                        text(&boss, "name").unwrap_or("?"),
                    )));
                }
            }
        }
        let below: Vec<Record> = db::find::<Record>("position").filter("parent", crate::common::id_of(position)?).limit(1000).all()?;
        for child in &below {
            if !allowed(mine, position_rank_pending(child, Some(pending))?)? {
                return Err(Error::msg(format!(
                    "{rule}: that grade would put the position {} below one that reports to it ({})",
                    text(position, "name").unwrap_or("?"),
                    text(child, "name").unwrap_or("?"),
                )));
            }
        }
    }
    Ok(())
}
