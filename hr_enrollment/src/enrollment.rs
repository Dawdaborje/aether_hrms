//! HR's side: start an enrollment, invite, send sections back, approve, cancel.

use aether_sdk::db::Filter;
use aether_sdk::prelude::*;

use crate::common::{explain, get, id_of, in_days, now, now_text, pick, require_hr, text, Empty, Id, Record, HR_MANAGER};
use crate::{rules, token};

const START: &[&str] = &[
    "first_name", "middle_name", "last_name", "email", "work_email", "phone", "department", "job", "position", "manager",
    "work_location", "cost_center", "employment_type", "hire_date", "probation_end", "source",
];
/// What the hire needs besides the sections.
const TERMS: &[&str] = &[
    "first_name", "middle_name", "last_name", "phone", "department", "job", "position", "manager",
    "work_location", "cost_center", "employment_type", "hire_date", "probation_end",
];

/// Leave out empty values: a form sends "" for a field nobody touched, and a link field refuses it.
fn clean(mut value: Record) -> Record {
    if let Some(fields) = value.as_object_mut() {
        fields.retain(|_, v| !(v.is_null() || v.as_str().is_some_and(|s| s.trim().is_empty())));
        for v in fields.values_mut() {
            if let Some(s) = v.as_str() {
                *v = json!(s.trim());
            }
        }
        // A phone that is only the country's dialling code is no phone.
        if fields.get("phone").and_then(Value::as_str).is_some_and(|p| rules::phone(p) == rules::Phone::Empty) {
            fields.remove("phone");
        }
    }
    value
}

/// A row as HR sees it. The token hash never leaves the plugin.
fn shown(mut row: Record) -> Record {
    let name = format!("{} {}", text(&row, "first_name").unwrap_or(""), text(&row, "last_name").unwrap_or(""));
    if let Some(fields) = row.as_object_mut() {
        fields.remove("token_hash");
        fields.insert("name".into(), json!(name.trim()));
    }
    row
}

fn sections_of(id: &str) -> Result<Vec<Record>> {
    // A link filter needs the row's full record id; pages pass the bare key, so read it from the row.
    let row = get(id)?;
    db::find("enrollment_section").filter("enrollment", id_of(&row)?).limit(20).all()
}

fn settings() -> Result<Record> {
    Ok(db::find::<Record>("enr_setting").first()?.unwrap_or_else(|| json!({ "invite_days": 14 })))
}

fn change(id: &str, from: &str, to: &str, mut data: Record) -> Result<Record> {
    if !rules::can_move(from, to) {
        return Err(Error::msg(format!("an enrollment that is {} cannot become {}", from.replace('_', " "), to.replace('_', " "))));
    }
    data["status"] = json!(to);
    db::update::<Record>("enrollment", id, &data)?.ok_or_else(|| Error::msg("the enrollment is gone"))
}

#[derive(Deserialize, Default)]
struct Search {
    #[serde(default)]
    status: Option<String>,
    #[serde(default, alias = "q")]
    text: Option<String>,
    /// Kernel filter JSON from the list's Filters menu.
    #[serde(default)]
    filter: Option<Value>,
}

/// A position names its job and (usually) its department. Whatever job or department is chosen with it must be those,
/// or approval would create terms that disagree with the seat. The department stays optional.
fn check_placement(data: &Record) -> Result<()> {
    let Some(position) = text(data, "position") else { return Ok(()) };
    let seat: Option<Record> = plugins::call("hr", "get_position", &json!({ "id": position }))?;
    let seat = seat.ok_or_else(|| Error::msg("there is no such position"))?;
    let key = |id: &str| id.rsplit_once(':').map_or(id, |(_, k)| k).to_string();
    for (field, what) in [("job", "job"), ("department", "department")] {
        if let (Some(chosen), Some(held)) = (text(data, field), text(&seat, field)) {
            if key(chosen) != key(held) {
                return Err(Error::msg(format!("the position belongs to a different {what}: pick the position's own {what}, or leave it out")));
            }
        }
    }
    Ok(())
}

