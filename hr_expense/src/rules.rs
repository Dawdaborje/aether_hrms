//! The arithmetic and rules of expenses, with nothing read from a database.
//!
//! What the Odoo and Frappe sources taught (see `docs/design.md`): money is exact, a rate is stored with
//! the amount it converted, a line is converted once and the report is the sum of its lines; a policy limit
//! is looked up for the *expense date*, and can warn, ask for a reason, block, or cap; an approved amount
//! never exceeds the claimed one; an advance can only be used up to what is unclaimed, in its own currency.

use aether_sdk::decimal::Decimal;
use aether_sdk::{Error, Result};

/// Digits after the point of a stored exchange rate.
pub const RATE_SCALE: u32 = 9;

/// A rate (a float in the currency plugin) as an exact decimal.
pub fn exact_rate(rate: f64) -> Result<Decimal> {
    if !rate.is_finite() || rate <= 0.0 {
        return Err(Error::msg("an exchange rate is a number greater than zero"));
    }
    Decimal::parse(&format!("{rate:.9}"))
}

/// How many units of the settlement currency one unit of the expense currency is worth, from the two
/// currencies' rates against the base currency ("units of this currency for one unit of the base").
pub fn cross_rate(rate_expense: Decimal, rate_settlement: Decimal) -> Result<Decimal> {
    rate_settlement.div(rate_expense, RATE_SCALE)
}

/// An amount at a rate, rounded once to the settlement currency's digits.
pub fn convert(amount: Decimal, rate: Decimal, places: u32) -> Result<Decimal> {
    amount.times(rate, places)
}

/// What a quantity at a unit rate comes to (kilometres, days of per diem).
pub fn quantity_amount(quantity: Decimal, unit_rate: Decimal, places: u32) -> Result<Decimal> {
    if quantity.is_negative() || quantity.is_zero() {
        return Err(Error::msg("the quantity must be more than zero"));
    }
    quantity.times(unit_rate, places)
}

/// The sum of amounts, all in one currency and scale.
pub fn total(amounts: &[Decimal], places: u32) -> Decimal {
    amounts.iter().fold(Decimal::zero(places), |sum, amount| sum + *amount)
}

// ------------------------------------------------------------------ policy

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    PerItem,
    PerDay,
    PerReport,
    PerMonth,
}

impl Period {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "per_item" => Self::PerItem,
            "per_day" => Self::PerDay,
            "per_report" => Self::PerReport,
            "per_month" => Self::PerMonth,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enforcement {
    /// Note it for the approver.
    Warn,
    /// The person must say why.
    Justify,
    /// It cannot be submitted.
    Block,
    /// It is submitted, but only the limit can be approved.
    Cap,
}

impl Enforcement {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "warn" => Self::Warn,
            "justify" => Self::Justify,
            "block" => Self::Block,
            "cap" => Self::Cap,
            _ => return None,
        })
    }
}

/// What a limit says about a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Within,
    Over {
        enforcement: Enforcement,
        /// How much of the line is above the limit.
        excess: Decimal,
        /// How much of the line the limit allows.
        allowed: Decimal,
    },
}

/// A line of `amount`, when `already` has been claimed in the same period, against `limit`.
pub fn check_limit(limit: Decimal, enforcement: Enforcement, already: Decimal, amount: Decimal) -> Outcome {
    let room = limit - already;
    let room = if room.is_negative() { Decimal::zero(limit.scale()) } else { room };
    if amount <= room {
        return Outcome::Within;
    }
    Outcome::Over { enforcement, excess: amount - room, allowed: room }
}

/// The most that can be approved of a line: never more than claimed, and no more than a `cap` allows.
pub fn approvable(claimed: Decimal, capped_at: Option<Decimal>) -> Decimal {
    match capped_at {
        Some(cap) if cap < claimed => cap,
        _ => claimed,
    }
}

