//! Exit interview (one per departure, confidential) and the final settlement builder.
//!
//! * An exit interview is scheduled against a live departure and completed once. The departing person may read that
//!   it exists; `private_notes` are only for onboarding officers and managers.
//! * `build_final_settlement` pulls what other plugins know about the leaver into settlement lines and return items,
//!   keyed by source so running it twice does not double-count. It quotes; it does not encash leave or approve
//!   gratuity by itself.

use aether_sdk::dates::format_date;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{employee, id_of, next_number, require, require_officer, text, today_date, Record};
use crate::rules::{exit_next, source_key, ExitState};

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Schedule {
    departure: String,
    #[serde(default)]
    interviewer: Option<String>,
    #[serde(default)]
    scheduled_on: Option<String>,
}

#[derive(Deserialize)]
struct Complete {
    id: String,
    #[serde(default)]
    reason_for_leaving: Option<String>,
    #[serde(default)]
    would_recommend: Option<bool>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    private_notes: Option<String>,
}

#[derive(Deserialize)]
struct Build {
    departure: String,
    /// Daily rate used to quote encashable leave. Without it, leave balances are reported but not lined.
    #[serde(default)]
    leave_daily_rate: Option<Decimal>,
    /// Gratuity rule and monthly base; without both, gratuity is skipped.
    #[serde(default)]
    gratuity_rule: Option<String>,
    #[serde(default)]
    gratuity_base: Option<Decimal>,
    #[serde(default)]
    currency: Option<String>,
}

fn departure_live(id: &str) -> Result<Record> {
    let departure = require("ob_departure", id, "departure")?;
    if matches!(text(&departure, "status"), Some("closed" | "cancelled")) {
        return Err(Error::msg(format!("this departure is {}", text(&departure, "status").unwrap_or("over"))));
    }
    Ok(departure)
}

fn schedule(input: Schedule) -> Result<Record> {
    require_officer()?;
    let departure = departure_live(&input.departure)?;
    if let Some(existing) = db::find::<Record>("ob_exit_interview").filter("departure", input.departure.as_str()).first()? {
        return Err(Error::msg(format!(
            "this departure already has exit interview {} ({})",
            text(&existing, "reference").unwrap_or("?"),
            text(&existing, "state").unwrap_or("?")
        )));
    }
    if let Some(who) = &input.interviewer {
        employee(who)?;
    }
    let scheduled_on = match input.scheduled_on {
        Some(d) => aether_sdk::dates::parse_date(&d).map(format_date)?,
        None => format_date(today_date()?),
    };
    db::create(
        "ob_exit_interview",
        &json!({
            "reference": next_number("exit_interview", "EXIT-", 5)?,
            "departure": input.departure,
            "employee": departure["employee"],
            "interviewer": input.interviewer,
            "scheduled_on": scheduled_on,
            "state": "scheduled",
        }),
    )
}

fn complete(input: Complete) -> Result<Record> {
    require_officer()?;
    let row = require("ob_exit_interview", &input.id, "exit interview")?;
    let state = ExitState::parse(text(&row, "state").unwrap_or_default()).ok_or_else(|| Error::msg("unknown interview state"))?;
    exit_next(state, ExitState::Completed)?;
    if input.reason_for_leaving.as_deref().map(str::trim).unwrap_or_default().is_empty()
        && input.summary.as_deref().map(str::trim).unwrap_or_default().is_empty()
    {
        return Err(Error::msg("record a reason for leaving or a summary"));
    }
    db::update(
        "ob_exit_interview",
        &input.id,
        &json!({
            "state": "completed",
            "reason_for_leaving": input.reason_for_leaving,
            "would_recommend": input.would_recommend,
            "summary": input.summary,
            "private_notes": input.private_notes,
            "completed_on": format_date(today_date()?),
            "completed_by": context::current()?.actor.id.unwrap_or_default(),
        }),
    )?
    .ok_or_else(|| Error::msg("the interview is gone"))
}

