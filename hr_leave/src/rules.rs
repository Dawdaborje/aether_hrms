//! The arithmetic of leave, with no records read: what a request costs, what a hire is owed for
//! the part of a year, what an earned-leave schedule gives, and the **ledger**.
//!
//! The ledger is the design that comes out of reading both Odoo 19 and Frappe HRMS. Odoo replays
//! every leave against every allocation each time a balance is asked for and edits accrual
//! allocations in place, leaving no history; Frappe has a ledger but deletes rows and finds expiry
//! rows by timestamp. Here the ledger is append-only: an entry is a signed number of days with the
//! day it counts from and (for grants) the day it lapses, a cancellation is a **reversal** entry,
//! and the balance is a single pass over the entries, spending the soonest-expiring grant first.

use std::collections::HashSet;

use aether_sdk::dates::{Calendar, Datelike, Duration, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::{Error, Result};

/// Digits after the point of every day count.
pub const SCALE: u32 = 2;

/// Which half of a single day is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Half {
    Whole,
    Morning,
    Afternoon,
}

impl Half {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "" | "none" => Some(Self::Whole),
            "morning" => Some(Self::Morning),
            "afternoon" => Some(Self::Afternoon),
            _ => None,
        }
    }
}

fn whole(days: u32) -> Result<Decimal> {
    Decimal::whole(i64::from(days), SCALE)
}

/// Days a request costs. Holidays and weekends inside the period are free unless the type counts
/// every calendar day (`count_all_days`, as for maternity leave). A half day is a single day.
pub fn leave_days(calendar: &Calendar, start: NaiveDate, end: NaiveDate, half: Half, count_all_days: bool) -> Result<Decimal> {
    if end < start {
        return Err(Error::msg("the leave ends before it starts"));
    }
    if half != Half::Whole && start != end {
        return Err(Error::msg("half a day can only be taken on a single day"));
    }
    let days = if count_all_days {
        u32::try_from((end - start).num_days() + 1).unwrap_or(0)
    } else {
        calendar.working_days(start, end)
    };
    if half != Half::Whole {
        return if days == 0 { Ok(Decimal::zero(SCALE)) } else { Decimal::parse("0.5")?.with_scale(SCALE) };
    }
    whole(days)
}

/// The days off between the end of one leave and the start of the next, when nothing but days off
/// lies between them. Under the sandwich rule those days count as leave too. `None` when there is a
/// working day in between (or no gap at all).
pub fn sandwiched_days(calendar: &Calendar, earlier_end: NaiveDate, later_start: NaiveDate) -> Option<u32> {
    if later_start <= earlier_end + Duration::days(1) {
        return None;
    }
    let mut day = earlier_end + Duration::days(1);
    let mut count = 0;
    while day < later_start {
        if calendar.is_working_day(day) {
            return None;
        }
        count += 1;
        day += Duration::days(1);
    }
    Some(count)
}

/// What a person hired during `year` is owed of the year's `annual` days: the share of the year they
/// are employed, rounded to the nearest half day. Everyone hired earlier gets all of it.
pub fn share_of_year(annual: Decimal, hired: Option<NaiveDate>, year: i32) -> Result<Decimal> {
    let Some(hired) = hired else { return Ok(annual) };
    if hired.year() < year {
        return Ok(annual);
    }
    if hired.year() > year {
        return Ok(Decimal::zero(annual.scale()));
    }
    let (first, last) = year_bounds(year).ok_or_else(|| Error::msg("that is not a year"))?;
    let in_year = (last - hired).num_days() + 1;
    let length = (last - first).num_days() + 1;
    annual.times_ratio(in_year, length)?.round_to_half()
}

/// How often earned leave is granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frequency {
    Monthly,
    Quarterly,
    HalfYearly,
    Yearly,
}

impl Frequency {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "monthly" => Some(Self::Monthly),
            "quarterly" => Some(Self::Quarterly),
            "half_yearly" => Some(Self::HalfYearly),
            "yearly" => Some(Self::Yearly),
            _ => None,
        }
    }

    pub fn periods(self) -> u32 {
        match self {
            Self::Monthly => 12,
            Self::Quarterly => 4,
            Self::HalfYearly => 2,
            Self::Yearly => 1,
        }
    }
}

