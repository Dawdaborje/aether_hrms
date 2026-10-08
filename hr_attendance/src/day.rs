//! The derived day: one record per person and date, worked out by `rules::compute` and rewritten
//! only when what it was worked out from has changed.

use aether_sdk::dates::{format_date, format_datetime, parse_date, parse_datetime, Duration, NaiveDate, NaiveDateTime};
use aether_sdk::db::Filter;
use aether_sdk::prelude::*;

use crate::common::{day_kind, employed_on, employee, id_of, is_admin, leave_cover, my_employee, require_admin, text, today_date, Record};
use crate::rules::{compute, inputs_hash, Direction, Facts, Punch, Status};
use crate::rules::DayKind;
use crate::setup::{assignment_on, rotation_day_off, shift_of};

#[derive(Deserialize)]
struct Range {
    #[serde(default)]
    employee: Option<String>,
    from: String,
    to: String,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Deserialize)]
struct OnDay {
    employee: String,
    date: String,
}

#[derive(Deserialize)]
struct Override {
    employee: String,
    date: String,
    status: String,
    reason: String,
}

#[derive(Deserialize)]
struct Nightly {
    #[serde(default)]
    on: Option<String>,
}

#[derive(Deserialize)]
struct Lock {
    from: String,
    to: String,
}

fn to_punch(record: &Record) -> Result<Punch> {
    Ok(Punch {
        id: id_of(record)?.to_string(),
        at: parse_datetime(text(record, "ts").ok_or_else(|| Error::msg("a punch has no time"))?)?,
        direction: Direction::parse(text(record, "direction").unwrap_or("auto")).unwrap_or(Direction::Auto),
    })
}

/// The punches of a person between two instants, without those a later punch replaced.
pub fn punches_between(employee_id: &str, from: NaiveDateTime, to: NaiveDateTime) -> Result<Vec<Punch>> {
    let rows: Vec<Record> = db::find::<Record>("att_punch")
        .matching(
            Filter::eq("employee", employee_id)
                .and(Filter::gte("ts", format_datetime(from)))
                .and(Filter::lte("ts", format_datetime(to))),
        )
        .order_by("ts")
        .limit(500)
        .all()?;
    let replaced: Vec<&str> = rows.iter().filter_map(|r| text(r, "supersedes")).collect();
    rows.iter()
        .filter(|r| text(r, "id").is_none_or(|id| !replaced.contains(&id)))
        .map(to_punch)
        .collect()
}

/// Everything a day is worked out from, and the shift record it used.
pub fn gather(person: &Record, date: NaiveDate) -> Result<(Facts, Option<Record>)> {
    let person_id = id_of(person)?;
    let assignment = assignment_on(person_id, date)?;
    let (shift, shift_record) = match assignment {
        Some((_, record)) => (Some(shift_of(&record)?), Some(record)),
        None => (None, None),
    };
    let (from, to) = match &shift {
        Some(shift) => {
            let window = shift.window(date);
            (window.from, window.to)
        }
        None => (date.and_hms_opt(0, 0, 0).unwrap_or_default(), date.and_hms_opt(23, 59, 59).unwrap_or_default()),
    };
    let mut punches = punches_between(person_id, from, to)?;
    let mut kind = day_kind(person, date)?;
    if shift.is_none() {
        // With no shift the window is the calendar day, so a punch that really ends a neighbouring
        // night shift (its morning out) must not be counted twice: leave out what a neighbour's
        // shift window already holds.
        for neighbour in [date - Duration::days(1), date + Duration::days(1)] {
            if let Some((_, record)) = assignment_on(person_id, neighbour)? {
                let window = shift_of(&record)?.window(neighbour);
                punches.retain(|p| !(p.at >= window.from && p.at <= window.to));
            }
        }
        // A day off in the person's rotation is a weekly off (a holiday stays a holiday).
        if kind == DayKind::Working && rotation_day_off(person_id, date)? {
            kind = DayKind::WeeklyOff;
        }
    }
    let facts = Facts {
        work_date: date,
        shift,
        kind,
        leave: leave_cover(person_id, date)?,
        punches,
    };
    Ok((facts, shift_record))
}

