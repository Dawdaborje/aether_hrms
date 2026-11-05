//! The state machine, the conflict check and the SLA arithmetic of a grievance, with no records read.

use aether_sdk::dates::{Duration, NaiveDate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Submitted,
    Investigating,
    FindingsSubmitted,
    Resolved,
    Appealed,
    Closed,
    Withdrawn,
}

impl State {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "submitted" => Self::Submitted,
            "investigating" => Self::Investigating,
            "findings_submitted" => Self::FindingsSubmitted,
            "resolved" => Self::Resolved,
            "appealed" => Self::Appealed,
            "closed" => Self::Closed,
            "withdrawn" => Self::Withdrawn,
            _ => return None,
        })
    }

    /// Whether the clock is running: nothing is owed on a decided or finished case.
    pub fn is_open(self) -> bool {
        matches!(self, Self::Submitted | Self::Investigating | Self::FindingsSubmitted | Self::Appealed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Assign,
    SubmitFindings,
    Decide,
    Appeal,
    Withdraw,
    Dismiss,
    Close,
}

/// The state an action leads to, or `None` if the action is not allowed from here.
pub fn next(state: State, action: Action) -> Option<State> {
    use Action::*;
    use State::*;
    Some(match (state, action) {
        (Submitted, Assign) | (Appealed, Assign) => Investigating,
        (Investigating, SubmitFindings) => FindingsSubmitted,
        (FindingsSubmitted, Decide) => Resolved,
        (Resolved, Appeal) => Appealed,
        (Submitted | Investigating, Withdraw) => Withdrawn,
        (Submitted, Dismiss) => Closed,
        (Resolved, Close) => Closed,
        _ => return None,
    })
}

/// Someone and the person they report to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub id: String,
    pub manager: Option<String>,
}

/// Why this person may not investigate: a party to the case, or in a direct reporting line with one.
pub fn conflict(candidate: &Person, complainant: &Person, accused: Option<&Person>, past: &[String]) -> Option<&'static str> {
    let parties: Vec<&Person> = std::iter::once(complainant).chain(accused).collect();
    for party in &parties {
        if candidate.id == party.id {
            return Some("an investigator cannot be a party to the case");
        }
        if candidate.manager.as_deref() == Some(party.id.as_str()) {
            return Some("an investigator cannot report to a party to the case");
        }
        if party.manager.as_deref() == Some(candidate.id.as_str()) {
            return Some("an investigator cannot be the manager of a party to the case");
        }
    }
    if past.iter().any(|p| *p == candidate.id) {
        return Some("this person already investigated this case: an appeal needs someone new");
    }
    None
}

/// The day a case is due: its opening day plus the category's days.
pub fn due(opened: NaiveDate, sla_days: i64) -> NaiveDate {
    opened + Duration::days(sla_days.max(1))
}

/// How far a case has escalated on a day: 0 on time, 1 overdue, 2 overdue by as long again as the limit.
pub fn escalation(today: NaiveDate, opened: NaiveDate, due: NaiveDate) -> i64 {
    if today <= due {
        return 0;
    }
    let limit = (due - opened).num_days().max(1);
    if (today - due).num_days() >= limit {
        2
    } else {
        1
    }
}

/// Whether the appeal window (14 days after the decision) is still open.
pub fn appeal_open(today: NaiveDate, resolved_on: NaiveDate) -> bool {
    (today - resolved_on).num_days() <= 14
}

#[cfg(test)]
mod tests {
    use aether_sdk::dates::parse_date;

    use super::*;

    fn d(text: &str) -> NaiveDate {
        parse_date(text).unwrap_or_default()
    }

    fn p(id: &str, manager: Option<&str>) -> Person {
        Person { id: id.into(), manager: manager.map(str::to_string) }
    }

    #[test]
    fn a_case_follows_the_states_and_nothing_else() {
        assert_eq!(next(State::Submitted, Action::Assign), Some(State::Investigating));
        assert_eq!(next(State::Investigating, Action::SubmitFindings), Some(State::FindingsSubmitted));
        assert_eq!(next(State::FindingsSubmitted, Action::Decide), Some(State::Resolved));
        assert_eq!(next(State::Resolved, Action::Appeal), Some(State::Appealed));
        assert_eq!(next(State::Appealed, Action::Assign), Some(State::Investigating));
        assert_eq!(next(State::Submitted, Action::Decide), None);
        assert_eq!(next(State::Closed, Action::Appeal), None);
        assert_eq!(next(State::FindingsSubmitted, Action::Withdraw), None);
        assert!(State::Appealed.is_open() && !State::Resolved.is_open());
    }

    #[test]
    fn an_investigator_is_not_a_party_nor_in_line_with_one() {
        let complainant = p("c", Some("cm"));
        let accused = p("a", Some("am"));
        assert!(conflict(&p("x", None), &complainant, Some(&accused), &[]).is_none());
        assert!(conflict(&p("c", None), &complainant, Some(&accused), &[]).is_some());
        assert!(conflict(&p("a", None), &complainant, Some(&accused), &[]).is_some());
        assert!(conflict(&p("cm", None), &complainant, Some(&accused), &[]).is_some(), "manager of the complainant");
        assert!(conflict(&p("am", None), &complainant, Some(&accused), &[]).is_some(), "manager of the accused");
        assert!(conflict(&p("x", Some("a")), &complainant, Some(&accused), &[]).is_some(), "reports to the accused");
        assert!(conflict(&p("x", None), &complainant, None, &["x".to_string()]).is_some(), "investigated before");
    }

    #[test]
    fn sla_and_escalation() {
        let opened = d("2026-01-01");
        let due_on = due(opened, 10);
        assert_eq!(due_on, d("2026-01-11"));
        assert_eq!(escalation(d("2026-01-11"), opened, due_on), 0);
        assert_eq!(escalation(d("2026-01-12"), opened, due_on), 1);
        assert_eq!(escalation(d("2026-01-21"), opened, due_on), 2);
    }

    #[test]
    fn an_appeal_has_fourteen_days() {
        assert!(appeal_open(d("2026-01-15"), d("2026-01-01")));
        assert!(!appeal_open(d("2026-01-16"), d("2026-01-01")));
    }
}
