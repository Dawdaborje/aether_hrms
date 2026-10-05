//! Stages, rejection reasons and interview rounds.

use aether_sdk::prelude::*;

use crate::common::{pick, require_manager, text, Record};
use crate::rules::{check_criteria, Criterion};

const DEFAULT_STAGES: &[(&str, i64, &str)] = &[
    ("New", 1, "sourcing"),
    ("Screening", 2, "screening"),
    ("Interview", 3, "interview"),
    ("Offer", 4, "offer"),
    ("Hired", 5, "hired"),
    ("Rejected", 6, "rejected"),
];

#[derive(Deserialize)]
struct Round {
    name: String,
    criteria: Vec<CriterionInput>,
    #[serde(default)]
    min_average: Option<aether_sdk::decimal::Decimal>,
    #[serde(default)]
    min_panelists: Option<i64>,
}

#[derive(Deserialize, Serialize)]
struct CriterionInput {
    name: String,
    weight: i64,
}

/// Start with the usual stages, if there are none yet.
fn seed() -> Result<Vec<Record>> {
    require_manager()?;
    if db::count("rec_stage", aether_sdk::db::Filter::all())? > 0 {
        return db::find("rec_stage").order_by("sequence").limit(50).all();
    }
    let mut made = Vec::new();
    for (name, sequence, kind) in DEFAULT_STAGES {
        made.push(db::create::<Record>("rec_stage", &json!({ "name": name, "sequence": sequence, "kind": kind }))?);
    }
    Ok(made)
}

fn new_stage(input: Record) -> Result<Record> {
    require_manager()?;
    let data = pick(&input, &["name", "sequence", "kind", "rot_days", "is_active"]);
    let kind = text(&data, "kind").unwrap_or_default();
    if crate::rules::StageKind::parse(kind).is_none() {
        return Err(Error::msg("kind is sourcing, screening, interview, offer, hired or rejected"));
    }
    // One place for people who were hired and one for those who were not.
    if matches!(kind, "hired" | "rejected") && db::count("rec_stage", aether_sdk::db::Filter::eq("kind", kind))? > 0 {
        return Err(Error::msg(format!("there is already a `{kind}` stage")));
    }
    db::create("rec_stage", &data).map_err(|e| e.or("could not create the stage (the sequence may be taken)"))
}

fn new_round(input: Round) -> Result<Record> {
    require_manager()?;
    let criteria: Vec<Criterion> = input.criteria.iter().map(|c| Criterion { name: c.name.clone(), weight: c.weight }).collect();
    check_criteria(&criteria)?;
    let min_average = input.min_average.unwrap_or(aether_sdk::decimal::Decimal::parse("3.00")?);
    if min_average < aether_sdk::decimal::Decimal::parse("1")? || min_average > aether_sdk::decimal::Decimal::parse("5")? {
        return Err(Error::msg("the average needed is between 1 and 5"));
    }
    let min_panelists = input.min_panelists.unwrap_or(1);
    if !(1..=10).contains(&min_panelists) {
        return Err(Error::msg("1 to 10 interviewers must answer"));
    }
    db::create(
        "rec_round",
        &json!({ "name": input.name, "criteria": input.criteria, "min_average": min_average.with_scale(2)?, "min_panelists": min_panelists }),
    )
    .map_err(|e| e.or("could not create the round (the name may be taken)"))
}

/// A round's criteria as the rules see them.
pub fn criteria_of(round: &Record) -> Result<Vec<Criterion>> {
    let list = round.get("criteria").and_then(Value::as_array).ok_or_else(|| Error::msg("the round has no criteria"))?;
    list.iter()
        .map(|item| {
            Ok(Criterion {
                name: item.get("name").and_then(Value::as_str).ok_or_else(|| Error::msg("a criterion has no name"))?.to_string(),
                weight: item.get("weight").and_then(Value::as_i64).ok_or_else(|| Error::msg("a criterion has no weight"))?,
            })
        })
        .collect()
}

fn new_reason(input: Record) -> Result<Record> {
    require_manager()?;
    if text(&input, "name").is_none() {
        return Err(Error::msg("a reason needs a name"));
    }
    db::create("rec_reject_reason", &pick(&input, &["name", "is_active"])).map_err(|e| e.or("could not create the reason (the name may be taken)"))
}

handler! {
    /// Create the usual stages (New, Screening, Interview, Offer, Hired, Rejected) if there are none.
    fn seed_stages(_: Empty) -> Vec<Record> {
        seed()
    }

    fn create_stage(input: Record) -> Record {
        new_stage(input)
    }

    fn list_stages(_: Empty) -> Vec<Record> {
        db::find("rec_stage").order_by("sequence").limit(50).all()
    }

    fn create_reject_reason(input: Record) -> Record {
        new_reason(input)
    }

    fn list_reject_reasons(_: Empty) -> Vec<Record> {
        db::find("rec_reject_reason").order_by("name").limit(100).all()
    }

    /// An interview round: what it rates (weights add up to 100) and the bar to advance.
    fn create_round(input: Round) -> Record {
        new_round(input)
    }

    fn list_rounds(_: Empty) -> Vec<Record> {
        db::find("rec_round").order_by("name").limit(100).all()
    }
}
