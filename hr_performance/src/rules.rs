//! The rules of performance review, with nothing read from a database.
//!
//! What the Frappe and Odoo sources taught (see `docs/design.md`): every score is normalised to one
//! scale and computed with exact numbers; a missing component is a decision the cycle states
//! (Frappe counts it as zero in the mean without saying so); a goal tree rolls up with weights
//! (Frappe's parent is a plain mean of children); the phases of a cycle each allow their own writes.

use std::collections::BTreeMap;

use aether_sdk::dates::{Duration, NaiveDate};
use aether_sdk::decimal::Decimal;
use aether_sdk::{Error, Result};

pub const MIN_SCALE: i64 = 3;
pub const MAX_SCALE: i64 = 10;

/// Names with weights that must add up to 100 (the criteria of a review, the key result areas).
pub fn check_weights(items: &[(String, i64)], what: &str) -> Result<()> {
    if items.is_empty() {
        return Err(Error::msg(format!("{what}: list at least one")));
    }
    let mut names: Vec<&str> = items.iter().map(|(n, _)| n.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    if names.len() != items.len() || items.iter().any(|(n, _)| n.trim().is_empty()) {
        return Err(Error::msg(format!("{what}: each has its own name")));
    }
    if items.iter().any(|(_, w)| *w <= 0) {
        return Err(Error::msg(format!("{what}: each counts for something")));
    }
    let sum: i64 = items.iter().map(|(_, w)| *w).sum();
    if sum != 100 {
        return Err(Error::msg(format!("{what}: the weights add up to {sum}, not 100")));
    }
    Ok(())
}

pub fn check_scale(max: i64) -> Result<()> {
    if (MIN_SCALE..=MAX_SCALE).contains(&max) {
        Ok(())
    } else {
        Err(Error::msg(format!("the rating scale goes from 1 up to between {MIN_SCALE} and {MAX_SCALE}")))
    }
}

/// A review's score from its ratings, 0.00 to 100.00. Every criterion is rated, once, from 1 to `scale_max`.
pub fn rating_score(criteria: &[(String, i64)], ratings: &BTreeMap<String, i64>, scale_max: i64) -> Result<Decimal> {
    check_scale(scale_max)?;
    if let Some(stray) = ratings.keys().find(|k| !criteria.iter().any(|(n, _)| n == *k)) {
        return Err(Error::msg(format!("`{stray}` is not something this review rates")));
    }
    let mut sum = 0i64;
    for (name, weight) in criteria {
        let rating = ratings.get(name).ok_or_else(|| Error::msg(format!("rate `{name}`")))?;
        if !(1..=scale_max).contains(rating) {
            return Err(Error::msg(format!("`{name}` is rated 1 to {scale_max}")));
        }
        sum += rating * weight;
    }
    Decimal::whole(sum, 2)?.times_ratio(1, scale_max)
}

/// A score on the cycle's own scale: 72.00 on a scale of 5 is 3.60.
pub fn on_scale(percent: Decimal, scale_max: i64) -> Result<Decimal> {
    percent.times_ratio(scale_max, 100)
}

pub fn mean(scores: &[Decimal]) -> Result<Option<Decimal>> {
    if scores.is_empty() {
        return Ok(None);
    }
    let sum = scores.iter().fold(Decimal::zero(2), |sum, score| sum + *score);
    sum.times_ratio(1, scores.len() as i64).map(Some)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// Leave it out and weigh the rest among themselves.
    Exclude,
    /// Count it as nothing.
    Zero,
    /// Refuse to score until it is there.
    Block,
}

impl Missing {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "exclude" => Self::Exclude,
            "zero" => Self::Zero,
            "block" => Self::Block,
            _ => return None,
        })
    }
}

/// The cycle's weights for each component, adding up to 100.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Weights {
    pub goal: i64,
    pub own: i64,
    pub manager: i64,
    pub peer: i64,
}

