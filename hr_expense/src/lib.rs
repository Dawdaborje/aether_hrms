//! Expenses: reports of lines, policies, receipts, advances, and events for finance and payroll.
//!
//! Accounting entry and its status is never derived from one: approval announces the report with its
//! Lines (`report_approved`), and the paying system answers with `record_reimbursement`.

mod advance;
mod common;
mod decision;
mod report;
mod rules;
mod setup;
