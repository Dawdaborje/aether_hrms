//! What an enrollment is allowed to do and what the new hire must give. Pure, so it is tested here.

use aether_sdk::dates::{parse_date, Datelike, NaiveDate};
use serde_json::{json, Value};

pub type Record = Value;

pub const OPEN_FOR_HIRE: &[&str] = &["invited", "in_progress", "changes_requested"];
pub const OPEN: &[&str] = &["draft", "invited", "in_progress", "submitted", "changes_requested"];

/// Where an enrollment may go next.
pub fn can_move(from: &str, to: &str) -> bool {
    match (from, to) {
        ("draft", "invited") => true,
        ("invited", "in_progress" | "submitted") => true,
        ("in_progress", "submitted") => true,
        ("submitted", "changes_requested" | "approved") => true,
        ("changes_requested", "submitted" | "in_progress") => true,
        ("expired", "invited") => true,
        (state, "expired" | "cancelled") => OPEN.contains(&state),
        _ => false,
    }
}

/// The sections of the form and the fields each one holds. The names are those of HR's private details,
/// so approval can hand them straight to `hire_employee`.
pub const SECTIONS: &[(&str, &[&str])] = &[
    ("personal", &["legal_name", "date_of_birth", "place_of_birth", "gender", "marital_status", "nationality", "private_email", "private_phone", "private_address"]),
    ("emergency", &["emergency_name", "emergency_relation", "emergency_phone"]),
    ("identity", &["passport_no", "passport_expiry", "visa_no", "work_permit_expiry"]),
    ("bank", &["bank_name", "bank_account_no"]),
    ("acknowledgement", &[]),
];

pub const PHONE_FIELDS: &[&str] = &["private_phone", "emergency_phone"];

/// What a phone box holds. The phone field fills in the country's dialling code (`+220`) when a country is
/// chosen, so a box nobody typed a number into arrives as just that code: it counts as empty.
#[derive(Debug, PartialEq)]
pub enum Phone {
    Empty,
    Number(String),
    Invalid,
}

pub fn phone(raw: &str) -> Phone {
    let compact: String = raw.chars().filter(|c| !matches!(c, ' ' | '-' | '.' | '(' | ')')).collect();
    if compact.is_empty() {
        return Phone::Empty;
    }
    let digits = compact.strip_prefix('+').unwrap_or(&compact);
    if compact.starts_with('+') && digits.len() <= 4 && digits.chars().all(|c| c.is_ascii_digit()) {
        return Phone::Empty;
    }
    let good = compact.starts_with('+')
        && (8..=15).contains(&digits.len())
        && digits.chars().all(|c| c.is_ascii_digit())
        && !digits.starts_with('0');
    if good { Phone::Number(compact) } else { Phone::Invalid }
}

pub const DATE_FIELDS: &[&str] = &["date_of_birth", "passport_expiry", "work_permit_expiry"];

/// The fields of one section taken from what the form sent, trimmed, empty ones left out.
pub fn section_data(kind: &str, input: &Record) -> Record {
    let mut data = json!({});
    if let Some((_, fields)) = SECTIONS.iter().find(|(name, _)| *name == kind) {
        for field in *fields {
            if let Some(value) = input.get(*field).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()) {
                if PHONE_FIELDS.contains(field) && phone(value) == Phone::Empty {
                    continue;
                }
                data[*field] = json!(value);
            }
        }
    }
    data
}

fn longest(field: &str) -> usize {
    if field == "private_address" { 500 } else { 200 }
}