/// The minimum working age HR has now. A failure to read it is passed on: a link is never issued under an age that
/// is only guessed.
fn current_minimum_age() -> Result<u64> {
    let setting: Record = plugins::call("hr", "get_hr_setting", &json!({}))?;
    Ok(setting.get("minimum_working_age").and_then(Value::as_u64).filter(|a| *a > 0).unwrap_or(14))
}

fn start(input: Record) -> Result<Record> {
    require_hr()?;
    let mut data = clean(pick(&input, START));
    for required in ["first_name", "last_name", "email", "job"] {
        if text(&data, required).is_none() {
            return Err(Error::msg(format!("{} is required", if required == "job" { "the job (designation)".to_string() } else { required.replace('_', " ") })));
        }
    }
    if text(&data, "phone").is_some_and(|p| rules::phone(p) == rules::Phone::Invalid) {
        return Err(Error::msg("the phone is a phone number in international form, such as +2348012345678"));
    }
    let email = text(&data, "email").unwrap_or_default().to_lowercase();
    if !rules::fits_email(&email) {
        return Err(Error::msg("give a valid email address"));
    }
    data["email"] = json!(email);
    if let Some(work) = text(&data, "work_email").map(str::to_lowercase) {
        if !rules::fits_email(&work) {
            return Err(Error::msg("give a valid work email address, or leave it empty"));
        }
        data["work_email"] = json!(work);
    }
    check_placement(&data)?;
    let open: Vec<Record> = db::find("enrollment").filter("email", email.as_str()).limit(50).all()?;
    if open.iter().any(|row| rules::OPEN.contains(&text(row, "status").unwrap_or(""))) {
        return Err(Error::msg("this person already has an enrollment in progress"));
    }
    data["status"] = json!("draft");
    let row: Record = explain(db::create("enrollment", &data), "could not start this enrollment")?;
    events::emit("enrollment_started", &json!({ "enrollment": row["id"] }))?;
    Ok(shown(row))
}

fn list(input: Search) -> Result<Vec<Record>> {
    require_hr()?;
    let mut find = db::find::<Record>("enrollment");
    // The Filters menu sends its choice as `filter`. With no status chosen the list shows
    // what is waiting for HR; the "all" option shows everything.
    let mut chosen = input.filter.as_ref().and_then(Value::as_object).cloned().unwrap_or_default();
    let status = chosen.remove("status");
    let all = status.as_ref().and_then(Value::as_str) == Some("all");
    let status = match status {
        _ if all => None,
        Some(s) => Some(s),
        None => input.status.as_deref().filter(|s| !s.is_empty()).map(|s| json!(s)).or_else(|| Some(json!("submitted"))),
    };
    if let Some(status) = status {
        chosen.insert("status".to_string(), status);
    }
    find = find.matching(Filter::from_json(Value::Object(chosen)));
    let needle = input.text.as_deref().map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty());
    let rows = find.order_by("last_name").limit(300).all()?;
    Ok(rows
        .into_iter()
        .map(shown)
        .filter(|row| match &needle {
            Some(n) => [text(row, "name"), text(row, "email")].iter().flatten().any(|v| v.to_lowercase().contains(n.as_str())),
            None => true,
        })
        .collect())
}

