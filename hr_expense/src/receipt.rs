//! Reading a receipt: what the kernel's document parsing finds on it, offered as suggestions for the line.
//!
//! Nothing is filled in for the person. `read_receipt` answers the text and what looks like the total, the date and
//! the merchant, and the person decides what goes on the line. A suggestion is a guess from words and numbers, not a
//! parser of every receipt layout.

use aether_sdk::prelude::*;

use crate::common::{require, text, Record};

#[derive(Deserialize)]
struct Read {
    line: String,
    /// Which receipt of the line, from 1; the first when left out.
    #[serde(default)]
    number: Option<usize>,
}

/// What a receipt's text suggests.
#[derive(Debug, PartialEq)]
pub struct Suggestion {
    pub amount: Option<String>,
    pub date: Option<String>,
    pub merchant: Option<String>,
}

/// Numbers written as money on a line: `1,234.50`, `12.00`, `45,90` (a comma before exactly two digits).
fn amounts_in(line: &str) -> Vec<f64> {
    let mut found = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == ',' || chars[i] == '.') {
                i += 1;
            }
            let token: String = chars[start..i].iter().collect::<String>().trim_end_matches([',', '.']).to_string();
            // A decimal part of exactly two digits makes it money; the other separator groups thousands.
            let split = token.rfind(['.', ',']);
            if let Some(at) = split.filter(|at| token.len() - at - 1 == 2) {
                let whole: String = token[..at].chars().filter(char::is_ascii_digit).collect();
                if let Ok(value) = format!("{whole}.{}", &token[at + 1..]).parse::<f64>() {
                    found.push(value);
                }
            }
        } else {
            i += 1;
        }
    }
    found
}

/// A date written `2026-10-16`, `16/10/2026`, `16-10-2026` or `16.10.2026`, as `YYYY-MM-DD`. Day first, as most of the world writes it.
fn date_in(line: &str) -> Option<String> {
    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    for word in line.split(|c: char| c.is_whitespace() || c == ',') {
        let word = word.trim_matches(|c: char| !c.is_ascii_digit());
        let parts: Vec<&str> = word.split(['-', '/', '.']).collect();
        if parts.len() != 3 || !parts.iter().all(|p| digits(p) && !p.is_empty()) {
            continue;
        }
        let number = |p: &str| p.parse::<u32>().ok();
        let (year, month, day) = if parts[0].len() == 4 {
            (number(parts[0]), number(parts[1]), number(parts[2]))
        } else if parts[2].len() == 4 {
            (number(parts[2]), number(parts[1]), number(parts[0]))
        } else {
            continue;
        };
        if let (Some(y), Some(m), Some(d)) = (year, month, day) {
            if (1990..=2100).contains(&y) && (1..=12).contains(&m) && (1..=31).contains(&d) {
                return Some(format!("{y:04}-{m:02}-{d:02}"));
            }
        }
    }
    None
}

pub fn suggest(receipt: &str) -> Suggestion {
    let lines: Vec<&str> = receipt.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    // The total is on a line that says so (the last such line wins: a subtotal comes before it); otherwise the largest amount.
    let says_total = |l: &&&str| {
        let lower = l.to_lowercase();
        (lower.contains("total") || lower.contains("amount due") || lower.contains("balance due")) && !lower.contains("subtotal") && !lower.contains("sub total")
    };
    let amount = lines
        .iter()
        .filter(says_total)
        .filter_map(|l| amounts_in(l).into_iter().fold(None, |best: Option<f64>, v| Some(best.map_or(v, |b| b.max(v)))))
        .last()
        .or_else(|| lines.iter().flat_map(|l| amounts_in(l)).fold(None, |best: Option<f64>, v| Some(best.map_or(v, |b| b.max(v)))));
    let date = lines.iter().find_map(|l| date_in(l));
    // The merchant is the first line that is words rather than a number or a date.
    let merchant = lines
        .iter()
        .take(5)
        .find(|l| l.chars().filter(|c| c.is_alphabetic()).count() >= 3 && date_in(l).is_none() && amounts_in(l).is_empty())
        .map(|l| l.chars().take(80).collect::<String>());
    Suggestion { amount: amount.map(|a| format!("{a:.2}")), date, merchant }
}

/// Read one receipt of a line (the caller must be able to see the line).
fn read(input: Read) -> Result<Value> {
    let line = require("expense_line", &input.line, "line")?;
    let keys: Vec<&str> = text(&line, "receipts").map(|s| s.split(',').collect()).unwrap_or_default();
    let number = input.number.unwrap_or(1);
    let key = number.checked_sub(1).and_then(|i| keys.get(i)).ok_or_else(|| Error::msg("that receipt is not on the line"))?;
    // The file is in this plugin's own storage, not in the organization's media.
    let read = ocr::parse(&json!({ "storage_key": key }))?;
    let body = read["text"].as_str().unwrap_or_default();
    let suggestion = suggest(body);
    Ok(json!({
        "text": body,
        "ocr_used": read["ocr_used"],
        "suggested": { "amount": suggestion.amount, "date": suggestion.date, "merchant": suggestion.merchant },
    }))
}

handler! {
    /// Read a receipt on a line and suggest its amount, date and merchant; nothing is changed.
    fn read_receipt(input: Read) -> Value {
        read(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_is_found_with_either_separator() {
        assert_eq!(amounts_in("Total 1,234.50 NGN"), vec![1234.5]);
        assert_eq!(amounts_in("TOTAL: 45,90"), vec![45.9]);
        assert_eq!(amounts_in("Qty 3 @ 12.00 = 36.00"), vec![12.0, 36.0]);
        assert!(amounts_in("Tel 0801234567 table 12").is_empty());
    }

    #[test]
    fn dates_come_out_as_iso() {
        assert_eq!(date_in("Date: 16/10/2026 14:02"), Some("2026-10-16".into()));
        assert_eq!(date_in("2026-10-16"), Some("2026-10-16".into()));
        assert_eq!(date_in("on 03.04.2025."), Some("2025-04-03".into()));
        assert_eq!(date_in("32/13/2026"), None);
        assert_eq!(date_in("no date here"), None);
    }

    #[test]
    fn a_receipt_suggests_its_total_date_and_merchant() {
        let receipt = "Green Fork Cafe\n12 Marina Road\nDate 16/10/2026\nCoffee 3.50\nLunch 18.00\nSubtotal 21.50\nVAT 1.61\nTOTAL 23.11\nThank you";
        assert_eq!(
            suggest(receipt),
            Suggestion { amount: Some("23.11".into()), date: Some("2026-10-16".into()), merchant: Some("Green Fork Cafe".into()) }
        );
        // Without a total line, the largest amount.
        assert_eq!(suggest("Shop\n5.00\n9.99").amount, Some("9.99".into()));
        assert_eq!(suggest("nothing useful"), Suggestion { amount: None, date: None, merchant: Some("nothing useful".into()) });
    }
}
