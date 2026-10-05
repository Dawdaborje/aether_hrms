//! Numbers about the organization.

use aether_sdk::dates::{self, Datelike, Duration, NaiveDate};
use aether_sdk::db::{Figure, Filter};
use aether_sdk::prelude::*;
use crate::rules::status;

use crate::common::{text, Record};

#[derive(Deserialize, Default)]
struct Scope {
    #[serde(default)]
    department: Option<String>,
}

/// Headcount by department and status, and by employment type.
fn headcount(scope: Scope) -> Result<Value> {
    let mut filter = Filter::all();
    if let Some(department) = scope.department {
        filter = filter.and(Filter::eq("department", department));
    }
    let by_department = db::aggregate("employee", filter.clone(), &["department", "status"], &[("people", Figure::Count)])?;
    let by_type = db::aggregate(
        "employee",
        filter.and(Filter::one_of("status", status::PRESENT.to_vec())),
        &["employment_type"],
        &[("people", Figure::Count)],
    )?;
    let present = db::count("employee", Filter::one_of("status", status::PRESENT.to_vec()))?;
    Ok(json!({ "present": present, "by_department_and_status": by_department, "by_employment_type": by_type }))
}

/// People whose probation ends within the next days.
fn probation_ending(days: u32) -> Result<Vec<Record>> {
    let today = crate::common::today()?;
    let limit = {
        let start = dates::parse_date(&today)?;
        dates::format_date(start + Duration::days(i64::from(days)))
    };
    db::find::<Record>("employee")
        .matching(
            Filter::eq("status", "probation")
                .and(Filter::gte("probation_end", today.as_str()))
                .and(Filter::lte("probation_end", limit)),
        )
        .order_by("probation_end")
        .limit(500)
        .all()
}

#[derive(Deserialize)]
struct Days {
    #[serde(default = "thirty")]
    days: u32,
}

fn thirty() -> u32 {
    30
}

/// Employees whose birthday falls in the next days (by month and day, any year). Birth dates are
/// private, so only those the caller is allowed to read are considered.
fn birthdays(days: u32) -> Result<Vec<Record>> {
    let today = dates::parse_date(&crate::common::today()?)?;
    let private: Vec<Record> = db::find::<Record>("employee_private").matching(Filter::is_set("date_of_birth")).limit(1000).all()?;
    let mut upcoming: Vec<(u32, Record)> = Vec::new();
    for details in private {
        let Some(born) = text(&details, "date_of_birth").and_then(|d| dates::parse_date(d).ok()) else { continue };
        let Some(employee_id) = text(&details, "employee") else { continue };
        let Some(employee) = db::get::<Record>("employee", employee_id)? else { continue };
        if !status::counts_in_headcount(text(&employee, "status").unwrap_or("active")) {
            continue;
        }
        // The next time the day comes round, counting today.
        let this_year = NaiveDate::from_ymd_opt(today.year(), born.month(), born.day()).or_else(|| NaiveDate::from_ymd_opt(today.year(), 3, 1));
        let Some(mut next) = this_year else { continue };
        if next < today {
            next = NaiveDate::from_ymd_opt(today.year() + 1, born.month(), born.day()).unwrap_or(next);
        }
        let away = (next - today).num_days();
        if (0..=i64::from(days)).contains(&away) {
            upcoming.push((away as u32, employee));
        }
    }
    upcoming.sort_by_key(|(away, _)| *away);
    Ok(upcoming.into_iter().map(|(_, employee)| employee).collect())
}

handler! {
    fn headcount_report(input: Scope) -> Value {
        headcount(input)
    }

    fn probation_ending_soon(input: Days) -> Vec<Record> {
        probation_ending(input.days)
    }

    fn upcoming_birthdays(input: Days) -> Vec<Record> {
        birthdays(input.days)
    }
}