/// The stored day of a person and date, if any.
pub fn stored(employee_id: &str, date: NaiveDate) -> Result<Option<Record>> {
    db::find::<Record>("att_day").filter("employee", employee_id).filter("work_date", format_date(date).as_str()).first()
}

fn write_day(person_id: &str, date: NaiveDate, day: &crate::rules::Day, shift: Option<&Record>, hash: &str) -> Result<Record> {
    let review: Vec<&str> = day.review.iter().map(|r| r.as_str()).collect();
    let mut data = json!({
        "status": day.status.as_str(), "worked_min": day.worked, "break_min": day.breaks, "scheduled_min": day.scheduled,
        "late_min": day.late, "early_exit_min": day.early_exit, "overtime_min": day.overtime,
        "is_late": day.is_late, "is_early_exit": day.is_early_exit, "review": review.join(","), "inputs_hash": hash,
    });
    data["first_in"] = day.first_in.map_or(Value::Null, |t| json!(format_datetime(t)));
    data["last_out"] = day.last_out.map_or(Value::Null, |t| json!(format_datetime(t)));
    data["shift"] = shift.and_then(|s| text(s, "id")).map_or(Value::Null, |id| json!(id));
    match stored(person_id, date)? {
        Some(existing) => db::update::<Record>("att_day", id_of(&existing)?, &data)?.ok_or_else(|| Error::msg("the day is gone")),
        None => {
            data["employee"] = json!(person_id);
            data["work_date"] = json!(format_date(date));
            db::create("att_day", &data)
        }
    }
}

/// Work the day out again and store it if anything it depends on has changed. A locked day is
/// never touched, and a manager's override is kept. `None`: there is nothing to say about the day.
pub fn recompute(person: &Record, date: NaiveDate) -> Result<Option<Record>> {
    let person_id = id_of(person)?;
    let existing = stored(person_id, date)?;
    if existing.as_ref().is_some_and(|d| d.get("locked") == Some(&json!(true))) {
        return Ok(existing);
    }
    let (facts, shift_record) = gather(person, date)?;
    let Some(day) = compute(&facts) else { return Ok(existing) };
    let hash = inputs_hash(&facts);
    if existing.as_ref().is_some_and(|d| text(d, "inputs_hash") == Some(hash.as_str())) {
        return Ok(existing);
    }
    let written = write_day(person_id, date, &day, shift_record.as_ref(), &hash)?;
    crate::overtime::settle(person_id, date, facts.kind, day.overtime)?;
    if !day.review.is_empty() {
        events::emit("day_needs_review", &json!({ "employee": person_id, "date": format_date(date), "review": text(&written, "review") }))?;
    }
    Ok(Some(written))
}

/// A working day with nobody there: absent, once the day is over. Only for people who were meant to be at work.
fn mark_absent(person: &Record, date: NaiveDate, now: NaiveDateTime) -> Result<bool> {
    let person_id = id_of(person)?;
    if !employed_on(person, date) || stored(person_id, date)?.is_some() {
        return Ok(false);
    }
    let (facts, shift_record) = gather(person, date)?;
    let Some(shift) = &facts.shift else { return Ok(false) };
    if shift.window(date).to > now {
        return Ok(false);
    }
    if compute(&facts).is_some() {
        // Punches, leave, a holiday or a weekly off: `recompute` handles it.
        recompute(person, date)?;
        return Ok(false);
    }
    let absent = crate::rules::Day {
        status: Status::Absent,
        first_in: None,
        last_out: None,
        worked: 0,
        breaks: 0,
        scheduled: shift.scheduled_minutes(),
        late: 0,
        early_exit: 0,
        overtime: 0,
        is_late: false,
        is_early_exit: false,
        review: Vec::new(),
    };
    write_day(person_id, date, &absent, shift_record.as_ref(), &inputs_hash(&facts))?;
    Ok(true)
}

fn present_employees() -> Result<Vec<Record>> {
    let mut all = Vec::new();
    let mut offset = 0u32;
    loop {
        let page: Vec<Record> = plugins::call("hr", "list_employees", &json!({ "limit": 200, "offset": offset }))?;
        let count = page.len();
        all.extend(page.into_iter().filter(|e| matches!(text(e, "status"), Some("active" | "probation" | "on_leave" | "suspended") | None)));
        if count < 200 {
            break;
        }
        offset += count as u32;
    }
    Ok(all)
}

