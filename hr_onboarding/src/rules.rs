//! The rules of onboarding and offboarding, with nothing read from a database.
//!
//! What the Odoo and Frappe sources taught (see `docs/design.md`): a due date must follow the working
//! calendar and never end before it starts (Frappe rolls forward only, Odoo ignores holidays); an
//! assignee is resolved up the management chain with a bound and a loop check, and the fallback that
//! was used is kept (Odoo); status is computed from the tasks, not stored by hand (Frappe writes it
//! around its own events); a departure closes only when its checklist is clear.

use aether_sdk::dates::{Calendar, Duration, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::{Error, Result};

/// How far up the management chain an assignee is looked for.
pub const MAX_CHAIN: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Roll {
    None,
    Next,
    Previous,
}

impl Roll {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "none" => Self::None,
            "next" => Self::Next,
            "previous" => Self::Previous,
            _ => return None,
        })
    }
}

/// The last working day on or before `date` (the date itself if the calendar has none to give).
fn previous_working_day(calendar: &Calendar, mut date: NaiveDate) -> NaiveDate {
    let origin = date;
    for _ in 0..366 {
        if calendar.is_working_day(date) {
            return date;
        }
        date -= Duration::days(1);
    }
    origin
}

fn rolled(calendar: &Calendar, date: NaiveDate, roll: Roll) -> NaiveDate {
    match roll {
        Roll::None => date,
        Roll::Next => calendar.next_working_day(date),
        Roll::Previous => previous_working_day(calendar, date),
    }
}

/// When a step starts and is due, from the anchor day. Both ends follow the roll rule; the end never
/// comes before the start.
pub fn due_dates(anchor: NaiveDate, offset_days: i64, duration_days: i64, roll: Roll, calendar: &Calendar) -> Result<(NaiveDate, NaiveDate)> {
    if duration_days < 0 {
        return Err(Error::msg("a step cannot last a negative number of days"));
    }
    if offset_days.abs() > 3650 || duration_days > 3650 {
        return Err(Error::msg("a step is within ten years of its anchor"));
    }
    let raw_start = anchor + Duration::days(offset_days);
    let start = rolled(calendar, raw_start, roll);
    let end = rolled(calendar, raw_start + Duration::days(duration_days), roll);
    Ok((start, end.max(start)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssigneeKind {
    Employee,
    Manager,
    Manager2,
    User,
    Role,
    Owner,
}

impl AssigneeKind {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "employee" => Self::Employee,
            "manager" => Self::Manager,
            "manager2" => Self::Manager2,
            "user" => Self::User,
            "role" => Self::Role,
            "owner" => Self::Owner,
            _ => return None,
        })
    }
}

/// Who the people around a task are: the person it is about (if they are an employee yet), their
/// managers nearest first, whoever owns the run, and whoever started it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct People {
    pub subject: Option<Option<String>>,
    pub managers: Vec<Option<String>>,
    pub owner: Option<String>,
    pub caller: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Assignee {
    pub user: Option<String>,
    pub role: Option<String>,
    /// Which fallback was used, or why nobody was found. Empty when the first choice worked.
    pub note: String,
}

fn first_with_user(managers: &[Option<String>]) -> Option<(usize, String)> {
    managers.iter().take(MAX_CHAIN).enumerate().find_map(|(i, user)| user.clone().map(|u| (i, u)))
}

fn owner_or_caller(people: &People, why: &str) -> Assignee {
    if let Some(owner) = &people.owner {
        return Assignee { user: Some(owner.clone()), role: None, note: format!("{why}: given to the owner of the run") };
    }
    match &people.caller {
        Some(caller) => Assignee { user: Some(caller.clone()), role: None, note: format!("{why}: given to whoever started the run") },
        None => Assignee { user: None, role: None, note: format!("{why}: nobody to give it to") },
    }
}

