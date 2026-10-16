//! Insurers, plans and providers: the masters everything else points at.

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{currency_places, require, require_staff, Record};

#[derive(Deserialize)]
struct NewInsurer {
    code: String,
    name: String,
    #[serde(default)]
    contact_email: Option<String>,
    #[serde(default)]
    contact_phone: Option<String>,
}

#[derive(Deserialize)]
struct NewPlan {
    code: String,
    name: String,
    insurer: String,
    #[serde(default = "standard")]
    coverage_type: String,
    currency: String,
    member_limit: Decimal,
    #[serde(default)]
    deductible: Option<Decimal>,
    #[serde(default)]
    copay_percent: Option<Decimal>,
    #[serde(default)]
    network_only: bool,
    #[serde(default)]
    preauth_threshold: Option<Decimal>,
    premium_amount: Decimal,
    #[serde(default = "monthly")]
    premium_period: String,
    #[serde(default)]
    employer_share_percent: Option<Decimal>,
    #[serde(default = "yes")]
    allow_spouse: bool,
    #[serde(default = "yes")]
    allow_children: bool,
    #[serde(default)]
    max_children: Option<i64>,
    #[serde(default)]
    child_max_age: Option<i64>,
    #[serde(default)]
    claim_window_days: Option<i64>,
}

#[derive(Deserialize)]
struct NewProvider {
    name: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    phone: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    is_network: bool,
}

fn standard() -> String {
    "standard".into()
}
fn monthly() -> String {
    "monthly".into()
}
fn yes() -> bool {
    true
}

fn percent(label: &str, value: Option<Decimal>) -> Result<Decimal> {
    let value = value.unwrap_or(Decimal::zero(2));
    if value.is_negative() || value > Decimal::whole(100, 0)? {
        return Err(Error::msg(format!("{label} is between 0 and 100 percent")));
    }
    Ok(value.with_scale(2)?)
}

fn insurer(input: NewInsurer) -> Result<Record> {
    require_staff()?;
    if input.name.trim().is_empty() {
        return Err(Error::msg("an insurer has a name"));
    }
    db::create(
        "ins_insurer",
        &json!({ "code": input.code.trim().to_uppercase(), "name": input.name, "contact_email": input.contact_email, "contact_phone": input.contact_phone, "is_active": true }),
    )
    .map_err(|e| e.or("could not create the insurer (the code may be taken)"))
}

fn plan(input: NewPlan) -> Result<Record> {
    require_staff()?;
    let insurer = require("ins_insurer", &input.insurer, "insurer")?;
    if insurer.get("is_active") == Some(&json!(false)) {
        return Err(Error::msg("this insurer is no longer used"));
    }
    let currency = input.currency.to_uppercase();
    let places = currency_places(&currency)?;
    if input.member_limit.is_negative() || input.member_limit.is_zero() {
        return Err(Error::msg("a member limit is more than zero"));
    }
    if input.premium_amount.is_negative() {
        return Err(Error::msg("a premium is not negative"));
    }
    if !matches!(input.premium_period.as_str(), "monthly" | "annual") {
        return Err(Error::msg("a premium is priced monthly or annual"));
    }
    let deductible = input.deductible.unwrap_or(Decimal::zero(places));
    if deductible.is_negative() {
        return Err(Error::msg("a deductible is not negative"));
    }
    db::create(
        "ins_plan",
        &json!({
            "code": input.code.trim().to_uppercase(), "name": input.name, "insurer": input.insurer, "coverage_type": input.coverage_type,
            "currency": currency, "member_limit": input.member_limit.with_scale(places)?, "deductible": deductible.with_scale(places)?,
            "copay_percent": percent("a copay", input.copay_percent)?, "network_only": input.network_only,
            "preauth_threshold": input.preauth_threshold.map(|d| d.with_scale(places)).transpose()?,
            "premium_amount": input.premium_amount.with_scale(places)?, "premium_period": input.premium_period,
            "employer_share_percent": percent("the employer share", input.employer_share_percent.or(Some(Decimal::whole(100, 2)?)))?,
            "allow_spouse": input.allow_spouse, "allow_children": input.allow_children, "max_children": input.max_children,
            "child_max_age": input.child_max_age.unwrap_or(25), "claim_window_days": input.claim_window_days.unwrap_or(90), "is_active": true,
        }),
    )
    .map_err(|e| e.or("could not create the plan (the code may be taken)"))
}

fn provider(input: NewProvider) -> Result<Record> {
    require_staff()?;
    if input.name.trim().is_empty() {
        return Err(Error::msg("a provider has a name"));
    }
    db::create(
        "ins_provider",
        &json!({
            "name": input.name, "kind": input.kind.unwrap_or_else(|| "clinic".into()), "country": input.country, "city": input.city,
            "phone": input.phone, "email": input.email, "is_network": input.is_network, "is_active": true,
        }),
    )
}

handler! {
    fn create_insurer(input: NewInsurer) -> Record {
        insurer(input)
    }

    fn list_insurers(_: Empty) -> Vec<Record> {
        db::find("ins_insurer").order_by("name").limit(500).all()
    }

    fn create_insurance_plan(input: NewPlan) -> Record {
        plan(input)
    }

    fn list_insurance_plans(_: Empty) -> Vec<Record> {
        db::find("ins_plan").order_by("name").limit(500).all()
    }

    fn create_insurance_provider(input: NewProvider) -> Record {
        provider(input)
    }

    fn list_insurance_providers(_: Empty) -> Vec<Record> {
        db::find("ins_provider").order_by("name").limit(2000).all()
    }
}
