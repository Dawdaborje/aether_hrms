//! Telling people their payslip is ready. The email names the period and where to open the payslip and carries
//! no amounts: pay figures do not belong in an inbox. It goes to the work email, in small pages queued one
//! after another so a large run never has to finish in one call. One person's failure is counted, not fatal.

use aether_sdk::communication;
use aether_sdk::prelude::*;
use aether_sdk::scheduler;

use crate::common::{id_of, int, require, require_finance, text, Record};

const PAGE: u32 = 25;
const QUEUE: &str = "payroll";

#[derive(Deserialize, Serialize)]
struct Page {
    run: String,
    #[serde(default)]
    after: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

/// Queue the first page of emails for a run. Used on approval, and by hand to send again.
pub(crate) fn queue(run: &str) -> Result<()> {
    scheduler::enqueue("email_payslip_page")
        .payload(&Page { run: run.to_string(), after: None })
        .queue(QUEUE)
        .unique_key(&format!("{run}:mail:start:{}", context::current()?.now))
        .send()?;
    Ok(())
}

fn page_job(page: Page) -> Result<Value> {
    let run = require("pay_run", &page.run, "run")?;
    if !matches!(text(&run, "status"), Some("approved" | "paid")) {
        return Ok(json!({ "skipped": "the run is not approved" }));
    }
    let mut filter = aether_sdk::db::Filter::eq("run", page.run.as_str()).and(aether_sdk::db::Filter::eq("attempt", int(&run, "attempt")));
    if let Some(after) = &page.after {
        filter = filter.and(aether_sdk::db::Filter::gt("employee", after.as_str()));
    }
    let rows: Vec<Record> = db::find::<Record>("payslip").matching(filter).order_by("employee").limit(PAGE).all()?;
    let number = text(&run, "number").unwrap_or("payroll").to_string();
    let period = format!("{} to {}", text(&run, "period_start").unwrap_or(""), text(&run, "period_end").unwrap_or(""));
    let correction = text(&run, "correction_of").is_some();
    let (mut sent, mut no_address, mut failed) = (0, 0, 0);
    for slip in &rows {
        let who = text(slip, "employee").unwrap_or_default();
        let person: Option<Record> = plugins::call("hr", "get_employee", &json!({ "id": who })).ok().flatten();
        let Some(address) = person.as_ref().and_then(|p| text(p, "work_email")).filter(|a| a.contains('@')) else {
            no_address += 1;
            continue;
        };
        let name = person.as_ref().and_then(|p| text(p, "display_name")).unwrap_or("there");
        let subject = if correction { format!("A correction to your pay for {period}") } else { format!("Your payslip for {period} is ready") };
        let body = format!(
            "Hello {name},\n\n{}\n\nOpen it under My payslips (/hr/my-payslips) to see each line and how it was worked out.\n\nReference: {number}\n",
            if correction { "A correction was made to your pay for this period." } else { "Your payslip for this period is ready." }
        );
        match communication::email([address]).subject(&subject).text(&body).send() {
            Ok(_) => sent += 1,
            Err(_) => failed += 1,
        }
    }
    if rows.len() as u32 == PAGE {
        if let Some(last) = rows.last().and_then(|r| text(r, "employee")) {
            scheduler::enqueue("email_payslip_page")
                .payload(&Page { run: page.run.clone(), after: Some(last.to_string()) })
                .queue(QUEUE)
                .unique_key(&format!("{}:mail:{last}", page.run))
                .send()?;
        }
    }
    Ok(json!({ "sent": sent, "no_address": no_address, "failed": failed, "more": rows.len() as u32 == PAGE }))
}

handler! {
    fn email_payslip_page(input: Page) -> Value {
        page_job(input)
    }

    /// Send the "payslip ready" emails for an approved run (again).
    fn send_payslip_emails(input: Id) -> Value {
        require_finance()?;
        let run = require("pay_run", &input.id, "run")?;
        if !matches!(text(&run, "status"), Some("approved" | "paid")) {
            return Err(Error::msg("payslips are emailed for an approved run"));
        }
        queue(id_of(&run)?)?;
        Ok(json!({ "queued": true }))
    }
}