impl Weights {
    pub fn check(&self) -> Result<()> {
        let all = [self.goal, self.own, self.manager, self.peer];
        if all.iter().any(|w| *w < 0) {
            return Err(Error::msg("a weight cannot be negative"));
        }
        let sum: i64 = all.iter().sum();
        if sum != 100 {
            return Err(Error::msg(format!("the weights of goals, self, manager and peers add up to {sum}, not 100")));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Components {
    pub goal: Option<Decimal>,
    pub own: Option<Decimal>,
    pub manager: Option<Decimal>,
    pub peer: Option<Decimal>,
}

/// The final score and the components that were missing (those with no weight are never missing).
pub fn final_score(components: &Components, weights: &Weights, policy: Missing) -> Result<(Decimal, Vec<&'static str>)> {
    weights.check()?;
    let parts = [
        ("goal", components.goal, weights.goal),
        ("self", components.own, weights.own),
        ("manager", components.manager, weights.manager),
        ("peer", components.peer, weights.peer),
    ];
    let missing: Vec<&'static str> = parts.iter().filter(|(_, score, w)| *w > 0 && score.is_none()).map(|(n, _, _)| *n).collect();
    if !missing.is_empty() && policy == Missing::Block {
        return Err(Error::msg(format!("these are not there yet: {}", missing.join(", "))));
    }
    let mut total = Decimal::zero(2);
    let mut counted = 0i64;
    for (_, score, weight) in &parts {
        if *weight == 0 {
            continue;
        }
        match (score, policy) {
            (Some(score), _) => {
                total = total + score.times_ratio(*weight, 1)?;
                counted += weight;
            }
            (None, Missing::Zero) => counted += weight,
            (None, _) => {}
        }
    }
    if counted == 0 {
        return Err(Error::msg("there is nothing to score yet"));
    }
    Ok((total.times_ratio(1, counted)?, missing))
}

/// A leaf goal's progress from what is done against its target, 0.00 to 100.00.
pub fn leaf_progress(current: Decimal, target: Decimal) -> Result<Decimal> {
    if target <= Decimal::zero(2) {
        return Err(Error::msg("a goal's target is above zero"));
    }
    let percent = current.div(target, 4)?.times_ratio(100, 1)?.round_to(2);
    Ok(percent.max(Decimal::zero(2)).min(Decimal::whole(100, 2)?))
}

/// A parent's progress: the mean of its children's, each counted by its weight.
pub fn rollup(children: &[(i64, Decimal)]) -> Result<Decimal> {
    let weight: i64 = children.iter().map(|(w, _)| (*w).max(1)).sum();
    if weight == 0 {
        return Ok(Decimal::zero(2));
    }
    let sum = children.iter().try_fold(Decimal::zero(2), |sum, (w, p)| p.times_ratio((*w).max(1), 1).map(|part| sum + part))?;
    sum.times_ratio(1, weight)
}

/// The goal component from each key result area's weight and the progress of its top goals (none: the
/// area has no goals). Areas with no goals follow the policy; it names the uncovered ones.
pub fn goal_component(areas: &[(String, i64, Option<Decimal>)], policy: Missing) -> Result<(Option<Decimal>, Vec<String>)> {
    let uncovered: Vec<String> = areas.iter().filter(|(_, _, p)| p.is_none()).map(|(n, _, _)| n.clone()).collect();
    if !uncovered.is_empty() && policy == Missing::Block {
        return Err(Error::msg(format!("no goals yet for: {}", uncovered.join(", "))));
    }
    let mut total = Decimal::zero(2);
    let mut counted = 0i64;
    for (_, weight, progress) in areas {
        match (progress, policy) {
            (Some(p), _) => {
                total = total + p.times_ratio(*weight, 1)?;
                counted += weight;
            }
            (None, Missing::Zero) => counted += weight,
            (None, _) => {}
        }
    }
    if counted == 0 {
        return Ok((None, uncovered));
    }
    Ok((Some(total.times_ratio(1, counted)?), uncovered))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    Draft,
    GoalSetting,
    SelfReview,
    ManagerReview,
    Calibration,
    Published,
    Closed,
}

impl Phase {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "draft" => Self::Draft,
            "goal_setting" => Self::GoalSetting,
            "self_review" => Self::SelfReview,
            "manager_review" => Self::ManagerReview,
            "calibration" => Self::Calibration,
            "published" => Self::Published,
            "closed" => Self::Closed,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::GoalSetting => "goal_setting",
            Self::SelfReview => "self_review",
            Self::ManagerReview => "manager_review",
            Self::Calibration => "calibration",
            Self::Published => "published",
            Self::Closed => "closed",
        }
    }

    pub fn next(self) -> Option<Self> {
        Some(match self {
            Self::Draft => Self::GoalSetting,
            Self::GoalSetting => Self::SelfReview,
            Self::SelfReview => Self::ManagerReview,
            Self::ManagerReview => Self::Calibration,
            Self::Calibration => Self::Published,
            Self::Published => Self::Closed,
            Self::Closed => return None,
        })
    }

    /// Goals are written until the managers have started to review; progress is reported until calibration.
    pub fn goals_editable(self) -> bool {
        matches!(self, Self::GoalSetting | Self::SelfReview)
    }

    pub fn progress_reportable(self) -> bool {
        matches!(self, Self::GoalSetting | Self::SelfReview | Self::ManagerReview)
    }
}

/// Whether a phase deadline is close enough to remind about (within `lead` days) and not long past.
pub fn reminder_due(deadline: Option<NaiveDate>, today: NaiveDate, lead: i64) -> bool {
    deadline.is_some_and(|d| today >= d - Duration::days(lead))
}

/// How far a person's level is below the target for a skill (0 when at or above it, or the whole
/// target when they have no level).
pub fn skill_gap(target: i64, current: Option<i64>) -> i64 {
    (target - current.unwrap_or(0)).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_sdk::dates::parse_date;

    fn d(text: &str) -> Result<Decimal> {
        Decimal::parse(text)
    }

    fn crit() -> Vec<(String, i64)> {
        vec![("quality".into(), 60), ("teamwork".into(), 40)]
    }

    fn ratings(q: i64, t: i64) -> BTreeMap<String, i64> {
        BTreeMap::from([("quality".to_string(), q), ("teamwork".to_string(), t)])
    }

    #[test]
    fn weights_must_total_100() -> Result<()> {
        check_weights(&crit(), "criteria")?;
        assert!(check_weights(&[("a".into(), 50)], "criteria").is_err());
        assert!(check_weights(&[("a".into(), 50), ("a".into(), 50)], "criteria").is_err());
        assert!(check_weights(&[], "criteria").is_err());
        Ok(())
    }

    #[test]
    fn a_score_is_a_percent_of_the_scale() -> Result<()> {
        assert_eq!(rating_score(&crit(), &ratings(4, 3), 5)?, d("72.00")?);
        assert_eq!(rating_score(&crit(), &ratings(5, 5), 5)?, d("100.00")?);
        assert_eq!(rating_score(&crit(), &ratings(1, 1), 5)?, d("20.00")?);
        assert_eq!(rating_score(&crit(), &ratings(4, 3), 10)?, d("36.00")?, "the same ratings are lower on a longer scale");
        assert!(rating_score(&crit(), &ratings(6, 3), 5).is_err());
        assert!(rating_score(&crit(), &ratings(0, 3), 5).is_err());
        let mut partial = ratings(4, 3);
        partial.remove("teamwork");
        assert!(rating_score(&crit(), &partial, 5).is_err());
        let mut stray = ratings(4, 3);
        stray.insert("luck".into(), 5);
        assert!(rating_score(&crit(), &stray, 5).is_err());
        assert!(rating_score(&crit(), &ratings(4, 3), 2).is_err());
        assert_eq!(on_scale(d("72.00")?, 5)?, d("3.60")?);
        Ok(())
    }

    #[test]
    fn a_mean_of_nothing_is_nothing() -> Result<()> {
        assert_eq!(mean(&[])?, None);
        assert_eq!(mean(&[d("70.00")?, d("80.50")?])?, Some(d("75.25")?));
        Ok(())
    }

    fn w(goal: i64, own: i64, manager: i64, peer: i64) -> Weights {
        Weights { goal, own, manager, peer }
    }

    #[test]
    fn the_final_score_weighs_the_components() -> Result<()> {
        let all = Components { goal: Some(d("80.00")?), own: Some(d("90.00")?), manager: Some(d("70.00")?), peer: Some(d("60.00")?) };
        let (score, missing) = final_score(&all, &w(40, 10, 40, 10), Missing::Exclude)?;
        assert_eq!((score, missing.len()), (d("75.00")?, 0));
        assert!(final_score(&all, &w(40, 10, 40, 20), Missing::Exclude).is_err());
        Ok(())
    }

    #[test]
    fn a_missing_component_follows_the_policy() -> Result<()> {
        let none_from_peers = Components { goal: Some(d("80.00")?), own: Some(d("90.00")?), manager: Some(d("70.00")?), peer: None };
        let weights = w(40, 10, 40, 10);
        let (excluded, missing) = final_score(&none_from_peers, &weights, Missing::Exclude)?;
        assert_eq!((excluded, missing), (d("76.67")?, vec!["peer"]));
        let (zeroed, _) = final_score(&none_from_peers, &weights, Missing::Zero)?;
        assert_eq!(zeroed, d("69.00")?);
        assert!(final_score(&none_from_peers, &weights, Missing::Block).is_err());
        // A component nobody weighs is never missing.
        let (score, missing) = final_score(&none_from_peers, &w(40, 10, 50, 0), Missing::Block)?;
        assert_eq!((score, missing.len()), (d("76.00")?, 0));
        assert!(final_score(&Components::default(), &weights, Missing::Exclude).is_err());
        Ok(())
    }

    #[test]
    fn a_leaf_goal_progress_is_capped() -> Result<()> {
        assert_eq!(leaf_progress(d("25")?, d("100")?)?, d("25.00")?);
        assert_eq!(leaf_progress(d("150")?, d("100")?)?, d("100.00")?);
        assert_eq!(leaf_progress(d("-5")?, d("100")?)?, d("0.00")?);
        assert_eq!(leaf_progress(d("1")?, d("3")?)?, d("33.33")?);
        assert!(leaf_progress(d("1")?, d("0")?).is_err());
        Ok(())
    }

    #[test]
    fn a_parent_is_the_weighted_mean_of_its_children() -> Result<()> {
        assert_eq!(rollup(&[(3, d("100.00")?), (1, d("0.00")?)])?, d("75.00")?);
        assert_eq!(rollup(&[(1, d("100.00")?), (1, d("50.00")?)])?, d("75.00")?);
        assert_eq!(rollup(&[])?, d("0.00")?);
        Ok(())
    }

    #[test]
    fn areas_without_goals_follow_the_policy() -> Result<()> {
        let areas = vec![("delivery".to_string(), 70, Some(d("80.00")?)), ("learning".to_string(), 30, None)];
        let (excluded, uncovered) = goal_component(&areas, Missing::Exclude)?;
        assert_eq!((excluded, uncovered), (Some(d("80.00")?), vec!["learning".to_string()]));
        let (zeroed, _) = goal_component(&areas, Missing::Zero)?;
        assert_eq!(zeroed, Some(d("56.00")?));
        assert!(goal_component(&areas, Missing::Block).is_err());
        let (nothing, _) = goal_component(&[("a".to_string(), 100, None)], Missing::Exclude)?;
        assert_eq!(nothing, None);
        Ok(())
    }

    #[test]
    fn phases_go_forward_one_at_a_time() {
        let mut phase = Phase::Draft;
        let mut seen = vec![phase.as_str()];
        while let Some(next) = phase.next() {
            phase = next;
            seen.push(phase.as_str());
        }
        assert_eq!(seen, ["draft", "goal_setting", "self_review", "manager_review", "calibration", "published", "closed"]);
        assert!(Phase::GoalSetting.goals_editable() && !Phase::ManagerReview.goals_editable());
        assert!(Phase::ManagerReview.progress_reportable() && !Phase::Calibration.progress_reportable());
        assert!(Phase::parse("nope").is_none());
    }

    #[test]
    fn reminders_start_a_few_days_before_the_deadline() -> Result<()> {
        let deadline = Some(parse_date("2026-11-10")?);
        assert!(!reminder_due(deadline, parse_date("2026-11-01")?, 3));
        assert!(reminder_due(deadline, parse_date("2026-11-07")?, 3));
        assert!(reminder_due(deadline, parse_date("2026-11-20")?, 3));
        assert!(!reminder_due(None, parse_date("2026-11-20")?, 3));
        Ok(())
    }

    #[test]
    fn a_skill_gap_is_the_shortfall() {
        assert_eq!(skill_gap(4, Some(2)), 2);
        assert_eq!(skill_gap(4, Some(5)), 0);
        assert_eq!(skill_gap(3, None), 3);
    }
}
