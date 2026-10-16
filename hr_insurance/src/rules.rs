//! The insurance arithmetic and eligibility rules, with nothing read from a database.
//!
//! One implementation of "what does the insurer pay": deductible first, then the member's share (copay) of the
//! rest, then the member's remaining limit for the policy year as a ceiling. Every amount a claim shows is the
//! result of `settle`, so the numbers can be checked here without a database.

use aether_sdk::dates::{age_on, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::{Error, Result};

/// What a claim comes to once the plan's terms are applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settlement {
    pub deductible: Decimal,
    pub copay: Decimal,
    pub approved: Decimal,
}

/// `claimed`: the sum of the claim's lines. `deductible_left`: what remains of the plan's deductible for this
/// member in this policy year. `copay_percent`: 0 to 100. `limit_left`: what remains of the member's limit.
pub fn settle(claimed: Decimal, deductible_left: Decimal, copay_percent: Decimal, limit_left: Decimal) -> Result<Settlement> {
    let scale = claimed.scale();
    if claimed.is_negative() || claimed.is_zero() {
        return Err(Error::msg("a claim is for more than zero"));
    }
    if copay_percent.is_negative() || copay_percent > Decimal::whole(100, copay_percent.scale())? {
        return Err(Error::msg("a copay is between 0 and 100 percent"));
    }
    let deductible_left = if deductible_left.is_negative() { Decimal::zero(scale) } else { deductible_left.with_scale(scale)? };
    let limit_left = if limit_left.is_negative() { Decimal::zero(scale) } else { limit_left.with_scale(scale)? };
    let deductible = if claimed < deductible_left { claimed } else { deductible_left };
    let after = claimed.checked_sub(deductible)?;
    let copay = after.times(copay_percent, scale + copay_percent.scale())?.div(Decimal::whole(100, 0)?, scale)?.round_to(scale);
    let payable = after.checked_sub(copay)?;
    let approved = if payable < limit_left { payable } else { limit_left };
    Ok(Settlement { deductible, copay, approved })
}

/// Splits a premium into (employee, employer) shares; the employee pays what rounding leaves, so the two always
/// add up to the premium.
pub fn split_premium(total: Decimal, employer_percent: Decimal) -> Result<(Decimal, Decimal)> {
    let scale = total.scale();
    if employer_percent.is_negative() || employer_percent > Decimal::whole(100, employer_percent.scale())? {
        return Err(Error::msg("the employer share is between 0 and 100 percent"));
    }
    let employer = total.times(employer_percent, scale + employer_percent.scale())?.div(Decimal::whole(100, 0)?, scale)?.round_to(scale);
    Ok((total.checked_sub(employer)?, employer))
}

/// The premium for one month of a plan priced `monthly` or `annual`.
pub fn monthly_premium(amount: Decimal, period: &str) -> Result<Decimal> {
    match period {
        "monthly" => Ok(amount),
        "annual" => Ok(amount.times_ratio(1, 12)?.round_to(amount.scale())),
        _ => Err(Error::msg("a premium is priced monthly or annual")),
    }
}

/// Whether a person is covered on `day`: the window is inclusive and the end may be open.
pub fn covered_on(start: NaiveDate, end: Option<NaiveDate>, day: NaiveDate) -> bool {
    start <= day && end.map_or(true, |e| day <= e)
}

/// How a person relates to the employee, as far as the plan's eligibility goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    Own,
    Spouse,
    Child,
    Parent,
    Other,
}

impl Relation {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "self" => Self::Own,
            "spouse" => Self::Spouse,
            "child" => Self::Child,
            "parent" => Self::Parent,
            "other" => Self::Other,
            _ => return None,
        })
    }
}

/// The plan's eligibility terms.
#[derive(Debug, Clone, Copy)]
pub struct Eligibility {
    pub allow_spouse: bool,
    pub allow_children: bool,
    pub max_children: Option<i64>,
    pub child_max_age: i64,
}

/// One person asked to be covered.
#[derive(Debug, Clone, Copy)]
pub struct Candidate {
    pub relation: Relation,
    pub born: Option<NaiveDate>,
}

