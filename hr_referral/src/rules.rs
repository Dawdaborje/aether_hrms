//! The referral state machine and the tenure gate on the bonus, with nothing read from a database.
//!
//! Frappe's Employee Referral has a status that `validate` always resets to Pending, a bonus flag anyone can tick,
//! and `create_additional_salary` that does not actually insert the extra pay. Here the states follow a table of
//! moves, the bonus is set by the policy (amount, wait in days) copied onto the referral, and it is paid only after
//! the hire has stayed that long, never to the referrer for themselves.

use aether_sdk::dates::NaiveDate;
use aether_sdk::decimal::Decimal;
use aether_sdk::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Submitted,
    InProcess,
    Hired,
    Rejected,
    Withdrawn,
    BonusDue,
    Paid,
    Forfeited,
}

impl State {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "submitted" => Self::Submitted,
            "in_process" => Self::InProcess,
            "hired" => Self::Hired,
            "rejected" => Self::Rejected,
            "withdrawn" => Self::Withdrawn,
            "bonus_due" => Self::BonusDue,
            "paid" => Self::Paid,
            "forfeited" => Self::Forfeited,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Submitted => "submitted",
            Self::InProcess => "in_process",
            Self::Hired => "hired",
            Self::Rejected => "rejected",
            Self::Withdrawn => "withdrawn",
            Self::BonusDue => "bonus_due",
            Self::Paid => "paid",
            Self::Forfeited => "forfeited",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Accept,
    Hire,
    Reject,
    Withdraw,
    ReleaseBonus,
    Pay,
    Forfeit,
}

/// The state an action leads to, or `None` if it is not allowed from here.
pub fn next(state: State, action: Action) -> Option<State> {
    use Action::*;
    use State::*;
    Some(match (state, action) {
        (Submitted, Accept) => InProcess,
        (InProcess, Hire) => Hired,
        (Submitted | InProcess, Reject) => Rejected,
        (Submitted, Withdraw) => Withdrawn,
        (Hired, ReleaseBonus) => BonusDue,
        (BonusDue, Pay) => Paid,
        (Hired | BonusDue, Forfeit) => Forfeited,
        _ => return None,
    })
}

/// The day the bonus is due: the hire date plus the policy's wait in days.
pub fn due_on(hired: NaiveDate, wait_days: i64) -> NaiveDate {
    hired + aether_sdk::dates::Duration::days(wait_days.max(0))
}

/// Whether the hire has stayed long enough, and is still working.
pub fn bonus_ready(today: NaiveDate, hired: NaiveDate, wait_days: i64, still_working: bool) -> Result<()> {
    if !still_working {
        return Err(Error::msg("the hire has left: the bonus is forfeited"));
    }
    if today < due_on(hired, wait_days) {
        return Err(Error::msg("the hire has not stayed long enough for the bonus"));
    }
    Ok(())
}

/// A bonus amount must be more than zero.
pub fn bonus_ok(amount: Decimal) -> Result<()> {
    if amount.is_negative() || amount.is_zero() {
        return Err(Error::msg("a referral bonus is more than zero"));
    }
    Ok(())
}

/// You cannot refer yourself (same employee id, or the same email as the referrer's work email).
pub fn not_self(referrer_id: &str, referrer_email: Option<&str>, candidate_email: &str) -> Result<()> {
    if candidate_email.trim().is_empty() {
        return Err(Error::msg("the candidate has an email"));
    }
    if referrer_email.is_some_and(|e| e.eq_ignore_ascii_case(candidate_email.trim())) {
        return Err(Error::msg("you cannot refer yourself"));
    }
    if referrer_id.eq_ignore_ascii_case(candidate_email) {
        return Err(Error::msg("you cannot refer yourself"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use aether_sdk::dates::parse_date;

    use super::*;

    fn d(text: &str) -> NaiveDate {
        parse_date(text).unwrap_or_default()
    }

    fn n(text: &str) -> Decimal {
        Decimal::parse(text).unwrap_or(Decimal::zero(2))
    }

    #[test]
    fn a_referral_follows_the_states_and_nothing_else() {
        assert_eq!(next(State::Submitted, Action::Accept), Some(State::InProcess));
        assert_eq!(next(State::InProcess, Action::Hire), Some(State::Hired));
        assert_eq!(next(State::Hired, Action::ReleaseBonus), Some(State::BonusDue));
        assert_eq!(next(State::BonusDue, Action::Pay), Some(State::Paid));
        assert_eq!(next(State::Hired, Action::Forfeit), Some(State::Forfeited));
        assert_eq!(next(State::Submitted, Action::Hire), None);
        assert_eq!(next(State::Paid, Action::Forfeit), None);
        assert_eq!(next(State::InProcess, Action::Withdraw), None, "withdrawn only before recruitment takes it");
    }

    #[test]
    fn the_bonus_waits_and_is_forfeited_if_the_hire_leaves() {
        let hired = d("2026-01-01");
        assert!(bonus_ready(d("2026-03-31"), hired, 90, true).is_err());
        assert!(bonus_ready(d("2026-04-01"), hired, 90, true).is_ok());
        assert!(bonus_ready(d("2026-12-01"), hired, 90, false).is_err());
        assert_eq!(due_on(hired, 90), d("2026-04-01"));
    }

    #[test]
    fn you_cannot_refer_yourself_and_a_bonus_is_more_than_zero() {
        assert!(not_self("e1", Some("ada@x.io"), "ada@x.io").is_err());
        assert!(not_self("e1", Some("ada@x.io"), "bob@x.io").is_ok());
        assert!(bonus_ok(n("500.00")).is_ok());
        assert!(bonus_ok(n("0.00")).is_err());
    }
}
