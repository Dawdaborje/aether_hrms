//! Desk demo data for shifts and locations.

use aether_sdk::prelude::*;

use crate::common::id_of;
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

const SHIFTS: &[(&str, &str, &str, &str)] = &[
    ("DAY", "Day shift", "09:00", "17:00"),
    ("MOR", "Morning", "06:00", "14:00"),
    ("AFT", "Afternoon", "14:00", "22:00"),
    ("FLEX", "Flexible day", "08:00", "16:00"),
    ("HALF", "Half day", "09:00", "13:00"),
    ("STD", "Standard", "08:30", "17:30"),
    ("EAR", "Early bird", "07:00", "15:00"),
    ("LAT", "Late desk", "11:00", "19:00"),
    ("MID", "Midday", "10:00", "18:00"),
    ("CORE", "Core hours", "09:30", "17:30"),
];

const LOCATIONS: &[&str] = &[
    "Main gate", "Office lobby", "Warehouse A", "Plant floor", "Branch reception", "Lab entrance",
    "Remote check-in", "Training center", "Store front", "Yard gate",
];

fn demo_shifts(count: u32) -> Result<Value> {
    let stamp: String = context::current()
        .ok()
        .map(|c| c.now.chars().filter(|ch| ch.is_ascii_digit()).take(6).collect())
        .unwrap_or_else(|| "demo".into());
    let mut ids = Vec::new();
    for i in 0..count {
        let (code, name, start, end) = SHIFTS[i as usize % SHIFTS.len()];
        let code = format!("{code}{i}-{stamp}");
        let name = if count as usize > SHIFTS.len() {
            format!("{name} {}", i + 1)
        } else {
            name.to_string()
        };
        let row = setup::create_shift_for_demo(json!({
            "code": code,
            "name": name,
            "start_time": start,
            "end_time": end,
            "utc_offset_min": 0,
            "early_window_min": 60,
            "late_window_min": 60,
            "break_min": 60,
            "grace_late_min": 15,
            "grace_early_min": 15,
            "half_day_below_min": 240,
            "absent_below_min": 120,
            "is_active": true,
        }))?;
        ids.push(id_of(&row)?.to_string());
    }
    Ok(json!({ "created": ids.len(), "ids": ids }))
}

fn demo_locations(count: u32) -> Result<Value> {
    let stamp: String = context::current()
        .ok()
        .map(|c| c.now.chars().filter(|ch| ch.is_ascii_digit()).take(4).collect())
        .unwrap_or_else(|| "demo".into());
    let mut ids = Vec::new();
    for i in 0..count {
        let name = LOCATIONS[i as usize % LOCATIONS.len()];
        let label = if count as usize > LOCATIONS.len() {
            format!("{name} {} ({stamp})", i + 1)
        } else {
            format!("{name} ({stamp})")
        };
        let row = setup::create_location_for_demo(json!({
            "name": label,
            "geo_mode": "off",
            "is_active": true,
        }))?;
        ids.push(id_of(&row)?.to_string());
    }
    Ok(json!({ "created": ids.len(), "ids": ids }))
}

handler! {
    fn generate_demo_att_shift(input: DemoCount) -> Value {
        demo_shifts(count_of(input))
    }

    fn generate_demo_att_location(input: DemoCount) -> Value {
        demo_locations(count_of(input))
    }
}
