//! Desk demo data: generate sample org structure and people.
//!
//! Commands declare `demo_model` in `plugin.toml`. Desk passes `count`; the plugin creates records.

use aether_sdk::db::Filter;
use aether_sdk::prelude::*;

use crate::common::{explain, id_of, text, Record};
use crate::department;
use crate::employee;
use crate::position;

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

const FIRST_NAMES: &[&str] = &[
    "Ada", "Ben", "Chi", "Dee", "Eva", "Femi", "Gina", "Hugo", "Ife", "Jade", "Kai", "Lina", "Mo", "Nia",
    "Omar", "Pia", "Quinn", "Ravi", "Sara", "Tom", "Umi", "Vera", "Wes", "Xena", "Yuri", "Zara",
];
const LAST_NAMES: &[&str] = &[
    "Okeke", "Mensah", "Diallo", "Nguyen", "Silva", "Patel", "Kim", "Garcia", "Hassan", "Brown", "Chen",
    "Ibrahim", "Lopez", "Mwangi", "Anders", "Costa", "Singh", "Ali", "Park", "Novak",
];

const DEPARTMENTS: &[&str] = &[
    "Operations", "Finance", "People", "Engineering", "Sales", "Marketing", "Support", "Legal", "Product",
    "Facilities",
];
const JOBS: &[&str] = &[
    "Analyst", "Specialist", "Coordinator", "Manager", "Engineer", "Associate", "Lead", "Officer", "Advisor",
    "Director",
];
const SKILLS: &[(&str, &str)] = &[
    ("Communication", "soft"),
    ("Leadership", "soft"),
    ("Excel", "tools"),
    ("SQL", "tools"),
    ("Python", "tools"),
    ("Project management", "soft"),
    ("Customer service", "soft"),
    ("Writing", "soft"),
    ("Design", "craft"),
    ("Accounting", "domain"),
    ("Recruiting", "domain"),
    ("Safety", "domain"),
];
const LOCATIONS: &[&str] = &[
    "Head office", "Downtown branch", "Warehouse", "Remote hub", "Airport site", "Factory floor",
    "Customer center", "R&D lab", "Training room", "Field office",
];

