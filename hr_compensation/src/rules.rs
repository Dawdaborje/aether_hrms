//! The rules of extra pay, with nothing read from a database.
//!
//! What the Frappe sources taught (see `docs/design.md`): an amount lands in a period by its dates, and a
//! recurring one that only partly covers a period is prorated by the days it covers (Frappe skips it unless it runs
//! to the end of the period); gratuity slabs are half-open so a boundary year matches one slab (Frappe's are
//! inclusive both sides); years of service are exact decimals; a cancel never edits what was paid.

use aether_sdk::dates::{add_months, Duration, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::{Error, Result};
use serde::{Deserialize, Serialize};

fn zero() -> Decimal {
    Decimal::zero(2)
}

/// Days two periods share, both ends included; a missing end of the first never ends.
pub fn overlap_days(from: NaiveDate, to: Option<NaiveDate>, start: NaiveDate, end: NaiveDate) -> i64 {
    let first = from.max(start);
    let last = to.map_or(end, |t| t.min(end));
    if last < first { 0 } else { (last - first).num_days() + 1 }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terms {
    OneOff { pay_date: NaiveDate },
    Recurring { from: NaiveDate, to: Option<NaiveDate>, prorate: bool },
}

/// What an adjustment adds to a pay period.
pub fn period_amount(amount: Decimal, terms: Terms, start: NaiveDate, end: NaiveDate) -> Result<Decimal> {
    match terms {
        Terms::OneOff { pay_date } => Ok(if start <= pay_date && pay_date <= end { amount } else { zero() }),
        Terms::Recurring { from, to, prorate } => {
            let days = overlap_days(from, to, start, end);
            if days == 0 {
                return Ok(zero());
            }
            let period = (end - start).num_days() + 1;
            if prorate && days < period { amount.times_ratio(days, period) } else { Ok(amount) }
        }
    }
}

/// The monthly cycles of a withholding: `count` calendar months from `from`, each from its start to the day before
/// the next one.
pub fn monthly_cycles(from: NaiveDate, count: u32) -> Vec<(NaiveDate, NaiveDate)> {
    (0..count)
        .map(|i| {
            let start = add_months(from, i);
            let next = add_months(from, i + 1);
            (start, next - Duration::days(1))
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slab {
    pub from: Decimal,
    /// Empty: no upper limit.
    #[serde(default)]
    pub to: Option<Decimal>,
    pub fraction: Decimal,
}

/// Slabs cover years from 0 without gap or overlap, each `from <= years < to`; only the last is open.
pub fn check_slabs(slabs: &[Slab]) -> Result<()> {
    let Some(first) = slabs.first() else { return Err(Error::msg("a gratuity rule has at least one slab")) };
    if !first.from.is_zero() {
        return Err(Error::msg("the first slab starts at year 0"));
    }
    for (index, slab) in slabs.iter().enumerate() {
        if slab.fraction.is_negative() || slab.fraction > Decimal::whole(10, 0)? {
            return Err(Error::msg("a slab's fraction of the monthly pay is between 0 and 10"));
        }
        match (&slab.to, slabs.get(index + 1)) {
            (None, Some(_)) => return Err(Error::msg("only the last slab may be open-ended")),
            (Some(to), Some(next)) => {
                if *to <= slab.from {
                    return Err(Error::msg(format!("slab {} ends before it starts", index + 1)));
                }
                if next.from != *to {
                    return Err(Error::msg(format!("slab {} ends at {to} but the next starts at {}: no gaps and no overlaps", index + 1, next.from)));
                }
            }
            (Some(to), None) if *to <= slab.from => return Err(Error::msg(format!("slab {} ends before it starts", index + 1))),
            _ => {}
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YearsMethod {
    Exact,
    Round,
    Floor,
}

impl YearsMethod {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "exact" => Self::Exact,
            "round" => Self::Round,
            "floor" => Self::Floor,
            _ => return None,
        })
    }
}

/// Completed service from the joining day to the relieving day, less unpaid days, as years with four digits.
pub fn years_of_service(joining: NaiveDate, relieving: NaiveDate, unpaid_days: i64, days_per_year: i64, method: YearsMethod) -> Result<Decimal> {
    if relieving < joining {
        return Err(Error::msg("the relieving date is before the joining date"));
    }
    if days_per_year <= 0 || unpaid_days < 0 {
        return Err(Error::msg("days per year is above zero and unpaid days are not negative"));
    }
    let days = ((relieving - joining).num_days() - unpaid_days).max(0);
    let exact = Decimal::whole(days, 4)?.times_ratio(1, days_per_year)?;
    Ok(match method {
        YearsMethod::Exact => exact,
        YearsMethod::Round => exact.round_to(0).with_scale(4)?,
        YearsMethod::Floor => Decimal::whole(days / days_per_year, 4)?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    CurrentSlab,
    Cumulative,
}

impl Mode {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "current_slab" => Self::CurrentSlab,
            "cumulative" => Self::Cumulative,
            _ => return None,
        })
    }
}

/// The gratuity for `years` of service on a monthly `base`, by the rule's slabs (checked first).
pub fn gratuity_amount(years: Decimal, base: Decimal, slabs: &[Slab], mode: Mode, min_years: Decimal) -> Result<Decimal> {
    check_slabs(slabs)?;
    if years < min_years {
        return Err(Error::msg(format!("{years} years of service is below the {min_years} the rule asks for")));
    }
    let mut total = Decimal::zero(6);
    match mode {
        Mode::CurrentSlab => {
            let slab = slabs.iter().find(|s| s.from <= years && s.to.is_none_or(|to| years < to)).ok_or_else(|| Error::msg("no slab holds this length of service"))?;
            total = base.times(years, 6)?.times(slab.fraction, 6)?;
        }
        Mode::Cumulative => {
            for slab in slabs {
                let top = slab.to.map_or(years, |to| to.min(years));
                if top > slab.from {
                    total = total + base.times(top - slab.from, 6)?.times(slab.fraction, 6)?;
                }
            }
        }
    }
    Ok(total.round_to(2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_sdk::dates::parse_date;

    fn day(text: &str) -> Result<NaiveDate> {
        parse_date(text)
    }

    fn d(text: &str) -> Result<Decimal> {
        Decimal::parse(text)
    }

    #[test]
    fn a_one_off_lands_in_the_period_that_holds_its_day() -> Result<()> {
        let (start, end) = (day("2026-10-01")?, day("2026-10-31")?);
        let terms = Terms::OneOff { pay_date: day("2026-10-31")? };
        assert_eq!(period_amount(d("100.00")?, terms, start, end)?, d("100.00")?);
        assert_eq!(period_amount(d("100.00")?, terms, day("2026-11-01")?, day("2026-11-30")?)?, d("0.00")?);
        Ok(())
    }

    #[test]
    fn a_recurring_amount_covers_every_period_it_touches() -> Result<()> {
        let (start, end) = (day("2026-10-01")?, day("2026-10-31")?);
        let open = Terms::Recurring { from: day("2026-01-01")?, to: None, prorate: false };
        assert_eq!(period_amount(d("50.00")?, open, start, end)?, d("50.00")?);
        // Ending mid-period is not skipped (Frappe skips it): paid whole, or by the share of the days.
        let ends_early = Terms::Recurring { from: day("2026-01-01")?, to: Some(day("2026-10-10")?), prorate: false };
        assert_eq!(period_amount(d("50.00")?, ends_early, start, end)?, d("50.00")?);
        let prorated = Terms::Recurring { from: day("2026-01-01")?, to: Some(day("2026-10-10")?), prorate: true };
        assert_eq!(period_amount(d("310.00")?, prorated, start, end)?, d("100.00")?, "10 of 31 days");
        let before = Terms::Recurring { from: day("2026-11-01")?, to: None, prorate: true };
        assert_eq!(period_amount(d("50.00")?, before, start, end)?, d("0.00")?);
        Ok(())
    }

    #[test]
    fn cycles_follow_the_calendar() -> Result<()> {
        let cycles = monthly_cycles(day("2026-01-31")?, 3);
        assert_eq!(cycles[0], (day("2026-01-31")?, day("2026-02-27")?));
        assert_eq!(cycles.len(), 3);
        assert!(cycles.windows(2).all(|w| w[1].0 == w[0].1 + Duration::days(1)), "no gap and no overlap between cycles");
        Ok(())
    }

    fn slabs() -> Result<Vec<Slab>> {
        Ok(vec![
            Slab { from: d("0")?, to: Some(d("5")?), fraction: d("0.5")? },
            Slab { from: d("5")?, to: Some(d("10")?), fraction: d("0.75")? },
            Slab { from: d("10")?, to: None, fraction: d("1")? },
        ])
    }

    #[test]
    fn slabs_are_checked() -> Result<()> {
        check_slabs(&slabs()?)?;
        assert!(check_slabs(&[]).is_err());
        assert!(check_slabs(&[Slab { from: d("1")?, to: None, fraction: d("1")? }]).is_err(), "must start at 0");
        let gap = vec![Slab { from: d("0")?, to: Some(d("5")?), fraction: d("1")? }, Slab { from: d("6")?, to: None, fraction: d("1")? }];
        assert!(check_slabs(&gap).is_err());
        let overlap = vec![Slab { from: d("0")?, to: Some(d("5")?), fraction: d("1")? }, Slab { from: d("4")?, to: None, fraction: d("1")? }];
        assert!(check_slabs(&overlap).is_err());
        let open_middle = vec![Slab { from: d("0")?, to: None, fraction: d("1")? }, Slab { from: d("5")?, to: None, fraction: d("1")? }];
        assert!(check_slabs(&open_middle).is_err());
        Ok(())
    }

    #[test]
    fn years_of_service_are_exact_or_rounded_on_purpose() -> Result<()> {
        let (join, leave) = (day("2020-01-01")?, day("2026-07-02")?);
        // 2374 days
        assert_eq!(years_of_service(join, leave, 0, 365, YearsMethod::Exact)?, d("6.5041")?);
        assert_eq!(years_of_service(join, leave, 0, 365, YearsMethod::Round)?, d("7.0000")?);
        assert_eq!(years_of_service(join, leave, 0, 365, YearsMethod::Floor)?, d("6.0000")?);
        assert_eq!(years_of_service(join, leave, 365, 365, YearsMethod::Floor)?, d("5.0000")?, "unpaid days are taken off");
        assert!(years_of_service(leave, join, 0, 365, YearsMethod::Exact).is_err());
        Ok(())
    }

    #[test]
    fn a_boundary_year_belongs_to_one_slab() -> Result<()> {
        let s = slabs()?;
        let base = d("1000.00")?;
        let at_five = gratuity_amount(d("5")?, base, &s, Mode::CurrentSlab, d("1")?)?;
        assert_eq!(at_five, d("3750.00")?, "exactly 5 years is in the second slab: 1000 x 5 x 0.75");
        let at_four = gratuity_amount(d("4.9999")?, base, &s, Mode::CurrentSlab, d("1")?)?;
        assert_eq!(at_four, d("2499.95")?, "just under 5 years stays in the first slab: 1000 x 4.9999 x 0.5");
        Ok(())
    }

    #[test]
    fn cumulative_pays_each_slab_for_its_own_years() -> Result<()> {
        let s = slabs()?;
        // 8 years: 5 at 0.5 and 3 at 0.75 of the monthly base.
        assert_eq!(gratuity_amount(d("8")?, d("1000.00")?, &s, Mode::Cumulative, d("1")?)?, d("4750.00")?);
        assert_eq!(gratuity_amount(d("12")?, d("1000.00")?, &s, Mode::Cumulative, d("1")?)?, d("2500.00")? + d("3750.00")? + d("2000.00")?);
        assert!(gratuity_amount(d("0.5")?, d("1000.00")?, &s, Mode::Cumulative, d("1")?).is_err(), "below the minimum service");
        Ok(())
    }
}
