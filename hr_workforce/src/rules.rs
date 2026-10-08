//! The arithmetic of seats and plans, with no records read.

use aether_sdk::dates::{overlaps, NaiveDate};
use aether_sdk::{Error, Result};

/// Seats a new requisition may still ask for on a position: what the position has free, less the seats that
/// approved requisitions have already claimed and not yet filled.
pub fn free_seats(headcount: u64, held: u64, committed_open: u64) -> u64 {
    headcount.saturating_sub(held).saturating_sub(committed_open)
}

/// What is left of a plan line after the seats requisitions have used.
pub fn plan_room(planned: u64, used: u64) -> u64 {
    planned.saturating_sub(used)
}

/// Whether two plan periods share a day.
pub fn periods_overlap(a: (NaiveDate, NaiveDate), b: (NaiveDate, NaiveDate)) -> bool {
    overlaps(a.0, Some(a.1), b.0, Some(b.1))
}

/// Whole days from the day a requisition was posted to the day it was filled.
pub fn days_to_fill(posted: NaiveDate, filled: NaiveDate) -> Result<i64> {
    if filled < posted {
        return Err(Error::msg("filled before it was posted"));
    }
    Ok((filled - posted).num_days())
}

/// Whether a requisition's seats are all filled.
pub fn is_filled(seats: u64, filled: u64) -> bool {
    seats > 0 && filled >= seats
}

#[cfg(test)]
mod tests {
    use aether_sdk::dates::parse_date;

    use super::*;

    fn d(text: &str) -> NaiveDate {
        parse_date(text).unwrap_or_default()
    }

    #[test]
    fn free_seats_never_go_below_zero_and_count_claims() {
        assert_eq!(free_seats(5, 3, 0), 2);
        assert_eq!(free_seats(5, 3, 1), 1);
        assert_eq!(free_seats(5, 3, 4), 0);
        assert_eq!(free_seats(2, 5, 0), 0);
        assert_eq!(plan_room(4, 6), 0);
        assert_eq!(plan_room(4, 1), 3);
    }

    #[test]
    fn periods_share_a_day_or_not() {
        assert!(periods_overlap((d("2026-01-01"), d("2026-06-30")), (d("2026-06-30"), d("2026-12-31"))));
        assert!(!periods_overlap((d("2026-01-01"), d("2026-06-29")), (d("2026-06-30"), d("2026-12-31"))));
    }

    #[test]
    fn time_to_fill_counts_days_and_fills_are_whole() {
        assert_eq!(days_to_fill(d("2026-01-01"), d("2026-01-31")).unwrap_or(-1), 30);
        assert!(days_to_fill(d("2026-02-01"), d("2026-01-31")).is_err());
        assert!(is_filled(2, 2));
        assert!(!is_filled(2, 1));
        assert!(!is_filled(0, 0));
    }
}