/// When in its period earned leave arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocateOn {
    First,
    Last,
}

/// The grants of a year's earned leave that are due by `today`: `(period number, day)`. The first
/// period is number 1. Only periods whose day has come are listed.
pub fn earned_due(frequency: Frequency, allocate_on: AllocateOn, year: i32, today: NaiveDate) -> Vec<(u32, NaiveDate)> {
    let months = 12 / frequency.periods();
    let mut due = Vec::new();
    for period in 1..=frequency.periods() {
        let first_month = (period - 1) * months + 1;
        let day = match allocate_on {
            AllocateOn::First => NaiveDate::from_ymd_opt(year, first_month, 1),
            AllocateOn::Last => {
                let next = if first_month + months > 12 { NaiveDate::from_ymd_opt(year + 1, 1, 1) } else { NaiveDate::from_ymd_opt(year, first_month + months, 1) };
                next.map(|next| next - Duration::days(1))
            }
        };
        if let Some(day) = day {
            if day <= today {
                due.push((period, day));
            }
        }
    }
    due
}

/// One grant of earned leave: the year's days divided over the periods, rounded as the type says
/// (`0` for no rounding, or `0.25`, `0.5`, `1`).
pub fn earned_amount(annual: Decimal, frequency: Frequency, rounding: Option<&str>) -> Result<Decimal> {
    // Work in hundredths so the division and the rounding are exact.
    let share = annual.with_scale(SCALE)?.times_ratio(1, i64::from(frequency.periods()))?;
    let step = match rounding {
        None | Some("") | Some("0") => return Ok(share),
        Some(step) => decimal_units(Decimal::parse(step)?)?,
    };
    if step <= 0 {
        return Ok(share);
    }
    let hundredths = decimal_units(share)?;
    let rounded = ((hundredths + step / 2) / step) * step;
    Decimal::parse(&format!("{}.{:02}", rounded / 100, rounded % 100))?.with_scale(SCALE)
}

/// A decimal in hundredths of a day.
fn decimal_units(value: Decimal) -> Result<i64> {
    let scaled = value.round_to(SCALE).with_scale(SCALE)?;
    let text = scaled.to_string();
    let negative = text.starts_with('-');
    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    let units: i64 = digits.parse().map_err(|_| Error::msg("not a number"))?;
    Ok(if negative { -units } else { units })
}

/// Whether someone may take a kind of leave: the gender it is meant for and the service it needs.
pub fn eligible(rule_gender: &str, gender: Option<&str>, min_days: i64, hired: Option<NaiveDate>, on: NaiveDate) -> Result<()> {
    if !matches!(rule_gender, "" | "any") && gender != Some(rule_gender) {
        return Err(Error::msg(format!("this leave is for {rule_gender} employees")));
    }
    if min_days > 0 {
        let served = hired.map_or(0, |hired| (on - hired).num_days().max(0));
        if served < min_days {
            return Err(Error::msg(format!("this leave needs {min_days} days of service ({served} so far)")));
        }
    }
    Ok(())
}

/// The first and last day of a year.
pub fn year_bounds(year: i32) -> Option<(NaiveDate, NaiveDate)> {
    Some((NaiveDate::from_ymd_opt(year, 1, 1)?, NaiveDate::from_ymd_opt(year, 12, 31)?))
}

/// Whether two requests share a day.
pub fn overlap(a: (NaiveDate, NaiveDate), b: (NaiveDate, NaiveDate)) -> bool {
    a.0 <= b.1 && b.0 <= a.1
}

// ---------------------------------------------------------------- the ledger

/// What an entry is. Grants (`days` > 0) are `Allocation`, `Accrual` and `CarryForward`; what is
/// taken (`days` < 0) is `Usage` (approved leave), `Reservation` (a request waiting for approval),
/// `Encashment`; `Adjustment` goes either way; `Expiry` only records that a grant lapsed (it is not
/// counted, since a grant's lapse is worked out from its own end day); `Reversal` cancels the entry
/// it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Allocation,
    Accrual,
    CarryForward,
    Usage,
    Reservation,
    Expiry,
    Encashment,
    Adjustment,
    Reversal,
}

