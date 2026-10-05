//! Deciding reports and recording what happened to the money.
//!
//! The approver was fixed when the report was sent (the person's first active manager); a person never
//! decides their own report, and a report with no one above the person goes to finance. Approval writes
//! the approved amounts and the advances used in **one transaction**, then announces the report with its
//! lines. Whatever pays it answers with `record_reimbursement`; this plugin posts no accounting entries.

use aether_sdk::dates::{format_date, parse_date};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{
    currency_places, decimal_of, flags_of, id_of, is_finance, my_employee, optional_decimal, require, require_finance, text, today_date, Record,
};
use crate::rules::{advance_state, advance_unclaimed, approvable, check_allocation, check_approved, net_payable, total};

#[derive(Deserialize)]
struct LineDecision {
    line: String,
    approved: Decimal,
}

#[derive(Deserialize)]
struct Use {
    advance: String,
    amount: Decimal,
}

#[derive(Deserialize)]
struct Approve {
    id: String,
    /// Amounts to approve per line; a line not named is approved in full (up to its cap).
    #[serde(default)]
    lines: Vec<LineDecision>,
    #[serde(default)]
    advances: Vec<Use>,
    /// The approver has seen the duplicate and over-limit flags.
    #[serde(default)]
    acknowledge: bool,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct Reject {
    id: String,
    note: String,
}

#[derive(Deserialize)]
struct Reimburse {
    report: String,
    amount: Decimal,
    currency: String,
    reference: String,
    #[serde(default)]
    paid_on: Option<String>,
}

/// Whether the caller may decide this report: finance, or the approver fixed at submission, and never
/// the person.
fn may_decide(me: Option<&Record>, report: &Record) -> Result<bool> {
    if me.is_some_and(|m| text(m, "id") == text(report, "employee")) {
        return Ok(false);
    }
    if is_finance()? {
        return Ok(true);
    }
    Ok(me.is_some_and(|m| text(m, "id").is_some() && text(m, "id") == text(report, "approver")))
}

fn authorise(report: &Record) -> Result<Option<Record>> {
    if text(report, "state") != Some("submitted") {
        return Err(Error::msg("this report is not waiting for a decision"));
    }
    let me = my_employee()?;
    if !may_decide(me.as_ref(), report)? {
        return Err(Error::msg(if text(report, "approver").is_none() {
            "this person has no one above them: a finance approver decides, and never the person themselves"
        } else {
            "only this person's approver (or finance) can decide, and never the person themselves"
        }));
    }
    Ok(me)
}

fn approve(input: Approve) -> Result<Record> {
    let report = require("expense_report", &input.id, "report")?;
    let me = authorise(&report)?;
    let settlement = text(&report, "settlement_currency").unwrap_or("").to_string();
    let places = currency_places(&settlement)?;
    let lines: Vec<Record> = db::find::<Record>("expense_line").filter("report", input.id.as_str()).order_by("date").limit(500).all()?;
    if lines.is_empty() {
        return Err(Error::msg("the report has no lines"));
    }

    // Flags must be seen, and say so.
    let flagged: Vec<String> = lines
        .iter()
        .filter(|l| flags_of(l).iter().any(|f| f == "duplicate" || f == "over_limit"))
        .map(|l| format!("{} ({})", text(l, "description").unwrap_or("line"), flags_of(l).join("/")))
        .collect();
    if !flagged.is_empty() && !input.acknowledge {
        return Err(Error::msg(format!("{} line(s) are flagged: {}. Look at them and approve with acknowledge", flagged.len(), flagged.join(", "))));
    }

    // Amounts: every line approved in full up to its cap unless the approver says otherwise.
    let mut chosen: Vec<(String, Decimal)> = Vec::new();
    for line in &lines {
        let claimed = decimal_of(line, "claimed")?;
        let cap = optional_decimal(line, "cap")?;
        let id = id_of(line)?.to_string();
        let asked = match input.lines.iter().find(|d| d.line == id) {
            Some(decision) => decision.approved.with_scale(places).map_err(|_| Error::msg(format!("amounts have {places} digits after the point")))?,
            None => approvable(claimed, cap).with_scale(places)?,
        };
        chosen.push((id, check_approved(asked, claimed, cap)?));
    }
    if let Some(stray) = input.lines.iter().find(|d| !lines.iter().any(|l| text(l, "id") == Some(d.line.as_str()))) {
        return Err(Error::msg(format!("`{}` is not a line of this report", stray.line)));
    }
    let approved_total = total(&chosen.iter().map(|(_, a)| *a).collect::<Vec<_>>(), places);

    // Advances: the person's own, in the report's currency, up to what is left, and no more than approved.
    let person = text(&report, "employee").unwrap_or_default().to_string();
    let mut uses: Vec<(Record, Decimal, Decimal)> = Vec::new();
    let mut applied = Decimal::zero(places);
    for item in &input.advances {
        let advance = require("expense_advance", &item.advance, "advance")?;
        if text(&advance, "employee") != Some(person.as_str()) {
            return Err(Error::msg("an advance can only be used on its own person's report"));
        }
        if !matches!(text(&advance, "state"), Some("issued" | "partially_settled")) {
            return Err(Error::msg("only an advance that was paid out and is not used up can be used"));
        }
        let left = advance_unclaimed(decimal_of(&advance, "issued_total")?, decimal_of(&advance, "returned_total")?, decimal_of(&advance, "settled_total")?);
        let amount = check_allocation(item.amount.with_scale(places)?, left.with_scale(places)?, text(&advance, "currency").unwrap_or(""), &settlement)?;
        applied = applied + amount;
        uses.push((advance, amount, left));
    }
    if applied > approved_total {
        return Err(Error::msg(format!("advances of {applied} cannot cover more than the {approved_total} approved")));
    }
    let net = net_payable(approved_total, applied);
    let closes = net.is_zero() || net.is_negative();

    // One transaction: every amount, every advance, the report.
    let mut tx = db::transaction();
    for (id, amount) in &chosen {
        tx = tx.update("expense_line", id, &json!({ "approved": amount }));
    }
    for (advance, amount, _) in &uses {
        let settled = decimal_of(advance, "settled_total")? + *amount;
        let state = advance_state(decimal_of(advance, "issued_total")?, settled, decimal_of(advance, "returned_total")?);
        tx = tx
            .create("expense_advance_use", &json!({ "advance": id_of(advance)?, "report": input.id, "amount": amount, "currency": settlement }))
            .update("expense_advance", id_of(advance)?, &json!({ "settled_total": settled, "state": state }));
    }
    let me_id = me.as_ref().and_then(|m| text(m, "id")).map(str::to_string);
    tx = tx.update(
        "expense_report",
        &input.id,
        &json!({
            "state": if closes { "closed" } else { "approved" }, "approved_total": approved_total, "advance_applied_total": applied,
            "net_payable": net, "decided_by": me_id, "decided_on": format_date(today_date()?), "decision_note": input.note,
            "acknowledged_flags": !flagged.is_empty(),
        }),
    );
    tx.run().map_err(|e| e.or("could not approve the report: nothing was changed"))?;

    // Announce it, with what a finance or payroll plugin needs.
    let announced: Vec<Value> = lines
        .iter()
        .zip(&chosen)
        .map(|(line, (_, amount))| {
            json!({
                "line": line["id"], "category": line["category"], "date": line["date"], "payer": line.get("payer"),
                "amount": line["amount"], "currency": line["currency"], "rate": line["rate"], "rate_date": line["rate_date"],
                "approved": amount, "settlement_currency": settlement, "dimensions": null,
            })
        })
        .collect();
    events::emit(
        "report_approved",
        &json!({
            "report": input.id, "number": report["number"], "employee": person, "settlement_currency": settlement,
            "approved_total": approved_total, "advance_applied_total": applied, "net_payable": net, "lines": announced,
        }),
    )?;
    if closes {
        events::emit("report_closed", &json!({ "report": input.id, "employee": person }))?;
    }
    db::get::<Record>("expense_report", &input.id)?.ok_or_else(|| Error::msg("the report is gone"))
}

fn reject(input: Reject) -> Result<Record> {
    let report = require("expense_report", &input.id, "report")?;
    let me = authorise(&report)?;
    if input.note.trim().is_empty() {
        return Err(Error::msg("say why the report is rejected"));
    }
    let lines: Vec<Record> = db::find::<Record>("expense_line").filter("report", input.id.as_str()).limit(500).all()?;
    let zero = Decimal::zero(2);
    let mut tx = db::transaction();
    for line in &lines {
        tx = tx.update("expense_line", id_of(line)?, &json!({ "approved": zero }));
    }
    tx = tx.update(
        "expense_report",
        &input.id,
        &json!({
            "state": "rejected", "decided_by": me.as_ref().and_then(|m| text(m, "id")), "decided_on": format_date(today_date()?),
            "decision_note": input.note,
        }),
    );
    tx.run().map_err(|e| e.or("could not reject the report"))?;
    events::emit("report_rejected", &json!({ "report": input.id, "employee": text(&report, "employee") }))?;
    db::get::<Record>("expense_report", &input.id)?.ok_or_else(|| Error::msg("the report is gone"))
}

/// Money was paid for a report (called by the paying system, or by finance). Each reference is
/// applied once, an over-payment is refused, and a report paid in full is closed.
fn reimburse(input: Reimburse) -> Result<Value> {
    require_finance()?;
    let report = require("expense_report", &input.report, "report")?;
    if let Some(done) = db::find::<Record>("expense_payment").filter("reference", input.reference.as_str()).first()? {
        return Ok(json!({ "payment": done, "repeated": true }));
    }
    if text(&report, "state") != Some("approved") {
        return Err(Error::msg("only an approved report can be reimbursed"));
    }
    let settlement = text(&report, "settlement_currency").unwrap_or("").to_string();
    if !input.currency.eq_ignore_ascii_case(&settlement) {
        return Err(Error::msg(format!("the report is settled in {settlement}, not {}", input.currency)));
    }
    let places = currency_places(&settlement)?;
    let amount = input.amount.with_scale(places).map_err(|_| Error::msg(format!("{settlement} has {places} digits after the point")))?;
    if amount.is_negative() || amount.is_zero() {
        return Err(Error::msg("a payment is more than zero"));
    }
    let owed = decimal_of(&report, "net_payable")?;
    let paid = decimal_of(&report, "reimbursed_total")?;
    let left = owed - paid;
    if amount > left {
        return Err(Error::msg(format!("only {left} is left to pay on this report")));
    }
    let now_paid = paid + amount;
    let closed = now_paid >= owed;
    let paid_on = match &input.paid_on {
        Some(day) => format_date(parse_date(day)?),
        None => format_date(today_date()?),
    };
    let results = db::transaction()
        .create(
            "expense_payment",
            &json!({ "kind": "reimbursement", "report": input.report, "reference": input.reference, "amount": amount, "currency": settlement, "paid_on": paid_on }),
        )
        .update("expense_report", &input.report, &json!({ "reimbursed_total": now_paid, "state": if closed { "closed" } else { "approved" } }))
        .run()
        .map_err(|e| e.or("could not record the payment"))?;
    if closed {
        events::emit("report_closed", &json!({ "report": input.report, "employee": text(&report, "employee") }))?;
    }
    Ok(json!({ "payment": results.first(), "repeated": false, "closed": closed }))
}

/// Reports waiting for the caller: those they were fixed as approver of, and (for finance) any that
/// have no one above the person.
fn waiting() -> Result<Vec<Record>> {
    let me = my_employee()?;
    let mut found: Vec<Record> = Vec::new();
    if let Some(me) = &me {
        found = db::find::<Record>("expense_report")
            .filter("state", "submitted")
            .filter("approver", id_of(me)?)
            .order_by("submitted_on")
            .limit(500)
            .all()?;
    }
    if is_finance()? {
        for report in db::find::<Record>("expense_report").filter("state", "submitted").order_by("submitted_on").limit(500).all()? {
            if text(&report, "approver").is_none() && !found.iter().any(|f| text(f, "id") == text(&report, "id")) {
                found.push(report);
            }
        }
    }
    if let Some(me) = &me {
        found.retain(|r| text(r, "employee") != text(me, "id"));
    }
    Ok(found)
}

handler! {
    /// Approve a report: amounts per line (default: in full up to the cap) and advances to use.
    fn approve_report(input: Approve) -> Record {
        approve(input)
    }

    fn reject_report(input: Reject) -> Record {
        reject(input)
    }

    /// Money was paid for a report (finance). Idempotent by reference.
    fn record_reimbursement(input: Reimburse) -> Value {
        reimburse(input)
    }

    fn reports_waiting_for_me(_: Empty) -> Vec<Record> {
        waiting()
    }
}