/// The enrollment with what the new hire entered laid beside it.
fn show(id: &str) -> Result<Record> {
    require_hr()?;
    let mut row = shown(get(id)?);
    let mut summary = Vec::new();
    for section in sections_of(id)? {
        let kind = text(&section, "kind").unwrap_or("").to_string();
        let status = text(&section, "status").unwrap_or("").replace('_', " ");
        summary.push(format!("{kind}: {status}"));
        if let (Some(data), Some(fields)) = (section.get("data").and_then(Value::as_object), row.as_object_mut()) {
            for (key, value) in data {
                fields.insert(key.clone(), value.clone());
            }
        }
    }
    row["section_summary"] = json!(summary.join(" · "));
    row["status_note"] = json!(match text(&row, "status").unwrap_or("") {
        "draft" => "Started by HR. No invitation has been sent yet.",
        "invited" => "Invitation sent. Waiting for the person to open the link.",
        "in_progress" => "The person has opened the form but not submitted it.",
        "submitted" => "Submitted. Waiting for HR to review: approve and hire, or send back.",
        "changes_requested" => "Sent back to the person for corrections.",
        "approved" => "Approved. The employee has been created.",
        "expired" => "The link ran out before the form was submitted. Invite again.",
        "cancelled" => "Cancelled. The link no longer works.",
        _ => "",
    });
    Ok(row)
}

/// Make a fresh link, store only its hash and email the link. An old link stops working.
/// Mint a fresh single-use token for the enrollment (any earlier link stops working) and return the
/// saved row, the link and when it expires. Nothing is sent.
fn rotate(row: &Record, next_status: &str) -> Result<(Record, String, String)> {
    let id = id_of(row)?;
    let setting = settings()?;
    // The organization's general `domain` setting; a site address saved here earlier still works.
    let general = aether_sdk::settings::get_text("domain")?;
    let base = general
        .as_deref()
        .or_else(|| text(&setting, "base_url"))
        .map(|b| b.trim().trim_end_matches('/'))
        .filter(|b| !b.is_empty())
        .ok_or_else(|| Error::msg("set the Domain in the general settings before sending invitations"))?;
    let days = setting.get("invite_days").and_then(Value::as_i64).unwrap_or(14).clamp(1, 90);
    // The minimum working age now in force, kept on the enrollment for the public form to check against.
    let minimum_age = current_minimum_age()?;
    let secret = token::new_token(id)?;
    let expires = in_days(days)?;
    let saved = db::update::<Record>(
        "enrollment",
        id,
        &json!({ "token_hash": token::hash(&secret), "minimum_age": minimum_age, "invited_at": now_text()?, "expires_at": expires, "status": next_status }),
    )?
    .ok_or_else(|| Error::msg("the enrollment is gone"))?;
    Ok((saved, format!("{base}/enroll/{secret}"), expires))
}

fn issue(row: &Record, note: Option<&str>, next_status: &str) -> Result<Record> {
    let (saved, link, expires) = rotate(row, next_status)?;
    let first = text(row, "first_name").unwrap_or("");
    let intro = match note {
        Some(note) => format!("Hello {first},\n\nA few things need your attention before we can finish your enrollment:\n\n{note}\n"),
        None => format!("Hello {first},\n\nWelcome. Please complete your enrollment so we can set you up before you start.\n"),
    };
    let body = format!("{intro}\nOpen your enrollment form:\n{link}\n\nThe link is yours alone and works until {}.", &expires[..10]);
    let to = text(row, "email").unwrap_or_default().to_string();
    communication::email([to])
        .subject(if note.is_some() { "Changes needed on your enrollment" } else { "Complete your enrollment" })
        .text(&body)
        .send()
        .map_err(|error| Error::msg(format!("the email could not be sent ({error}); the old link no longer works, so send the invitation again")))?;
    Ok(shown(saved))
}

