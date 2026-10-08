//! The arithmetic of training, with no records read.

use aether_sdk::dates::{add_months, overlaps, NaiveDate};
use aether_sdk::decimal::Decimal;

/// Where a new enrolment lands: a seat if one is free, otherwise the waiting list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seat {
    Enrolled,
    Waitlisted,
}

pub fn seat(capacity: u64, taken: u64) -> Seat {
    if taken < capacity {
        Seat::Enrolled
    } else {
        Seat::Waitlisted
    }
}

/// What is left of a budget; negative when overspent.
pub fn budget_left(amount: Decimal, spent: Decimal) -> Decimal {
    amount - spent
}

/// Whether a seat of this price fits.
pub fn fits(amount: Decimal, spent: Decimal, price: Decimal) -> bool {
    budget_left(amount, spent) >= price
}

/// The day a certificate lapses: `months` after it is issued, on the day before the same date (so one issued on
/// 15 January for twelve months lasts through 14 January next year). `None`: it never lapses.
pub fn expiry(issued: NaiveDate, months: Option<u32>) -> Option<NaiveDate> {
    months.map(|m| add_months(issued, m) - aether_sdk::dates::Duration::days(1))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    Valid,
    ExpiringSoon,
    Expired,
}

/// A certificate's standing on a day.
pub fn standing(today: NaiveDate, expires: Option<NaiveDate>, warn_days: i64) -> Standing {
    match expires {
        None => Standing::Valid,
        Some(end) if end < today => Standing::Expired,
        Some(end) if (end - today).num_days() <= warn_days => Standing::ExpiringSoon,
        Some(_) => Standing::Valid,
    }
}

/// A pass: attended, and (if the course sets a pass score) scored at or above it.
pub fn passes(attended: bool, score: Option<Decimal>, pass_score: Option<Decimal>) -> bool {
    if !attended {
        return false;
    }
    match pass_score {
        None => true,
        Some(needed) => score.is_some_and(|s| s >= needed),
    }
}

pub fn sessions_overlap(a: (NaiveDate, NaiveDate), b: (NaiveDate, NaiveDate)) -> bool {
    overlaps(a.0, Some(a.1), b.0, Some(b.1))
}

#[cfg(test)]
mod tests {
    use aether_sdk::dates::parse_date;

    use super::*;

    fn d(text: &str) -> NaiveDate {
        parse_date(text).unwrap_or_default()
    }

    fn n(text: &str) -> Decimal {
        Decimal::parse(text).and_then(|v| v.with_scale(2)).unwrap_or_else(|_| Decimal::zero(2))
    }

    #[test]
    fn seats_run_out_and_people_wait() {
        assert_eq!(seat(3, 2), Seat::Enrolled);
        assert_eq!(seat(3, 3), Seat::Waitlisted);
        assert_eq!(seat(0, 0), Seat::Waitlisted);
    }

    #[test]
    fn a_budget_refuses_the_seat_that_overspends_it() {
        assert!(fits(n("1000"), n("800"), n("200")));
        assert!(!fits(n("1000"), n("800"), n("200.01")));
        assert_eq!(budget_left(n("100"), n("150")), n("-50"));
    }

    #[test]
    fn a_certificate_lasts_through_the_day_before_its_anniversary() {
        assert_eq!(expiry(d("2026-01-15"), Some(12)), Some(d("2027-01-14")));
        assert_eq!(expiry(d("2026-01-31"), Some(1)), Some(d("2026-02-27")));
        assert_eq!(expiry(d("2026-01-15"), None), None);
    }

    #[test]
    fn standing_warns_before_it_lapses() {
        let end = Some(d("2026-06-30"));
        assert_eq!(standing(d("2026-05-01"), end, 30), Standing::Valid);
        assert_eq!(standing(d("2026-06-01"), end, 30), Standing::ExpiringSoon);
        assert_eq!(standing(d("2026-06-30"), end, 30), Standing::ExpiringSoon);
        assert_eq!(standing(d("2026-07-01"), end, 30), Standing::Expired);
        assert_eq!(standing(d("2030-01-01"), None, 30), Standing::Valid);
    }

    #[test]
    fn a_pass_needs_attendance_and_the_score_when_one_is_set() {
        assert!(passes(true, None, None));
        assert!(!passes(false, Some(n("100")), None));
        assert!(passes(true, Some(n("70")), Some(n("70"))));
        assert!(!passes(true, Some(n("69.99")), Some(n("70"))));
        assert!(!passes(true, None, Some(n("70"))));
    }
}
