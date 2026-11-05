//! Move free-text relation / bank / branch values onto the new link fields.

use aether_sdk::prelude::*;

use crate::banks::{ensure_bank_named, ensure_branch_named};
use crate::common::{id_of, text, Record};
use crate::docs::ensure_relation_named;

fn run_migrate_links() -> Result<Value> {
    let mut relations = 0;
    let mut banks = 0;
    let mut branches = 0;

    for row in db::find::<Record>("doc_emergency").limit(5000).all()? {
        if text(&row, "relation").filter(|s| !s.is_empty()).is_some() {
            continue;
        }
        let Some(name) = text(&row, "relation_text").map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let rel = ensure_relation_named(name)?;
        db::update::<Record>(
            "doc_emergency",
            id_of(&row)?,
            &json!({ "relation": id_of(&rel)?, "relation_text": null }),
        )?;
        relations += 1;
    }

    for row in db::find::<Record>("doc_bank").limit(5000).all()? {
        let mut data = serde_json::Map::new();
        let bank_id = if let Some(existing) = text(&row, "bank").filter(|s| !s.is_empty()) {
            existing.to_string()
        } else if let Some(name) = text(&row, "bank_text").map(str::trim).filter(|s| !s.is_empty()) {
            let bank = ensure_bank_named(name)?;
            let id = id_of(&bank)?.to_string();
            data.insert("bank".into(), json!(id.clone()));
            data.insert("bank_text".into(), Value::Null);
            banks += 1;
            id
        } else {
            continue;
        };

        if text(&row, "branch").filter(|s| !s.is_empty()).is_none() {
            if let Some(name) = text(&row, "branch_text").map(str::trim).filter(|s| !s.is_empty()) {
                let branch = ensure_branch_named(&bank_id, name)?;
                data.insert("branch".into(), json!(id_of(&branch)?));
                data.insert("branch_text".into(), Value::Null);
                branches += 1;
            }
        }

        if !data.is_empty() {
            db::update::<Record>("doc_bank", id_of(&row)?, &Value::Object(data))?;
        }
    }

    Ok(json!({ "relations": relations, "banks": banks, "branches": branches }))
}

handler! {
    /// Copy free-text relation/bank/branch onto the link fields (idempotent).
    fn migrate_links(_: Empty) -> Value {
        run_migrate_links()
    }
}