fn next_for_invite(row: &Record) -> Result<&'static str> {
    let status = text(row, "status").unwrap_or("");
    let next = match status {
        "draft" | "invited" | "expired" => "invited",
        "in_progress" => "in_progress",
        "changes_requested" => "changes_requested",
        _ => return Err(Error::msg(format!("an enrollment that is {} cannot be invited", status.replace('_', " ")))),
    };
    if status != next && !rules::can_move(status, next) {
        return Err(Error::msg("this enrollment cannot be invited"));
    }
    // Reopening an expired enrollment must not leave two open ones for the same person, both able to be hired.
    if status == "expired" {
        let email = text(row, "email").unwrap_or_default().to_lowercase();
        let this = text(row, "id").unwrap_or_default();
        let key = |id: &str| id.rsplit_once(':').map_or(id, |(_, k)| k).to_string();
        let others: Vec<Record> = db::find("enrollment").filter("email", email.as_str()).limit(50).all()?;
        if others
            .iter()
            .any(|o| text(o, "id").map(key) != Some(key(this)) && rules::OPEN.contains(&text(o, "status").unwrap_or("")))
        {
            return Err(Error::msg("this person already has another enrollment in progress: finish or cancel it before reopening this one"));
        }
    }
    Ok(next)
}

/// A fresh link for HR to copy, without emailing it. The earlier link stops working.
fn copy_link(input: Id) -> Result<Value> {
    require_hr()?;
    let row = get(&input.id)?;
    let next = next_for_invite(&row)?;
    let (_, link, _) = rotate(&row, next)?;
    Ok(json!({ "copy_text": link, "copied_notice": "Invitation link copied. Any earlier link no longer works." }))
}

fn invite(input: Id) -> Result<Record> {
    require_hr()?;
    let row = get(&input.id)?;
    let next = next_for_invite(&row)?;
    issue(&row, None, next)
}

#[derive(Deserialize)]
struct Changes {
    id: String,
    note: String,
    #[serde(default)]
    personal: bool,
    #[serde(default)]
    emergency: bool,
    #[serde(default)]
    identity: bool,
    #[serde(default)]
    bank: bool,
    #[serde(default)]
    acknowledgement: bool,
}

fn ask_for_changes(input: Changes) -> Result<Record> {
    require_hr()?;
    let row = get(&input.id)?;
    if text(&row, "status") != Some("submitted") {
        return Err(Error::msg("only a submitted enrollment can be sent back"));
    }
    let note = input.note.trim();
    if note.is_empty() || note.chars().count() > 1000 {
        return Err(Error::msg("say what needs to change, in up to 1000 characters"));
    }
    let chosen: Vec<&str> = [
        ("personal", input.personal), ("emergency", input.emergency), ("identity", input.identity),
        ("bank", input.bank), ("acknowledgement", input.acknowledgement),
    ]
    .into_iter()
    .filter_map(|(kind, on)| on.then_some(kind))
    .collect();
    if chosen.is_empty() {
        return Err(Error::msg("pick at least one section to send back"));
    }
    let saved = issue(&row, Some(note), "changes_requested")?;
    for section in sections_of(&input.id)? {
        if chosen.contains(&text(&section, "kind").unwrap_or("")) {
            db::update::<Record>("enrollment_section", id_of(&section)?, &json!({ "status": "changes_requested", "comment": note }))?;
        }
    }
    db::update::<Record>("enrollment", &input.id, &json!({ "review_note": note }))?;
    Ok(shown(saved))
}

/// The country a person wrote as their nationality. HR keeps nationality as a country, the form takes text:
/// an exact name or code is used; otherwise the shortest start of the word (down to four letters) that
/// names one country, so "Japanese" finds Japan. Anything unclear is refused, never guessed.
fn country_id(typed: &str) -> Result<String> {
    if typed.contains(':') {
        return Ok(typed.to_string());
    }
    let find = |q: &str| -> Result<Vec<Record>> {
        let found: Record = plugins::call("country", "list_countries", &json!({ "q": q, "limit": 10, "all": true }))?;
        Ok(found.get("rows").and_then(Value::as_array).cloned().unwrap_or_default())
    };
    let lower = typed.trim().to_lowercase();
    let rows = find(typed.trim())?;
    let exact: Vec<&Record> = rows
        .iter()
        .filter(|c| ["name", "code", "code3"].iter().any(|f| text(c, f).is_some_and(|v| v.to_lowercase() == lower)))
        .collect();
    if let [only] = exact.as_slice() {
        return Ok(id_of(only)?.to_string());
    }
    let mut cut = lower.chars().count().saturating_sub(1);
    while cut >= 4 {
        let stem: String = lower.chars().take(cut).collect();
        let rows = find(&stem)?;
        if let [only] = rows.as_slice() {
            return Ok(id_of(only)?.to_string());
        }
        cut -= 1;
    }
    Err(Error::msg(format!("the nationality \"{typed}\" is not a country HR knows: send the personal section back to correct it")))
}

