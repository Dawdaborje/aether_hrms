//! The rules of running a payroll, with nothing read from a database.

use std::collections::BTreeMap;

use aether_sdk::dates::NaiveDate;
use aether_sdk::decimal::Decimal;
use aether_sdk::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Inputs the payroll fills in itself; a structure may declare them, an assignment may not set them.
/// A clerk may still correct `unpaid_leave_days` and `overtime_hours` for one person in one run (the other four
/// are facts about the period and the contract).
pub const RESERVED_INPUTS: &[&str] = &["base_salary", "period_days", "days_worked", "periods_per_year", "unpaid_leave_days", "overtime_hours", "exemptions_annual", "other_income_annual"];

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
    /// By structure id: the sum of each of its components, in the structure's order, as one comma-joined string
    /// (`rules::lines_text`). What a batch reports instead of `components`: a structure of a thousand
    /// components is one short string to read and add up rather than a thousand entries. Turned into
    /// `components` once, when the run is finalised ([`Totals::resolve_sums`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sums: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentTotal {
    pub kind: String,
    pub amount: Decimal,
}

impl Default for Totals {
    fn default() -> Self {
        let zero = Decimal::zero(2);
        Self { employees: 0, calculated: 0, skipped: 0, failed: 0, gross: zero, deductions: zero, net: zero, employer_cost: zero, shortfall: zero, components: BTreeMap::new(), sums: BTreeMap::new() }
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
        for (structure, text) in &other.sums {
            let mut total = parse_sums(self.sums.get(structure).map_or("", String::as_str));
            let more = parse_sums(text);
            if total.len() < more.len() {
                total.resize(more.len(), sp_engine::Decimal::ZERO);
            }
            for (sum, amount) in total.iter_mut().zip(&more) {
                *sum += *amount;
            }
            self.sums.insert(structure.clone(), lines_text(&total));
        }
    }

    /// Turn the sums by structure into totals by component, given each structure's components (id and kind).
    pub fn resolve_sums(&mut self, mut components_of: impl FnMut(&str) -> Result<Vec<(String, String)>>) -> Result<()> {
        for (structure, text) in std::mem::take(&mut self.sums) {
            let components = components_of(&structure)?;
            let components: Vec<(&str, &str)> = components.iter().map(|(id, kind)| (id.as_str(), kind.as_str())).collect();
            add_component_sums(self, &components, &parse_sums(&text))?;
        }
        Ok(())
    }

    /// What a payroll must satisfy: earnings less deductions is net, to the cent, with shortfall held back.
    pub fn is_consistent(&self) -> bool {
        let earnings = self.components.values().filter(|c| c.kind == "earning").fold(Decimal::zero(2), |s, c| s + c.amount);
        let deductions = self.components.values().filter(|c| c.kind == "deduction").fold(Decimal::zero(2), |s, c| s + c.amount);
        let employer = self.components.values().filter(|c| c.kind == "employer").fold(Decimal::zero(2), |s, c| s + c.amount);
        earnings == self.gross && deductions == self.deductions && employer == self.employer_cost && self.gross - self.deductions + self.shortfall == self.net
    }
}

/// A payslip keeps the amount of every component of its structure, in the structure's order, as text:
/// `["50000.00", "2000.00", ...]`. The structure is immutable and the payslip records which one it used, so the
/// ids, labels and kinds are not repeated on every payslip (they were about 100 bytes a line, and turning them
/// into JSON was most of the work of calculating a person).
/// A payslip's amounts as stored by a batch: one string, the amounts in the structure's order joined by commas
/// (one JSON value to read and write, however many components the structure has).
/// The amounts of a [`lines_text`] string.
pub fn parse_sums(text: &str) -> Vec<sp_engine::Decimal> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split(',').map(|a| a.parse().unwrap_or_default()).collect()
}

pub fn lines_text(amounts: &[sp_engine::Decimal]) -> String {
    amounts.iter().map(ToString::to_string).collect::<Vec<_>>().join(",")
}

/// The stored `lines` of a payslip as one value per line: the text form above split up, or the array forms
/// (compact or one object per line) as they are.
pub fn stored_lines(stored: &Value) -> Vec<Value> {
    match stored {
        Value::String(text) if text.is_empty() => Vec::new(),
        Value::String(text) => text.split(',').map(|a| Value::String(a.to_string())).collect(),
        Value::Array(items) => items.clone(),
        _ => Vec::new(),
    }
}

/// The amounts as an array of strings: how payslips were stored before `lines_text`.
#[cfg(test)]
pub fn compact_lines(amounts: &[sp_engine::Decimal]) -> Vec<Value> {
    amounts.iter().map(|a| Value::String(a.to_string())).collect()
}

