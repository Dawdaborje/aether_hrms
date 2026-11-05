//! Organisation banks and branches: picked on employee bank accounts, maintained in HR config.

use aether_sdk::prelude::*;

use crate::common::{id_of, is_admin, require, text, Record};

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Named {
    name: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    swift: Option<String>,
    #[serde(default = "yes")]
    is_active: bool,
}

#[derive(Deserialize)]
struct BankChange {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    swift: Option<String>,
    #[serde(default)]
    is_active: Option<bool>,
}

#[derive(Deserialize)]
struct BranchIn {
    bank: String,
    name: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default = "yes")]
    is_active: bool,
}

#[derive(Deserialize)]
struct BranchChange {
    id: String,
    #[serde(default)]
    bank: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    is_active: Option<bool>,
}

#[derive(Deserialize)]
struct BranchList {
    /// When set, only branches of this bank (the account form passes the selected bank).
    #[serde(default)]
    bank: Option<String>,
    /// When true, include inactive rows (configuration lists).
    #[serde(default)]
    all: bool,
}

fn yes() -> bool {
    true
}

fn key_of(id: &str) -> &str {
    id.rsplit(':').next().unwrap_or(id)
}

fn same_record(a: &str, b: &str) -> bool {
    !a.is_empty() && (a == b || key_of(a) == key_of(b))
}

fn require_bank_admin() -> Result<()> {
    let who = context::current()?;
    if is_admin()? || who.has_role("hr.hr_manager") {
        Ok(())
    } else {
        Err(Error::msg("only a documents administrator or HR manager can change banks"))
    }
}

fn active(row: &Record) -> bool {
    row.get("is_active") != Some(&json!(false))
}

fn new_bank(input: Named) -> Result<Record> {
    require_bank_admin()?;
    let name = input.name.trim();
    if name.is_empty() {
        return Err(Error::msg("name is required"));
    }
    db::create(
        "doc_bank_org",
        &json!({
            "name": name,
            "code": input.code.as_deref().map(str::trim).filter(|s| !s.is_empty()),
            "swift": input.swift.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| s.to_uppercase()),
            "is_active": input.is_active,
        }),
    )
    .map_err(|e| e.or("could not create the bank (the name may be taken)"))
}

fn edit_bank(input: BankChange) -> Result<Record> {
    require_bank_admin()?;
    let _ = require("doc_bank_org", &input.id, "bank")?;
    let mut data = serde_json::Map::new();
    if let Some(name) = input.name {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::msg("name is required"));
        }
        data.insert("name".into(), json!(name));
    }
    if let Some(code) = input.code {
        data.insert("code".into(), json!(code.trim()));
    }
    if let Some(swift) = input.swift {
        data.insert("swift".into(), json!(swift.trim().to_uppercase()));
    }
    if let Some(active) = input.is_active {
        data.insert("is_active".into(), json!(active));
    }
    if data.is_empty() {
        return Err(Error::msg("nothing to change"));
    }
    db::update("doc_bank_org", &input.id, &Value::Object(data))?.ok_or_else(|| Error::msg("the bank is gone"))
}

fn banks_list(all: bool) -> Result<Vec<Record>> {
    let rows: Vec<Record> = db::find("doc_bank_org").order_by("name").limit(500).all()?;
    Ok(if all {
        rows
    } else {
        rows.into_iter().filter(active).collect()
    })
}

fn new_branch(input: BranchIn) -> Result<Record> {
    require_bank_admin()?;
    let bank = require("doc_bank_org", input.bank.trim(), "bank")?;
    let name = input.name.trim();
    if name.is_empty() {
        return Err(Error::msg("name is required"));
    }
    db::create(
        "doc_bank_branch",
        &json!({
            "bank": id_of(&bank)?,
            "name": name,
            "code": input.code.as_deref().map(str::trim).filter(|s| !s.is_empty()),
            "is_active": input.is_active,
        }),
    )
    .map_err(|e| e.or("could not create the branch"))
}

fn edit_branch(input: BranchChange) -> Result<Record> {
    require_bank_admin()?;
    let _ = require("doc_bank_branch", &input.id, "branch")?;
    let mut data = serde_json::Map::new();
    if let Some(bank) = input.bank {
        let bank = require("doc_bank_org", bank.trim(), "bank")?;
        data.insert("bank".into(), json!(id_of(&bank)?));
    }
    if let Some(name) = input.name {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::msg("name is required"));
        }
        data.insert("name".into(), json!(name));
    }
    if let Some(code) = input.code {
        data.insert("code".into(), json!(code.trim()));
    }
    if let Some(active) = input.is_active {
        data.insert("is_active".into(), json!(active));
    }
    if data.is_empty() {
        return Err(Error::msg("nothing to change"));
    }
    db::update("doc_bank_branch", &input.id, &Value::Object(data))?.ok_or_else(|| Error::msg("the branch is gone"))
}

