//! Identity documents, emergency contacts and bank accounts with a salary split.
//!
//! * The person (or a documents administrator) maintains their own lists. A primary identity document and a primary
//!   emergency contact are unique per person.
//! * Bank accounts that are active must have shares adding to exactly 100; inactive ones are left out of the sum.
//! * Nightly, identity documents that hit 30, 7 or 0 days to expiry are reported once each.

use aether_sdk::dates::{format_date, parse_date};
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{employee, id_of, is_admin, my_employee, require, require_admin, text, today_date, Record};
use crate::rules::{contacts_ok, document_ok, expiry_of, split_ok, warn_today, DocKind, Expiry};

#[derive(Deserialize)]
struct IdentityIn {
    #[serde(default)]
    employee: Option<String>,
    kind: String,
    number: String,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    issued_on: Option<String>,
    #[serde(default)]
    expires_on: Option<String>,
    #[serde(default)]
    is_primary: bool,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Deserialize)]
struct ContactIn {
    #[serde(default)]
    employee: Option<String>,
    name: String,
    relation: String,
    phone: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    is_primary: bool,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Deserialize)]
struct BankIn {
    #[serde(default)]
    employee: Option<String>,
    account_name: String,
    account_no: String,
    bank_name: String,
    share_percent: Decimal,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    iban: Option<String>,
    #[serde(default)]
    swift: Option<String>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default = "yes")]
    is_active: bool,
    #[serde(default)]
    notes: Option<String>,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Payees {
    employees: Vec<String>,
}

#[derive(Deserialize)]
struct Who {
    #[serde(default)]
    employee: Option<String>,
}

