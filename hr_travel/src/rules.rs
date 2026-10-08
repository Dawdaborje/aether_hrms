//! The arithmetic and checks of a trip, with nothing read from a database.
//!
//! What the Frappe `travel_request` taught and what is done differently: Frappe keeps a free-form itinerary and
//! a costing table with nothing checking either. Here the trip's dates frame the legs, a per-diem policy fixes
//! the daily allowance (less a share for days when meals are provided), a lodging cap is checked against the
//! lodging costed, a trip above the policy's limit needs a second approver, and an advance can only be a share
//! of the estimate. The policy's figures are copied onto the trip, so changing a policy never rewrites a trip.

use aether_sdk::dates::{Duration, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::{Error, Result};

/// Nights away: the days between leaving and coming back.
pub fn nights(start: NaiveDate, end: NaiveDate) -> i64 {
    (end - start).num_days()
}

/// Calendar days of the trip, both ends counted.
pub fn days(start: NaiveDate, end: NaiveDate) -> i64 {
    nights(start, end) + 1
}

/// The allowance: the daily rate for every day, less `deduction_percent` of the rate for each day on which meals
/// were provided (never more days than the trip has).
pub fn per_diem(rate: Decimal, days: i64, meal_days: i64, deduction_percent: i64) -> Result<Decimal> {
    if meal_days < 0 || meal_days > days {
        return Err(Error::msg("days with meals provided must be between 0 and the days of the trip"));
    }
    if !(0..=100).contains(&deduction_percent) {
        return Err(Error::msg("the meal deduction is a percentage between 0 and 100"));
    }
    let full = rate.times_ratio(days, 1)?;
    let cut = rate.times_ratio(meal_days * deduction_percent, 100)?;
    Ok(full - cut)
}

/// What lodging costs above the cap for the nights away; `None` when it fits (or there is no cap).
pub fn lodging_excess(cap_per_night: Option<Decimal>, nights: i64, lodging: Decimal) -> Result<Option<Decimal>> {
    let Some(cap) = cap_per_night else { return Ok(None) };
    let allowed = cap.times_ratio(nights.max(1), 1)?;
    Ok((lodging > allowed).then(|| lodging - allowed))
}

/// Whether a second approver is needed: the estimate is above the policy's limit.
pub fn needs_second(total: Decimal, limit: Option<Decimal>) -> bool {
    limit.is_some_and(|limit| total > limit)
}

/// The most an advance can be: a share of the estimate.
pub fn advance_cap(total: Decimal, percent: i64) -> Result<Decimal> {
    if !(0..=100).contains(&percent) {
        return Err(Error::msg("an advance share is a percentage between 0 and 100"));
    }
    total.times_ratio(percent, 100)
}

/// Two trips overlap when they share a day.
pub fn overlaps(a: (NaiveDate, NaiveDate), b: (NaiveDate, NaiveDate)) -> bool {
    a.0 <= b.1 && b.0 <= a.1
}

/// One leg of a journey, as dates.
#[derive(Debug, Clone, Copy)]
pub struct Leg {
    pub depart: NaiveDate,
    pub arrive: NaiveDate,
}

/// Legs must lie inside the trip, arrive no earlier than they leave, and follow one another.
pub fn legs_fit(legs: &[Leg], start: NaiveDate, end: NaiveDate) -> Result<()> {
    let mut previous: Option<NaiveDate> = None;
    for (i, leg) in legs.iter().enumerate() {
        let n = i + 1;
        if leg.arrive < leg.depart {
            return Err(Error::msg(format!("leg {n} arrives before it leaves")));
        }
        if leg.depart < start || leg.arrive > end {
            return Err(Error::msg(format!("leg {n} is outside the dates of the trip")));
        }
        if previous.is_some_and(|p| leg.depart < p) {
            return Err(Error::msg(format!("leg {n} leaves before the one before it arrives")));
        }
        previous = Some(leg.arrive);
    }
    Ok(())
}

/// When an approved trip that ended has been left unsettled: days after the end.
pub fn settle_by(end: NaiveDate, grace_days: i64) -> NaiveDate {
    end + Duration::days(grace_days)
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
    fn days_and_nights() {
        assert_eq!(days(d("2026-03-02"), d("2026-03-05")), 4);
        assert_eq!(nights(d("2026-03-02"), d("2026-03-05")), 3);
        assert_eq!(days(d("2026-03-02"), d("2026-03-02")), 1);
    }

    #[test]
    fn per_diem_is_less_a_share_for_meal_days() -> Result<()> {
        assert_eq!(per_diem(n("50.00"), 4, 0, 25)?, n("200.00"));
        assert_eq!(per_diem(n("50.00"), 4, 2, 25)?, n("175.00"));
        assert_eq!(per_diem(n("50.00"), 4, 4, 100)?, n("0.00"));
        assert!(per_diem(n("50.00"), 4, 5, 25).is_err());
        assert!(per_diem(n("50.00"), 4, 1, 101).is_err());
        Ok(())
    }

    #[test]
    fn lodging_is_checked_against_the_cap_for_the_nights() -> Result<()> {
        assert_eq!(lodging_excess(Some(n("100.00")), 3, n("300.00"))?, None);
        assert_eq!(lodging_excess(Some(n("100.00")), 3, n("340.50"))?, Some(n("40.50")));
        assert_eq!(lodging_excess(None, 3, n("9999.00"))?, None);
        Ok(())
    }

    #[test]
    fn a_big_trip_needs_a_second_approver_and_an_advance_is_a_share() -> Result<()> {
        assert!(needs_second(n("1000.01"), Some(n("1000.00"))));
        assert!(!needs_second(n("1000.00"), Some(n("1000.00"))));
        assert!(!needs_second(n("9999.00"), None));
        assert_eq!(advance_cap(n("1000.00"), 60)?, n("600.00"));
        assert!(advance_cap(n("1.00"), 101).is_err());
        Ok(())
    }

    #[test]
    fn trips_overlap_when_they_share_a_day() {
        assert!(overlaps((d("2026-03-01"), d("2026-03-05")), (d("2026-03-05"), d("2026-03-07"))));
        assert!(!overlaps((d("2026-03-01"), d("2026-03-04")), (d("2026-03-05"), d("2026-03-07"))));
    }

    #[test]
    fn legs_stay_inside_the_trip_in_order() {
        let (s, e) = (d("2026-03-02"), d("2026-03-06"));
        let leg = |a: &str, b: &str| Leg { depart: d(a), arrive: d(b) };
        assert!(legs_fit(&[leg("2026-03-02", "2026-03-02"), leg("2026-03-06", "2026-03-06")], s, e).is_ok());
        assert!(legs_fit(&[leg("2026-03-01", "2026-03-02")], s, e).is_err());
        assert!(legs_fit(&[leg("2026-03-04", "2026-03-03")], s, e).is_err());
        assert!(legs_fit(&[leg("2026-03-02", "2026-03-04"), leg("2026-03-03", "2026-03-05")], s, e).is_err());
    }
}
