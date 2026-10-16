//! A review of a calculated run before it is approved: what changed against the person's pay in the
//! previous approved run, who is new, who dropped out, and who has a problem a clerk would want to see
//! (nothing to pay, a shortfall, no bank account on file). It only reads; nothing here blocks approval.

use std::collections::BTreeMap;

use aether_sdk::db::Filter;
use aether_sdk::decimal::Decimal;
use aether_sdk::prelude::*;

use crate::common::{id_of, int, require, require_payroll_staff, text, Record};

#[derive(Deserialize)]
struct Review {
    #[serde(alias = "id")]
    run: String,
    /// A change in net pay above this many percent is flagged; 20 when left out.
    #[serde(default)]
    threshold: Option<i64>,
}

fn dec(row: &Record, field: &str) -> Decimal {
    match row.get(field) {
        Some(Value::String(s)) => Decimal::parse(s).unwrap_or(Decimal::zero(2)),
        Some(other) => Decimal::parse(&other.to_string()).unwrap_or(Decimal::zero(2)),
        None => Decimal::zero(2),
    }
}

fn slips_of(run: &Record) -> Result<BTreeMap<String, Record>> {
    let rows: Vec<Record> = db::find::<Record>("payslip").filter("run", id_of(run)?).filter("attempt", int(run, "attempt")).limit(2000).all()?;
    Ok(rows.into_iter().filter_map(|r| text(&r, "employee").map(|e| (e.to_string(), r.clone()))).collect())
}

fn review(input: Review) -> Result<Value> {
    require_payroll_staff()?;
    let run = require("pay_run", &input.run, "run")?;
    let limit = input.threshold.unwrap_or(20).clamp(1, 1000);
    let now = slips_of(&run)?;
    // The latest earlier run that was approved or paid.
    let earlier: Vec<Record> = db::find::<Record>("pay_run")
        .matching(Filter::one_of("status", ["approved", "paid"]).and(Filter::lt("period_end", text(&run, "period_end").unwrap_or_default())))
        .order_by("-period_end")
        .limit(1)
        .all()?;
    let before = match earlier.first() {
        Some(r) => slips_of(r)?,
        None => BTreeMap::new(),
    };
    let ids: Vec<String> = now.keys().cloned().collect();
    // Bank accounts are for payroll finance; a reviewer without that access simply gets no bank flag.
    let accounts: Option<Vec<Record>> = plugins::call("hr_documents", "payment_accounts", &json!({ "employees": ids })).ok();
    let with_account: std::collections::BTreeSet<String> = accounts
        .iter()
        .flatten()
        .filter_map(|a| text(a, "employee").map(str::to_string))
        .collect();
    let hundred = Decimal::whole(100, 2)?;
    let mut flagged = Vec::new();
    let (mut new_people, mut big_changes, mut no_bank, mut zero_pay, mut shortfalls) = (0, 0, 0, 0, 0);
    for (who, slip) in &now {
        let mut flags: Vec<String> = Vec::new();
        let net = dec(slip, "net");
        let mut change: Option<String> = None;
        match before.get(who) {
            None if !before.is_empty() => {
                new_people += 1;
                flags.push("new in this run".into());
            }
            Some(old) => {
                let old_net = dec(old, "net");
                if !old_net.is_zero() {
                    let diff = net.checked_sub(old_net)?;
                    let pct = diff.times(hundred, 4)?.div(old_net, 2)?;
                    change = Some(pct.to_string());
                    let abs = if pct.is_negative() { Decimal::zero(2).checked_sub(pct)? } else { pct };
                    if abs > Decimal::whole(limit, 2)? {
                        big_changes += 1;
                        flags.push(format!("net changed {pct}%"));
                    }
                }
            }
            None => {}
        }
        if net.is_zero() {
            zero_pay += 1;
            flags.push("nothing to pay".into());
        }
        if !dec(slip, "shortfall").is_zero() {
            shortfalls += 1;
            flags.push("deductions exceed pay (shortfall)".into());
        }
        if accounts.is_some() && !with_account.contains(who) && !net.is_zero() {
            no_bank += 1;
            flags.push("no bank account on file".into());
        }
        if !flags.is_empty() {
            flagged.push(json!({
                "employee": who, "employee_name": slip.get("employee_name"), "net": net.to_string(),
                "previous_net": before.get(who).map(|o| dec(o, "net").to_string()), "change_percent": change, "flags": flags.join("; "),
            }));
        }
    }
    let dropped: Vec<Value> = before
        .iter()
        .filter(|(who, _)| !now.contains_key(*who))
        .map(|(who, old)| json!({ "employee": who, "employee_name": old.get("employee_name"), "previous_net": dec(old, "net").to_string(), "flags": "paid last time, not in this run" }))
        .collect();
    flagged.extend(dropped.iter().cloned());
    Ok(json!({
        "run": text(&run, "number"), "compared_with": earlier.first().and_then(|r| text(r, "number")), "people": now.len(),
        "new_people": new_people, "dropped": dropped.len(), "big_changes": big_changes, "no_bank_account": no_bank,
        "nothing_to_pay": zero_pay, "shortfalls": shortfalls, "bank_check_done": accounts.is_some(),
        "threshold_percent": limit, "flagged": flagged,
    }))
}

handler! {
    /// What deserves a look before a run is approved.
    fn review_pay_run(input: Review) -> Value {
        review(input)
    }

    /// The flagged people as rows for a list.
    fn review_pay_run_rows(input: Review) -> Vec<Value> {
        let all = review(input)?;
        Ok(all["flagged"].as_array().cloned().unwrap_or_default())
    }
}
