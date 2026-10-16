//! Configurable employee numbers: pattern rules matched by department / employment type.

use aether_sdk::dates::{numbered, parse_date, NaiveDate};
use aether_sdk::prelude::*;

use crate::common::{explain, id_of, next_number, require, text, today, Record};

const FALLBACK_PATTERN: &str = "EMP-{number:5}";
const MODEL: &str = "employee_number_rule";

/// What a hire (or preview) knows when allocating a number.
#[derive(Clone, Debug, Default)]
pub struct NumberContext {
    pub department: Option<String>,
    pub employment_type: Option<String>,
    pub hire_date: String,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct RuleIn {
    name: String,
    pattern: String,
    #[serde(default)]
    department: Option<String>,
    #[serde(default)]
    employment_type: Option<String>,
    #[serde(default)]
    priority: Option<i64>,
    #[serde(default)]
    reset: Option<String>,
    #[serde(default)]
    is_default: Option<bool>,
    #[serde(default = "yes")]
    is_active: bool,
}

#[derive(Deserialize)]
struct RuleChange {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default)]
    department: Option<String>,
    #[serde(default)]
    employment_type: Option<String>,
    #[serde(default)]
    priority: Option<i64>,
    #[serde(default)]
    reset: Option<String>,
    #[serde(default)]
    is_default: Option<bool>,
    #[serde(default)]
    is_active: Option<bool>,
}

#[derive(Deserialize)]
struct PreviewIn {
    #[serde(default)]
    department: Option<String>,
    #[serde(default)]
    employment_type: Option<String>,
    #[serde(default)]
    hire_date: Option<String>,
}

fn yes() -> bool {
    true
}

fn require_manager() -> Result<()> {
    let who = context::current()?;
    if who.has_role("hr.hr_manager") {
        Ok(())
    } else {
        Err(Error::msg("only an HR manager can change employee number rules"))
    }
}

fn same_record(a: &str, b: &str) -> bool {
    !a.is_empty() && (a == b || a.rsplit(':').next() == b.rsplit(':').next())
}

/// Department code for patterns: `code` if set, else a short sanitized name.
pub fn department_token(department_id: Option<&str>) -> Result<String> {
    let Some(id) = department_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(String::new());
    };
    let row = require("department", id, "department")?;
    if let Some(code) = text(&row, "code").map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(sanitize_token(code));
    }
    Ok(sanitize_token(text(&row, "name").unwrap_or("DEPT")))
}

fn sanitize_token(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
        } else if ch == '-' || ch == '_' || ch.is_whitespace() {
            if !out.ends_with('-') && !out.is_empty() {
                out.push('-');
            }
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "X".into()
    } else {
        out.chars().take(20).collect()
    }
}

/// Tokens allowed inside `{…}` (without the braces). `{number:N}` is handled separately.
fn known_token(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "number"
            | "year"
            | "yy"
            | "month"
            | "mm"
            | "department"
            | "department_code"
            | "employment_type"
    ) || name
        .to_ascii_lowercase()
        .strip_prefix("number:")
        .is_some_and(|n| n.parse::<usize>().is_ok())
}

/// Reject unknown `{tokens}` in a pattern.
pub fn validate_pattern(pattern: &str) -> Result<()> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return Err(Error::msg("pattern is required"));
    }
    if pattern.len() > 120 {
        return Err(Error::msg("pattern is too long"));
    }
    let mut rest = pattern;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            return Err(Error::msg("pattern has an unclosed `{`"));
        };
        let token = after[..end].trim();
        if token.is_empty() || !known_token(token) {
            return Err(Error::msg(format!(
                "unknown token `{{{token}}}`: use {{number}}, {{number:N}}, {{year}}, {{yy}}, {{month}}, {{mm}}, {{department_code}}, {{department}}, or {{employment_type}}"
            )));
        }
        rest = &after[end + 1..];
    }
    Ok(())
}

fn number_width(pattern: &str) -> usize {
    let lower = pattern.to_ascii_lowercase();
    let mut rest = lower.as_str();
    while let Some(start) = rest.find("{number") {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else { break };
        let token = &after[..end];
        if token == "number" {
            return 5;
        }
        if let Some(n) = token.strip_prefix("number:") {
            if let Ok(width) = n.parse::<usize>() {
                return width.clamp(1, 12);
            }
        }
        rest = &after[end + 1..];
    }
    5
}

