//! Ceilings, remaining balance, dependents, and the insurance window, with nothing read from a database.
//!
//! Frappe's Employee Benefit Application / Claim / Ledger tie a yearly pot to a salary component and write ledger
//! rows from a salary slip (and delete them when the slip is cancelled). One claim per month is a workaround for
//! over-claiming. Here a plan has a ceiling per person and a year, money moves only as ledger entries that are
//! never deleted, a claim is refused when it would go over the remaining balance, and health insurance is a
//! separate enrolment with a window of dependents.

use aether_sdk::dates::NaiveDate;
use aether_sdk::decimal::Decimal;
use aether_sdk::{Error, Result};

/// How a plan pays out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Accrues a share of the yearly ceiling each month; a claim draws from what has accrued.
    AccrueThenClaim,
    /// The yearly ceiling is available from the first day; a claim draws from it.
    ClaimAgainstCeiling,
    /// Paid as a payroll amount, not claimed.
    Payroll,
}

impl Kind {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "accrue_then_claim" => Self::AccrueThenClaim,
            "claim_against_ceiling" => Self::ClaimAgainstCeiling,
            "payroll" => Self::Payroll,
            _ => return None,
        })
    }
}

/// What is left of a yearly pot: accruals minus claims, plus reversals of claims. Never below zero.
pub fn remaining(ceiling: Decimal, accrued: Decimal, claimed: Decimal, reversed: Decimal) -> Result<Decimal> {
    let used = claimed.checked_sub(reversed)?;
    let pool = match accrued > ceiling {
        true => ceiling,
        false => accrued,
    };
    let left = pool.checked_sub(used)?;
    Ok(if left.is_negative() { Decimal::zero(ceiling.scale()) } else { left })
}

/// What has accrued by `month` of `months` in the year (1-based). The last month takes what rounding leaves.
pub fn accrued_by(ceiling: Decimal, month: i64, months: i64) -> Result<Decimal> {
    if months < 1 || month < 1 || month > months {
        return Err(Error::msg("the month is between 1 and the months of the year"));
    }
    if month == months {
        return Ok(ceiling);
    }
    ceiling.times_ratio(month, months)
}

/// Whether a claim of `amount` fits in `left`.
pub fn claim_fits(left: Decimal, amount: Decimal) -> Result<()> {
    if amount.is_negative() || amount.is_zero() {
        return Err(Error::msg("a claim is more than zero"));
    }
    if amount > left {
        return Err(Error::msg(format!("this claim of {amount} is more than the {left} left on the plan")));
    }
    Ok(())
}

/// A dependent of an enrolment: spouse, child or other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    Spouse,
    Child,
    Other,
}

impl Relation {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "spouse" => Self::Spouse,
            "child" => Self::Child,
            "other" => Self::Other,
            _ => return None,
        })
    }
}

/// Whether this set of dependents fits the plan's window.
pub fn dependents_fit(max_dependents: i64, allow_spouse: bool, allow_children: bool, relations: &[Relation]) -> Result<()> {
    if relations.len() as i64 > max_dependents {
        return Err(Error::msg(format!("this plan covers at most {max_dependents} dependents")));
    }
    let spouses = relations.iter().filter(|r| **r == Relation::Spouse).count();
    if spouses > 1 {
        return Err(Error::msg("an enrolment names at most one spouse"));
    }
    if spouses == 1 && !allow_spouse {
        return Err(Error::msg("this plan does not cover a spouse"));
    }
    if relations.iter().any(|r| *r == Relation::Child) && !allow_children {
        return Err(Error::msg("this plan does not cover children"));
    }
    Ok(())
}

/// Whether two windows overlap.
pub fn windows_overlap(a: (NaiveDate, Option<NaiveDate>), b: (NaiveDate, Option<NaiveDate>)) -> bool {
    let a_end = a.1.unwrap_or(NaiveDate::MAX);
    let b_end = b.1.unwrap_or(NaiveDate::MAX);
    a.0 <= b_end && b.0 <= a_end
}

/// Calendar year of a date.
pub fn year_of(date: NaiveDate) -> i32 {
    use aether_sdk::dates::Datelike;
    date.year()
}

#[cfg(test)]
mod tests {
    use aether_sdk::dates::parse_date;

    use super::*;

    fn n(text: &str) -> Decimal {
        Decimal::parse(text).unwrap_or(Decimal::zero(2))
    }

    fn d(text: &str) -> NaiveDate {
        parse_date(text).unwrap_or_default()
    }

    #[test]
    fn remaining_is_accrued_minus_claims_plus_reversals() -> Result<()> {
        assert_eq!(remaining(n("1200.00"), n("400.00"), n("100.00"), n("0.00"))?, n("300.00"));
        assert_eq!(remaining(n("1200.00"), n("1200.00"), n("1200.00"), n("200.00"))?, n("200.00"));
        assert_eq!(remaining(n("1200.00"), n("1300.00"), n("0.00"), n("0.00"))?, n("1200.00"), "never more than the ceiling");
        Ok(())
    }

    #[test]
    fn accrual_reaches_the_ceiling_in_the_last_month() -> Result<()> {
        assert_eq!(accrued_by(n("1200.00"), 1, 12)?, n("100.00"));
        assert_eq!(accrued_by(n("1200.00"), 6, 12)?, n("600.00"));
        assert_eq!(accrued_by(n("100.00"), 3, 3)?, n("100.00"));
        assert_eq!(accrued_by(n("100.00"), 2, 3)?, n("66.67"));
        Ok(())
    }

    #[test]
    fn a_claim_that_would_go_over_is_refused() {
        assert!(claim_fits(n("50.00"), n("50.00")).is_ok());
        assert!(claim_fits(n("50.00"), n("50.01")).is_err());
        assert!(claim_fits(n("50.00"), n("0.00")).is_err());
    }

    #[test]
    fn dependents_fit_the_window() {
        use Relation::*;
        assert!(dependents_fit(3, true, true, &[Spouse, Child, Child]).is_ok());
        assert!(dependents_fit(2, true, true, &[Spouse, Child, Child]).is_err());
        assert!(dependents_fit(3, false, true, &[Spouse]).is_err());
        assert!(dependents_fit(3, true, false, &[Child]).is_err());
        assert!(dependents_fit(2, true, true, &[Spouse, Spouse]).is_err());
    }

    #[test]
    fn enrolments_overlap_when_they_share_a_day() {
        assert!(windows_overlap((d("2026-01-01"), Some(d("2026-12-31"))), (d("2026-12-31"), None)));
        assert!(!windows_overlap((d("2026-01-01"), Some(d("2026-06-30"))), (d("2026-07-01"), None)));
    }
}