/// Who does a task. `fixed_user` and `role` come from the step.
pub fn resolve_assignee(kind: AssigneeKind, fixed_user: Option<&str>, role: Option<&str>, people: &People) -> Assignee {
    match kind {
        AssigneeKind::User => match fixed_user {
            Some(user) => Assignee { user: Some(user.to_string()), role: None, note: String::new() },
            None => owner_or_caller(people, "the step names no user"),
        },
        AssigneeKind::Role => match role {
            Some(role) => Assignee { user: None, role: Some(role.to_string()), note: String::new() },
            None => owner_or_caller(people, "the step names no role"),
        },
        AssigneeKind::Owner => owner_or_caller(people, "no owner"),
        AssigneeKind::Employee => match &people.subject {
            None => owner_or_caller(people, "the person is not an employee yet"),
            Some(Some(user)) => Assignee { user: Some(user.clone()), role: None, note: String::new() },
            Some(None) => match first_with_user(&people.managers) {
                Some((_, user)) => Assignee { user: Some(user), role: None, note: "the person has no login: given to their manager".into() },
                None => owner_or_caller(people, "the person has no login and no manager does"),
            },
        },
        AssigneeKind::Manager | AssigneeKind::Manager2 => {
            let wanted = if kind == AssigneeKind::Manager { 0 } else { 1 };
            if people.subject.is_none() {
                return owner_or_caller(people, "the person is not an employee yet");
            }
            match people.managers.get(wanted).cloned().flatten() {
                Some(user) => Assignee { user: Some(user), role: None, note: String::new() },
                None => match first_with_user(&people.managers) {
                    Some((_, user)) => Assignee { user: Some(user), role: None, note: "that manager has no login: given to the closest one who does".into() },
                    None => owner_or_caller(people, "nobody above them has a login"),
                },
            }
        }
    }
}

/// A management chain from a person upwards, nearest first, stopping at a loop or after [`MAX_CHAIN`].
/// Returns the ids and whether it stopped because it came back on itself.
pub fn walk_chain(first: Option<String>, mut next_of: impl FnMut(&str) -> Result<Option<String>>) -> Result<(Vec<String>, bool)> {
    let mut seen: Vec<String> = Vec::new();
    let mut current = first;
    while let Some(id) = current {
        if seen.contains(&id) {
            return Ok((seen, true));
        }
        seen.push(id.clone());
        if seen.len() >= MAX_CHAIN {
            break;
        }
        current = next_of(&id)?;
    }
    Ok((seen, false))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Todo,
    Done,
    Waived,
    Void,
}

impl TaskState {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "todo" => Self::Todo,
            "done" => Self::Done,
            "waived" => Self::Waived,
            "void" => Self::Void,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Pending,
    InProcess,
    Done,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProcess => "in_process",
            Self::Done => "done",
        }
    }
}

/// A run's status from its tasks (state, weight). Waived and void tasks are left out of the count;
/// when every task is waived the run is done, when there are none at all it is pending.
pub fn run_status(tasks: &[(TaskState, i64)]) -> RunStatus {
    let counted: Vec<&(TaskState, i64)> = tasks.iter().filter(|(s, _)| matches!(s, TaskState::Todo | TaskState::Done)).collect();
    if counted.is_empty() {
        return if tasks.iter().any(|(s, _)| *s == TaskState::Waived) { RunStatus::Done } else { RunStatus::Pending };
    }
    let total: i64 = counted.iter().map(|(_, w)| (*w).max(1)).sum();
    let done: i64 = counted.iter().filter(|(s, _)| *s == TaskState::Done).map(|(_, w)| (*w).max(1)).sum();
    if done == 0 {
        RunStatus::Pending
    } else if done == total {
        RunStatus::Done
    } else {
        RunStatus::InProcess
    }
}

/// A gate is open when every task carrying it is done or waived (void ones are ignored).
pub fn gate_open(tasks: &[(TaskState, bool)]) -> bool {
    tasks.iter().all(|(state, on_gate)| !on_gate || matches!(state, TaskState::Done | TaskState::Waived | TaskState::Void))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepartureStatus {
    Draft,
    Notice,
    Clearance,
    Closed,
    Cancelled,
}

impl DepartureStatus {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "draft" => Self::Draft,
            "notice" => Self::Notice,
            "clearance" => Self::Clearance,
            "closed" => Self::Closed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Notice => "notice",
            Self::Clearance => "clearance",
            Self::Closed => "closed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_live(self) -> bool {
        matches!(self, Self::Draft | Self::Notice | Self::Clearance)
    }
}

/// The status a departure should have on a given day: in notice until its last day has been reached,
/// then in clearance. A draft stays a draft until it is confirmed.
pub fn status_on(status: DepartureStatus, last_day: NaiveDate, today: NaiveDate) -> DepartureStatus {
    match status {
        DepartureStatus::Notice if today >= last_day => DepartureStatus::Clearance,
        other => other,
    }
}

/// What stands in the way of closing a departure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Clearance {
    pub open_gate_tasks: usize,
    pub items_owed: usize,
    pub lines_open: usize,
}

