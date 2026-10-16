//! Payments: reimbursing an approved claim to the employee (a payroll adjustment), remitting premiums to the insurer,
//! and refunds. Confirming is idempotent: the same claim or premium pays once.

use aether_sdk::dates::format_date;
use aether_sdk::prelude::*;

use crate::common::*;

const REIMBURSE_CODE: &str = "insurance_reimbursement";

#[derive(Deserialize)]
struct PayClaim {
    #[serde(alias = "id")]
    claim: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    external_ref: Option<String>,
}

#[derive(Deserialize)]
struct Remit {
    #[serde(alias = "id")]
    premium: String,
    #[serde(default)]
    external_ref: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

fn existing(claim: Option<&str>, premium: Option<&str>) -> Result<Option<Record>> {
    let mut q = db::find::<Record>("ins_payment");
    if let Some(c) = claim {
        q = q.filter("claim", c);
    }
    if let Some(p) = premium {
        q = q.filter("premium", p);
    }
    let rows = q.limit(20).all()?;
    Ok(rows.into_iter().find(|r| text(r, "state") != Some("cancelled") && text(r, "state") != Some("failed")))
}

fn pay_claim(input: PayClaim) -> Result<Record> {
    require_staff()?;
    let claim = require("ins_claim", &input.claim, "claim")?;
    if let Some(done) = existing(Some(&input.claim), None)? {
        return Ok(done);
    }
    if text(&claim, "state") != Some("approved") {
        return Err(Error::msg("only an approved claim is paid"));
    }
    if my_employee_id()?.as_deref() == text(&claim, "employee") {
        return Err(Error::msg("nobody pays their own claim"));
    }
    let currency = text(&claim, "currency").unwrap_or_default().to_string();
    let places = currency_places(&currency)?;
    let amount = decimal_of(&claim, "approved_amount", places)?;
    let who = text(&claim, "employee").unwrap_or_default().to_string();
    let today = format_date(today_date()?);
    let payment: Record = db::create(
        "ins_payment",
        &json!({
            "reference": next_number("payment", "PAY-", 6)?, "kind": "claim_reimbursement", "claim": input.claim, "employee": who,
            "amount": amount, "currency": currency, "state": "confirmed", "method": input.method.unwrap_or_else(|| "payroll".into()),
            "external_ref": input.external_ref, "confirmed_on": today,
        }),
    )?;
    if !amount.is_zero() {
        let _: Record = plugins::call("hr_compensation", "ensure_pay_code", &json!({ "code": REIMBURSE_CODE, "kind": "earning" }))?;
        let adj: Record = plugins::call(
            "hr_compensation",
            "add_adjustment",
            &json!({
                "employee": who, "code": REIMBURSE_CODE, "amount": amount, "mode": "one_off", "pay_date": today,
                "source_plugin": "hr_insurance", "source_ref": input.claim, "reason": format!("Insurance claim {}", text(&claim, "reference").unwrap_or("?")),
            }),
        )?;
        db::update::<Record>("ins_claim", &input.claim, &json!({ "adjustment": adj.get("id") }))?;
    }
    db::update::<Record>("ins_claim", &input.claim, &json!({ "state": "paid" }))?;
    events::emit("claim_paid", &json!({ "claim": input.claim, "payment": id_of(&payment)?, "employee": who, "amount": amount, "currency": currency, "date": today }))?;
    Ok(payment)
}

fn remit(input: Remit) -> Result<Record> {
    require_staff()?;
    let premium = require("ins_premium", &input.premium, "premium")?;
    if let Some(done) = existing(None, Some(&input.premium))? {
        return Ok(done);
    }
    if text(&premium, "state") != Some("deducted") {
        return Err(Error::msg("a premium is remitted after it has been deducted"));
    }
    let currency = text(&premium, "currency").unwrap_or_default().to_string();
    let places = currency_places(&currency)?;
    let amount = decimal_of(&premium, "total", places)?;
    let today = format_date(today_date()?);
    let payment: Record = db::create(
        "ins_payment",
        &json!({
            "reference": next_number("payment", "PAY-", 6)?, "kind": "premium_remittance", "premium": input.premium, "amount": amount,
            "currency": currency, "state": "confirmed", "external_ref": input.external_ref, "confirmed_on": today,
        }),
    )?;
    db::update::<Record>("ins_premium", &input.premium, &json!({ "state": "remitted" }))?;
    events::emit("premium_paid", &json!({
            "premium": input.premium, "payment": id_of(&payment)?, "amount": amount, "currency": currency, "date": today,
            "employee_share": premium["employee_share"], "employer_share": premium["employer_share"],
        }),
    )?;
    Ok(payment)
}

fn cancel(input: Id) -> Result<Record> {
    require_admin()?;
    let payment = require("ins_payment", &input.id, "payment")?;
    if text(&payment, "state") != Some("confirmed") {
        return Err(Error::msg("only a confirmed payment is cancelled"));
    }
    if let Some(claim) = text(&payment, "claim") {
        if let Some(adj) = require("ins_claim", claim, "claim")?.get("adjustment").and_then(Value::as_str) {
            let _: Record = plugins::call("hr_compensation", "cancel_adjustment", &json!({ "id": adj }))?;
        }
        db::update::<Record>("ins_claim", claim, &json!({ "state": "approved", "adjustment": null }))?;
    }
    if let Some(p) = text(&payment, "premium") {
        db::update::<Record>("ins_premium", p, &json!({ "state": "deducted" }))?;
    }
    let out = db::update("ins_payment", &input.id, &json!({ "state": "cancelled" }))?.ok_or_else(|| Error::msg("the payment is gone"))?;
    events::emit(
        "claim_refunded",
        &json!({ "payment": input.id, "amount": payment["amount"], "currency": payment["currency"], "employee": payment.get("employee"), "date": format_date(today_date()?) }),
    )?;
    Ok(out)
}

handler! {
    fn pay_insurance_claim(input: PayClaim) -> Record {
        pay_claim(input)
    }

    fn remit_insurance_premium(input: Remit) -> Record {
        remit(input)
    }

    fn cancel_insurance_payment(input: Id) -> Record {
        cancel(input)
    }

    fn list_insurance_payments(_: Empty) -> Vec<Record> {
        require_staff()?;
        db::find("ins_payment").limit(5000).all()
    }
}