/// The page of the employee an approved enrollment created.
fn employee_page(input: Id) -> Result<Value> {
    require_hr()?;
    let row = get(&input.id)?;
    let employee = text(&row, "employee").ok_or_else(|| Error::msg("this enrollment has no employee yet: it has not been approved"))?;
    let key = employee.rsplit_once(':').map_or(employee, |(_, key)| key);
    Ok(json!({ "open_page": format!("/hr/employees/{key}") }))
}

fn approve(input: Id) -> Result<Record> {
    require_hr()?;
    let row = get(&input.id)?;
    if text(&row, "status") != Some("submitted") {
        return Err(Error::msg("only a submitted enrollment can be approved"));
    }
    let sections = sections_of(&input.id)?;
    for needed in ["personal", "emergency", "acknowledgement"] {
        if !sections.iter().any(|s| text(s, "kind") == Some(needed)) {
            return Err(Error::msg(format!("the {needed} section is missing")));
        }
    }
    if sections.iter().any(|s| text(s, "status") == Some("changes_requested")) {
        return Err(Error::msg("a section is still waiting for changes"));
    }
    let mut terms = clean(pick(&row, TERMS));
    check_placement(&terms)?;
    // The invitation address is the person's own contact; only an address HR gave as the work email becomes one.
    if let Some(contact) = text(&row, "email") {
        terms["contact_email"] = json!(contact);
    }
    if let Some(work) = text(&row, "work_email").map(str::trim).filter(|w| !w.is_empty()) {
        terms["email"] = json!(work);
    }
    for section in &sections {
        if let (Some(data), Some(fields)) = (section.get("data").and_then(Value::as_object), terms.as_object_mut()) {
            for (key, value) in data {
                fields.insert(key.clone(), value.clone());
            }
        }
    }
    if let Some(typed) = text(&terms, "nationality").map(str::to_string) {
        terms["nationality"] = json!(country_id(&typed)?);
    }
    // HR refuses with its own reason (a hold, a missing field, a duplicate), which reaches the caller.
    // Nothing here has changed yet, so the enrollment stays submitted and can be fixed and approved again.
    let hired: Record = plugins::call("hr", "hire_employee", &terms)?;
    let employee = id_of(&hired)?.to_string();
    let actor = context::current()?.actor.id.unwrap_or_default();
    let saved = change(
        &input.id,
        "submitted",
        "approved",
        json!({ "employee": employee, "reviewed_by": actor, "token_hash": null, "review_note": null }),
    )?;
    for section in &sections {
        db::update::<Record>("enrollment_section", id_of(section)?, &json!({ "status": "accepted" }))?;
    }
    events::emit("enrollment_approved", &json!({ "enrollment": input.id, "employee": employee }))?;
    Ok(shown(saved))
}

fn cancel(input: Id) -> Result<Record> {
    require_hr()?;
    let row = get(&input.id)?;
    let actor = context::current()?.actor.id.unwrap_or_default();
    let saved = change(&input.id, text(&row, "status").unwrap_or(""), "cancelled", json!({ "token_hash": null, "reviewed_by": actor }))?;
    Ok(shown(saved))
}

