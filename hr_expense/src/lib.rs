//! Expenses: reports of lines, policies, receipts, advances, and events for finance and payroll.
//!
//! Built from a reading of Odoo 19 and Frappe HRMS (see `docs/design.md`). This plugin never posts an
//! accounting entry and its status is never derived from one: approval announces the report with its
//! lines (`report_approved`), and the paying system answers with `record_reimbursement`.

mod advance;
mod common;
mod decision;
mod report;
mod rules;
mod setup;
