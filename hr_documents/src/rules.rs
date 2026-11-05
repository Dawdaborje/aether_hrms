//! Checks for documents, contacts and the salary split, with nothing read from a database.
//!
//! Each identity document, emergency contact, and bank account is its own row (not flattened onto
//! the person). Bank account shares of salary must add up to exactly 100. An expiry sweep reports
//! what is due or already past, once per threshold.

use aether_sdk::dates::NaiveDate;
use aether_sdk::decimal::Decimal;
use aether_sdk::{Error, Result};

/// Kinds of identity document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Passport,
    NationalId,
    Visa,
    WorkPermit,
    DrivingLicence,
    Other,
}

impl DocKind {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "passport" => Self::Passport,
            "national_id" => Self::NationalId,
            "visa" => Self::Visa,
            "work_permit" => Self::WorkPermit,
            "driving_licence" => Self::DrivingLicence,
            "other" => Self::Other,
            _ => return None,
        })
    }

    /// Whether this kind must have an expiry date.
    pub fn needs_expiry(self) -> bool {
        matches!(self, Self::Passport | Self::Visa | Self::WorkPermit | Self::DrivingLicence)
    }
}

/// How soon a document needs attention: already past, within the warn window, or fine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expiry {
    Expired,
    DueSoon,
    Ok,
}

/// Classify an expiry against today and a warn window in days (default 30).
pub fn expiry_of(today: NaiveDate, expires: Option<NaiveDate>, warn_days: i64) -> Expiry {
    let Some(expires) = expires else { return Expiry::Ok };
    if expires < today {
        Expiry::Expired
    } else if (expires - today).num_days() <= warn_days.max(0) {
        Expiry::DueSoon
    } else {
        Expiry::Ok
    }
}

/// Thresholds at which an expiry is reported once: 30 days, 7 days, and the day itself (0).
pub const WARN_AT: [i64; 3] = [30, 7, 0];

/// Whether today hits one of the warn thresholds for this expiry (so the nightly job reports once).
pub fn warn_today(today: NaiveDate, expires: NaiveDate) -> Option<i64> {
    let left = (expires - today).num_days();
    WARN_AT.iter().copied().find(|&d| left == d)
}

/// Shares of a salary split: each between 0 and 100 exclusive of zero, summing to exactly 100.
pub fn split_ok(shares: &[Decimal]) -> Result<()> {
    if shares.is_empty() {
        return Err(Error::msg("a salary split needs at least one bank account"));
    }
    let mut total = Decimal::zero(2);
    for share in shares {
        let s = share.with_scale(2)?;
        if s.is_negative() || s.is_zero() || s > Decimal::parse("100.00")? {
            return Err(Error::msg("each share is more than 0 and at most 100"));
        }
        total = total + s;
    }
    if total != Decimal::parse("100.00")? {
        return Err(Error::msg(format!("the shares add up to {total}, not 100")));
    }
    Ok(())
}

/// At most one primary contact; at least one contact when any are given is not required here.
pub fn contacts_ok(primaries: usize) -> Result<()> {
    if primaries > 1 {
        return Err(Error::msg("only one emergency contact is primary"));
    }
    Ok(())
}

/// An identity document: number required; expiry required for kinds that expire.
pub fn document_ok(kind: DocKind, number: &str, expires: Option<NaiveDate>) -> Result<()> {
    if number.trim().is_empty() {
        return Err(Error::msg("the document needs a number"));
    }
    if kind.needs_expiry() && expires.is_none() {
        let what = match kind {
            DocKind::Passport => "a passport",
            DocKind::Visa => "a visa",
            DocKind::WorkPermit => "a work permit",
            DocKind::DrivingLicence => "a driving licence",
            DocKind::NationalId => "a national id",
            DocKind::Other => "this document",
        };
        return Err(Error::msg(format!("{what} needs an expiry date")));
    }
    Ok(())
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
    fn expiry_windows() {
        let today = d("2026-10-08");
        assert_eq!(expiry_of(today, Some(d("2026-10-01")), 30), Expiry::Expired);
        assert_eq!(expiry_of(today, Some(d("2026-10-20")), 30), Expiry::DueSoon);
        assert_eq!(expiry_of(today, Some(d("2027-01-01")), 30), Expiry::Ok);
        assert_eq!(expiry_of(today, None, 30), Expiry::Ok);
        assert_eq!(warn_today(today, d("2026-11-07")), Some(30));
        assert_eq!(warn_today(today, d("2026-10-15")), Some(7));
        assert_eq!(warn_today(today, d("2026-10-08")), Some(0));
        assert_eq!(warn_today(today, d("2026-10-09")), None);
    }

    #[test]
    fn salary_shares_add_to_one_hundred() -> Result<()> {
        assert!(split_ok(&[n("100.00")]).is_ok());
        assert!(split_ok(&[n("60.00"), n("40.00")]).is_ok());
        assert!(split_ok(&[n("33.33"), n("33.33"), n("33.34")]).is_ok());
        assert!(split_ok(&[n("50.00"), n("40.00")]).is_err());
        assert!(split_ok(&[n("0.00")]).is_err());
        assert!(split_ok(&[]).is_err());
        Ok(())
    }

    #[test]
    fn documents_that_expire_need_a_date() {
        assert!(document_ok(DocKind::Passport, "P1", Some(d("2027-01-01"))).is_ok());
        assert!(document_ok(DocKind::Passport, "P1", None).is_err());
        assert!(document_ok(DocKind::NationalId, "N1", None).is_ok());
        assert!(document_ok(DocKind::Passport, "", Some(d("2027-01-01"))).is_err());
        assert!(contacts_ok(1).is_ok());
        assert!(contacts_ok(2).is_err());
    }
}