fn cancel(input: Id) -> Result<Record> {
    require_officer()?;
    let row = require("ob_exit_interview", &input.id, "exit interview")?;
    let state = ExitState::parse(text(&row, "state").unwrap_or_default()).ok_or_else(|| Error::msg("unknown interview state"))?;
    exit_next(state, ExitState::Cancelled)?;
    db::update("ob_exit_interview", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the interview is gone"))
}

fn get_interview(input: Id) -> Result<Record> {
    require("ob_exit_interview", &input.id, "exit interview")
}

fn interview_of_departure(input: Id) -> Result<Option<Record>> {
    db::find("ob_exit_interview").filter("departure", input.id.as_str()).first()
}

fn existing_sources(departure: &str) -> Result<std::collections::BTreeSet<String>> {
    let lines: Vec<Record> = db::find("ob_settlement_line").filter("departure", departure).limit(500).all()?;
    let items: Vec<Record> = db::find("ob_return_item").filter("departure", departure).limit(500).all()?;
    let mut out = std::collections::BTreeSet::new();
    for line in lines {
        if let Some(s) = text(&line, "source").filter(|s| !s.is_empty()) {
            out.insert(s.to_string());
        }
    }
    for item in items {
        if let Some(s) = text(&item, "note").filter(|s| s.starts_with("source:")) {
            out.insert(s.trim_start_matches("source:").to_string());
        }
    }
    Ok(out)
}

fn add_line(departure: &str, kind: &str, label: &str, amount: Decimal, currency: Option<&str>, source: &str) -> Result<Record> {
    let amount = amount.with_scale(2)?;
    if amount.is_zero() || amount.is_negative() {
        return Err(Error::msg("a settlement line is above zero"));
    }
    db::create(
        "ob_settlement_line",
        &json!({
            "departure": departure, "kind": kind, "label": label, "amount": amount, "currency": currency,
            "source": source, "status": "open",
        }),
    )
}

fn add_return(departure: &str, name: &str, source: &str) -> Result<Record> {
    db::create(
        "ob_return_item",
        &json!({ "departure": departure, "name": name, "status": "owed", "note": format!("source:{source}") }),
    )
}

/// Pull loans, assets, encashable leave and gratuity into the departure checklist. Idempotent by source.
fn build(input: Build) -> Result<Value> {
    require_officer()?;
    let departure = departure_live(&input.departure)?;
    let person = text(&departure, "employee").unwrap_or_default().to_string();
    let last_day = text(&departure, "last_day").unwrap_or_default().to_string();
    let currency = input.currency.as_deref();
    let mut have = existing_sources(&input.departure)?;
    let mut added_lines = Vec::new();
    let mut added_items = Vec::new();
    let mut skipped = Vec::new();
    let mut notes = Vec::new();

    // Loans: receivable = cost to close today.
    match plugins::call::<Value>("hr_loan", "leaver_loan_balance", &json!({ "employee": person })) {
        Ok(body) => {
            let loans = body.get("loans").and_then(Value::as_array).cloned().unwrap_or_default();
            for loan in loans {
                let id = loan.get("loan").and_then(Value::as_str).unwrap_or_default();
                let key = source_key("loan", id);
                if have.contains(&key) {
                    skipped.push(key);
                    continue;
                }
                let amount = loan
                    .get("to_close_today")
                    .and_then(|v| serde_json::from_value::<Decimal>(v.clone()).ok())
                    .unwrap_or(Decimal::zero(2));
                if amount.is_zero() || amount.is_negative() {
                    continue;
                }
                let label = format!("Loan payoff ({id})");
                let line = add_line(&input.departure, "receivable", &label, amount, currency, &key)?;
                have.insert(key);
                added_lines.push(line);
            }
        }
        Err(e) => notes.push(format!("loans skipped: {e}")),
    }

    // Assets still held → return items.
    match plugins::call::<Vec<Record>>("hr_assets", "outstanding_assets", &json!({ "employee": person })) {
        Ok(assets) => {
            for asset in assets {
                let id = id_of(&asset)?.to_string();
                let key = source_key("asset", &id);
                if have.contains(&key) {
                    skipped.push(key);
                    continue;
                }
                let name = format!(
                    "{} ({})",
                    text(&asset, "name").unwrap_or("Asset"),
                    text(&asset, "tag").unwrap_or(&id)
                );
                let item = add_return(&input.departure, &name, &key)?;
                have.insert(key);
                added_items.push(item);
            }
        }
        Err(e) => notes.push(format!("assets skipped: {e}")),
    }

    // Encashable leave balances → payable quotes when a daily rate is given.
    let encashable_types: std::collections::BTreeSet<String> = match plugins::call::<Vec<Record>>("hr_leave", "list_leave_types", &json!({})) {
        Ok(types) => types
            .into_iter()
            .filter(|t| t.get("encashable") == Some(&json!(true)))
            .filter_map(|t| text(&t, "id").map(str::to_string))
            .collect(),
        Err(e) => {
            notes.push(format!("leave types skipped: {e}"));
            std::collections::BTreeSet::new()
        }
    };
    match plugins::call::<Vec<Value>>("hr_leave", "leave_balances", &json!({ "employee": person, "on": last_day })) {
        Ok(balances) => {
            let rate = input.leave_daily_rate;
            for row in balances {
                let type_id = row.get("leave_type").and_then(Value::as_str).unwrap_or_default();
                if !encashable_types.is_empty() && !encashable_types.contains(type_id) {
                    continue;
                }
                let available = row
                    .get("available")
                    .and_then(|v| serde_json::from_value::<Decimal>(v.clone()).ok())
                    .unwrap_or(Decimal::zero(2));
                if available.is_zero() || available.is_negative() {
                    continue;
                }
                let key = source_key("leave", type_id);
                if have.contains(&key) {
                    skipped.push(key);
                    continue;
                }
                let name = row.get("name").and_then(Value::as_str).unwrap_or(type_id);
                match rate {
                    Some(daily) => {
                        let amount = available.times(daily.with_scale(2)?, 2)?;
                        if amount.is_zero() {
                            continue;
                        }
                        let label = format!("Leave encashment: {name} ({available} days)");
                        let line = add_line(&input.departure, "payable", &label, amount, currency, &key)?;
                        have.insert(key);
                        added_lines.push(line);
                    }
                    None => notes.push(format!("leave {name}: {available} days available (give leave_daily_rate to quote)")),
                }
            }
        }
        Err(e) => notes.push(format!("leave skipped: {e}")),
    }

    // Gratuity: calculate (not approve) and quote the amount.
    match (&input.gratuity_rule, &input.gratuity_base) {
        (Some(rule), Some(base)) => {
            match plugins::call::<Record>(
                "hr_compensation",
                "calculate_gratuity",
                &json!({ "employee": person, "rule": rule, "base_amount": base, "relieving_date": last_day }),
            ) {
                Ok(g) => {
                    let id = id_of(&g)?.to_string();
                    let key = source_key("gratuity", &id);
                    if have.contains(&key) {
                        skipped.push(key);
                    } else {
                        let amount = g
                            .get("amount")
                            .and_then(|v| serde_json::from_value::<Decimal>(v.clone()).ok())
                            .unwrap_or(Decimal::zero(2));
                        if !amount.is_zero() && !amount.is_negative() {
                            let line = add_line(&input.departure, "payable", "End-of-service gratuity", amount, currency, &key)?;
                            have.insert(key);
                            added_lines.push(line);
                        }
                    }
                }
                Err(e) => notes.push(format!("gratuity skipped: {e}")),
            }
        }
        _ => notes.push("gratuity skipped: give gratuity_rule and gratuity_base".into()),
    }

    Ok(json!({
        "departure": input.departure,
        "lines_added": added_lines,
        "items_added": added_items,
        "skipped_sources": skipped,
        "notes": notes,
    }))
}

handler! {
    /// Schedule the exit interview for a departure (one only).
    fn schedule_exit_interview(input: Schedule) -> Record {
        schedule(input)
    }

    fn complete_exit_interview(input: Complete) -> Record {
        complete(input)
    }

    fn cancel_exit_interview(input: Id) -> Record {
        cancel(input)
    }

    fn get_exit_interview(input: Id) -> Record {
        get_interview(input)
    }

    fn exit_interview_of_departure(input: Id) -> Option<Record> {
        interview_of_departure(input)
    }

    /// Quote final settlement from loans, outstanding assets, encashable leave and gratuity. Safe to run again.
    fn build_final_settlement(input: Build) -> Value {
        build(input)
    }
}
