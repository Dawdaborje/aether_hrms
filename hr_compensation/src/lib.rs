//! Compensation: the one funnel for extra pay, and what hangs off it.
//!
//! Pay adjustments (one-off and recurring, with a code, a source and an idempotency key), incentive and retention
//! awards decided by someone other than the requester, salary withholding by calendar month, gratuity by slabs, and
//! the period lock payroll closes when it approves a run. Payroll asks `payroll_adjustments` and nothing else, so
//! loans, benefits, referrals, leave encashment and travel advances reach a payslip by pushing adjustments here,
//! never by editing a slip. Built from a reading of Frappe HRMS `additional_salary`, `employee_incentive`,
//! `retention_bonus`, `salary_withholding` and `gratuity` (see `docs/design.md`).

mod adjust;
mod common;
mod gratuity;
mod rules;
mod withhold;
