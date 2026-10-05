//! The rules of hiring, with nothing read from a database.
//!
//! What the Odoo and Frappe sources taught (see `docs/design.md`): vacancies are *seats* checked against
//! a position's headcount, at the moment an opening opens and again when an offer goes out, not a counter
//! that floors at zero or a count of offers; an interview's outcome is a function of the submitted
//! feedback (Frappe stores the result and never uses it); a stage can only be entered in order, and the
//! hired stage only by actually hiring.

use std::collections::BTreeMap;

use aether_sdk::decimal::Decimal;
use aether_sdk::{Error, Result};

/// The scale of a rating: 1 (poor) to 5 (excellent).
pub const MIN_RATING: i64 = 1;
pub const MAX_RATING: i64 = 5;

/// One thing an interview rates, and how much it counts (weights add up to 100).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Criterion {
    pub name: String,
    pub weight: i64,
}

/// Criteria must have distinct names and weights that add up to exactly 100.
pub fn check_criteria(criteria: &[Criterion]) -> Result<()> {
    if criteria.is_empty() {
        return Err(Error::msg("a round rates at least one thing"));
    }
    let mut names: Vec<&str> = criteria.iter().map(|c| c.name.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    if names.len() != criteria.len() || criteria.iter().any(|c| c.name.trim().is_empty()) {
        return Err(Error::msg("each criterion has its own name"));
    }
    if criteria.iter().any(|c| c.weight <= 0) {
        return Err(Error::msg("each criterion counts for something"));
    }
    let sum: i64 = criteria.iter().map(|c| c.weight).sum();
    if sum != 100 {
        return Err(Error::msg(format!("the weights add up to {sum}, not 100")));
    }
    Ok(())
}

/// One interviewer's weighted score, 1.00 to 5.00. Every criterion must be rated, once, 1 to 5.
pub fn weighted_score(criteria: &[Criterion], scores: &BTreeMap<String, i64>) -> Result<Decimal> {
    if let Some(stray) = scores.keys().find(|k| !criteria.iter().any(|c| &c.name == *k)) {
        return Err(Error::msg(format!("`{stray}` is not something this round rates")));
    }
    let mut weighted = 0i64;
    for criterion in criteria {
        let score = scores.get(&criterion.name).ok_or_else(|| Error::msg(format!("rate `{}`", criterion.name)))?;
        if !(MIN_RATING..=MAX_RATING).contains(score) {
            return Err(Error::msg(format!("`{}` is rated {MIN_RATING} to {MAX_RATING}", criterion.name)));
        }
        weighted += score * criterion.weight;
    }
    // weighted is hundredths of a point (a score times a weight out of 100).
    Decimal::whole(weighted, 2)?.times_ratio(1, 100).map(|d| d.round_to(2))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recommendation {
    StrongYes,
    Yes,
    No,
    StrongNo,
}

impl Recommendation {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "strong_yes" => Self::StrongYes,
            "yes" => Self::Yes,
            "no" => Self::No,
            "strong_no" => Self::StrongNo,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Not enough feedback yet.
    Pending,
    Advance,
    Reject,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Advance => "advance",
            Self::Reject => "reject",
        }
    }
}

/// The result of an interview from the feedback in. It waits until `min_panelists` (or the whole panel,
/// if smaller) have answered. It advances when the mean of the weighted scores reaches `min_average`
/// and nobody said `strong_no`; a `strong_no` rejects.
pub fn interview_outcome(feedback: &[(Decimal, Recommendation)], min_average: Decimal, min_panelists: usize, panel_size: usize) -> Result<Outcome> {
    let needed = min_panelists.max(1).min(panel_size.max(1));
    if feedback.len() < needed {
        return Ok(Outcome::Pending);
    }
    if feedback.iter().any(|(_, r)| *r == Recommendation::StrongNo) {
        return Ok(Outcome::Reject);
    }
    let sum = feedback.iter().fold(Decimal::zero(2), |sum, (score, _)| sum + *score);
    let mean = sum.times_ratio(1, feedback.len() as i64)?;
    Ok(if mean >= min_average { Outcome::Advance } else { Outcome::Reject })
}

/// Seats an opening may ask for: the position's headcount, less those who hold it, less the seats of
/// other openings for the same position that are open.
pub fn seats_available(headcount: i64, held: i64, other_open_seats: i64) -> i64 {
    (headcount - held - other_open_seats).max(0)
}

/// Whether an opening for `seats` may open (or an offer go out) given what is free.
pub fn check_seats(seats: i64, available: i64) -> Result<()> {
    if seats < 1 {
        return Err(Error::msg("an opening is for at least one seat"));
    }
    if seats > available {
        return Err(Error::msg(format!("the position has {available} free seat(s) not already promised to other openings, and {seats} are asked for")));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageKind {
    Sourcing,
    Screening,
    Interview,
    Offer,
    Hired,
    Rejected,
}

impl StageKind {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "sourcing" => Self::Sourcing,
            "screening" => Self::Screening,
            "interview" => Self::Interview,
            "offer" => Self::Offer,
            "hired" => Self::Hired,
            "rejected" => Self::Rejected,
            _ => return None,
        })
    }
}

/// Whether an application may move from one stage to another. Forward only, one step or more, unless
/// the mover is a recruitment manager. Hired is reached by hiring, and rejection by rejecting, never by
/// dragging.
pub fn check_move(from_sequence: i64, to_sequence: i64, to: StageKind, manager: bool) -> Result<()> {
    if matches!(to, StageKind::Hired) {
        return Err(Error::msg("an application becomes hired by hiring the person, not by moving it"));
    }
    if matches!(to, StageKind::Rejected) {
        return Err(Error::msg("reject the application with a reason instead of moving it"));
    }
    if to_sequence == from_sequence {
        return Err(Error::msg("it is already at that stage"));
    }
    if to_sequence < from_sequence && !manager {
        return Err(Error::msg("an application only moves forward; a recruitment manager can send it back"));
    }
    Ok(())
}