/// Nightly: settle yesterday and the day before (late punches and corrections), and mark the
/// people who should have been at work and were not. Safe to run again.
fn nightly(on: Option<NaiveDate>) -> Result<Value> {
    let today = today_date()?;
    let now = crate::common::now_utc()?;
    let (mut recomputed, mut absent, mut failed) = (0u64, 0u64, 0u64);
    // A named day (a backfill, by an administrator); otherwise the last two.
    let days: Vec<NaiveDate> = match on {
        Some(day) => {
            require_admin()?;
            vec![day]
        }
        None => vec![today - Duration::days(2), today - Duration::days(1)],
    };
    for person in present_employees()? {
        for date in days.iter().copied() {
            // One person's failure does not stop the others.
            let outcome: Result<()> = (|| {
                if employed_on(&person, date) {
                    let before = stored(id_of(&person)?, date)?.and_then(|d| text(&d, "inputs_hash").map(str::to_string));
                    let after = recompute(&person, date)?.and_then(|d| text(&d, "inputs_hash").map(str::to_string));
                    if after.is_some() && before != after {
                        recomputed += 1;
                    }
                    if mark_absent(&person, date, now)? {
                        absent += 1;
                    }
                }
                Ok(())
            })();
            if let Err(error) = outcome {
                failed += 1;
                log::error(&format!("attendance for {} on {date}: {error}", text(&person, "employee_no").unwrap_or("?")));
            }
        }
    }
    Ok(json!({ "recomputed": recomputed, "marked_absent": absent, "failed": failed }))
}

fn days(input: Range) -> Result<Vec<Record>> {
    let (from, to) = (parse_date(&input.from)?, parse_date(&input.to)?);
    if to < from || (to - from).num_days() > 400 {
        return Err(Error::msg("ask for at most about a year at a time, in order"));
    }
    let mut filter = Filter::gte("work_date", format_date(from)).and(Filter::lte("work_date", format_date(to)));
    if let Some(employee) = &input.employee {
        filter = filter.and(Filter::eq("employee", employee.as_str()));
    }
    if let Some(status) = &input.status {
        filter = filter.and(Filter::eq("status", status.as_str()));
    }
    db::find::<Record>("att_day").matching(filter).order_by("-work_date").limit(1000).all()
}

fn override_day(input: Override) -> Result<Record> {
    require_admin()?;
    if input.reason.trim().is_empty() {
        return Err(Error::msg("say why the day is overridden"));
    }
    let person = employee(&input.employee)?;
    let date = parse_date(&input.date)?;
    let me = my_employee()?;
    if me.as_ref().is_some_and(|m| text(m, "id") == text(&person, "id")) {
        return Err(Error::msg("nobody overrides their own attendance"));
    }
    let existing = match stored(id_of(&person)?, date)? {
        Some(day) => day,
        None => {
            let (facts, shift) = gather(&person, date)?;
            let blank = compute(&facts).unwrap_or(crate::rules::Day {
                status: Status::Unscheduled,
                first_in: None,
                last_out: None,
                worked: 0,
                breaks: 0,
                scheduled: 0,
                late: 0,
                early_exit: 0,
                overtime: 0,
                is_late: false,
                is_early_exit: false,
                review: Vec::new(),
            });
            write_day(id_of(&person)?, date, &blank, shift.as_ref(), &inputs_hash(&facts))?
        }
    };
    if existing.get("locked") == Some(&json!(true)) {
        return Err(Error::msg("this day is closed for payroll"));
    }
    db::update::<Record>(
        "att_day",
        id_of(&existing)?,
        &json!({ "override_status": input.status, "override_reason": input.reason, "override_by": me.and_then(|m| text(&m, "id").map(str::to_string)) }),
    )?
    .ok_or_else(|| Error::msg("the day is gone"))
}

