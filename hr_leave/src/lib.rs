//! Leave: types of leave, a ledger of days, requests and their approval.
//!
//! Built from a reading of Odoo 19 and Frappe HRMS (see `docs/design.md`): rules live on the leave
//! type; days live in an append-only ledger (grants, accruals and carry-over add; requests hold
//! and then spend; a cancellation is a reversal); a request costs working days by the person's
//! holiday calendar and is decided by their manager, by HR, or both, never by themselves.

mod common;
mod ledger;
mod request;
mod rules;
mod setup;
mod plan;
mod extra;