fn looks_like_date(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() == 10 && b[4] == b'-' && b[7] == b'-' && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// A day that exists on the calendar: `1990-02-30` has the right shape and is still refused.
fn is_calendar_date(value: &str) -> bool {
    looks_like_date(value) && parse_date(value).is_ok()
}

/// The youngest age to be hired when nothing was set.
pub const DEFAULT_MINIMUM_AGE: u32 = 14;
/// The oldest age a birth date may give.
pub const OLDEST_AGE: i32 = 100;

/// The same day `years` earlier; 29 February becomes 28 February where that year has no 29th.
fn years_before(date: NaiveDate, years: i32) -> NaiveDate {
    NaiveDate::from_ymd_opt(date.year() - years, date.month(), date.day())
        .or_else(|| NaiveDate::from_ymd_opt(date.year() - years, date.month(), 28))
        .unwrap_or(date)
}

/// The birth dates that make sense: no earlier than `OLDEST_AGE` years before today, and no later than the day
/// that makes the person `minimum_age` on the day they join (never after today). Both ends are inclusive.
pub fn birth_window(joining: Option<&str>, today: &str, minimum_age: u32) -> Result<(NaiveDate, NaiveDate), String> {
    let today = parse_date(today).map_err(|e| e.to_string())?;
    let join = joining.and_then(|j| parse_date(j).ok()).unwrap_or(today);
    let latest = years_before(join, minimum_age as i32).min(today);
    Ok((years_before(today, OLDEST_AGE), latest))
}

/// What the new hire must have filled in before they can submit. The first problem is returned.
/// `joining` is the date they join, `today` is today's date and `minimum_age` the youngest age allowed to join.
pub fn check_submission(input: &Record, joining: Option<&str>, today: &str, minimum_age: u32) -> Result<(), String> {
    for (kind, _) in SECTIONS {
        for (field, value) in section_data(kind, input).as_object().into_iter().flatten() {
            let value = value.as_str().unwrap_or_default();
            if value.chars().count() > longest(field) {
                return Err(format!("{field} is too long"));
            }
            if PHONE_FIELDS.contains(&field.as_str()) && phone(value) == Phone::Invalid {
                return Err(format!("{} must be a phone number in international form, such as +2348012345678", field.replace('_', " ")));
            }
            if DATE_FIELDS.contains(&field.as_str()) && !is_calendar_date(value) {
                return Err(format!("{} must be a real date, such as 1990-02-28", field.replace('_', " ")));
            }
        }
    }
    let personal = section_data("personal", input);
    for required in ["legal_name", "date_of_birth", "private_address"] {
        if personal.get(required).is_none() {
            return Err(format!("{} is required", required.replace('_', " ")));
        }
    }
    if let Some(born) = personal.get("date_of_birth").and_then(Value::as_str) {
        // The shape was checked above; a day that does not exist is an error, never a reason to skip the age check.
        let born = parse_date(born).map_err(|_| "date of birth must be a real date, such as 1990-02-28".to_string())?;
        let (earliest, latest) = birth_window(joining, today, minimum_age)?;
        let today_date = parse_date(today).map_err(|e| e.to_string())?;
        if born > today_date {
            return Err("the date of birth cannot be in the future".into());
        }
        if born > latest {
            return Err(format!(
                "the date of birth is too recent: a person must be at least {minimum_age} on the day they join, so the latest date of birth is {latest}"
            ));
        }
        if born < earliest {
            return Err(format!("the date of birth is more than {OLDEST_AGE} years ago: check the year"));
        }
    }
    let emergency = section_data("emergency", input);
    for required in ["emergency_name", "emergency_relation", "emergency_phone"] {
        if emergency.get(required).is_none() {
            return Err(format!("{} is required", required.replace('_', " ")));
        }
    }
    let bank = section_data("bank", input);
    if bank.get("bank_name").is_some() != bank.get("bank_account_no").is_some() {
        return Err("give both the bank and the account number, or neither".into());
    }
    if input.get("acknowledged").and_then(Value::as_bool) != Some(true) {
        return Err("confirm that you have read the policies".into());
    }
    Ok(())
}

/// Whether the link has run out.
pub fn expired(expires_at: Option<&str>, now: &str) -> bool {
    expires_at.is_some_and(|at| at < now)
}

pub fn fits_email(email: &str) -> bool {
    let email = email.trim();
    email.len() <= 160 && !email.contains(' ') && email.split_once('@').is_some_and(|(user, host)| !user.is_empty() && host.contains('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> Record {
        json!({
            "legal_name": "Ada Lovelace", "date_of_birth": "1990-12-10", "private_address": "1 Main St",
            "emergency_name": "Byron", "emergency_relation": "friend", "emergency_phone": "+1 555 0100",
            "acknowledged": true
        })
    }

    #[test]
    fn a_complete_form_passes() {
        assert_eq!(check_submission(&good(), None, "2026-11-01", 14), Ok(()));
    }

    #[test]
    fn each_gap_is_named() {
        for (field, message) in [("legal_name", "legal name is required"), ("emergency_phone", "emergency phone is required")] {
            let mut input = good();
            input.as_object_mut().unwrap().remove(field);
            assert_eq!(check_submission(&input, None, "2026-11-01", 14), Err(message.to_string()));
        }
        let mut input = good();
        input["acknowledged"] = json!(false);
        assert!(check_submission(&input, None, "2026-11-01", 14).is_err());
        input = good();
        input["date_of_birth"] = json!("10/12/1990");
        assert_eq!(check_submission(&input, None, "2026-11-01", 14), Err("date of birth must be a real date, such as 1990-02-28".into()));
        // The right shape is not enough: the day has to exist.
        for (field, value) in [("date_of_birth", "1990-02-30"), ("passport_expiry", "2030-02-30"), ("work_permit_expiry", "2030-13-01")] {
            input = good();
            input[field] = json!(value);
            assert!(check_submission(&input, None, "2026-11-01", 14).unwrap_err().contains("real date"), "{field} {value}");
        }
        input = good();
        input["passport_expiry"] = json!("2030-02-28");
        assert_eq!(check_submission(&input, None, "2026-11-01", 14), Ok(()));
        input = good();
        input["bank_name"] = json!("Acme Bank");
        assert!(check_submission(&input, None, "2026-11-01", 14).is_err());
    }

    #[test]
    fn birth_dates_must_make_sense() {
        let with = |dob: &str| {
            let mut input = good();
            input["date_of_birth"] = json!(dob);
            check_submission(&input, Some("2026-12-01"), "2026-11-01", 14)
        };
        assert_eq!(with("1990-12-10"), Ok(()));
        assert!(with("2027-01-01").unwrap_err().contains("future"));
        assert!(with("2015-01-01").unwrap_err().contains("at least 14"));
        // Exactly 14 on the joining day is fine; a day later is not.
        assert_eq!(with("2012-12-01"), Ok(()));
        assert!(with("2012-12-02").is_err());
        assert!(with("1900-01-01").unwrap_err().contains("100 years"));
    }

    #[test]
    fn the_window_ends_where_the_age_does() {
        let (earliest, latest) = birth_window(Some("2026-12-01"), "2026-11-01", 14).unwrap();
        assert_eq!(latest.to_string(), "2012-12-01");
        assert_eq!(earliest.to_string(), "1926-11-01");
        // No joining date: counted from today.
        assert_eq!(birth_window(None, "2026-11-01", 14).unwrap().1.to_string(), "2012-11-01");
        // 29 February.
        assert_eq!(birth_window(Some("2028-02-29"), "2026-11-01", 14).unwrap().1.to_string(), "2014-02-28");
    }

    #[test]
    fn sections_keep_only_their_fields() {
        let data = section_data("emergency", &good());
        assert_eq!(data.as_object().unwrap().len(), 3);
        assert!(data.get("legal_name").is_none());
    }

    #[test]
    fn states_move_forward_only() {
        assert!(can_move("draft", "invited"));
        assert!(can_move("submitted", "approved"));
        assert!(can_move("submitted", "changes_requested"));
        assert!(can_move("changes_requested", "submitted"));
        assert!(!can_move("draft", "approved"));
        assert!(!can_move("approved", "cancelled"));
        assert!(can_move("expired", "invited"));
        assert!(!can_move("cancelled", "invited"));
        assert!(can_move("invited", "expired"));
    }

    #[test]
    fn expiry_and_email() {
        assert!(expired(Some("2026-01-01T00:00:00Z"), "2026-01-02T00:00:00Z"));
        assert!(!expired(None, "2026-01-02T00:00:00Z"));
        assert!(fits_email("a@b.co") && !fits_email("a b@c.co") && !fits_email("nope"));
    }
}

#[cfg(test)]
mod phone_tests {
    use super::*;

    #[test]
    fn a_lone_dialling_code_is_empty() {
        assert_eq!(phone("+220"), Phone::Empty);
        assert_eq!(phone(" + 1 "), Phone::Empty);
        assert_eq!(phone(""), Phone::Empty);
    }

    #[test]
    fn real_numbers_pass_and_partial_ones_do_not() {
        assert_eq!(phone("+234 801 234 5678"), Phone::Number("+2348012345678".into()));
        assert_eq!(phone("+22012"), Phone::Invalid);
        assert_eq!(phone("08012345678"), Phone::Invalid);
    }
}