/// An approved amount asked for must be in `0 ..= approvable`.
pub fn check_approved(asked: Decimal, claimed: Decimal, capped_at: Option<Decimal>) -> Result<Decimal> {
    if asked.is_negative() {
        return Err(Error::msg("an approved amount cannot be negative"));
    }
    if asked > claimed {
        return Err(Error::msg("an approved amount cannot be more than was claimed"));
    }
    if let Some(cap) = capped_at {
        if asked > cap {
            return Err(Error::msg(format!("policy allows at most {cap} for this line")));
        }
    }
    Ok(asked)
}

// ----------------------------------------------------------------- advances

/// What may still be allocated from an advance to a report: issued less what was returned and what
/// earlier reports already used.
pub fn advance_unclaimed(issued: Decimal, returned: Decimal, settled: Decimal) -> Decimal {
    let left = issued - returned - settled;
    if left.is_negative() { Decimal::zero(issued.scale()) } else { left }
}

/// The allocation to a report from one advance: no more than the advance has left, and the advance
/// must be in the report's currency.
pub fn check_allocation(asked: Decimal, left: Decimal, advance_currency: &str, report_currency: &str) -> Result<Decimal> {
    if advance_currency != report_currency {
        return Err(Error::msg(format!(
            "the advance is in {advance_currency} and the report in {report_currency}: advances are settled in their own currency"
        )));
    }
    if asked.is_negative() || asked.is_zero() {
        return Err(Error::msg("allocate more than zero"));
    }
    if asked > left {
        return Err(Error::msg(format!("only {left} of the advance is left to use")));
    }
    Ok(asked)
}

/// What the person is owed, or (negative) owes back: approved less what advances covered.
pub fn net_payable(approved: Decimal, advances_applied: Decimal) -> Decimal {
    approved - advances_applied
}

/// An advance's state from its totals.
pub fn advance_state(issued: Decimal, settled: Decimal, returned: Decimal) -> &'static str {
    let zero = Decimal::zero(issued.scale());
    if issued <= zero {
        "approved"
    } else if settled + returned >= issued {
        if settled.is_zero() && returned >= issued { "returned" } else { "settled" }
    } else if settled > zero || returned > zero {
        "partially_settled"
    } else {
        "issued"
    }
}

// ------------------------------------------------------------------ the rest