/// Closing needs no open gate task and no item still owed; open settlement lines need a manager's
/// override with a reason.
pub fn check_close(clearance: Clearance, last_day: NaiveDate, today: NaiveDate, override_reason: Option<&str>) -> Result<()> {
    if today < last_day {
        return Err(Error::msg(format!("the last day is {last_day}: a departure closes on or after it")));
    }
    if clearance.open_gate_tasks > 0 {
        return Err(Error::msg(format!("{} checklist task(s) that block the departure are still open", clearance.open_gate_tasks)));
    }
    if clearance.items_owed > 0 {
        return Err(Error::msg(format!("{} item(s) have not been returned, recovered or waived", clearance.items_owed)));
    }
    if clearance.lines_open > 0 && override_reason.map(str::trim).unwrap_or_default().is_empty() {
        return Err(Error::msg(format!("{} settlement line(s) are not settled: settle them, or close with a reason", clearance.lines_open)));
    }
    Ok(())
}

/// What is owed to the employee: payables less receivables (negative when they owe the company).
pub fn net_settlement(lines: &[(bool, Decimal)]) -> Decimal {
    lines.iter().fold(Decimal::zero(2), |sum, (payable, amount)| if *payable { sum + *amount } else { sum - *amount })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_sdk::dates::parse_date;

    fn day(text: &str) -> Result<NaiveDate> {
        parse_date(text)
    }

    fn people() -> People {
        People { subject: Some(Some("users:ann".into())), managers: vec![None, Some("users:boss".into())], owner: Some("users:hr".into()), caller: Some("users:me".into()) }
    }

    #[test]
    fn due_dates_follow_the_calendar() -> Result<()> {
        let calendar = Calendar::default();
        // 2026-10-10 is a Saturday.
        let (start, end) = due_dates(day("2026-10-05")?, 5, 0, Roll::Next, &calendar)?;
        assert_eq!((start, end), (day("2026-10-12")?, day("2026-10-12")?));
        let (start, end) = due_dates(day("2026-10-05")?, 5, 0, Roll::Previous, &calendar)?;
        assert_eq!((start, end), (day("2026-10-09")?, day("2026-10-09")?));
        let (start, _) = due_dates(day("2026-10-05")?, 5, 0, Roll::None, &calendar)?;
        assert_eq!(start, day("2026-10-10")?);
        Ok(())
    }

    #[test]
    fn an_end_never_comes_before_its_start() -> Result<()> {
        let calendar = Calendar::default();
        // Start Sat 10th rolls back to Fri 9th; end Sun 11th rolls back to Fri 9th.
        let (start, end) = due_dates(day("2026-10-10")?, 0, 1, Roll::Previous, &calendar)?;
        assert!(end >= start);
        assert!(due_dates(day("2026-10-10")?, 0, -1, Roll::Next, &calendar).is_err());
        assert!(due_dates(day("2026-10-10")?, 99999, 0, Roll::Next, &calendar).is_err());
        Ok(())
    }

    #[test]
    fn holidays_count_as_days_off() -> Result<()> {
        let calendar = Calendar { weekend: vec![], holidays: vec![day("2026-10-06")?] };
        let (start, _) = due_dates(day("2026-10-05")?, 1, 0, Roll::Next, &calendar)?;
        assert_eq!(start, day("2026-10-07")?);
        Ok(())
    }

    #[test]
    fn assignees_fall_back_up_the_chain_and_say_so() -> Result<()> {
        let p = people();
        assert_eq!(resolve_assignee(AssigneeKind::Employee, None, None, &p).user.as_deref(), Some("users:ann"));
        let manager = resolve_assignee(AssigneeKind::Manager, None, None, &p);
        assert_eq!(manager.user.as_deref(), Some("users:boss"));
        assert!(!manager.note.is_empty(), "the direct manager has no login, so a fallback was used");
        assert!(resolve_assignee(AssigneeKind::Manager2, None, None, &p).note.is_empty());
        let nobody = People { subject: Some(None), managers: vec![None], owner: None, caller: None };
        let a = resolve_assignee(AssigneeKind::Employee, None, None, &nobody);
        assert!(a.user.is_none() && !a.note.is_empty());
        Ok(())
    }

    #[test]
    fn before_there_is_an_employee_the_owner_gets_it() -> Result<()> {
        let p = People { subject: None, managers: vec![], owner: Some("users:hr".into()), caller: Some("users:me".into()) };
        assert_eq!(resolve_assignee(AssigneeKind::Manager, None, None, &p).user.as_deref(), Some("users:hr"));
        let no_owner = People { owner: None, ..p };
        assert_eq!(resolve_assignee(AssigneeKind::Employee, None, None, &no_owner).user.as_deref(), Some("users:me"));
        Ok(())
    }

    #[test]
    fn a_role_step_names_the_role() -> Result<()> {
        let a = resolve_assignee(AssigneeKind::Role, None, Some("it.admin"), &people());
        assert_eq!((a.user, a.role.as_deref()), (None, Some("it.admin")));
        Ok(())
    }

    #[test]
    fn a_chain_stops_at_a_loop_and_at_the_bound() -> Result<()> {
        let (chain, looped) = walk_chain(Some("a".into()), |id| Ok(Some(if id == "a" { "b".into() } else { "a".into() })))?;
        assert_eq!((chain, looped), (vec!["a".to_string(), "b".to_string()], true));
        let (chain, looped) = walk_chain(Some("0".into()), |id| Ok(Some(format!("{id}x"))))?;
        assert_eq!((chain.len(), looped), (MAX_CHAIN, false));
        let (chain, _) = walk_chain(None, |_| Ok(None))?;
        assert!(chain.is_empty());
        Ok(())
    }

    #[test]
    fn status_is_computed_from_the_tasks() -> Result<()> {
        use TaskState::*;
        assert_eq!(run_status(&[]), RunStatus::Pending);
        assert_eq!(run_status(&[(Todo, 1), (Todo, 1)]), RunStatus::Pending);
        assert_eq!(run_status(&[(Done, 1), (Todo, 1)]), RunStatus::InProcess);
        assert_eq!(run_status(&[(Done, 1), (Done, 3)]), RunStatus::Done);
        assert_eq!(run_status(&[(Done, 1), (Waived, 1), (Void, 1)]), RunStatus::Done, "waived and void do not count against it");
        assert_eq!(run_status(&[(Waived, 1)]), RunStatus::Done);
        assert_eq!(run_status(&[(Void, 1)]), RunStatus::Pending);
        // A heavy task counts for more.
        assert_eq!(run_status(&[(Done, 1), (Todo, 5)]), RunStatus::InProcess);
        Ok(())
    }

    #[test]
    fn a_gate_opens_when_its_tasks_are_done_or_waived() -> Result<()> {
        use TaskState::*;
        assert!(gate_open(&[]));
        assert!(gate_open(&[(Done, true), (Waived, true), (Todo, false)]));
        assert!(!gate_open(&[(Done, true), (Todo, true)]));
        Ok(())
    }

    #[test]
    fn a_departure_goes_from_notice_to_clearance_on_its_last_day() -> Result<()> {
        let last = day("2026-10-31")?;
        assert_eq!(status_on(DepartureStatus::Notice, last, day("2026-10-30")?), DepartureStatus::Notice);
        assert_eq!(status_on(DepartureStatus::Notice, last, last), DepartureStatus::Clearance);
        assert_eq!(status_on(DepartureStatus::Draft, last, day("2027-01-01")?), DepartureStatus::Draft);
        Ok(())
    }

    #[test]
    fn closing_needs_a_clear_checklist() -> Result<()> {
        let last = day("2026-10-31")?;
        let clear = Clearance::default();
        assert!(check_close(clear, last, day("2026-10-30")?, None).is_err(), "not before the last day");
        assert!(check_close(clear, last, last, None).is_ok());
        assert!(check_close(Clearance { open_gate_tasks: 1, ..clear }, last, last, Some("anyway")).is_err(), "a blocking task is never overridden");
        assert!(check_close(Clearance { items_owed: 1, ..clear }, last, last, Some("anyway")).is_err());
        assert!(check_close(Clearance { lines_open: 2, ..clear }, last, last, None).is_err());
        assert!(check_close(Clearance { lines_open: 2, ..clear }, last, last, Some("  ")).is_err());
        assert!(check_close(Clearance { lines_open: 2, ..clear }, last, last, Some("settled with finance by hand")).is_ok());
        Ok(())
    }

    #[test]
    fn the_net_settlement_is_payables_less_receivables() -> Result<()> {
        let d = |t: &str| Decimal::parse(t);
        assert_eq!(net_settlement(&[(true, d("1000.00")?), (false, d("250.50")?), (true, d("10.00")?)]), d("759.50")?);
        assert_eq!(net_settlement(&[(false, d("5.00")?)]), d("-5.00")?);
        Ok(())
    }
}
