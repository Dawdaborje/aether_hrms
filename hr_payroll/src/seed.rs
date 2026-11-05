//! The rule packs shipped with the plugin: `assets/rule_packs.zip`, built into the plugin and loaded into
//! `pay_rule_pack` when the plugin is installed or upgraded.
//!
//! Layout of the zip: `<country>/<name>.cel` (the expression) beside `<country>/<name>.json` (the description,
//! with its own tests). The archive is stored without compression, so the plugin needs no decompressor
//! (`scripts/pack_zip.py` in the tax rules repository makes it). A pack whose id is already there is left
//! alone: an edition is never changed, a new edition has a new id. A pack that fails its own tests is
//! skipped and named in the result; the others are still loaded.

use std::collections::BTreeMap;

use aether_sdk::prelude::*;

use crate::common::{require_admin, text};

const BUNDLE: &[u8] = include_bytes!("../assets/rule_packs.zip");

fn u16_at(b: &[u8], at: usize) -> Result<usize> {
    b.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]]) as usize).ok_or_else(|| Error::msg("the rule pack archive is cut short"))
}

fn u32_at(b: &[u8], at: usize) -> Result<usize> {
    b.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize).ok_or_else(|| Error::msg("the rule pack archive is cut short"))
}

/// The files of a stored (uncompressed) zip archive, by name.
pub(crate) fn unzip_stored(bytes: &[u8]) -> Result<BTreeMap<String, String>> {
    // The end-of-central-directory record is the last 22+ bytes; search back for its signature.
    let eocd = (0..bytes.len().saturating_sub(21))
        .rev()
        .take(70_000)
        .find(|&i| bytes[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])
        .ok_or_else(|| Error::msg("this is not a zip archive"))?;
    let count = u16_at(bytes, eocd + 10)?;
    let mut at = u32_at(bytes, eocd + 16)?;
    let mut files = BTreeMap::new();
    for _ in 0..count {
        if bytes.get(at..at + 4) != Some(&[0x50, 0x4b, 0x01, 0x02]) {
            return Err(Error::msg("the zip directory is damaged"));
        }
        let method = u16_at(bytes, at + 10)?;
        let size = u32_at(bytes, at + 20)?;
        let (name_len, extra_len, comment_len) = (u16_at(bytes, at + 28)?, u16_at(bytes, at + 30)?, u16_at(bytes, at + 32)?);
        let local = u32_at(bytes, at + 42)?;
        let name = String::from_utf8_lossy(bytes.get(at + 46..at + 46 + name_len).ok_or_else(|| Error::msg("the zip directory is damaged"))?).to_string();
        at += 46 + name_len + extra_len + comment_len;
        if name.ends_with('/') {
            continue;
        }
        if method != 0 {
            return Err(Error::msg(format!("`{name}` is compressed: make the archive without compression (stored)")));
        }
        let (l_name, l_extra) = (u16_at(bytes, local + 26)?, u16_at(bytes, local + 28)?);
        let start = local + 30 + l_name + l_extra;
        let data = bytes.get(start..start + size).ok_or_else(|| Error::msg(format!("`{name}` is cut short")))?;
        let body = String::from_utf8(data.to_vec()).map_err(|_| Error::msg(format!("`{name}` is not text")))?;
        files.insert(name, body);
    }
    Ok(files)
}

/// Load every shipped pack that is not there yet. Safe to run again.
fn seed() -> Result<Value> {
    let files = unzip_stored(BUNDLE)?;
    let (mut added, mut kept, mut failed) = (Vec::new(), Vec::new(), Vec::new());
    for (name, expression) in files.iter().filter(|(n, _)| n.ends_with(".cel")) {
        let meta = format!("{}.json", name.trim_end_matches(".cel"));
        let Some(description) = files.get(&meta) else {
            failed.push(json!({ "pack": name, "error": "no description beside it" }));
            continue;
        };
        let parsed: Value = match serde_json::from_str(description) {
            Ok(v) => v,
            Err(e) => {
                failed.push(json!({ "pack": name, "error": format!("the description: {e}") }));
                continue;
            }
        };
        let id = parsed.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        if db::count("pay_rule_pack", aether_sdk::db::Filter::eq("pack_id", id.as_str()))? > 0 {
            kept.push(id);
            continue;
        }
        match crate::setup::store_pack(parsed, expression.clone()) {
            Ok(row) => added.push(text(&row, "pack_id").unwrap_or(&id).to_string()),
            Err(e) => failed.push(json!({ "pack": id, "error": e.to_string() })),
        }
    }
    Ok(json!({ "added": added, "already_there": kept.len(), "failed": failed }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_archive_reads_and_every_pack_has_a_description() -> Result<()> {
        let files = unzip_stored(BUNDLE)?;
        let cel: Vec<&String> = files.keys().filter(|n| n.ends_with(".cel")).collect();
        assert!(!cel.is_empty(), "the archive holds no pack");
        for name in cel {
            assert!(files.contains_key(&format!("{}.json", name.trim_end_matches(".cel"))), "{name} has no description");
        }
        Ok(())
    }

    #[test]
    fn every_shipped_pack_passes_its_own_tests() -> Result<()> {
        let files = unzip_stored(BUNDLE)?;
        for (name, expression) in files.iter().filter(|(n, _)| n.ends_with(".cel")) {
            let description = &files[&format!("{}.json", name.trim_end_matches(".cel"))];
            let pack = sp_engine::RulePack::parse(description, expression).map_err(Error::msg)?;
            let results = pack.run_tests();
            assert!(!results.is_empty(), "{name} has no tests");
            for (test, outcome) in results {
                assert!(outcome.is_ok(), "{name} / {test}: {outcome:?}");
            }
        }
        Ok(())
    }
}

handler! {
    /// Load the packs shipped with the plugin (runs on install and upgrade).
    fn seed_rule_packs(_: Empty) -> Value {
        seed()
    }

    /// The same, from the setup screen: for an administrator who wants to load packs added by an upgrade.
    fn reload_shipped_rule_packs(_: Empty) -> Value {
        require_admin()?;
        seed()
    }
}