#[derive(Deserialize)]
struct ChangeBank {
    id: String,
    #[serde(default)]
    share_percent: Option<Decimal>,
    #[serde(default)]
    is_active: Option<bool>,
    #[serde(default)]
    account_name: Option<String>,
    #[serde(default)]
    bank_name: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

fn me_id() -> Result<Option<String>> {
    Ok(my_employee()?.and_then(|m| text(&m, "id").map(str::to_string)))
}

fn target_employee(named: Option<String>) -> Result<String> {
    let me = me_id()?;
    // A page hands over the id it was opened with, often just the key part; compare and store
    // the full record id (`table:key`), which is what a query and a link field need.
    let key = |id: &str| id.rsplit(':').next().unwrap_or(id).to_string();
    match named.filter(|name| !name.trim().is_empty()) {
        Some(other) => {
            if let Some(mine) = me.as_ref().filter(|mine| key(mine) == key(&other)) {
                return Ok(mine.clone());
            }
            require_admin()?;
            let found = employee(&other)?;
            Ok(id_of(&found)?.to_string())
        }
        None => me.ok_or_else(|| Error::msg("you have no employee record: name the employee")),
    }
}

fn require_owner_or_admin(employee_id: &str) -> Result<()> {
    if me_id()?.as_deref() == Some(employee_id) || is_admin()? {
        Ok(())
    } else {
        Err(Error::msg("only the person (or a documents administrator) can change this"))
    }
}

fn clear_primary_identity(employee: &str, except: Option<&str>) -> Result<()> {
    let rows: Vec<Record> = db::find("doc_identity").filter("employee", employee).limit(100).all()?;
    for row in rows {
        if except == text(&row, "id") {
            continue;
        }
        if row.get("is_primary") == Some(&json!(true)) {
            db::update::<Record>("doc_identity", id_of(&row)?, &json!({ "is_primary": false }))?;
        }
    }
    Ok(())
}

fn clear_primary_contact(employee: &str, except: Option<&str>) -> Result<()> {
    let rows: Vec<Record> = db::find("doc_emergency").filter("employee", employee).limit(100).all()?;
    for row in rows {
        if except == text(&row, "id") {
            continue;
        }
        if row.get("is_primary") == Some(&json!(true)) {
            db::update::<Record>("doc_emergency", id_of(&row)?, &json!({ "is_primary": false }))?;
        }
    }
    Ok(())
}

fn active_shares(employee: &str, except: Option<&str>, override_id: Option<&str>, override_share: Option<Decimal>, override_active: Option<bool>) -> Result<Vec<Decimal>> {
    let rows: Vec<Record> = db::find("doc_bank").filter("employee", employee).limit(50).all()?;
    let mut shares = Vec::new();
    for row in rows {
        let id = id_of(&row)?;
        if except == Some(id) {
            continue;
        }
        let active = if override_id == Some(id) {
            override_active.unwrap_or(row.get("is_active") != Some(&json!(false)))
        } else {
            row.get("is_active") != Some(&json!(false))
        };
        if !active {
            continue;
        }
        let share = if override_id == Some(id) {
            override_share.unwrap_or_else(|| {
                row.get("share_percent")
                    .and_then(|v| serde_json::from_value::<Decimal>(v.clone()).ok())
                    .unwrap_or(Decimal::zero(2))
            })
        } else {
            row.get("share_percent")
                .and_then(|v| serde_json::from_value::<Decimal>(v.clone()).ok())
                .unwrap_or(Decimal::zero(2))
        };
        shares.push(share.with_scale(2)?);
    }
    Ok(shares)
}

fn add_identity(input: IdentityIn) -> Result<Record> {
    let who = target_employee(input.employee)?;
    require_owner_or_admin(&who)?;
    let kind = DocKind::parse(&input.kind).ok_or_else(|| Error::msg("kind is passport, national_id, visa, work_permit, driving_licence or other"))?;
    let expires = input.expires_on.as_deref().map(parse_date).transpose()?;
    document_ok(kind, &input.number, expires)?;
    if input.is_primary {
        clear_primary_identity(&who, None)?;
    }
    db::create(
        "doc_identity",
        &json!({
            "employee": who, "kind": input.kind, "number": input.number.trim(), "country": input.country,
            "issued_on": input.issued_on.map(|d| parse_date(&d).map(format_date)).transpose()?,
            "expires_on": expires.map(format_date), "is_primary": input.is_primary, "notes": input.notes,
        }),
    )
}

fn remove_identity(input: Id) -> Result<Value> {
    let row = require("doc_identity", &input.id, "document")?;
    require_owner_or_admin(text(&row, "employee").unwrap_or_default())?;
    db::delete::<Record>("doc_identity", &input.id)?;
    Ok(json!({ "removed": input.id }))
}

fn add_contact(input: ContactIn) -> Result<Record> {
    let who = target_employee(input.employee)?;
    require_owner_or_admin(&who)?;
    if input.name.trim().is_empty() || input.phone.trim().is_empty() || input.relation.trim().is_empty() {
        return Err(Error::msg("name, relation and phone are required"));
    }
    if input.is_primary {
        clear_primary_contact(&who, None)?;
    } else {
        let existing: Vec<Record> = db::find("doc_emergency").filter("employee", who.as_str()).limit(50).all()?;
        let primaries = existing.iter().filter(|r| r.get("is_primary") == Some(&json!(true))).count();
        contacts_ok(primaries)?;
    }
    db::create(
        "doc_emergency",
        &json!({
            "employee": who, "name": input.name.trim(), "relation": input.relation.trim(), "phone": input.phone.trim(),
            "email": input.email, "is_primary": input.is_primary, "notes": input.notes,
        }),
    )
}

fn remove_contact(input: Id) -> Result<Value> {
    let row = require("doc_emergency", &input.id, "contact")?;
    require_owner_or_admin(text(&row, "employee").unwrap_or_default())?;
    db::delete::<Record>("doc_emergency", &input.id)?;
    Ok(json!({ "removed": input.id }))
}

fn add_bank(input: BankIn) -> Result<Record> {
    let who = target_employee(input.employee)?;
    require_owner_or_admin(&who)?;
    if input.account_name.trim().is_empty() || input.account_no.trim().is_empty() || input.bank_name.trim().is_empty() {
        return Err(Error::msg("account name, number and bank are required"));
    }
    let share = input.share_percent.with_scale(2)?;
    let mut shares = active_shares(&who, None, None, None, None)?;
    if input.is_active {
        shares.push(share);
        split_ok(&shares)?;
    } else if share.is_negative() || share.is_zero() || share > Decimal::parse("100.00")? {
        return Err(Error::msg("each share is more than 0 and at most 100"));
    }
    db::create(
        "doc_bank",
        &json!({
            "employee": who, "account_name": input.account_name.trim(), "account_no": input.account_no.trim(),
            "bank_name": input.bank_name.trim(), "branch": input.branch, "iban": input.iban, "swift": input.swift,
            "currency": input.currency.map(|c| c.to_uppercase()), "share_percent": share, "is_active": input.is_active,
            "notes": input.notes,
        }),
    )
}

fn change_bank(input: ChangeBank) -> Result<Record> {
    let row = require("doc_bank", &input.id, "bank account")?;
    let who = text(&row, "employee").unwrap_or_default();
    require_owner_or_admin(who)?;
    // A change may leave the split short of 100 so another account can be added or adjusted next;
    // `salary_split` reports whether it is complete. Only the bounds of this share are checked here.
    if let Some(share) = input.share_percent {
        let share = share.with_scale(2)?;
        if share.is_negative() || share.is_zero() || share > Decimal::parse("100.00")? {
            return Err(Error::msg("each share is more than 0 and at most 100"));
        }
    }
    let mut data = serde_json::Map::new();
    if let Some(s) = input.share_percent {
        data.insert("share_percent".into(), json!(s.with_scale(2)?));
    }
    if let Some(a) = input.is_active {
        data.insert("is_active".into(), json!(a));
    }
    if let Some(v) = input.account_name {
        data.insert("account_name".into(), json!(v));
    }
    if let Some(v) = input.bank_name {
        data.insert("bank_name".into(), json!(v));
    }
    if let Some(v) = input.branch {
        data.insert("branch".into(), json!(v));
    }
    if let Some(v) = input.notes {
        data.insert("notes".into(), json!(v));
    }
    if data.is_empty() {
        return Err(Error::msg("nothing to change"));
    }
    // Turning an account on (or changing its share while on) must leave a valid split when it would
    // be the only move that completes the pot: if every active share including this one adds to 100, fine;
    // if the result is over 100, refuse.
    let share = input.share_percent.map(|s| s.with_scale(2)).transpose()?;
    let shares = active_shares(who, None, Some(&input.id), share, input.is_active)?;
    if !shares.is_empty() {
        let total = shares.iter().fold(Decimal::zero(2), |a, b| a + *b);
        if total > Decimal::parse("100.00")? {
            return Err(Error::msg(format!("the shares would add up to {total}, which is over 100")));
        }
    }
    db::update("doc_bank", &input.id, &Value::Object(data))?.ok_or_else(|| Error::msg("the bank account is gone"))
}

fn remove_bank(input: Id) -> Result<Value> {
    let row = require("doc_bank", &input.id, "bank account")?;
    let who = text(&row, "employee").unwrap_or_default();
    require_owner_or_admin(who)?;
    // Removing may leave the split incomplete; salary_split says so.
    db::delete::<Record>("doc_bank", &input.id)?;
    Ok(json!({ "removed": input.id }))
}

fn list_for(model: &str, input: Who) -> Result<Vec<Record>> {
    // A caller who is not an employee has nothing of their own: an empty list, not an error
    // (the panels sit on "My profile", which anyone may open).
    if input.employee.as_deref().is_none_or(|named| named.trim().is_empty()) && me_id()?.is_none() {
        return Ok(Vec::new());
    }
    let who = target_employee(input.employee)?;
    if me_id()?.as_deref() != Some(who.as_str()) && !is_admin()? {
        return Err(Error::msg("you can see your own records, or anyone's if you run documents"));
    }
    db::find(model).filter("employee", who.as_str()).limit(200).all()
}

fn expiring() -> Result<Vec<Record>> {
    require_admin()?;
    let today = today_date()?;
    let rows: Vec<Record> = db::find::<Record>("doc_identity").limit(10_000).all()?;
    let mut out = Vec::new();
    for row in rows {
        let expires = text(&row, "expires_on").map(parse_date).transpose()?;
        if matches!(expiry_of(today, expires, 30), Expiry::Expired | Expiry::DueSoon) {
            out.push(row);
        }
    }
    Ok(out)
}

fn split_of(input: Who) -> Result<Value> {
    let who = target_employee(input.employee)?;
    if me_id()?.as_deref() != Some(who.as_str()) && !is_admin()? {
        return Err(Error::msg("you can see your own split, or anyone's if you run documents"));
    }
    let rows: Vec<Record> = db::find("doc_bank").filter("employee", who.as_str()).limit(50).all()?;
    let active: Vec<&Record> = rows.iter().filter(|r| r.get("is_active") != Some(&json!(false))).collect();
    let shares: Result<Vec<Decimal>> = active
        .iter()
        .map(|r| {
            r.get("share_percent")
                .and_then(|v| serde_json::from_value::<Decimal>(v.clone()).ok())
                .ok_or_else(|| Error::msg("a bank account has no share"))
                .and_then(|s| s.with_scale(2))
        })
        .collect();
    let shares = shares?;
    let ok = split_ok(&shares).is_ok();
    Ok(json!({ "employee": who, "accounts": active, "complete": ok, "total": shares.iter().fold(Decimal::zero(2), |a, b| a + *b) }))
}

fn nightly() -> Result<Value> {
    let today = today_date()?;
    let rows: Vec<Record> = db::find::<Record>("doc_identity").limit(20_000).all()?;
    let mut warned = 0;
    for row in rows {
        let Some(expires) = text(&row, "expires_on").map(parse_date).transpose()? else { continue };
        let Some(days) = warn_today(today, expires) else { continue };
        let already = row.get("last_warn_days").and_then(Value::as_i64);
        if already == Some(days) {
            continue;
        }
        db::update::<Record>("doc_identity", id_of(&row)?, &json!({ "last_warn_days": days }))?;
        events::emit(
            "document_expiring",
            &json!({
                "document": row["id"], "employee": row["employee"], "kind": row["kind"], "expires_on": row["expires_on"],
                "days_left": days,
            }),
        )?;
        warned += 1;
    }
    Ok(json!({ "warned": warned }))
}

handler! {
    fn add_identity_document(input: IdentityIn) -> Record {
        add_identity(input)
    }

    fn remove_identity_document(input: Id) -> Value {
        remove_identity(input)
    }

    fn my_identity_documents(_: Empty) -> Vec<Record> {
        list_for("doc_identity", Who { employee: None })
    }

    fn list_identity_documents(input: Who) -> Vec<Record> {
        list_for("doc_identity", input)
    }

    fn add_emergency_contact(input: ContactIn) -> Record {
        add_contact(input)
    }

    fn remove_emergency_contact(input: Id) -> Value {
        remove_contact(input)
    }

    fn my_emergency_contacts(_: Empty) -> Vec<Record> {
        list_for("doc_emergency", Who { employee: None })
    }

    fn list_emergency_contacts(input: Who) -> Vec<Record> {
        list_for("doc_emergency", input)
    }

    fn add_bank_account(input: BankIn) -> Record {
        add_bank(input)
    }

    fn update_bank_account(input: ChangeBank) -> Record {
        change_bank(input)
    }

    fn remove_bank_account(input: Id) -> Value {
        remove_bank(input)
    }

    fn my_bank_accounts(_: Empty) -> Vec<Record> {
        list_for("doc_bank", Who { employee: None })
    }

    /// Active bank accounts of many people at once, for paying a payroll run. Documents administrators and
    /// payroll finance or admin only. At most 1000 people.
    fn payment_accounts(input: Payees) -> Vec<Record> {
        let who = context::current()?;
        if !(is_admin()? || who.has_role("hr_payroll.payroll_finance") || who.has_role("hr_payroll.payroll_admin")) {
            return Err(Error::msg("bank details are for documents administrators and payroll finance"));
        }
        if input.employees.len() > 1000 {
            return Err(Error::msg("at most 1000 people per call"));
        }
        let mut out = Vec::new();
        for chunk in input.employees.chunks(100) {
            let rows: Vec<Record> = db::find::<Record>("doc_bank")
                .matching(aether_sdk::db::Filter::one_of("employee", chunk.to_vec()).and(aether_sdk::db::Filter::eq("is_active", true)))
                .limit(500)
                .all()?;
            out.extend(rows);
        }
        Ok(out)
    }

    fn list_bank_accounts(input: Who) -> Vec<Record> {
        list_for("doc_bank", input)
    }

    /// Active bank accounts and whether their shares add up to 100.
    fn salary_split(input: Who) -> Value {
        split_of(input)
    }

    /// Identity documents expired or due within 30 days (documents administrators).
    fn expiring_documents(_: Empty) -> Vec<Record> {
        expiring()
    }

    fn documents_nightly(_: Empty) -> Value {
        nightly()
    }
}