fn stamp() -> String {
    context::current()
        .ok()
        .map(|ctx| {
            let now = ctx.now.replace([':', '-', 'T', 'Z', '.'], "");
            now.chars().take(14).collect::<String>()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "demo".into())
}

fn ensure_scaffold() -> Result<(String, String, String)> {
    let dept = match db::find::<Record>("department")
        .matching(Filter::eq("is_active", true))
        .order_by("name")
        .limit(1)
        .first()?
    {
        Some(row) => id_of(&row)?.to_string(),
        None => {
            let tag = stamp();
            let row = department::create_for_demo(json!({
                "name": "General",
                "code": format!("GEN-{}", &tag[..6.min(tag.len())]),
                "description": "Created for demo data",
            }))?;
            id_of(&row)?.to_string()
        }
    };
    let job = match db::find::<Record>("job")
        .matching(Filter::eq("is_active", true))
        .order_by("name")
        .limit(1)
        .first()?
    {
        Some(row) => id_of(&row)?.to_string(),
        None => {
            let tag = stamp();
            let row = position::create_job_for_demo(json!({
                "name": "Staff",
                "code": format!("JOB-{}", &tag[..6.min(tag.len())]),
                "department": dept,
            }))?;
            id_of(&row)?.to_string()
        }
    };
    let pos = match db::find::<Record>("position")
        .matching(Filter::ne("status", "frozen"))
        .order_by("name")
        .limit(1)
        .first()?
    {
        Some(row) => id_of(&row)?.to_string(),
        None => {
            let tag = stamp();
            let row = position::create_for_demo(json!({
                "name": "General staff",
                "code": format!("POS-{}", &tag[..6.min(tag.len())]),
                "job": job,
                "department": dept,
                "headcount": 1000,
                "status": "open",
            }))?;
            id_of(&row)?.to_string()
        }
    };
    // Grow headcount so demo hires fit.
    if let Ok(Some(existing)) = db::get::<Record>("position", &pos) {
        let held = position::holders(&pos).unwrap_or(0);
        let headcount = existing.get("headcount").and_then(Value::as_u64).unwrap_or(1);
        if headcount < held + 50 {
            let _ = db::update::<Record>("position", &pos, &json!({ "headcount": held + 200 }));
        }
    }
    Ok((dept, job, pos))
}

fn demo_departments(count: u32) -> Result<Value> {
    let tag = stamp();
    let mut ids = Vec::new();
    for i in 0..count {
        let name = DEPARTMENTS[i as usize % DEPARTMENTS.len()];
        let label = if count as usize > DEPARTMENTS.len() {
            format!("{name} {}", i + 1)
        } else {
            name.to_string()
        };
        let row = department::create_for_demo(json!({
            "name": label,
            "code": format!("D{}-{}", i + 1, &tag[..6.min(tag.len())]),
            "description": "Demo department",
        }))?;
        ids.push(id_of(&row)?.to_string());
    }
    Ok(json!({ "created": ids.len(), "ids": ids }))
}

fn demo_jobs(count: u32) -> Result<Value> {
    let (dept, _, _) = ensure_scaffold()?;
    let tag = stamp();
    let mut ids = Vec::new();
    for i in 0..count {
        let name = JOBS[i as usize % JOBS.len()];
        let label = if count as usize > JOBS.len() {
            format!("{name} {}", i + 1)
        } else {
            name.to_string()
        };
        let row = position::create_job_for_demo(json!({
            "name": label,
            "code": format!("J{}-{}", i + 1, &tag[..6.min(tag.len())]),
            "department": dept,
            "description": "Demo job",
        }))?;
        ids.push(id_of(&row)?.to_string());
    }
    Ok(json!({ "created": ids.len(), "ids": ids }))
}

fn demo_positions(count: u32) -> Result<Value> {
    let (dept, job, _) = ensure_scaffold()?;
    let tag = stamp();
    let mut ids = Vec::new();
    for i in 0..count {
        let name = format!("Seat {}", i + 1);
        let row = position::create_for_demo(json!({
            "name": name,
            "code": format!("P{}-{}", i + 1, &tag[..6.min(tag.len())]),
            "job": job,
            "department": dept,
            "headcount": 5,
            "status": "open",
        }))?;
        ids.push(id_of(&row)?.to_string());
    }
    Ok(json!({ "created": ids.len(), "ids": ids }))
}

fn demo_work_locations(count: u32) -> Result<Value> {
    let tag = stamp();
    let mut ids = Vec::new();
    for i in 0..count {
        let name = LOCATIONS[i as usize % LOCATIONS.len()];
        let label = if count as usize > LOCATIONS.len() {
            format!("{name} {}", i + 1)
        } else {
            name.to_string()
        };
        let row = explain(
            db::create(
                "work_location",
                &json!({
                    "name": format!("{label} ({})", &tag[..4.min(tag.len())]),
                    "kind": if i % 3 == 0 { "home" } else { "office" },
                    "is_active": true,
                }),
            ),
            "could not create work location",
        )?;
        ids.push(id_of(&row)?.to_string());
    }
    Ok(json!({ "created": ids.len(), "ids": ids }))
}

fn demo_skills(count: u32) -> Result<Value> {
    let mut ids = Vec::new();
    let mut created = 0u32;
    let mut skipped = 0u32;
    for i in 0..count {
        let (name, category) = SKILLS[i as usize % SKILLS.len()];
        let label = if count as usize > SKILLS.len() {
            format!("{name} {}", i + 1)
        } else {
            name.to_string()
        };
        match db::find::<Record>("skill").filter("name", label.as_str()).limit(1).first()? {
            Some(existing) => {
                skipped += 1;
                ids.push(id_of(&existing)?.to_string());
            }
            None => {
                let row = explain(
                    db::create("skill", &json!({ "name": label, "category": category })),
                    "could not create skill",
                )?;
                created += 1;
                ids.push(id_of(&row)?.to_string());
            }
        }
    }
    Ok(json!({ "created": created, "already_there": skipped, "ids": ids }))
}

fn demo_employees(count: u32) -> Result<Value> {
    let (dept, job, pos) = ensure_scaffold()?;
    let location = db::find::<Record>("work_location")
        .matching(Filter::eq("is_active", true))
        .limit(1)
        .first()?
        .and_then(|row| text(&row, "id").map(str::to_string));
    let tag = stamp();
    let mut ids = Vec::new();
    let mut errors = Vec::new();
    for i in 0..count {
        let first = FIRST_NAMES[i as usize % FIRST_NAMES.len()];
        let last = LAST_NAMES[i as usize % LAST_NAMES.len()];
        let email = format!(
            "{}.{}.{}@demo.aether.local",
            first.to_lowercase(),
            last.to_lowercase(),
            i + 1
        );
        let mut hire = json!({
            "first_name": first,
            "last_name": last,
            "email": email,
            "department": dept,
            "job": job,
            "position": pos,
            "employment_type": "permanent",
            "gender": if i % 2 == 0 { "female" } else { "male" },
        });
        if let Some(loc) = &location {
            hire["work_location"] = json!(loc);
        }
        match employee::hire_for_demo(hire) {
            Ok(row) => ids.push(id_of(&row)?.to_string()),
            Err(error) => errors.push(format!("#{i} ({first} {last} / {tag}): {error}")),
        }
    }
    Ok(json!({
        "created": ids.len(),
        "failed": errors.len(),
        "ids": ids,
        "errors": errors,
    }))
}

handler! {
    fn generate_demo_department(input: DemoCount) -> Value {
        demo_departments(count_of(input))
    }

    fn generate_demo_job(input: DemoCount) -> Value {
        demo_jobs(count_of(input))
    }

    fn generate_demo_position(input: DemoCount) -> Value {
        demo_positions(count_of(input))
    }

    fn generate_demo_work_location(input: DemoCount) -> Value {
        demo_work_locations(count_of(input))
    }

    fn generate_demo_skill(input: DemoCount) -> Value {
        demo_skills(count_of(input))
    }

    fn generate_demo_employee(input: DemoCount) -> Value {
        demo_employees(count_of(input))
    }
}