fn branches_list(input: BranchList) -> Result<Vec<Record>> {
    let mut rows: Vec<Record> = match input.bank.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(bank) => db::find("doc_bank_branch").filter("bank", bank).order_by("name").limit(500).all()?,
        None => db::find("doc_bank_branch").order_by("name").limit(500).all()?,
    };
    if !input.all {
        rows.retain(active);
    }
    Ok(rows)
}

/// Find a bank by name (case-insensitive) or create it. Used when migrating free-text names.
pub fn ensure_bank_named(name: &str) -> Result<Record> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::msg("name is required"));
    }
    let needle = name.to_lowercase();
    let rows: Vec<Record> = db::find("doc_bank_org").limit(500).all()?;
    if let Some(found) = rows
        .into_iter()
        .find(|r| text(r, "name").map(|n| n.to_lowercase()) == Some(needle.clone()))
    {
        return Ok(found);
    }
    db::create("doc_bank_org", &json!({ "name": name, "is_active": true }))
}

/// Find a branch of `bank` by name or create it.
pub fn ensure_branch_named(bank_id: &str, name: &str) -> Result<Record> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::msg("name is required"));
    }
    let needle = name.to_lowercase();
    let rows: Vec<Record> = db::find("doc_bank_branch").filter("bank", bank_id).limit(500).all()?;
    if let Some(found) = rows
        .into_iter()
        .find(|r| text(r, "name").map(|n| n.to_lowercase()) == Some(needle.clone()))
    {
        return Ok(found);
    }
    db::create(
        "doc_bank_branch",
        &json!({ "bank": bank_id, "name": name, "is_active": true }),
    )
}

/// Resolve a bank id and optional branch; branch must belong to that bank when set.
pub fn resolve_bank_and_branch(bank: &str, branch: Option<&str>) -> Result<(String, Option<String>)> {
    let bank_row = require("doc_bank_org", bank.trim(), "bank")?;
    if !active(&bank_row) {
        return Err(Error::msg("that bank is not in use"));
    }
    let bank_id = id_of(&bank_row)?.to_string();
    let Some(branch) = branch.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok((bank_id, None));
    };
    let branch_row = require("doc_bank_branch", branch, "branch")?;
    if !active(&branch_row) {
        return Err(Error::msg("that branch is not in use"));
    }
    let branch_bank = text(&branch_row, "bank").unwrap_or_default();
    if !same_record(branch_bank, &bank_id) {
        return Err(Error::msg("that branch does not belong to the selected bank"));
    }
    Ok((bank_id, Some(id_of(&branch_row)?.to_string())))
}

handler! {
    fn create_bank(input: Named) -> Record {
        new_bank(input)
    }

    fn update_bank(input: BankChange) -> Record {
        edit_bank(input)
    }

    fn get_bank(input: Id) -> Option<Record> {
        db::get("doc_bank_org", &input.id)
    }

    /// Active banks for pickers.
    fn list_banks(_: Empty) -> Vec<Record> {
        banks_list(false)
    }

    /// All banks for configuration.
    fn list_all_banks(_: Empty) -> Vec<Record> {
        require_bank_admin()?;
        banks_list(true)
    }

    fn create_bank_branch(input: BranchIn) -> Record {
        new_branch(input)
    }

    fn update_bank_branch(input: BranchChange) -> Record {
        edit_branch(input)
    }

    fn get_bank_branch(input: Id) -> Option<Record> {
        db::get("doc_bank_branch", &input.id)
    }

    /// Branches for pickers; pass `bank` to limit to that bank. Empty bank → empty list.
    fn list_bank_branches(input: BranchList) -> Vec<Record> {
        if input.bank.as_deref().map(str::trim).filter(|s| !s.is_empty()).is_none() && !input.all {
            return Ok(vec![]);
        }
        branches_list(BranchList { all: false, ..input })
    }

    /// All branches for configuration (optional `bank` filter).
    fn list_all_bank_branches(input: BranchList) -> Vec<Record> {
        require_bank_admin()?;
        branches_list(BranchList { all: true, bank: input.bank })
    }
}
