//! The bank payment file of an approved run: one line per account, a person's net split by the shares of
//! their bank accounts. A person with no account still appears, with a note, so nobody is paid by accident
//! of omission. A CSV every bank portal reads; a bank-specific format (ISO 20022, SEPA) is a later layer.

use std::collections::BTreeMap;

use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{id_of, require, require_finance, text, Record};

#[derive(Deserialize)]
struct Bank {
    #[serde(alias = "id")]
    run: String,
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn cell(value: &str) -> String {
    if value.contains([',', '"', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// The amounts a net is split into, by percent shares; the last takes the remainder so the parts add up exactly.
pub(crate) fn split(net: Decimal, shares: &[Decimal]) -> Result<Vec<Decimal>> {
    if shares.is_empty() {
        return Ok(Vec::new());
    }
    let hundred = Decimal::whole(100, 2)?;
    let mut parts = Vec::new();
    let mut paid = Decimal::zero(2);
    for (i, share) in shares.iter().enumerate() {
        let part = if i + 1 == shares.len() {
            net.checked_sub(paid)?
        } else {
            net.times(*share, 4)?.div(hundred, 2)?.with_scale(2)?
        };
        paid = paid.checked_add(part)?;
        parts.push(part);
    }
    Ok(parts)
}

struct PayRow {
    number: String,
    name: String,
    account_name: String,
    bank: String,
    branch: String,
    account_no: String,
    iban: String,
    swift: String,
    currency: String,
    amount: Decimal,
    note: &'static str,
}

/// One row per bank account to pay (or one for a person without any, with a note).
fn rows_of(run_id: &str) -> Result<(Record, Vec<PayRow>)> {
    require_finance()?;
    let run = require("pay_run", run_id, "run")?;
    if !matches!(text(&run, "status"), Some("approved" | "paid")) {
        return Err(Error::msg("a payment file is made for an approved run"));
    }
    let attempt = run.get("attempt").and_then(Value::as_i64).unwrap_or(0);
    let slips: Vec<Record> = db::find::<Record>("payslip").filter("run", id_of(&run)?).filter("attempt", attempt).order_by("employee").limit(1000).all()?;
    let ids: Vec<String> = slips.iter().filter_map(|s| text(s, "employee").map(str::to_string)).collect();
    let accounts: Vec<Record> = plugins::call("hr_documents", "payment_accounts", &json!({ "employees": ids }))?;
    let mut by_person: BTreeMap<String, Vec<&Record>> = BTreeMap::new();
    for account in &accounts {
        if let Some(who) = text(account, "employee") {
            by_person.entry(who.to_string()).or_default().push(account);
        }
    }
    let number = text(&run, "number").unwrap_or("run").to_string();
    let mut rows = Vec::new();
    for slip in &slips {
        let who = text(slip, "employee").unwrap_or_default();
        let name = text(slip, "employee_name").unwrap_or_default().to_string();
        let net = Decimal::parse(slip.get("net").and_then(Value::as_str).unwrap_or("0"))?.with_scale(2)?;
        let currency = text(slip, "currency").unwrap_or_default().to_string();
        let base = |note: &'static str, amount: Decimal| PayRow {
            number: number.clone(), name: name.clone(), account_name: String::new(), bank: String::new(), branch: String::new(),
            account_no: String::new(), iban: String::new(), swift: String::new(), currency: currency.clone(), amount, note,
        };
        match by_person.get(who) {
            Some(list) if !list.is_empty() => {
                let shares: Vec<Decimal> = list
                    .iter()
                    .map(|a| Decimal::parse(a.get("share_percent").and_then(Value::as_str).unwrap_or("100")).unwrap_or(Decimal::whole(100, 2).unwrap_or(Decimal::zero(2))))
                    .collect();
                let total = shares.iter().try_fold(Decimal::zero(2), |acc, s| acc.checked_add(*s))?;
                // Shares that do not add to 100 are not guessed at: the whole net goes to the first account, flagged.
                let (parts, note) = if total == Decimal::whole(100, 2)? {
                    (split(net, &shares)?, "")
                } else if list.len() == 1 {
                    (vec![net], "")
                } else {
                    (vec![net], "shares do not add to 100: paid to the first account")
                };
                for (a, part) in list.iter().zip(parts.iter()) {
                    let get = |k: &str| text(a, k).unwrap_or_default().to_string();
                    let mut row = base(note, *part);
                    row.account_name = get("account_name");
                    row.bank = get("bank_name");
                    row.branch = get("branch");
                    row.account_no = get("account_no");
                    row.iban = get("iban");
                    row.swift = get("swift");
                    rows.push(row);
                }
            }
            _ => rows.push(base("no bank account on file", net)),
        }
    }
    Ok((run, rows))
}

fn build(input: Bank) -> Result<Value> {
    let (run, rows) = rows_of(&input.run)?;
    let mut csv = String::from("reference,employee,account_name,bank,branch,account_no,iban,swift,currency,amount,note\n");
    for r in &rows {
        csv.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{}\n",
            cell(&r.number), cell(&r.name), cell(&r.account_name), cell(&r.bank), cell(&r.branch), cell(&r.account_no),
            cell(&r.iban), cell(&r.swift), cell(&r.currency), r.amount, r.note
        ));
    }
    let number = text(&run, "number").unwrap_or("run");
    Ok(json!({ "filename": format!("{number}-payments.csv"), "content_type": "text/csv", "data": base64(csv.as_bytes()) }))
}

fn xml_text(raw: &str) -> String {
    raw.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[derive(Deserialize)]
struct XmlIn {
    #[serde(alias = "id")]
    run: String,
    /// The paying company: name, IBAN and BIC of the account the salaries leave from.
    #[serde(default)]
    debtor_name: Option<String>,
    #[serde(default)]
    debtor_iban: Option<String>,
    #[serde(default)]
    debtor_bic: Option<String>,
}

/// A SEPA/ISO 20022 `pain.001.001.03` credit transfer file. Only accounts with an IBAN can be in it; the
/// others are counted in `skipped` (the CSV lists them by name).
fn build_xml(input: XmlIn) -> Result<Value> {
    // The paying account is the payroll setting's unless the call names another.
    let setting: Option<Record> = db::find("pay_setting").first()?;
    let pick = |given: &Option<String>, key: &str| -> String {
        given
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .or_else(|| setting.as_ref().and_then(|s| text(s, key)).map(str::to_string))
            .unwrap_or_default()
    };
    let input = XmlIn {
        run: input.run.clone(),
        debtor_name: Some(pick(&input.debtor_name, "payer_name")),
        debtor_iban: Some(pick(&input.debtor_iban, "payer_iban")),
        debtor_bic: Some(pick(&input.debtor_bic, "payer_bic")),
    };
    let (debtor_name, debtor_iban) = (input.debtor_name.clone().unwrap_or_default(), input.debtor_iban.clone().unwrap_or_default());
    if debtor_name.trim().is_empty() || debtor_iban.trim().is_empty() {
        return Err(Error::msg("set the paying company's name and IBAN in the payroll settings"));
    }
    let (run, rows) = rows_of(&input.run)?;
    let payable: Vec<&PayRow> = rows.iter().filter(|r| !r.iban.trim().is_empty() && r.amount > Decimal::zero(2)).collect();
    let skipped = rows.len() - payable.len();
    if payable.is_empty() {
        return Err(Error::msg("no account in this run has an IBAN: use the CSV file"));
    }
    let number = text(&run, "number").unwrap_or("run").to_string();
    let control = payable.iter().try_fold(Decimal::zero(2), |a, r| a.checked_add(r.amount))?;
    let now = context::current()?.now;
    let day = now.get(..10).unwrap_or("1970-01-01").to_string();
    let mut x = String::new();
    x.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:pain.001.001.03\">\n<CstmrCdtTrfInitn>\n");
    x.push_str(&format!(
        "<GrpHdr><MsgId>{}</MsgId><CreDtTm>{}</CreDtTm><NbOfTxs>{}</NbOfTxs><CtrlSum>{}</CtrlSum><InitgPty><Nm>{}</Nm></InitgPty></GrpHdr>\n",
        xml_text(&number), xml_text(now.get(..19).unwrap_or(&now)), payable.len(), control, xml_text(&debtor_name)
    ));
    x.push_str(&format!(
        "<PmtInf><PmtInfId>{}</PmtInfId><PmtMtd>TRF</PmtMtd><NbOfTxs>{}</NbOfTxs><CtrlSum>{}</CtrlSum><PmtTpInf><SvcLvl><Cd>SEPA</Cd></SvcLvl></PmtTpInf><ReqdExctnDt>{}</ReqdExctnDt><Dbtr><Nm>{}</Nm></Dbtr><DbtrAcct><Id><IBAN>{}</IBAN></Id></DbtrAcct>",
        xml_text(&number), payable.len(), control, day, xml_text(&debtor_name), xml_text(debtor_iban.trim())
    ));
    match input.debtor_bic.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        Some(bic) => x.push_str(&format!("<DbtrAgt><FinInstnId><BIC>{}</BIC></FinInstnId></DbtrAgt>", xml_text(bic))),
        None => x.push_str("<DbtrAgt><FinInstnId><Othr><Id>NOTPROVIDED</Id></Othr></FinInstnId></DbtrAgt>"),
    }
    x.push_str("<ChrgBr>SLEV</ChrgBr>\n");
    for (i, r) in payable.iter().enumerate() {
        x.push_str(&format!(
            "<CdtTrfTxInf><PmtId><EndToEndId>{}-{}</EndToEndId></PmtId><Amt><InstdAmt Ccy=\"{}\">{}</InstdAmt></Amt>",
            xml_text(&number), i + 1, xml_text(&r.currency), r.amount
        ));
        if !r.swift.trim().is_empty() {
            x.push_str(&format!("<CdtrAgt><FinInstnId><BIC>{}</BIC></FinInstnId></CdtrAgt>", xml_text(r.swift.trim())));
        }
        let holder = if r.account_name.trim().is_empty() { &r.name } else { &r.account_name };
        x.push_str(&format!(
            "<Cdtr><Nm>{}</Nm></Cdtr><CdtrAcct><Id><IBAN>{}</IBAN></Id></CdtrAcct><RmtInf><Ustrd>Salary {}</Ustrd></RmtInf></CdtTrfTxInf>\n",
            xml_text(holder), xml_text(r.iban.trim()), xml_text(&number)
        ));
    }
    x.push_str("</PmtInf>\n</CstmrCdtTrfInitn>\n</Document>\n");
    Ok(json!({ "filename": format!("{number}-pain001.xml"), "content_type": "application/xml", "data": base64(x.as_bytes()), "skipped": skipped }))
}

handler! {
    /// The bank payment file (CSV) of an approved or paid run.
    fn export_bank_file(input: Bank) -> Value {
        build(input)
    }

    /// The same payments as an ISO 20022 (pain.001) credit transfer file.
    fn export_bank_file_xml(input: XmlIn) -> Value {
        build_xml(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
    }

    #[test]
    fn a_split_adds_up_to_the_net() -> Result<()> {
        let net = Decimal::parse("1000.01")?;
        let parts = split(net, &[Decimal::parse("33.33")?, Decimal::parse("33.33")?, Decimal::parse("33.34")?])?;
        let total = parts.iter().try_fold(Decimal::zero(2), |a, p| a.checked_add(*p))?;
        assert_eq!(total, net);
        Ok(())
    }
}