/// An offered salary inside the opening's band (a missing end of the band is open).
pub fn in_band(salary: Decimal, min: Option<Decimal>, max: Option<Decimal>) -> bool {
    min.is_none_or(|min| salary >= min) && max.is_none_or(|max| salary <= max)
}

/// An email as compared for sameness: no spaces, lower case.
pub fn normalise_email(email: &str) -> String {
    email.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(text: &str) -> Decimal {
        Decimal::parse(text).unwrap_or(Decimal::zero(0))
    }

    fn crit(items: &[(&str, i64)]) -> Vec<Criterion> {
        items.iter().map(|(name, weight)| Criterion { name: (*name).into(), weight: *weight }).collect()
    }

    fn scores(items: &[(&str, i64)]) -> BTreeMap<String, i64> {
        items.iter().map(|(k, v)| ((*k).to_string(), *v)).collect()
    }

    #[test]
    fn criteria_weights_must_add_up_to_exactly_100() {
        assert!(check_criteria(&crit(&[("skills", 60), ("fit", 40)])).is_ok());
        assert!(check_criteria(&crit(&[("skills", 60), ("fit", 30)])).is_err());
        assert!(check_criteria(&crit(&[("skills", 100), ("skills", 0)])).is_err());
        assert!(check_criteria(&crit(&[("a", 50), ("a", 50)])).is_err(), "names are distinct");
        assert!(check_criteria(&[]).is_err());
        assert!(check_criteria(&crit(&[("a", 110), ("b", -10)])).is_err());
    }

    #[test]
    fn a_score_is_the_weighted_mean_and_every_criterion_must_be_rated() -> Result<()> {
        let criteria = crit(&[("skills", 60), ("fit", 40)]);
        assert_eq!(weighted_score(&criteria, &scores(&[("skills", 5), ("fit", 3)]))?.to_string(), "4.20");
        assert_eq!(weighted_score(&criteria, &scores(&[("skills", 1), ("fit", 1)]))?.to_string(), "1.00");
        assert!(weighted_score(&criteria, &scores(&[("skills", 5)])).is_err(), "fit was not rated");
        assert!(weighted_score(&criteria, &scores(&[("skills", 6), ("fit", 3)])).is_err());
        assert!(weighted_score(&criteria, &scores(&[("skills", 0), ("fit", 3)])).is_err());
        assert!(weighted_score(&criteria, &scores(&[("skills", 3), ("fit", 3), ("luck", 5)])).is_err());
        Ok(())
    }

    #[test]
    fn an_interview_waits_for_enough_feedback_then_decides() -> Result<()> {
        let yes = (n("4.20"), Recommendation::Yes);
        let low = (n("2.40"), Recommendation::No);
        assert_eq!(interview_outcome(&[], n("3.5"), 2, 3)?, Outcome::Pending);
        assert_eq!(interview_outcome(&[yes], n("3.5"), 2, 3)?, Outcome::Pending, "one of two needed");
        assert_eq!(interview_outcome(&[yes, (n("3.80"), Recommendation::StrongYes)], n("3.5"), 2, 3)?, Outcome::Advance);
        assert_eq!(interview_outcome(&[yes, low], n("3.5"), 2, 3)?, Outcome::Reject, "mean 3.30 is below the bar");
        // A strong no rejects whatever the mean.
        assert_eq!(interview_outcome(&[(n("5.00"), Recommendation::StrongYes), (n("4.90"), Recommendation::StrongNo)], n("3.5"), 2, 3)?, Outcome::Reject);
        // A one-person panel needs one answer, whatever min_panelists says.
        assert_eq!(interview_outcome(&[yes], n("3.5"), 3, 1)?, Outcome::Advance);
        Ok(())
    }

    #[test]
    fn seats_come_from_the_headcount_not_a_counter() {
        assert_eq!(seats_available(3, 1, 1), 1);
        assert_eq!(seats_available(1, 1, 0), 0);
        assert_eq!(seats_available(1, 2, 0), 0, "never negative");
        assert!(check_seats(1, 1).is_ok());
        assert!(check_seats(2, 1).is_err());
        assert!(check_seats(0, 5).is_err());
    }

    #[test]
    fn stages_are_entered_in_order() {
        assert!(check_move(1, 2, StageKind::Screening, false).is_ok());
        assert!(check_move(1, 4, StageKind::Offer, false).is_ok(), "skipping ahead is allowed");
        assert!(check_move(3, 2, StageKind::Screening, false).is_err());
        assert!(check_move(3, 2, StageKind::Screening, true).is_ok(), "a manager can send it back");
        assert!(check_move(1, 5, StageKind::Hired, true).is_err(), "hired is reached by hiring");
        assert!(check_move(1, 6, StageKind::Rejected, true).is_err());
        assert!(check_move(2, 2, StageKind::Screening, true).is_err());
    }

    #[test]
    fn salary_bands_may_be_open_ended() {
        assert!(in_band(n("5000"), Some(n("4000")), Some(n("6000"))));
        assert!(!in_band(n("7000"), Some(n("4000")), Some(n("6000"))));
        assert!(in_band(n("7000"), Some(n("4000")), None));
        assert!(in_band(n("1"), None, None));
        assert_eq!(normalise_email("  Ann@X.Test "), "ann@x.test");
    }
}