/// Close a period for payroll: its days are never recomputed or overridden again.
fn lock(input: Lock) -> Result<Value> {
    require_admin()?;
    let (from, to) = (parse_date(&input.from)?, parse_date(&input.to)?);
    if to < from {
        return Err(Error::msg("the period ends before it starts"));
    }
    if to >= today_date()? {
        return Err(Error::msg("only days that are over can be closed"));
    }
    let rows: Vec<Record> = db::find::<Record>("att_day")
        .matching(Filter::gte("work_date", format_date(from)).and(Filter::lte("work_date", format_date(to))).and(Filter::eq("locked", false)))
        .limit(5000)
        .all()?;
    for row in &rows {
        db::update::<Record>("att_day", id_of(row)?, &json!({ "locked": true }))?;
    }
    Ok(json!({ "locked": rows.len() }))
}

#[derive(Deserialize)]
struct Summary {
    employees: Vec<String>,
    from: String,
    to: String,
}

/// For payroll: overtime, lateness and the days recorded of many people in a period.
fn payroll_summary(input: Summary) -> Result<Value> {
    if !is_admin()? && !matches!(context::current()?.actor.kind, aether_sdk::context::ActorKind::System) {
        return Err(Error::msg("attendance summaries are for administrators and for payroll"));
    }
    if input.employees.is_empty() || input.employees.len() > 300 {
        return Err(Error::msg("ask for 1 to 300 people at a time"));
    }
    let (from, to) = (parse_date(&input.from)?, parse_date(&input.to)?);
    let rows = db::find::<Record>("att_day")
        .matching(Filter::one_of("employee", input.employees.clone()).and(Filter::gte("work_date", format_date(from))).and(Filter::lte("work_date", format_date(to))))
        .aggregate(
            &["employee"],
            &[("overtime", aether_sdk::db::Figure::sum("overtime_min")), ("late", aether_sdk::db::Figure::sum("late_min")), ("days", aether_sdk::db::Figure::sum("worked_min"))],
        )?;
    let mut out = serde_json::Map::new();
    let approved = crate::overtime::payroll_overtime(&input.employees, from, to)?;
    for row in rows {
        let id = row["employee"].as_str().unwrap_or_default().to_string();
        let claim = approved.get(&id).cloned().unwrap_or_else(|| json!({}));
        out.insert(id, json!({
            "overtime_minutes": row["overtime"], "late_minutes": row["late"], "worked_minutes": row["days"],
            "approved_overtime_weighted_minutes": claim.get("approved_weighted_minutes").cloned().unwrap_or(json!(0)),
            "approved_overtime_minutes": claim.get("approved_minutes").cloned().unwrap_or(json!(0)),
            "pending_overtime_minutes": claim.get("pending_minutes").cloned().unwrap_or(json!(0)),
        }));
    }
    Ok(Value::Object(out))
}

handler! {
    /// Overtime and lateness of many people within a period, for payroll.
    fn payroll_attendance_summary(input: Summary) -> Value {
        payroll_summary(input)
    }

    /// The caller's own days between two dates.
    fn my_days(input: Option<Range>) -> Vec<Record> {
        let Some(me) = my_employee()? else { return Ok(Vec::new()) };
        let today = today_date()?;
        let input = input.unwrap_or(Range { employee: None, from: format_date(today - Duration::days(30)), to: format_date(today), status: None });
        days(Range { employee: Some(id_of(&me)?.to_string()), ..input })
    }

    /// Days of the people the caller may see.
    fn list_days(input: Range) -> Vec<Record> {
        days(input)
    }

    /// Days that need a person to look at them.
    fn days_needing_review(input: Range) -> Vec<Record> {
        Ok(days(input)?.into_iter().filter(|d| text(d, "review").is_some()).collect())
    }

    /// Work a day out again (a manager for their team, an administrator for anyone).
    fn recompute_day(input: OnDay) -> Option<Record> {
        let person = employee(&input.employee)?;
        recompute(&person, parse_date(&input.date)?)
    }

    fn override_attendance_day(input: Override) -> Record {
        override_day(input)
    }

    /// Close a finished period for payroll.
    fn lock_attendance_days(input: Lock) -> Value {
        lock(input)
    }

    /// Nightly: settle recent days and mark absences. An administrator may name one day to settle.
    fn attendance_nightly(input: Option<Nightly>) -> Value {
        nightly(input.and_then(|n| n.on).map(|d| parse_date(&d)).transpose()?)
    }
}
