//! The rules of running a payroll, with nothing read from a database.

use std::collections::BTreeMap;

use aether_sdk::dates::NaiveDate;
use aether_sdk::decimal::Decimal;
use aether_sdk::{Error, Result};
use serde::{Deserialize, Serialize};

/// Inputs the payroll fills in itself; a structure may declare them, an assignment may not set them.
/// A clerk may still correct `unpaid_leave_days` and `overtime_hours` for one person in one run (the other four
/// are facts about the period and the contract).
pub const RESERVED_INPUTS: &[&str] = &["base_salary", "period_days", "days_worked", "periods_per_year", "unpaid_leave_days", "overtime_hours"];

pub const FIXED_FOR_A_RUN: &[&str] = &["base_salary", "period_days", "days_worked", "periods_per_year"];

/// Overtime minutes as hours with two digits (90 minutes is 1.50).
pub fn hours_of(minutes: i64) -> Result<Decimal> {
    Decimal::whole(minutes, 2)?.times_ratio(1, 60)
}

/// A monthly run covers at most 31 days.
pub fn check_period(start: NaiveDate, end: NaiveDate) -> Result<()> {
    if end < start {
        return Err(Error::msg("the period ends after it starts"));
    }
    if (end - start).num_days() > 30 {
        return Err(Error::msg("a pay period is at most 31 days"));
    }
    Ok(())
}

/// The days in a period, and the days the person was employed in it (hire and last day count);
/// `None` when they were not employed at all in the period.
pub fn employed_days(start: NaiveDate, end: NaiveDate, hired: Option<NaiveDate>, last_day: Option<NaiveDate>) -> Option<(i64, i64)> {
    let period = (end - start).num_days() + 1;
    let from = hired.map_or(start, |h| h.max(start));
    let to = last_day.map_or(end, |l| l.min(end));
    if to < from {
        return None;
    }
    Some((period, (to - from).num_days() + 1))
}

/// Whether a dated assignment (or any dated record) overlaps a period.
pub fn overlaps_period(valid_from: NaiveDate, valid_to: Option<NaiveDate>, start: NaiveDate, end: NaiveDate) -> bool {
    valid_from <= end && valid_to.is_none_or(|to| to >= start)
}

/// An assignment that would overlap another for the same person is refused: one structure at a time.
pub fn assignments_overlap(a_from: NaiveDate, a_to: Option<NaiveDate>, b_from: NaiveDate, b_to: Option<NaiveDate>) -> bool {
    a_from <= b_to.unwrap_or(NaiveDate::MAX) && b_from <= a_to.unwrap_or(NaiveDate::MAX)
}

/// Sums for a batch or a whole run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    pub employees: i64,
    pub calculated: i64,
    pub skipped: i64,
    pub failed: i64,
    pub gross: Decimal,
    pub deductions: Decimal,
    pub net: Decimal,
    pub employer_cost: Decimal,
    pub shortfall: Decimal,
    /// By component id: its kind and the sum of its lines.
    pub components: BTreeMap<String, ComponentTotal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentTotal {
    pub kind: String,
    pub amount: Decimal,
}

impl Default for Totals {
    fn default() -> Self {
        let zero = Decimal::zero(2);
        Self { employees: 0, calculated: 0, skipped: 0, failed: 0, gross: zero, deductions: zero, net: zero, employer_cost: zero, shortfall: zero, components: BTreeMap::new() }
    }
}

impl Totals {
    pub fn add_line(&mut self, id: &str, kind: &str, amount: Decimal) {
        let entry = self.components.entry(id.to_string()).or_insert_with(|| ComponentTotal { kind: kind.to_string(), amount: Decimal::zero(2) });
        entry.amount = entry.amount + amount;
    }

    pub fn merge(&mut self, other: &Totals) {
        self.employees += other.employees;
        self.calculated += other.calculated;
        self.skipped += other.skipped;
        self.failed += other.failed;
        self.gross = self.gross + other.gross;
        self.deductions = self.deductions + other.deductions;
        self.net = self.net + other.net;
        self.employer_cost = self.employer_cost + other.employer_cost;
        self.shortfall = self.shortfall + other.shortfall;
        for (id, total) in &other.components {
            self.add_line(id, &total.kind, total.amount);
        }
    }

    /// What a payroll must satisfy: earnings less deductions is net, to the cent, with shortfall held back.
    pub fn is_consistent(&self) -> bool {
        let earnings = self.components.values().filter(|c| c.kind == "earning").fold(Decimal::zero(2), |s, c| s + c.amount);
        let deductions = self.components.values().filter(|c| c.kind == "deduction").fold(Decimal::zero(2), |s, c| s + c.amount);
        let employer = self.components.values().filter(|c| c.kind == "employer").fold(Decimal::zero(2), |s, c| s + c.amount);
        earnings == self.gross && deductions == self.deductions && employer == self.employer_cost && self.gross - self.deductions + self.shortfall == self.net
    }
}

/// The first day of the next page of a cursor-walk is the last key seen.
pub fn last_key(keys: &[String]) -> Option<&String> {
    keys.last()
}

