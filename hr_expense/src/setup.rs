//! Categories and policies.

use aether_sdk::dates::{check_order, parse_optional};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{currency_places, pick, require, require_admin, text, Record};
use crate::rules::{Enforcement, Period};

const CATEGORY: &[&str] = &["code", "name", "kind", "unit", "unit_rate", "unit_currency", "receipt_required_over", "requires_note", "accounting_tag", "is_active"];
const POLICY: &[&str] = &["category", "scope_kind", "scope_ref", "period", "limit_amount", "currency", "enforcement", "valid_from", "valid_to"];

#[derive(Deserialize)]
struct Change {
    id: String,
    #[serde(flatten)]
    fields: Record,
}

fn money(data: &Record, field: &str) -> Result<()> {
    if let Some(value) = data.get(field).filter(|v| !v.is_null()) {
        let amount: Decimal = serde_json::from_value(value.clone()).map_err(|e| Error::msg(format!("{field}: {e}")))?;
        if amount.is_negative() {
            return Err(Error::msg(format!("{field} cannot be negative")));
        }
    }
    Ok(())
}

fn check_category(data: &Record) -> Result<()> {
    money(data, "unit_rate")?;
    money(data, "receipt_required_over")?;
    let kind = text(data, "kind").unwrap_or("receipt");
    if matches!(kind, "mileage" | "per_diem") {
        if data.get("unit_rate").is_none_or(Value::is_null) || text(data, "unit").is_none() || text(data, "unit_currency").is_none() {
            return Err(Error::msg("mileage and per diem need a unit, a rate per unit and its currency"));
        }
        currency_places(text(data, "unit_currency").unwrap_or_default())?;
    }
    Ok(())
}

fn new_category(input: Record) -> Result<Record> {
    require_admin()?;
    let data = pick(&input, CATEGORY);
    if text(&data, "code").is_none() || text(&data, "name").is_none() {
        return Err(Error::msg("a category needs a code and a name"));
    }
    check_category(&data)?;
    db::create("expense_category", &data).map_err(|e| e.or("could not create the category (the code may be taken)"))
}

fn change_category(input: Change) -> Result<Record> {
    require_admin()?;
    require("expense_category", &input.id, "category")?;
    let data = pick(&input.fields, CATEGORY);
    if data.as_object().is_some_and(|f| f.is_empty()) {
        return Err(Error::msg("there is nothing to change"));
    }
    check_category(&data)?;
    db::update::<Record>("expense_category", &input.id, &data)?.ok_or_else(|| Error::msg("the category is gone"))
}

fn new_policy(input: Record) -> Result<Record> {
    require_admin()?;
    let data = pick(&input, POLICY);
    require("expense_category", text(&data, "category").ok_or_else(|| Error::msg("a policy is for a category"))?, "category")?;
    if Period::parse(text(&data, "period").unwrap_or("per_item")).is_none() {
        return Err(Error::msg("period is per_item, per_day, per_report or per_month"));
    }
    if Enforcement::parse(text(&data, "enforcement").unwrap_or("warn")).is_none() {
        return Err(Error::msg("enforcement is warn, justify, block or cap"));
    }
    if data.get("limit_amount").is_none_or(Value::is_null) {
        return Err(Error::msg("a policy needs a limit_amount"));
    }
    money(&data, "limit_amount")?;
    currency_places(text(&data, "currency").ok_or_else(|| Error::msg("a policy needs the currency of its limit"))?)?;
    match text(&data, "scope_kind").unwrap_or("all") {
        "all" => {}
        "job" => {
            if text(&data, "scope_ref").is_none() {
                return Err(Error::msg("a job policy names the job"));
            }
        }
        "employee" => {
            if text(&data, "scope_ref").is_none() {
                return Err(Error::msg("an employee policy names the employee"));
            }
        }
        _ => return Err(Error::msg("scope_kind is all, job or employee")),
    }
    if let (Some(from), Some(to)) = (parse_optional(text(&data, "valid_from"))?, parse_optional(text(&data, "valid_to"))?) {
        check_order(from, Some(to), "policy")?;
    }
    db::create("expense_policy", &data)
}

handler! {
    fn create_expense_category(input: Record) -> Record {
        new_category(input)
    }

    fn update_expense_category(input: Change) -> Record {
        change_category(input)
    }

    fn list_expense_categories(_: Empty) -> Vec<Record> {
        db::find("expense_category").order_by("code").limit(500).all()
    }

    fn create_expense_policy(input: Record) -> Record {
        new_policy(input)
    }

    fn list_expense_policies(_: Empty) -> Vec<Record> {
        db::find("expense_policy").limit(1000).all()
    }
}