/// Checks the members of an enrolment against the plan, on the day cover starts. A child over the age limit is
/// refused, not silently dropped; at most one spouse; parents and others are not covered by this plan.
pub fn members_fit(terms: Eligibility, members: &[Candidate], on: NaiveDate) -> Result<()> {
    let own = members.iter().filter(|m| m.relation == Relation::Own).count();
    if own != 1 {
        return Err(Error::msg("an enrolment covers the employee exactly once"));
    }
    let spouses = members.iter().filter(|m| m.relation == Relation::Spouse).count();
    if spouses > 0 && !terms.allow_spouse {
        return Err(Error::msg("this plan does not cover a spouse"));
    }
    if spouses > 1 {
        return Err(Error::msg("a plan covers one spouse"));
    }
    let children: Vec<&Candidate> = members.iter().filter(|m| m.relation == Relation::Child).collect();
    if !children.is_empty() && !terms.allow_children {
        return Err(Error::msg("this plan does not cover children"));
    }
    if let Some(max) = terms.max_children {
        if children.len() as i64 > max {
            return Err(Error::msg(format!("this plan covers at most {max} children")));
        }
    }
    for child in children {
        let born = child.born.ok_or_else(|| Error::msg("a covered child needs a date of birth"))?;
        if i64::from(age_on(born, on)) > terms.child_max_age {
            return Err(Error::msg(format!("a child over {} is not covered", terms.child_max_age)));
        }
    }
    if members.iter().any(|m| matches!(m.relation, Relation::Parent | Relation::Other)) {
        return Err(Error::msg("this plan covers the employee, a spouse and children"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(text: &str) -> Decimal {
        Decimal::parse(text).unwrap()
    }

    fn day(y: i32, m: u32, dd: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, dd).unwrap()
    }

    #[test]
    fn settles_deductible_then_copay_then_limit() {
        let s = settle(d("1000.00"), d("100.00"), d("10"), d("5000.00")).unwrap();
        assert_eq!((s.deductible, s.copay, s.approved), (d("100.00"), d("90.00"), d("810.00")));
    }

    #[test]
    fn a_small_claim_is_all_deductible() {
        let s = settle(d("40.00"), d("100.00"), d("20"), d("5000.00")).unwrap();
        assert_eq!((s.deductible, s.copay, s.approved), (d("40.00"), d("0.00"), d("0.00")));
    }

    #[test]
    fn the_limit_caps_what_is_paid() {
        let s = settle(d("1000.00"), d("0.00"), d("0"), d("250.00")).unwrap();
        assert_eq!(s.approved, d("250.00"));
    }

    #[test]
    fn nothing_is_left_when_the_limit_is_used() {
        let s = settle(d("100.00"), d("0.00"), d("0"), d("-5.00")).unwrap();
        assert_eq!(s.approved, d("0.00"));
    }

    #[test]
    fn premium_shares_add_up() {
        let (employee, employer) = split_premium(d("100.01"), d("50")).unwrap();
        assert_eq!(employee + employer, d("100.01"));
    }

    #[test]
    fn an_annual_premium_is_a_twelfth() {
        assert_eq!(monthly_premium(d("1200.00"), "annual").unwrap(), d("100.00"));
    }

    #[test]
    fn children_over_the_age_limit_are_refused() {
        let terms = Eligibility { allow_spouse: true, allow_children: true, max_children: Some(2), child_max_age: 25 };
        let members = [
            Candidate { relation: Relation::Own, born: None },
            Candidate { relation: Relation::Child, born: Some(day(1990, 1, 1)) },
        ];
        assert!(members_fit(terms, &members, day(2026, 1, 1)).is_err());
        let ok = [Candidate { relation: Relation::Own, born: None }, Candidate { relation: Relation::Child, born: Some(day(2015, 1, 1)) }];
        assert!(members_fit(terms, &ok, day(2026, 1, 1)).is_ok());
    }

    #[test]
    fn covered_window_is_inclusive() {
        assert!(covered_on(day(2026, 1, 1), Some(day(2026, 12, 31)), day(2026, 12, 31)));
        assert!(!covered_on(day(2026, 1, 1), None, day(2025, 12, 31)));
    }
}