impl Kind {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "allocation" => Self::Allocation,
            "accrual" => Self::Accrual,
            "carry_forward" => Self::CarryForward,
            "usage" => Self::Usage,
            "reservation" => Self::Reservation,
            "expiry" => Self::Expiry,
            "encashment" => Self::Encashment,
            "adjustment" => Self::Adjustment,
            "reversal" => Self::Reversal,
            _ => return None,
        })
    }
}

/// One line of the ledger.
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: String,
    pub kind: Kind,
    /// Signed.
    pub days: Decimal,
    /// A grant counts from this day; a deduction is charged on it.
    pub date: NaiveDate,
    /// The last day a grant can be used; none: it never lapses.
    pub valid_to: Option<NaiveDate>,
    /// For a reversal, the entry it cancels.
    pub reverses: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Balance {
    /// Grants that can be used on the day.
    pub allocated: Decimal,
    /// Approved leave and other deductions charged against them.
    pub spent: Decimal,
    /// Requests waiting for approval, held back.
    pub pending: Decimal,
    /// What is left to ask for: allocated less spent and pending; negative when over.
    pub available: Decimal,
    /// Days that lapse on or before this horizon, so it is clear what must be used soon.
    pub expiring_soon: Decimal,
}

struct Bucket {
    id: String,
    date: NaiveDate,
    valid_to: Option<NaiveDate>,
    days: Decimal,
    spent: Decimal,
    pending: Decimal,
}

impl Bucket {
    fn remaining(&self) -> Decimal {
        self.days - self.spent - self.pending
    }

    fn live_on(&self, day: NaiveDate) -> bool {
        self.date <= day && self.valid_to.is_none_or(|end| day <= end)
    }
}

/// Charge every deduction to the grants valid on its own day, soonest to lapse first (carried-over
/// days before the new year's). What no grant covers is an overdraft.
fn consume(entries: &[Entry]) -> (Vec<Bucket>, Decimal) {
    let zero = Decimal::zero(SCALE);
    // A reversal and the entry it names cancel out, as if neither had been written.
    let reversed: HashSet<&str> = entries.iter().filter_map(|e| e.reverses.as_deref()).collect();
    let live: Vec<&Entry> = entries
        .iter()
        .filter(|e| !matches!(e.kind, Kind::Reversal | Kind::Expiry) && !reversed.contains(e.id.as_str()))
        .collect();

    let mut buckets: Vec<Bucket> = live
        .iter()
        .filter(|e| !e.days.is_negative() && !e.days.is_zero())
        .map(|e| Bucket { id: e.id.clone(), date: e.date, valid_to: e.valid_to, days: e.days, spent: zero, pending: zero })
        .collect();
    // Soonest to lapse first; a grant that never lapses last.
    buckets.sort_by_key(|b| (b.valid_to.is_none(), b.valid_to, b.date));

    let mut deductions: Vec<&&Entry> = live.iter().filter(|e| e.days.is_negative()).collect();
    deductions.sort_by_key(|e| e.date);
    let mut overdraft = zero;
    for entry in deductions {
        let mut left = -entry.days;
        let pending = entry.kind == Kind::Reservation;
        for bucket in buckets.iter_mut().filter(|b| b.live_on(entry.date)) {
            if left.is_zero() {
                break;
            }
            let room = bucket.remaining();
            if room.is_negative() || room.is_zero() {
                continue;
            }
            let take = if room < left { room } else { left };
            if pending {
                bucket.pending = bucket.pending + take;
            } else {
                bucket.spent = bucket.spent + take;
            }
            left = left - take;
        }
        overdraft = overdraft + left;
    }
    (buckets, overdraft)
}

/// The balance on `on`. `soon` is the horizon for "expiring soon".
pub fn balance(entries: &[Entry], on: NaiveDate, soon: NaiveDate) -> Result<Balance> {
    let zero = Decimal::zero(SCALE);
    let (buckets, overdraft) = consume(entries);
    let mut result = Balance { allocated: zero, spent: zero, pending: zero, available: zero, expiring_soon: zero };
    for bucket in buckets.iter().filter(|b| b.live_on(on)) {
        result.allocated = result.allocated + bucket.days;
        result.spent = result.spent + bucket.spent;
        result.pending = result.pending + bucket.pending;
        if bucket.valid_to.is_some_and(|end| end <= soon) {
            let left = bucket.remaining();
            if !left.is_negative() {
                result.expiring_soon = result.expiring_soon + left;
            }
        }
    }
    // Overdraft is charged on top of what the live grants still hold.
    result.spent = result.spent + overdraft;
    result.available = result.allocated - result.spent - result.pending;
    Ok(result)
}

