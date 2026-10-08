//! Schedules, allocation of repayments, the payroll cap and the foreclosure quote, with nothing read from a database.
//!
//! Neither Odoo nor Frappe HRMS gives a staff loan an engine worth copying (Frappe's loan module lives in a separate
//! lending app, and its schedule is rebuilt from a document). So: a loan has one schedule, made when it is requested
//! and copied from the product's terms; money moves only as entries in a ledger; a repayment is spread over the oldest
//! unpaid instalments and undone by a reversal entry, not by editing; payroll may take less than is due when the
//! employee's pay does not stretch, and the rest stays due.

use aether_sdk::dates::{add_months, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// Equal instalments; interest on the balance each month.
    Reducing,
    /// Interest on the whole principal for the whole term, spread evenly.
    Flat,
}

impl Method {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "reducing" => Some(Self::Reducing),
            "flat" => Some(Self::Flat),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Instalment {
    pub n: i64,
    pub due: NaiveDate,
    pub principal: Decimal,
    pub interest: Decimal,
}

impl Instalment {
    pub fn total(&self) -> Decimal {
        self.principal + self.interest
    }
}

/// `total` cut into `n` equal parts; the last takes what rounding leaves.
fn split(total: Decimal, n: i64) -> Result<Vec<Decimal>> {
    let base = total.times_ratio(1, n)?;
    let mut parts = vec![base; (n - 1) as usize];
    parts.push(total - base.times_ratio(n - 1, 1)?);
    Ok(parts)
}

/// The schedule of a loan. `first_due` is the date of the first instalment, the others follow monthly.
pub fn schedule(principal: Decimal, months: i64, annual_bps: i64, method: Method, first_due: NaiveDate) -> Result<Vec<Instalment>> {
    if months < 1 || months > 120 {
        return Err(Error::msg("a loan runs between 1 and 120 months"));
    }
    if principal.is_negative() || principal.is_zero() {
        return Err(Error::msg("a loan is more than zero"));
    }
    if !(0..=10_000_00).contains(&annual_bps) {
        return Err(Error::msg("the yearly rate is between 0 and 100 percent"));
    }
    let places = principal.scale();
    let due = |i: i64| add_months(first_due, i as u32);
    let mut out = Vec::new();
    if annual_bps == 0 || method == Method::Flat {
        // Flat interest: principal * rate * years, split evenly with the principal.
        let interest_total = if annual_bps == 0 { Decimal::zero(places) } else { principal.times_ratio(annual_bps * months, 12 * 10_000)? };
        let principals = split(principal, months)?;
        let interests = split(interest_total, months)?;
        for i in 0..months {
            out.push(Instalment { n: i + 1, due: due(i), principal: principals[i as usize], interest: interests[i as usize] });
        }
        return Ok(out);
    }
    // Reducing balance: one level payment, rounded; interest on what is left; the last instalment clears the rest.
    let r = annual_bps as f64 / 10_000.0 / 12.0;
    let level = principal_f(principal) * r / (1.0 - (1.0 + r).powi(-(months as i32)));
    let payment = Decimal::parse(&format!("{:.*}", places as usize, level))?;
    let mut balance = principal;
    for i in 0..months {
        let interest = balance.times_ratio(annual_bps, 12 * 10_000)?;
        let principal_part = if i == months - 1 { balance } else { payment - interest };
        if principal_part.is_negative() || principal_part > balance {
            return Err(Error::msg("these terms do not make a sensible schedule"));
        }
        balance = balance - principal_part;
        out.push(Instalment { n: i + 1, due: due(i), principal: principal_part, interest });
    }
    Ok(out)
}

fn principal_f(amount: Decimal) -> f64 {
    amount.to_string().parse().unwrap_or(0.0)
}

/// Spread a repayment over instalments in order, each taking at most what it still owes. Returns what each
/// took and what is left over.
pub fn allocate(owed: &[(i64, Decimal)], amount: Decimal) -> Result<(Vec<(i64, Decimal)>, Decimal)> {
    let mut left = amount;
    let mut out = Vec::new();
    for (n, owes) in owed {
        if left.is_zero() || left.is_negative() {
            break;
        }
        if owes.is_zero() || owes.is_negative() {
            continue;
        }
        let take = if left < *owes { left } else { *owes };
        out.push((*n, take));
        left = left - take;
    }
    Ok((out, left))
}

/// What payroll may take: what is due, but never more than `percent` of what is available. The rest stays due.
pub fn deduction(due: Decimal, available: Decimal, percent: i64) -> Result<Decimal> {
    if !(0..=100).contains(&percent) {
        return Err(Error::msg("a deduction cap is a percentage between 0 and 100"));
    }
    if available.is_negative() {
        return Ok(Decimal::zero(due.scale()));
    }
    let cap = available.times_ratio(percent, 100)?;
    Ok(if due < cap { due } else { cap })
}

/// To close a loan today: all principal still unpaid, plus interest only on instalments already due; interest on
/// the future is not charged.
pub fn foreclosure(unpaid: &[(Instalment, Decimal)], on: NaiveDate) -> Result<Decimal> {
    // (instalment, how much of its total is already paid)
    let mut sum = Decimal::zero(unpaid.first().map(|(i, _)| i.principal.scale()).unwrap_or(2));
    for (inst, paid) in unpaid {
        let owes = inst.total() - *paid;
        if owes.is_zero() || owes.is_negative() {
            continue;
        }
        if inst.due <= on {
            sum = sum + owes;
        } else {
            // Future instalment: its principal only (less anything paid toward it, taken off the principal).
            let principal_owed = inst.principal - (if *paid > inst.interest { *paid - inst.interest } else { Decimal::zero(sum.scale()) });
            if principal_owed.is_negative() {
                continue;
            }
            sum = sum + principal_owed;
        }
    }
    Ok(sum)
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

    fn total(rows: &[Instalment]) -> (Decimal, Decimal) {
        rows.iter().fold((Decimal::zero(2), Decimal::zero(2)), |(p, i), r| (p + r.principal, i + r.interest))
    }

    #[test]
    fn an_interest_free_loan_splits_the_principal_exactly() -> Result<()> {
        let rows = schedule(n("1000.00"), 3, 0, Method::Reducing, d("2026-11-01"))?;
        assert_eq!(rows.len(), 3);
        assert_eq!(total(&rows).0, n("1000.00"));
        assert!(rows.iter().all(|r| r.interest.is_zero()));
        assert_eq!(rows[2].principal, n("333.34"), "the last takes the rounding");
        assert_eq!(rows[1].due, d("2026-12-01"));
        Ok(())
    }

    #[test]
    fn a_reducing_loan_pays_off_to_the_cent() -> Result<()> {
        let rows = schedule(n("12000.00"), 12, 1200, Method::Reducing, d("2026-11-01"))?;
        assert_eq!(total(&rows).0, n("12000.00"));
        assert_eq!(rows[0].interest, n("120.00"), "1% a month on the first balance");
        assert!(rows[11].interest < rows[0].interest, "interest falls as the balance does");
        // Instalments are level but for the last, which absorbs rounding.
        assert_eq!(rows[0].total(), rows[5].total());
        Ok(())
    }

    #[test]
    fn flat_interest_is_on_the_whole_principal() -> Result<()> {
        let rows = schedule(n("12000.00"), 12, 1000, Method::Flat, d("2026-11-01"))?;
        assert_eq!(total(&rows), (n("12000.00"), n("1200.00")));
        Ok(())
    }

    #[test]
    fn nonsense_terms_are_refused() {
        assert!(schedule(n("0.00"), 3, 0, Method::Flat, d("2026-11-01")).is_err());
        assert!(schedule(n("100.00"), 0, 0, Method::Flat, d("2026-11-01")).is_err());
        assert!(schedule(n("100.00"), 121, 0, Method::Flat, d("2026-11-01")).is_err());
    }

    #[test]
    fn a_repayment_goes_to_the_oldest_instalment_first() -> Result<()> {
        let owed = [(1, n("100.00")), (2, n("100.00")), (3, n("100.00"))];
        let (taken, left) = allocate(&owed, n("250.00"))?;
        assert_eq!(taken, vec![(1, n("100.00")), (2, n("100.00")), (3, n("50.00"))]);
        assert!(left.is_zero());
        let (taken, left) = allocate(&owed[..1], n("150.00"))?;
        assert_eq!(taken.len(), 1);
        assert_eq!(left, n("50.00"));
        Ok(())
    }

    #[test]
    fn payroll_takes_no_more_than_the_cap() -> Result<()> {
        assert_eq!(deduction(n("300.00"), n("1000.00"), 40)?, n("300.00"));
        assert_eq!(deduction(n("300.00"), n("500.00"), 40)?, n("200.00"));
        assert!(deduction(n("300.00"), n("-5.00"), 40)?.is_zero());
        assert!(deduction(n("1.00"), n("1.00"), 101).is_err());
        Ok(())
    }

    #[test]
    fn foreclosure_charges_no_future_interest() -> Result<()> {
        let rows = schedule(n("12000.00"), 12, 1200, Method::Reducing, d("2026-11-01"))?;
        let all: Vec<(Instalment, Decimal)> = rows.iter().cloned().map(|r| (r, Decimal::zero(2))).collect();
        // On the day the first instalment is due: it is owed in full, the rest by principal only.
        let quote = foreclosure(&all, d("2026-11-01"))?;
        let expected = rows[0].total() + rows[1..].iter().fold(Decimal::zero(2), |s, r| s + r.principal);
        assert_eq!(quote, expected);
        assert!(quote < rows.iter().fold(Decimal::zero(2), |s, r| s + r.total()));
        Ok(())
    }
}