/// A batch's number across the whole run: the planning page it came from and its place in that page's
/// fan-out (every page counts its chunks from 0, so the chunk number alone would collide between pages).
pub fn batch_index(page: i64, chunk: u32) -> i64 {
    page * 1000 + i64::from(chunk)
}

/// The employees of one page, each once, in order (a person with two overlapping assignments shows twice).
pub fn distinct(keys: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(keys.len());
    for key in keys {
        if out.last() != Some(&key) {
            out.push(key);
        }
    }
    out
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
    fn overtime_minutes_become_hours() -> Result<()> {
        assert_eq!(hours_of(90)?, d("1.50")?);
        assert_eq!(hours_of(100)?, d("1.67")?);
        assert_eq!(hours_of(0)?, d("0.00")?);
        Ok(())
    }

    #[test]
    fn a_period_is_a_month_at_most() -> Result<()> {
        assert!(check_period(day("2026-10-01")?, day("2026-10-31")?).is_ok());
        assert!(check_period(day("2026-10-01")?, day("2026-11-05")?).is_err());
        assert!(check_period(day("2026-10-05")?, day("2026-10-01")?).is_err());
        Ok(())
    }

    #[test]
    fn days_worked_follow_hire_and_leaving() -> Result<()> {
        let (start, end) = (day("2026-10-01")?, day("2026-10-31")?);
        assert_eq!(employed_days(start, end, Some(day("2025-01-01")?), None), Some((31, 31)));
        assert_eq!(employed_days(start, end, Some(day("2026-10-21")?), None), Some((31, 11)), "hired on the 21st: the 21st to the 31st");
        assert_eq!(employed_days(start, end, None, Some(day("2026-10-10")?)), Some((31, 10)));
        assert_eq!(employed_days(start, end, Some(day("2026-10-05")?), Some(day("2026-10-05")?)), Some((31, 1)));
        assert_eq!(employed_days(start, end, Some(day("2026-11-01")?), None), None, "not yet hired");
        assert_eq!(employed_days(start, end, None, Some(day("2026-09-30")?)), None, "already left");
        Ok(())
    }

    #[test]
    fn periods_and_assignments_overlap_as_expected() -> Result<()> {
        let (start, end) = (day("2026-10-01")?, day("2026-10-31")?);
        assert!(overlaps_period(day("2026-10-31")?, None, start, end));
        assert!(!overlaps_period(day("2026-11-01")?, None, start, end));
        assert!(!overlaps_period(day("2026-01-01")?, Some(day("2026-09-30")?), start, end));
        assert!(overlaps_period(day("2026-01-01")?, Some(day("2026-10-01")?), start, end));
        assert!(assignments_overlap(day("2026-01-01")?, None, day("2026-06-01")?, None));
        assert!(!assignments_overlap(day("2026-01-01")?, Some(day("2026-05-31")?), day("2026-06-01")?, None));
        Ok(())
    }

    #[test]
    fn totals_add_up_across_batches() -> Result<()> {
        let mut a = Totals { employees: 2, calculated: 2, gross: d("3000.00")?, deductions: d("500.00")?, net: d("2500.00")?, employer_cost: d("150.00")?, ..Totals::default() };
        a.add_line("base", "earning", d("3000.00")?);
        a.add_line("paye", "deduction", d("500.00")?);
        a.add_line("nssf_employer", "employer", d("150.00")?);
        assert!(a.is_consistent());
        let mut b = Totals { employees: 1, calculated: 1, failed: 0, gross: d("1000.00")?, deductions: d("100.00")?, net: d("900.00")?, ..Totals::default() };
        b.add_line("base", "earning", d("1000.00")?);
        b.add_line("paye", "deduction", d("100.00")?);
        a.merge(&b);
        assert_eq!((a.employees, a.gross, a.net), (3, d("4000.00")?, d("3400.00")?));
        assert_eq!(a.components["base"].amount, d("4000.00")?);
        assert!(a.is_consistent());
        a.net = d("3401.00")?;
        assert!(!a.is_consistent(), "a net that does not follow from the lines is caught");
        Ok(())
    }

    #[test]
    fn a_held_back_shortfall_keeps_the_totals_consistent() -> Result<()> {
        let mut t = Totals { gross: d("100.00")?, deductions: d("130.00")?, net: d("0.00")?, shortfall: d("30.00")?, ..Totals::default() };
        t.add_line("base", "earning", d("100.00")?);
        t.add_line("loan", "deduction", d("130.00")?);
        assert!(t.is_consistent());
        Ok(())
    }

    #[test]
    fn batches_of_different_pages_never_share_a_number() {
        assert_ne!(batch_index(0, 1), batch_index(1, 1));
        assert!(batch_index(0, 19) < batch_index(1, 0), "numbers still go in the order of the pages");
    }

    #[test]
    fn a_page_lists_each_person_once() {
        let page = distinct(vec!["a".into(), "a".into(), "b".into(), "c".into(), "c".into()]);
        assert_eq!(page, vec!["a", "b", "c"]);
        assert_eq!(last_key(&page).map(String::as_str), Some("c"));
    }
}