/// Close enrollments whose link has run out, so the token stops working.
fn tick() -> Result<u64> {
    let moment = now_text()?;
    let _ = now()?;
    let mut closed = 0;
    for status in rules::OPEN_FOR_HIRE {
        let due: Vec<Record> = db::find("enrollment")
            .matching(Filter::eq("status", *status).and(Filter::lt("expires_at", moment.as_str())))
            .limit(500)
            .all()?;
        for row in due {
            db::update::<Record>("enrollment", id_of(&row)?, &json!({ "status": "expired", "token_hash": null }))?;
            closed += 1;
        }
    }
    Ok(closed)
}

/// HR changed a setting: open enrollments take the minimum working age now in force, so the form's date window and the
/// check at submission follow it even for a link issued earlier.
fn refresh_minimum_age() -> Result<u64> {
    let minimum = current_minimum_age()?;
    let mut changed = 0;
    for status in rules::OPEN_FOR_HIRE {
        let rows: Vec<Record> = db::find("enrollment").filter("status", *status).limit(1000).all()?;
        for row in rows {
            if row.get("minimum_age").and_then(Value::as_u64) != Some(minimum) {
                db::update::<Record>("enrollment", id_of(&row)?, &json!({ "minimum_age": minimum }))?;
                changed += 1;
            }
        }
    }
    Ok(changed)
}

fn read_settings() -> Result<Record> {
    require_hr()?;
    let mut setting = settings()?;
    if setting.get("invite_days").is_none_or(Value::is_null) {
        setting["invite_days"] = json!(14);
    }
    Ok(setting)
}

fn save_settings(input: Record) -> Result<Record> {
    context::current()?.require_role(HR_MANAGER)?;
    let mut data = clean(pick(&input, &["base_url", "invite_days"]));
    if let Some(base) = text(&data, "base_url") {
        if !(base.starts_with("https://") || base.starts_with("http://")) || base.len() > 200 {
            return Err(Error::msg("the site address starts with https:// (or http:// for a test server)"));
        }
        data["base_url"] = json!(base.trim_end_matches('/'));
    }
    match db::find::<Record>("enr_setting").first()? {
        Some(row) => db::update("enr_setting", id_of(&row)?, &data)?.ok_or_else(|| Error::msg("the setting is gone")),
        None => explain(db::create("enr_setting", &data), "could not save the settings"),
    }
}

handler! {
    /// HR starts an enrollment with the person's name, email and the job they join.
    fn start_enrollment(input: Record) -> Record {
        start(input)
    }

    fn list_enrollments(input: Search) -> Vec<Record> {
        list(input)
    }

    fn get_enrollment(input: Id) -> Record {
        show(&input.id)
    }

    /// Email the person a fresh single-use link. An earlier link stops working.
    fn invite_enrollment(input: Id) -> Record {
        invite(input)
    }

    /// Send chosen sections back with a note; the person gets a new link.
    fn request_enrollment_changes(input: Changes) -> Record {
        ask_for_changes(input)
    }

    /// Hire the person through HR with what they entered. HR's holds and checks apply.
    /// Open the employee record an approved enrollment created.
    fn open_enrollment_employee(input: Id) -> Value {
        employee_page(input)
    }

    fn approve_enrollment(input: Id) -> Record {
        approve(input)
    }

    /// A new single-use link for HR to copy; the earlier link stops working. Nothing is emailed.
    fn copy_enrollment_link(input: Id) -> Value {
        copy_link(input)
    }

    fn cancel_enrollment(input: Id) -> Record {
        cancel(input)
    }

    fn enrollment_tick(_: Empty) -> Value {
        Ok(json!({ "expired": tick()? }))
    }

    /// Listener for `hr.hr_setting_changed`.
    fn on_hr_setting_changed(_: Record) -> Value {
        Ok(json!({ "refreshed": refresh_minimum_age()? }))
    }

    fn get_enrollment_settings(_: Empty) -> Record {
        read_settings()
    }

    fn save_enrollment_settings(input: Record) -> Record {
        save_settings(input)
    }
}