/// What makes two lines the same expense: when, where, how much, in what currency.
pub fn duplicate_key(date: &str, merchant: &str, amount: &Decimal, currency: &str) -> String {
    let merchant: String = merchant.trim().to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{date}|{merchant}|{amount}|{}", currency.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(text: &str) -> Decimal {
        Decimal::parse(text).unwrap_or(Decimal::zero(0))
    }

    #[test]
    fn rates_are_exact_and_cross_rates_come_from_the_base() -> Result<()> {
        assert_eq!(exact_rate(0.85)?.to_string(), "0.850000000");
        assert!(exact_rate(0.0).is_err() && exact_rate(f64::NAN).is_err() && exact_rate(-1.0).is_err());
        // EUR 0.9 per base unit, GMD 70 per base unit: one euro is 77.777777778 dalasi.
        let rate = cross_rate(n("0.9"), n("70"))?;
        assert_eq!(rate.to_string(), "77.777777778");
        // Converted once, rounded once.
        assert_eq!(convert(n("12.50"), rate, 2)?.to_string(), "972.22");
        // The same currency is rate 1 and converts to itself.
        assert_eq!(convert(n("12.34"), cross_rate(n("0.9"), n("0.9"))?, 2)?.to_string(), "12.34");
        Ok(())
    }

    #[test]
    fn a_report_is_the_sum_of_its_rounded_lines() -> Result<()> {
        let rate = cross_rate(n("1"), n("3"))?; // 3 per unit
        let lines: Vec<Decimal> = ["0.333", "0.333", "0.334"].iter().map(|a| convert(n(a), rate, 2).unwrap_or(n("0"))).collect();
        assert_eq!(total(&lines, 2).to_string(), "3.00", "each line rounds to 1.00 and the report adds the rounded lines");
        Ok(())
    }

    #[test]
    fn mileage_is_quantity_times_the_rate() -> Result<()> {
        assert_eq!(quantity_amount(n("120"), n("0.45"), 2)?.to_string(), "54.00");
        assert_eq!(quantity_amount(n("3.5"), n("20"), 2)?.to_string(), "70.00");
        assert!(quantity_amount(n("0"), n("20"), 2).is_err());
        assert!(quantity_amount(n("-1"), n("20"), 2).is_err());
        Ok(())
    }

    #[test]
    fn limits_look_at_what_was_already_claimed_in_the_period() {
        let limit = n("100.00");
        assert_eq!(check_limit(limit, Enforcement::Block, n("0"), n("80")), Outcome::Within);
        assert_eq!(check_limit(limit, Enforcement::Block, n("30"), n("70")), Outcome::Within, "exactly at the limit");
        match check_limit(limit, Enforcement::Cap, n("30"), n("90")) {
            Outcome::Over { enforcement, excess, allowed } => {
                assert_eq!(enforcement, Enforcement::Cap);
                assert_eq!(excess.to_string(), "20.00");
                assert_eq!(allowed.to_string(), "70.00");
            }
            Outcome::Within => panic!("should be over"),
        }
        // Already over: nothing is allowed.
        match check_limit(limit, Enforcement::Warn, n("120"), n("10")) {
            Outcome::Over { allowed, excess, .. } => assert_eq!((allowed.to_string(), excess.to_string()), ("0.00".into(), "10.00".into())),
            Outcome::Within => panic!("should be over"),
        }
    }

    #[test]
    fn approved_is_never_more_than_claimed_or_the_cap() -> Result<()> {
        assert_eq!(approvable(n("90"), Some(n("70"))).to_string(), "70");
        assert_eq!(approvable(n("50"), Some(n("70"))).to_string(), "50");
        assert_eq!(approvable(n("50"), None).to_string(), "50");
        assert_eq!(check_approved(n("40"), n("50"), None)?.to_string(), "40");
        assert!(check_approved(n("60"), n("50"), None).is_err());
        assert!(check_approved(n("-1"), n("50"), None).is_err());
        assert!(check_approved(n("60"), n("90"), Some(n("70"))).is_ok());
        assert!(check_approved(n("80"), n("90"), Some(n("70"))).is_err());
        Ok(())
    }

    #[test]
    fn advances_are_used_in_their_own_currency_up_to_what_is_left() -> Result<()> {
        let left = advance_unclaimed(n("500"), n("50"), n("100"));
        assert_eq!(left.to_string(), "350");
        assert_eq!(check_allocation(n("300"), left, "GMD", "GMD")?.to_string(), "300");
        assert!(check_allocation(n("400"), left, "GMD", "GMD").is_err());
        assert!(check_allocation(n("100"), left, "USD", "GMD").is_err());
        assert!(check_allocation(n("0"), left, "GMD", "GMD").is_err());
        assert!(advance_unclaimed(n("100"), n("80"), n("50")).is_zero(), "never negative");
        assert_eq!(net_payable(n("300"), n("350")).to_string(), "-50", "the person owes 50 back");
        Ok(())
    }

    #[test]
    fn an_advance_state_follows_its_totals() {
        assert_eq!(advance_state(n("0"), n("0"), n("0")), "approved");
        assert_eq!(advance_state(n("500"), n("0"), n("0")), "issued");
        assert_eq!(advance_state(n("500"), n("200"), n("0")), "partially_settled");
        assert_eq!(advance_state(n("500"), n("500"), n("0")), "settled");
        assert_eq!(advance_state(n("500"), n("300"), n("200")), "settled");
        assert_eq!(advance_state(n("500"), n("0"), n("500")), "returned");
    }

    #[test]
    fn the_same_expense_has_the_same_key_however_it_was_typed() {
        let a = duplicate_key("2026-10-01", "  Hotel   Atlantic ", &n("120.00"), "gmd");
        let b = duplicate_key("2026-10-01", "hotel atlantic", &n("120.00"), "GMD");
        assert_eq!(a, b);
        assert_ne!(a, duplicate_key("2026-10-01", "hotel atlantic", &n("120.01"), "GMD"));
    }
}
