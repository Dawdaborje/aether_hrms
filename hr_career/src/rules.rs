//! The arithmetic of a career change, with no records read.

use aether_sdk::decimal::Decimal;
use aether_sdk::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Promotion,
    Demotion,
    Transfer,
    GradeChange,
    PayChange,
}

impl Kind {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "promotion" => Self::Promotion,
            "demotion" => Self::Demotion,
            "transfer" => Self::Transfer,
            "grade_change" => Self::GradeChange,
            "pay_change" => Self::PayChange,
            _ => return None,
        })
    }
}

/// What changed between the terms today and the proposed ones.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    pub grade: bool,
    pub job_or_position: bool,
    pub placement: bool,
    pub wage: bool,
    pub any: bool,
}

/// Whether the kind of change fits what actually changes and the ladder: a promotion must go up (a grade with a
/// higher rank, or a new job or position), a demotion must go down, a transfer must move someone, a pay change
/// may change only the wage.
pub fn check_kind(kind: Kind, diff: &Diff, old_rank: Option<i64>, new_rank: Option<i64>) -> Result<()> {
    if !diff.any {
        return Err(Error::msg("nothing would change: name at least one new term"));
    }
    let moved = match (old_rank, new_rank) {
        (Some(old), Some(new)) => Some(new.cmp(&old)),
        _ => None,
    };
    match kind {
        Kind::Promotion => {
            if let Some(order) = moved {
                if order.is_lt() {
                    return Err(Error::msg("a promotion cannot go to a lower grade: that is a demotion"));
                }
                if order.is_eq() && !diff.job_or_position {
                    return Err(Error::msg("a promotion changes the grade or the job or position"));
                }
            } else if !diff.grade && !diff.job_or_position {
                return Err(Error::msg("a promotion changes the grade or the job or position"));
            }
            Ok(())
        }
        Kind::Demotion => match moved {
            Some(order) if order.is_lt() => Ok(()),
            _ if diff.job_or_position && !diff.grade => Ok(()),
            _ => Err(Error::msg("a demotion goes to a lower grade, or to a lesser job or position")),
        },
        Kind::Transfer => {
            if !diff.placement && !diff.job_or_position {
                return Err(Error::msg("a transfer changes the department, manager, location, job or position"));
            }
            if diff.grade {
                return Err(Error::msg("a transfer does not change the grade: ask for a grade change as well"));
            }
            Ok(())
        }
        Kind::GradeChange => {
            if !diff.grade {
                return Err(Error::msg("a grade change names a new grade"));
            }
            Ok(())
        }
        Kind::PayChange => {
            if !diff.wage || diff.grade || diff.job_or_position || diff.placement {
                return Err(Error::msg("a pay change changes only the wage"));
            }
            Ok(())
        }
    }
}

/// Whether a wage lies in a grade's band; an unset bound is open.
pub fn within_band(wage: Decimal, min: Option<Decimal>, max: Option<Decimal>) -> bool {
    min.is_none_or(|m| wage >= m) && max.is_none_or(|m| wage <= m)
}

/// Fill `{{name}}` placeholders. A placeholder with no value is an error, not an empty space, so a letter never
/// goes out with a hole in it.
pub fn render(template: &str, values: &[(&str, String)]) -> Result<String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find("}}").ok_or_else(|| Error::msg("a placeholder is not closed: `{{` without `}}`"))?;
        let name = after[..end].trim();
        let value = values
            .iter()
            .find(|(k, v)| *k == name && !v.is_empty())
            .ok_or_else(|| Error::msg(format!("the letter needs `{name}` and there is no value for it")))?;
        out.push_str(&value.1);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(text: &str) -> Decimal {
        Decimal::parse(text).and_then(|v| v.with_scale(2)).unwrap_or_else(|_| Decimal::zero(2))
    }

    #[test]
    fn a_promotion_must_go_up_and_a_demotion_down() {
        let grade = Diff { grade: true, any: true, ..Diff::default() };
        assert!(check_kind(Kind::Promotion, &grade, Some(2), Some(3)).is_ok());
        assert!(check_kind(Kind::Promotion, &grade, Some(3), Some(2)).is_err());
        assert!(check_kind(Kind::Promotion, &grade, Some(3), Some(3)).is_err());
        assert!(check_kind(Kind::Demotion, &grade, Some(3), Some(2)).is_ok());
        assert!(check_kind(Kind::Demotion, &grade, Some(2), Some(3)).is_err());
        let job = Diff { job_or_position: true, any: true, ..Diff::default() };
        assert!(check_kind(Kind::Promotion, &job, Some(3), Some(3)).is_ok());
    }

    #[test]
    fn a_transfer_moves_and_a_pay_change_only_pays() {
        let place = Diff { placement: true, any: true, ..Diff::default() };
        assert!(check_kind(Kind::Transfer, &place, None, None).is_ok());
        let with_grade = Diff { placement: true, grade: true, any: true, ..Diff::default() };
        assert!(check_kind(Kind::Transfer, &with_grade, None, None).is_err());
        let wage = Diff { wage: true, any: true, ..Diff::default() };
        assert!(check_kind(Kind::PayChange, &wage, None, None).is_ok());
        assert!(check_kind(Kind::PayChange, &place, None, None).is_err());
        assert!(check_kind(Kind::PayChange, &Diff::default(), None, None).is_err());
    }

    #[test]
    fn a_wage_must_sit_in_the_band() {
        assert!(within_band(d("500"), Some(d("400")), Some(d("600"))));
        assert!(!within_band(d("700"), Some(d("400")), Some(d("600"))));
        assert!(within_band(d("700"), Some(d("400")), None));
        assert!(!within_band(d("300"), Some(d("400")), None));
        assert!(within_band(d("1"), None, None));
    }

    #[test]
    fn letters_refuse_holes() {
        let values = [("name", "Ada".to_string()), ("job", String::new())];
        assert_eq!(render("Dear {{ name }}, welcome.", &values).unwrap_or_default(), "Dear Ada, welcome.");
        assert!(render("Dear {{name}}, as {{job}}", &values).is_err());
        assert!(render("Dear {{missing}}", &values).is_err());
        assert!(render("oops {{name", &values).is_err());
        assert_eq!(render("no placeholders", &values).unwrap_or_default(), "no placeholders");
    }
}