/// Grants that lapsed before `today` with days still unused: `(entry id, days lost)`. The nightly
/// job writes an `Expiry` entry for each, once, so the history shows what was lost and when.
pub fn lapsed(entries: &[Entry], today: NaiveDate) -> Vec<(String, Decimal)> {
    let (buckets, _) = consume(entries);
    buckets
        .into_iter()
        .filter(|b| b.valid_to.is_some_and(|end| end < today))
        .filter_map(|b| {
            let left = b.remaining();
            (!left.is_negative() && !left.is_zero()).then_some((b.id, left))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use aether_sdk::dates::parse_date;

    use super::*;

    fn d(text: &str) -> NaiveDate {
        parse_date(text).unwrap_or_default()
    }

    fn n(text: &str) -> Decimal {
        Decimal::parse(text).unwrap_or(Decimal::zero(0))
    }

    fn entry(id: &str, kind: Kind, days: &str, date: &str, to: Option<&str>) -> Entry {
        Entry { id: id.into(), kind, days: n(days), date: d(date), valid_to: to.map(d), reverses: None }
    }

    fn reversal(id: &str, of: &str, days: &str, date: &str) -> Entry {
        Entry { id: id.into(), kind: Kind::Reversal, days: n(days), date: d(date), valid_to: None, reverses: Some(of.into()) }
    }

    #[test]
    fn a_request_costs_its_working_days() -> Result<()> {
        let mut calendar = Calendar::default();
        assert_eq!(leave_days(&calendar, d("2026-10-05"), d("2026-10-11"), Half::Whole, false)?.to_string(), "5.00");
        calendar.holidays.push(d("2026-10-07"));
        assert_eq!(leave_days(&calendar, d("2026-10-05"), d("2026-10-11"), Half::Whole, false)?.to_string(), "4.00");
        assert_eq!(leave_days(&calendar, d("2026-10-05"), d("2026-10-11"), Half::Whole, true)?.to_string(), "7.00");
        assert!(leave_days(&calendar, d("2026-10-11"), d("2026-10-05"), Half::Whole, false).is_err());
        Ok(())
    }

    #[test]
    fn half_days_are_single_working_days() -> Result<()> {
        let calendar = Calendar::default();
        assert_eq!(leave_days(&calendar, d("2026-10-05"), d("2026-10-05"), Half::Morning, false)?.to_string(), "0.50");
        assert!(leave_days(&calendar, d("2026-10-05"), d("2026-10-06"), Half::Morning, false).is_err());
        assert!(leave_days(&calendar, d("2026-10-10"), d("2026-10-10"), Half::Afternoon, false)?.is_zero());
        assert_eq!(Half::parse("none"), Some(Half::Whole));
        assert_eq!(Half::parse("evening"), None);
        Ok(())
    }

    #[test]
    fn the_sandwich_rule_counts_days_off_between_two_leaves() {
        let calendar = Calendar::default();
        // Friday 9 Oct and Monday 12 Oct: Saturday and Sunday lie between.
        assert_eq!(sandwiched_days(&calendar, d("2026-10-09"), d("2026-10-12")), Some(2));
        // Thursday and Monday: Friday is a working day in between.
        assert_eq!(sandwiched_days(&calendar, d("2026-10-08"), d("2026-10-12")), None);
        // Back to back: no gap.
        assert_eq!(sandwiched_days(&calendar, d("2026-10-09"), d("2026-10-10")), None);
        // A holiday on the Monday bridges further.
        let mut with_holiday = Calendar::default();
        with_holiday.holidays.push(d("2026-10-12"));
        assert_eq!(sandwiched_days(&with_holiday, d("2026-10-09"), d("2026-10-13")), Some(3));
    }

    #[test]
    fn a_new_hire_gets_the_share_of_the_year_left() -> Result<()> {
        let annual = n("21.00");
        assert_eq!(share_of_year(annual, Some(d("2025-06-01")), 2026)?.to_string(), "21.00");
        assert_eq!(share_of_year(annual, None, 2026)?.to_string(), "21.00");
        assert!(share_of_year(annual, Some(d("2027-01-01")), 2026)?.is_zero());
        assert_eq!(share_of_year(annual, Some(d("2026-07-01")), 2026)?.to_string(), "10.50");
        assert_eq!(share_of_year(annual, Some(d("2026-01-01")), 2026)?.to_string(), "21.00");
        Ok(())
    }

    #[test]
    fn earned_leave_arrives_in_periods_and_rounds_to_the_step() -> Result<()> {
        assert_eq!(earned_amount(n("21"), Frequency::Monthly, None)?.to_string(), "1.75");
        assert_eq!(earned_amount(n("21"), Frequency::Monthly, Some("0.5"))?.to_string(), "2.00");
        assert_eq!(earned_amount(n("20"), Frequency::Monthly, Some("0.5"))?.to_string(), "1.50");
        assert_eq!(earned_amount(n("21"), Frequency::Monthly, Some("1"))?.to_string(), "2.00");
        assert_eq!(earned_amount(n("24"), Frequency::Quarterly, Some("0.25"))?.to_string(), "6.00");
        assert_eq!(earned_amount(n("21"), Frequency::Yearly, None)?.to_string(), "21.00");
        let due = earned_due(Frequency::Monthly, AllocateOn::First, 2026, d("2026-03-15"));
        assert_eq!(due.iter().map(|(i, _)| *i).collect::<Vec<_>>(), [1, 2, 3]);
        let last = earned_due(Frequency::Monthly, AllocateOn::Last, 2026, d("2026-03-15"));
        assert_eq!(last.iter().map(|(_, day)| *day).collect::<Vec<_>>(), [d("2026-01-31"), d("2026-02-28")]);
        let quarterly = earned_due(Frequency::Quarterly, AllocateOn::Last, 2026, d("2026-12-31"));
        assert_eq!(quarterly.last().map(|(_, day)| *day), Some(d("2026-12-31")));
        Ok(())
    }

    #[test]
    fn eligibility_checks_gender_and_service() {
        assert!(eligible("any", None, 0, None, d("2026-01-01")).is_ok());
        assert!(eligible("female", Some("female"), 0, None, d("2026-01-01")).is_ok());
        assert!(eligible("female", Some("male"), 0, None, d("2026-01-01")).is_err());
        assert!(eligible("female", None, 0, None, d("2026-01-01")).is_err());
        assert!(eligible("any", None, 180, Some(d("2026-01-15")), d("2026-06-14")).is_err());
        assert!(eligible("any", None, 180, Some(d("2026-01-15")), d("2026-07-15")).is_ok());
    }

    #[test]
    fn a_balance_is_what_is_granted_less_what_is_spent_and_held() -> Result<()> {
        let entries = vec![
            entry("a", Kind::Allocation, "21", "2026-01-01", Some("2026-12-31")),
            entry("u", Kind::Usage, "-5", "2026-02-02", None),
            entry("r", Kind::Reservation, "-2.5", "2026-05-04", None),
        ];
        let b = balance(&entries, d("2026-05-01"), d("2026-05-01"))?;
        assert_eq!(b.allocated.to_string(), "21.00");
        assert_eq!(b.spent.to_string(), "5.00");
        assert_eq!(b.pending.to_string(), "2.50");
        assert_eq!(b.available.to_string(), "13.50");
        Ok(())
    }

    #[test]
    fn the_soonest_to_lapse_grant_is_spent_first() -> Result<()> {
        // Carried-over days lapse in March; leave taken in February uses them before the new grant.
        let entries = vec![
            entry("new", Kind::Allocation, "21", "2026-01-01", Some("2026-12-31")),
            entry("cf", Kind::CarryForward, "5", "2026-01-01", Some("2026-03-31")),
            entry("u", Kind::Usage, "-4", "2026-02-02", None),
        ];
        let february = balance(&entries, d("2026-02-10"), d("2026-02-10"))?;
        assert_eq!(february.available.to_string(), "22.00");
        // After March the carried day that was left (1) has lapsed; the new grant is untouched by the February leave.
        let april = balance(&entries, d("2026-04-10"), d("2026-04-10"))?;
        assert_eq!(april.allocated.to_string(), "21.00");
        assert_eq!(april.available.to_string(), "21.00");
        // On 20 March: 1 carried day left and it lapses within the horizon.
        let march = balance(&entries, d("2026-03-20"), d("2026-03-31"))?;
        assert_eq!(march.expiring_soon.to_string(), "1.00");
        Ok(())
    }

    #[test]
    fn a_cancelled_entry_and_its_reversal_vanish() -> Result<()> {
        let entries = vec![
            entry("a", Kind::Allocation, "10", "2026-01-01", Some("2026-12-31")),
            entry("r", Kind::Reservation, "-3", "2026-03-02", None),
            reversal("rev", "r", "3", "2026-03-03"),
            entry("u", Kind::Usage, "-3", "2026-03-02", None),
        ];
        let b = balance(&entries, d("2026-03-10"), d("2026-03-10"))?;
        assert_eq!(b.pending.to_string(), "0.00", "the reservation was reversed");
        assert_eq!(b.spent.to_string(), "3.00");
        assert_eq!(b.available.to_string(), "7.00");
        Ok(())
    }

    #[test]
    fn leave_beyond_the_grants_is_an_overdraft_and_future_grants_are_not_yet_usable() -> Result<()> {
        let entries = vec![
            entry("a", Kind::Allocation, "2", "2026-01-01", Some("2026-12-31")),
            entry("later", Kind::Allocation, "10", "2026-07-01", Some("2026-12-31")),
            entry("u", Kind::Usage, "-3", "2026-02-02", None),
        ];
        let b = balance(&entries, d("2026-02-10"), d("2026-02-10"))?;
        assert_eq!(b.allocated.to_string(), "2.00", "the July grant has not started");
        assert_eq!(b.available.to_string(), "-1.00");
        let later = balance(&entries, d("2026-08-01"), d("2026-08-01"))?;
        assert_eq!(later.allocated.to_string(), "12.00");
        Ok(())
    }

    #[test]
    fn expiry_entries_are_a_record_not_a_second_deduction() -> Result<()> {
        let entries = vec![
            entry("a", Kind::Allocation, "10", "2025-01-01", Some("2025-12-31")),
            entry("x", Kind::Expiry, "-10", "2026-01-01", None),
            entry("b", Kind::Allocation, "21", "2026-01-01", Some("2026-12-31")),
        ];
        let b = balance(&entries, d("2026-02-01"), d("2026-02-01"))?;
        assert_eq!(b.available.to_string(), "21.00");
        Ok(())
    }

    #[test]
    fn unused_days_of_a_lapsed_grant_are_reported_once_spent_ones_are_not() -> Result<()> {
        let entries = vec![
            entry("g1", Kind::Allocation, "10", "2025-01-01", Some("2025-12-31")),
            entry("u", Kind::Usage, "-7", "2025-06-02", None),
            entry("g2", Kind::Allocation, "5", "2025-01-01", Some("2025-12-31")),
            entry("live", Kind::Allocation, "21", "2026-01-01", Some("2026-12-31")),
        ];
        // The soonest-to-lapse tie goes to the earlier start: g1 pays for the 7 days, g2 (5) and g1's 3 days remain.
        let lost = lapsed(&entries, d("2026-01-01"));
        let total = lost.iter().fold(Decimal::zero(SCALE), |sum, (_, days)| sum + *days);
        assert_eq!(total.to_string(), "8.00");
        assert!(lapsed(&entries, d("2025-12-31")).is_empty(), "not lapsed yet on its last day");
        Ok(())
    }

    #[test]
    fn overlaps_share_a_day() {
        assert!(overlap((d("2026-01-01"), d("2026-01-05")), (d("2026-01-05"), d("2026-01-09"))));
        assert!(!overlap((d("2026-01-01"), d("2026-01-04")), (d("2026-01-05"), d("2026-01-09"))));
        assert_eq!(year_bounds(2026), Some((d("2026-01-01"), d("2026-12-31"))));
    }
}