fn parse_reset(value: Option<&str>) -> &'static str {
    match value.map(str::trim).unwrap_or("never") {
        "yearly" => "yearly",
        "monthly" => "monthly",
        _ => "never",
    }
}

fn specificity(rule: &Record) -> i64 {
    let mut score = 0;
    if text(rule, "department").is_some() {
        score += 2;
    }
    if text(rule, "employment_type").is_some() {
        score += 1;
    }
    score
}

fn rule_matches(rule: &Record, ctx: &NumberContext) -> bool {
    if rule.get("is_active") == Some(&json!(false)) {
        return false;
    }
    if let Some(dept) = text(rule, "department") {
        match ctx.department.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(hire_dept) if same_record(dept, hire_dept) => {}
            _ => return false,
        }
    }
    if let Some(kind) = text(rule, "employment_type") {
        match ctx.employment_type.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(hire_kind) if hire_kind.eq_ignore_ascii_case(kind) => {}
            _ => return false,
        }
    }
    true
}

/// Best matching active rule, or `None` to use the built-in fallback pattern.
pub fn pick_rule(rules: &[Record], ctx: &NumberContext) -> Option<Record> {
    let mut matched: Vec<&Record> = rules.iter().filter(|r| rule_matches(r, ctx)).collect();
    if matched.is_empty() {
        return None;
    }
    matched.sort_by(|a, b| {
        specificity(b)
            .cmp(&specificity(a))
            .then(
                b.get("priority")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    .cmp(&a.get("priority").and_then(Value::as_i64).unwrap_or(0)),
            )
            .then_with(|| {
                let a_def = a.get("is_default") == Some(&json!(true));
                let b_def = b.get("is_default") == Some(&json!(true));
                b_def.cmp(&a_def)
            })
            .then_with(|| {
                text(a, "name")
                    .unwrap_or_default()
                    .cmp(text(b, "name").unwrap_or_default())
            })
    });
    matched.first().map(|r| (*r).clone())
}

fn short_id(id: &str) -> String {
    let s = id.rsplit(':').next().unwrap_or(id);
    s.chars().take(16).collect()
}

fn series_key(rule_id: &str, ctx: &NumberContext, reset: &str, day: NaiveDate) -> String {
    let dept = ctx
        .department
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| short_id(s))
        .unwrap_or_else(|| "*".into());
    let kind = ctx
        .employment_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(8).collect::<String>().to_ascii_lowercase())
        .unwrap_or_else(|| "*".into());
    let year = match reset {
        "yearly" | "monthly" => day.format("%Y").to_string(),
        _ => "*".into(),
    };
    let month = match reset {
        "monthly" => day.format("%m").to_string(),
        _ => "*".into(),
    };
    // Fits counter.key max_length 60.
    format!("en:{}|d:{}|t:{}|y:{}|m:{}", short_id(rule_id), dept, kind, year, month)
}

/// Expand pattern tokens. `{number…}` becomes the padded sequence value.
pub fn expand_pattern(
    pattern: &str,
    number: i64,
    width: usize,
    day: NaiveDate,
    department_code: &str,
    employment_type: &str,
) -> String {
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            out.push_str(rest);
            return out;
        };
        let token = after[..end].trim();
        let lower = token.to_ascii_lowercase();
        let replacement = if lower == "number" || lower.starts_with("number:") {
            numbered("", number, width)
        } else {
            match lower.as_str() {
                "year" => day.format("%Y").to_string(),
                "yy" => day.format("%y").to_string(),
                "month" | "mm" => day.format("%m").to_string(),
                "department" | "department_code" => department_code.to_string(),
                "employment_type" => employment_type.to_string(),
                _ => format!("{{{token}}}"),
            }
        };
        out.push_str(&replacement);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

fn load_rules() -> Result<Vec<Record>> {
    db::find(MODEL).order_by("name").limit(500).all()
}

fn context_day(ctx: &NumberContext) -> Result<NaiveDate> {
    let raw = ctx.hire_date.trim();
    if raw.is_empty() {
        parse_date(&today()?)
    } else {
        parse_date(raw)
    }
}

