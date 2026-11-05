//! Desk demo data for leave types.

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

const TYPES: &[(&str, &str, &str, bool)] = &[
    ("Annual leave", "AL", "#3b82f6", true),
    ("Sick leave", "SL", "#ef4444", true),
    ("Unpaid leave", "UL", "#78716c", false),
    ("Maternity", "ML", "#ec4899", true),
    ("Paternity", "PL", "#8b5cf6", true),
    ("Compassionate", "CL", "#f59e0b", true),
    ("Study leave", "ST", "#14b8a6", true),
    ("Compensatory", "CO", "#22c55e", true),
    ("Religious", "RL", "#a855f7", true),
    ("Sabbatical", "SB", "#0ea5e9", true),
];

fn demo_leave_types(count: u32) -> Result<Value> {
    let stamp: String = context::current()
        .ok()
        .map(|c| c.now.chars().filter(|ch| ch.is_ascii_digit()).take(8).collect())
        .unwrap_or_else(|| "demo".into());
    let mut ids = Vec::new();
    let mut created = 0u32;
    let mut skipped = 0u32;
    for i in 0..count {
        let (name, code, color, paid) = TYPES[i as usize % TYPES.len()];
        let label = if count as usize > TYPES.len() {
            format!("{name} {}", i + 1)
        } else {
            name.to_string()
        };
        let code = if count as usize > TYPES.len() {
            format!("{code}{}", i + 1)
        } else {
            format!("{code}-{stamp}")
        };
        match db::find::<Record>("leave_type").filter("name", label.as_str()).limit(1).first()? {
            Some(existing) => {
                skipped += 1;
                ids.push(id_of(&existing)?.to_string());
            }
            None => {
                let row = setup::create_for_demo(json!({
                    "name": label,
                    "code": code,
                    "color": color,
                    "is_paid": paid,
                    "is_active": true,
                    "annual_days": 20,
                    "half_days": true,
                }))?;
                created += 1;
                ids.push(id_of(&row)?.to_string());
            }
        }
    }
    Ok(json!({ "created": created, "already_there": skipped, "ids": ids }))
}

handler! {
    fn generate_demo_leave_type(input: DemoCount) -> Value {
        demo_leave_types(count_of(input))
    }
}