/// The rows of a payslip as `{label, kind, amount}`, from what it stored: the compact form above (read against
/// the components of its structure) or the older form, one object per line, which is passed through.
pub fn expand_lines(stored: &[Value], components: &[Value]) -> Result<Vec<Value>> {
    if stored.iter().any(Value::is_string) && stored.len() != components.len() {
        return Err(Error::msg(format!("this payslip has {} amounts but its structure has {} components", stored.len(), components.len())));
    }
    Ok(stored
        .iter()
        .enumerate()
        .map(|(index, line)| match line {
            Value::String(_) => {
                let component = &components[index];
                let label = ["label", "id"].iter().find_map(|k| component.get(*k).and_then(Value::as_str)).unwrap_or_default();
                json!({ "label": label, "kind": component.get("kind").cloned().unwrap_or(Value::Null), "amount": line })
            }
            other => other.clone(),
        })
        .collect())
}

/// Add up what each component came to over a batch: `sums` is one running total per component of `components`
/// (id and kind), in the same order. Information lines are not part of any total. Adds the same as calling
/// `Totals::add_line` for every line of every person, with one conversion per component instead of one per line.
pub fn add_component_sums(totals: &mut Totals, components: &[(&str, &str)], sums: &[sp_engine::Decimal]) -> Result<()> {
    for ((id, kind), sum) in components.iter().zip(sums) {
        if *kind == "info" {
            continue;
        }
        // Straight from the engine's decimal (to the cent, as payslips are), not through its text.
        let mut cents = *sum;
        cents.rescale(2);
        let units = i64::try_from(cents.mantissa()).map_err(|_| Error::msg(format!("the sum of {id} is too large")))?;
        totals.add_line(id, kind, Decimal::from_units(units, 2)?);
    }
    Ok(())
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
    fn lines_are_kept_compactly_and_read_back_against_the_structure() -> Result<()> {
        let amounts: Vec<sp_engine::Decimal> = ["50000.00", "0.00", "2500.00"].iter().map(|s| s.parse().unwrap()).collect();
        let stored = compact_lines(&amounts);
        assert_eq!(stored, vec![json!("50000.00"), json!("0.00"), json!("2500.00")]);
        let components = vec![json!({ "id": "base", "label": "Base salary", "kind": "earning" }), json!({ "id": "loan", "label": "Loan", "kind": "deduction" }), json!({ "id": "paye" , "kind": "deduction" })];
        let rows = expand_lines(&stored, &components)?;
        assert_eq!(rows[0], json!({ "label": "Base salary", "kind": "earning", "amount": "50000.00" }));
        assert_eq!(rows[1], json!({ "label": "Loan", "kind": "deduction", "amount": "0.00" }));
        assert_eq!(rows[2]["label"], json!("paye"), "a component without a label shows its id");
        assert!(expand_lines(&stored, &components[..2]).is_err(), "amounts that do not fit the structure are refused, not shown wrongly");
        // The text form a batch stores reads back the same as the array form.
        let text = json!(lines_text(&amounts));
        assert_eq!(expand_lines(&stored_lines(&text), &components)?, rows);
        assert!(stored_lines(&json!("")).is_empty());
        Ok(())
    }

    #[test]
    fn payslips_stored_the_old_way_still_read() -> Result<()> {
        let old = vec![json!({ "id": "base", "label": "Base", "kind": "earning", "amount": "100.00", "applied": true, "tags": ["earning"] })];
        assert_eq!(expand_lines(&old, &[])?, old);
        assert!(expand_lines(&[], &[])?.is_empty());
        Ok(())
    }

    #[test]
    fn component_sums_equal_adding_every_line() -> Result<()> {
        let components = [("base", "earning"), ("gross", "info"), ("paye", "deduction"), ("nssf", "employer")];
        let people: Vec<Vec<&str>> = vec![vec!["1000.00", "1000.00", "100.10", "50.05"], vec!["2500.50", "2500.50", "0.00", "125.03"]];
        let mut by_lines = Totals::default();
        let mut sums: Vec<sp_engine::Decimal> = vec![Default::default(); components.len()];
        for person in &people {
            for (i, (id, kind)) in components.iter().enumerate() {
                let amount: sp_engine::Decimal = person[i].parse().unwrap();
                sums[i] += amount;
                if *kind != "info" {
                    by_lines.add_line(id, kind, d(person[i])?);
                }
            }
        }
        let mut by_sums = Totals::default();
        add_component_sums(&mut by_sums, &components, &sums)?;
        assert_eq!(by_sums.components, by_lines.components);
        assert!(!by_sums.components.contains_key("gross"));
        assert_eq!(by_sums.components["paye"].amount, d("100.10")?);
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
