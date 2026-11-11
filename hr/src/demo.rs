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

/// The grade ladder: code, name, rank, lowest and highest wage of the band.
const GRADES: &[(&str, &str, i64, i64, i64)] = &[
    ("G1", "Entry", 1, 1_000, 1_800),
    ("G2", "Associate", 2, 1_700, 2_800),
    ("G3", "Professional", 3, 2_600, 4_000),
    ("G4", "Senior", 4, 3_800, 5_600),
    ("G5", "Lead", 5, 5_400, 7_600),
    ("G6", "Manager", 6, 7_200, 10_000),
    ("G7", "Director", 7, 9_500, 14_000),
    ("G8", "Executive", 8, 13_000, 22_000),
];

/// The job ladder, top first: name and the grade of its positions. Each job reports to the one above it.
const JOB_LADDER: &[(&str, &str)] = &[
    ("Chief Executive", "G8"),
    ("Director", "G7"),
    ("Department Manager", "G6"),
    ("Team Lead", "G5"),
    ("Senior Specialist", "G4"),
    ("Specialist", "G3"),
    ("Associate", "G2"),
    ("Assistant", "G1"),
];

/// Make the grades that do not exist yet; those that do are left alone.
fn ensure_grades() -> Result<u32> {
    let mut made = 0;
    for (code, name, rank, min, max) in GRADES {
        if db::find::<Record>("grade").filter("code", *code).first()?.is_some() {
            continue;
        }
        explain(
            db::create::<Record>(
                "grade",
                &json!({ "code": code, "name": name, "rank": rank, "min_wage": min.to_string(), "max_wage": max.to_string(), "is_active": true }),
            ),
            "could not create a grade",
        )?;
        made += 1;
    }
    Ok(made)
}

/// Existing grades are kept as they are, so before a chain is built check that their ranks still let each job of the
/// ladder report to the one above it under the manager rank rule. Without this a position of one job could be refused
/// halfway through generation because, say, G8 was given a lower rank than G7.
fn check_grade_ladder() -> Result<()> {
    let mut ranks: Vec<(&str, Option<i64>)> = Vec::new();
    for (_, grade) in JOB_LADDER {
        let rank = db::find::<Record>("grade").filter("code", *grade).first()?.and_then(|g| g.get("rank").and_then(Value::as_i64));
        ranks.push((grade, rank));
    }
    for pair in ranks.windows(2) {
        let ((upper, upper_rank), (lower, lower_rank)) = (pair[0], pair[1]);
        if !crate::rank::allowed(upper_rank, lower_rank)? {
            return Err(Error::msg(format!(
                "the existing grades do not fit the demo job ladder: {upper} must rank above {lower} ({}); change their ranks in HR Configuration or use other grades",
                crate::rank::explain_rule()?
            )));
        }
    }
    Ok(())
}

fn demo_grades() -> Result<Value> {
    Ok(json!({ "created": ensure_grades()? }))
}

/// Jobs in chains of the ladder: each reports to the job above it and carries its grade.
fn demo_jobs(count: u32) -> Result<Value> {
    let (dept, _, _) = ensure_scaffold()?;
    ensure_grades()?;
    check_grade_ladder()?;
    let tag = stamp();
    let mut ids = Vec::new();
    let mut above: Option<String> = None;
    for i in 0..count {
        let level = i as usize % JOB_LADDER.len();
        let chain = i as usize / JOB_LADDER.len();
        if level == 0 {
            above = None;
        }
        let (name, grade) = JOB_LADDER[level];
        let label = if chain > 0 { format!("{name} {}", chain + 1) } else { name.to_string() };
        let row = position::create_job_for_demo(json!({
            "name": label,
            "code": format!("J{}-{}", i + 1, &tag[..6.min(tag.len())]),
            "department": dept,
            "description": "Demo job",
            "grade": grade,
            "parent": above,
        }))?;
        above = Some(id_of(&row)?.to_string());
        ids.push(above.clone().unwrap_or_default());
    }
    Ok(json!({ "created": ids.len(), "ids": ids }))
}

/// Positions across the jobs, highest rank first so each can report to a position of the job above.
/// The department is the job's, and a position's "Reports to" is filled in from the job tree.
fn demo_positions(count: u32) -> Result<Value> {
    let (scaffold_department, _, _) = ensure_scaffold()?;
    ensure_grades()?;
    check_grade_ladder()?;
    let mut jobs: Vec<(i64, Record)> = Vec::new();
    for job in db::find::<Record>("job").matching(Filter::eq("is_active", true)).limit(500).all()? {
        let rank = match text(&job, "grade") {
            Some(code) => db::find::<Record>("grade").filter("code", code).first()?.and_then(|g| g.get("rank").and_then(Value::as_i64)),
            None => None,
        };
        jobs.push((rank.unwrap_or(0), job));
    }
    if jobs.is_empty() {
        demo_jobs(JOB_LADDER.len() as u32)?;
        return demo_positions(count);
    }
    jobs.sort_by(|a, b| b.0.cmp(&a.0));
    let tag = stamp();
    let mut ids = Vec::new();
    let mut made_for: Vec<u32> = vec![0; jobs.len()];
    for i in 0..count as usize {
        let slot = i % jobs.len();
        let (rank, job) = &jobs[slot];
        made_for[slot] += 1;
        let headcount = match rank {
            7.. => 1,
            6 => 2,
            5 => 3,
            4 => 4,
            _ => 5,
        };
        let row = position::create_for_demo(json!({
            "name": format!("{} {}", text(job, "name").unwrap_or("Seat"), made_for[slot]),
            "code": format!("P{}-{}", i + 1, &tag[..6.min(tag.len())]),
            "job": id_of(job)?,
            // A job may have no department of its own; the scaffold's stands in so generation does not stop.
            "department": text(job, "department").unwrap_or(scaffold_department.as_str()),
            "headcount": headcount,
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

    fn generate_demo_grade(_input: DemoCount) -> Value {
        demo_grades()
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
