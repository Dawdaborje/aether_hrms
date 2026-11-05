//! Compensation: the one funnel for extra pay, and what hangs off it.
//!
//! Pay adjustments (one-off and recurring, with a code, a source and an idempotency key), incentive and retention
//! Awards decided by someone other than the requester, salary withholding by calendar month, gratuity by slabs, and
//! The period lock payroll closes when it approves a run. Payroll asks `payroll_adjustments` and nothing else, so
//! Loans, benefits, referrals, leave encashment and travel advances reach a payslip by pushing adjustments here,
//! `retention_bonus`, `salary_withholding` and `gratuity` (see `docs/design.md`).

mod adjust;
mod common;
mod gratuity;
mod rules;
mod withhold;