/// Allocate the next employee number for this hire context.
pub fn allocate(ctx: &NumberContext) -> Result<String> {
    let day = context_day(ctx)?;
    let rules = load_rules()?;
    let rule = pick_rule(&rules, ctx);
    let (pattern, reset, rule_id) = match &rule {
        Some(row) => (
            text(row, "pattern").unwrap_or(FALLBACK_PATTERN).to_string(),
            parse_reset(text(row, "reset")),
            id_of(row)?.to_string(),
        ),
        None => (FALLBACK_PATTERN.to_string(), "never", "fallback".to_string()),
    };
    let width = number_width(&pattern);
    let series = series_key(&rule_id, ctx, reset, day);
    // Empty prefix: digits only; expand_pattern places them where `{number}` sits.
    let seq = next_number(&series, "", width)?;
    let number = seq.parse::<i64>().unwrap_or(1);
    let dept_code = department_token(ctx.department.as_deref())?;
    let emp_type = ctx
        .employment_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("")
        .to_ascii_lowercase();
    Ok(expand_pattern(&pattern, number, width, day, &dept_code, &emp_type))
}

/// Preview the next number without consuming a counter (uses next+1 peek via pattern only with placeholder).
fn preview(ctx: &NumberContext) -> Result<Record> {
    let day = context_day(ctx)?;
    let rules = load_rules()?;
    let rule = pick_rule(&rules, ctx);
    let (pattern, name) = match &rule {
        Some(row) => (
            text(row, "pattern").unwrap_or(FALLBACK_PATTERN).to_string(),
            text(row, "name").unwrap_or("Rule").to_string(),
        ),
        None => (FALLBACK_PATTERN.to_string(), "Built-in fallback".into()),
    };
    let width = number_width(&pattern);
    let dept_code = department_token(ctx.department.as_deref())?;
    let emp_type = ctx
        .employment_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("")
        .to_ascii_lowercase();
    // Example uses 1 so the shape is clear without bumping counters.
    let example = expand_pattern(&pattern, 1, width, day, &dept_code, &emp_type);
    Ok(json!({
        "rule": name,
        "pattern": pattern,
        "preview": example,
        "note": "Preview shows sequence 1; the next hire gets the next number in the series.",
    }))
}

fn clear_other_defaults(except: Option<&str>) -> Result<()> {
    for row in load_rules()? {
        if row.get("is_default") != Some(&json!(true)) {
            continue;
        }
        let id = id_of(&row)?;
        if except.is_some_and(|e| same_record(e, id)) {
            continue;
        }
        db::update::<Record>(MODEL, id, &json!({ "is_default": false }))?;
    }
    Ok(())
}

fn normalize_employment_type(value: Option<&str>) -> Result<Option<String>> {
    let Some(raw) = value.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let allowed = ["permanent", "contract", "temporary", "intern", "casual"];
    if !allowed.iter().any(|a| a.eq_ignore_ascii_case(raw)) {
        return Err(Error::msg("employment type must be permanent, contract, temporary, intern or casual"));
    }
    Ok(Some(raw.to_ascii_lowercase()))
}

fn new_rule(input: RuleIn) -> Result<Record> {
    require_manager()?;
    let name = input.name.trim();
    if name.is_empty() {
        return Err(Error::msg("name is required"));
    }
    validate_pattern(&input.pattern)?;
    if let Some(dept) = input.department.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        require("department", dept, "department")?;
    }
    let employment_type = normalize_employment_type(input.employment_type.as_deref())?;
    let reset = parse_reset(input.reset.as_deref());
    let is_default = input.is_default.unwrap_or(false);
    if is_default {
        clear_other_defaults(None)?;
    }
    explain(
        db::create(
            MODEL,
            &json!({
                "name": name,
                "pattern": input.pattern.trim(),
                "department": input.department.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                "employment_type": employment_type,
                "priority": input.priority.unwrap_or(0),
                "reset": reset,
                "is_default": is_default,
                "is_active": input.is_active,
            }),
        ),
        "could not create the employee number rule",
    )
}

