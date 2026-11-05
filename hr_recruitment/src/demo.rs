//! Desk demo data for rejection reasons (and stages via seed when empty).

use aether_sdk::prelude::*;

use crate::common::{id_of, Record};
use crate::setup;

#[derive(Deserialize)]
struct DemoCount {
    #[serde(default = "default_count")]
    count: u32,
}

fn default_count() -> u32 {
    10
}

fn count_of(input: DemoCount) -> u32 {
    input.count.clamp(1, 10_000)
}

const REASONS: &[&str] = &[
    "Skills mismatch",
    "Salary expectations",
    "Role filled",
    "Withdrew",
    "Failed screening",
    "Culture fit",
    "Incomplete application",
    "Overqualified",
    "Underqualified",
    "Location",
];

fn demo_reject_reasons(count: u32) -> Result<Value> {
    let mut ids = Vec::new();
    let mut created = 0u32;
    let mut skipped = 0u32;
    for i in 0..count {
        let name = REASONS[i as usize % REASONS.len()];
        let label = if count as usize > REASONS.len() {
            format!("{name} {}", i + 1)
        } else {
            name.to_string()
        };
        match db::find::<Record>("rec_reject_reason").filter("name", label.as_str()).limit(1).first()? {
            Some(existing) => {
                skipped += 1;
                ids.push(id_of(&existing)?.to_string());
            }
            None => {
                let row = setup::create_reason_for_demo(json!({
                    "name": label,
                    "is_active": true,
                }))?;
                created += 1;
                ids.push(id_of(&row)?.to_string());
            }
        }
    }
    Ok(json!({ "created": created, "already_there": skipped, "ids": ids }))
}

fn demo_stages(count: u32) -> Result<Value> {
    // Stages are a fixed pipeline; seeding once is enough. Extra count is ignored after seed.
    let _ = count;
    let rows = setup::seed_stages_for_demo()?;
    let ids: Vec<String> = rows.iter().filter_map(|row| id_of(row).ok().map(str::to_string)).collect();
    Ok(json!({ "created": ids.len(), "ids": ids, "note": "idempotent pipeline stages" }))
}

handler! {
    fn generate_demo_rec_reject_reason(input: DemoCount) -> Value {
        demo_reject_reasons(count_of(input))
    }

    fn generate_demo_rec_stage(input: DemoCount) -> Value {
        demo_stages(count_of(input))
    }
}
