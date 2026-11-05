//! Work locations and cost centres: catalogs maintained in HR Configuration, picked on people.

use aether_sdk::prelude::*;

use crate::common::{explain, id_of, require, text, Record};

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
    kind: Option<String>,
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    timezone: Option<String>,
    #[serde(default = "yes")]
    is_active: bool,
}

#[derive(Deserialize)]
struct LocationChange {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    timezone: Option<String>,
    #[serde(default)]
    is_active: Option<bool>,
}

#[derive(Deserialize)]
struct CentreChange {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    is_active: Option<bool>,
}

fn yes() -> bool {
    true
}

fn require_hr() -> Result<()> {
    let who = context::current()?;
    if who.has_role("hr.hr_user") || who.has_role("hr.hr_manager") {
        Ok(())
    } else {
        Err(Error::msg("only HR can change this catalog"))
    }
}

fn active(row: &Record) -> bool {
    row.get("is_active") != Some(&json!(false))
}

fn list_named(model: &str, all: bool) -> Result<Vec<Record>> {
    let rows: Vec<Record> = db::find(model).order_by("name").limit(500).all()?;
    Ok(if all {
        rows
    } else {
        rows.into_iter().filter(active).collect()
    })
}

/// Find by name (case-insensitive) or create. Used when migrating free-text values.
pub fn ensure_named(model: &str, name: &str) -> Result<Record> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::msg("name is required"));
    }
    let needle = name.to_lowercase();
    let rows: Vec<Record> = db::find(model).limit(500).all()?;
    if let Some(found) = rows
        .into_iter()
        .find(|r| text(r, "name").map(|n| n.to_lowercase()) == Some(needle.clone()))
    {
        return Ok(found);
    }
    db::create(model, &json!({ "name": name, "is_active": true }))
}

fn new_location(input: Named) -> Result<Record> {
    require_hr()?;
    let name = input.name.trim();
    if name.is_empty() {
        return Err(Error::msg("name is required"));
    }
    explain(
        db::create(
            "work_location",
            &json!({
                "name": name,
                "kind": input.kind.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("office"),
                "address": input.address.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                "timezone": input.timezone.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                "is_active": input.is_active,
            }),
        ),
        "could not create the work location (the name may be taken)",
    )
}

fn edit_location(input: LocationChange) -> Result<Record> {
    require_hr()?;
    let _ = require("work_location", &input.id, "work location")?;
    let mut data = serde_json::Map::new();
    if let Some(name) = input.name {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::msg("name is required"));
        }
        data.insert("name".into(), json!(name));
    }
    if let Some(kind) = input.kind {
        data.insert("kind".into(), json!(kind.trim()));
    }
    if let Some(address) = input.address {
        data.insert("address".into(), json!(address.trim()));
    }
    if let Some(timezone) = input.timezone {
        data.insert("timezone".into(), json!(timezone.trim()));
    }
    if let Some(active) = input.is_active {
        data.insert("is_active".into(), json!(active));
    }
    if data.is_empty() {
        return Err(Error::msg("nothing to change"));
    }
    db::update("work_location", &input.id, &Value::Object(data))?.ok_or_else(|| Error::msg("the work location is gone"))
}

fn new_centre(input: Named) -> Result<Record> {
    require_hr()?;
    let name = input.name.trim();
    if name.is_empty() {
        return Err(Error::msg("name is required"));
    }
    explain(
        db::create(
            "cost_center",
            &json!({
                "name": name,
                "code": input.code.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                "is_active": input.is_active,
            }),
        ),
        "could not create the cost centre (the name may be taken)",
    )
}

fn edit_centre(input: CentreChange) -> Result<Record> {
    require_hr()?;
    let _ = require("cost_center", &input.id, "cost centre")?;
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
    if let Some(active) = input.is_active {
        data.insert("is_active".into(), json!(active));
    }
    if data.is_empty() {
        return Err(Error::msg("nothing to change"));
    }
    db::update("cost_center", &input.id, &Value::Object(data))?.ok_or_else(|| Error::msg("the cost centre is gone"))
}

/// Move free-text work location / cost centre values onto the link fields (idempotent).
fn run_migrate_catalog_links() -> Result<Value> {
    let mut locations = 0;
    let mut centres = 0;

    for (model, text_field, link_field, catalog) in [
        ("employee", "work_location_text", "work_location", "work_location"),
        ("employee", "cost_center_text", "cost_center", "cost_center"),
        ("employment", "cost_center_text", "cost_center", "cost_center"),
        ("department", "cost_center_text", "cost_center", "cost_center"),
    ] {
        for row in db::find::<Record>(model).limit(5000).all()? {
            if text(&row, link_field).filter(|s| !s.is_empty()).is_some() {
                continue;
            }
            let Some(raw) = text(&row, text_field).map(str::trim).filter(|s| !s.is_empty()) else {
                continue;
            };
            let linked = if db::get::<Record>(catalog, raw)?.is_some() {
                raw.to_string()
            } else {
                id_of(&ensure_named(catalog, raw)?)?.to_string()
            };
            db::update::<Record>(
                model,
                id_of(&row)?,
                &json!({ link_field: linked, text_field: null }),
            )?;
            if catalog == "work_location" {
                locations += 1;
            } else {
                centres += 1;
            }
        }
    }

    Ok(json!({ "work_locations": locations, "cost_centers": centres }))
}

handler! {
    fn create_work_location(input: Named) -> Record {
        new_location(input)
    }

    fn update_work_location(input: LocationChange) -> Record {
        edit_location(input)
    }

    fn get_work_location(input: Id) -> Option<Record> {
        db::get("work_location", &input.id)
    }

    fn list_work_locations(_: Empty) -> Vec<Record> {
        list_named("work_location", false)
    }

    fn list_all_work_locations(_: Empty) -> Vec<Record> {
        require_hr()?;
        list_named("work_location", true)
    }

    fn create_cost_center(input: Named) -> Record {
        new_centre(input)
    }

    fn update_cost_center(input: CentreChange) -> Record {
        edit_centre(input)
    }

    fn get_cost_center(input: Id) -> Option<Record> {
        db::get("cost_center", &input.id)
    }

    fn list_cost_centers(_: Empty) -> Vec<Record> {
        list_named("cost_center", false)
    }

    fn list_all_cost_centers(_: Empty) -> Vec<Record> {
        require_hr()?;
        list_named("cost_center", true)
    }

    /// Copy free-text location/centre values onto the link fields.
    fn migrate_catalog_links(_: Empty) -> Value {
        require_hr()?;
        run_migrate_catalog_links()
    }
}