fn edit_rule(input: RuleChange) -> Result<Record> {
    require_manager()?;
    let _ = require(MODEL, &input.id, "employee number rule")?;
    let mut data = serde_json::Map::new();
    if let Some(name) = input.name {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::msg("name is required"));
        }
        data.insert("name".into(), json!(name));
    }
    if let Some(pattern) = input.pattern {
        validate_pattern(&pattern)?;
        data.insert("pattern".into(), json!(pattern.trim()));
    }
    if let Some(department) = input.department {
        let department = department.trim();
        if department.is_empty() {
            data.insert("department".into(), Value::Null);
        } else {
            require("department", department, "department")?;
            data.insert("department".into(), json!(department));
        }
    }
    if let Some(employment_type) = input.employment_type {
        data.insert(
            "employment_type".into(),
            match normalize_employment_type(Some(&employment_type))? {
                Some(v) => json!(v),
                None => Value::Null,
            },
        );
    }
    if let Some(priority) = input.priority {
        data.insert("priority".into(), json!(priority));
    }
    if let Some(reset) = input.reset {
        data.insert("reset".into(), json!(parse_reset(Some(&reset))));
    }
    if let Some(is_default) = input.is_default {
        if is_default {
            clear_other_defaults(Some(&input.id))?;
        }
        data.insert("is_default".into(), json!(is_default));
    }
    if let Some(is_active) = input.is_active {
        data.insert("is_active".into(), json!(is_active));
    }
    if data.is_empty() {
        return Err(Error::msg("nothing to change"));
    }
    db::update(MODEL, &input.id, &Value::Object(data))?.ok_or_else(|| Error::msg("the rule is gone"))
}

fn seed_default() -> Result<Value> {
    let existing: Vec<Record> = db::find(MODEL).limit(1).all()?;
    if !existing.is_empty() {
        return Ok(json!({ "created": false, "reason": "rules already exist" }));
    }
    let row = db::create(
        MODEL,
        &json!({
            "name": "Default",
            "pattern": FALLBACK_PATTERN,
            "priority": 0,
            "reset": "never",
            "is_default": true,
            "is_active": true,
        }),
    )?;
    Ok(json!({ "created": true, "id": id_of(&row)? }))
}

handler! {
    fn create_employee_number_rule(input: RuleIn) -> Record {
        new_rule(input)
    }

    fn update_employee_number_rule(input: RuleChange) -> Record {
        edit_rule(input)
    }

    fn get_employee_number_rule(input: Id) -> Option<Record> {
        db::get(MODEL, &input.id)
    }

    fn list_employee_number_rules(_: Empty) -> Vec<Record> {
        load_rules()
    }

    fn list_all_employee_number_rules(_: Empty) -> Vec<Record> {
        require_manager()?;
        load_rules()
    }

    /// Example expansion for a sample hire (does not consume a counter).
    fn preview_employee_number(input: PreviewIn) -> Record {
        preview(&NumberContext {
            department: input.department,
            employment_type: input.employment_type,
            hire_date: input.hire_date.unwrap_or_default(),
        })
    }

    fn seed_employee_number_rules(_: Empty) -> Value {
        seed_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_known_tokens() {
        assert!(validate_pattern("EMP-{number:4}").is_ok());
        assert!(validate_pattern("emp-{department_code}-{year}-{number}").is_ok());
        assert!(validate_pattern("X-{yy}-{mm}-{employment_type}-{number:3}").is_ok());
    }

    #[test]
    fn validate_rejects_unknown() {
        assert!(validate_pattern("EMP-{foo}").is_err());
        assert!(validate_pattern("EMP-{number:}").is_err());
        assert!(validate_pattern("").is_err());
    }

    #[test]
    fn expand_builds_department_year_number() {
        let day = parse_date("2026-03-15").unwrap();
        let out = expand_pattern(
            "emp-{department_code}-{year}-{number:4}",
            7,
            4,
            day,
            "DEP-A",
            "permanent",
        );
        assert_eq!(out, "emp-DEP-A-2026-0007");
    }

    #[test]
    fn pick_prefers_department_scope() {
        let global = json!({
            "id": "r1", "name": "Default", "pattern": "EMP-{number:5}",
            "is_active": true, "is_default": true, "priority": 100
        });
        let dept = json!({
            "id": "r2", "name": "Dept A", "pattern": "A-{number:3}",
            "department": "department:a", "is_active": true, "priority": 0
        });
        let ctx = NumberContext {
            department: Some("department:a".into()),
            employment_type: Some("permanent".into()),
            hire_date: "2026-01-01".into(),
        };
        let picked = pick_rule(&[global, dept], &ctx).unwrap();
        assert_eq!(text(&picked, "name"), Some("Dept A"));
    }

    #[test]
    fn number_width_from_pattern() {
        assert_eq!(number_width("EMP-{number:4}"), 4);
        assert_eq!(number_width("EMP-{number}"), 5);
        assert_eq!(number_width("EMP-"), 5);
    }

    #[test]
    fn sanitize_department_code() {
        assert_eq!(sanitize_token("dep a"), "DEP-A");
        assert_eq!(sanitize_token("Finance"), "FINANCE");
    }
}
