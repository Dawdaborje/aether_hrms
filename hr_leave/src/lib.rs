//! Leave: types of leave, a ledger of days, requests and their approval.
//!
//! Type; days live in an append-only ledger (grants, accruals and carry-over add; requests hold
//! And then spend; a cancellation is a reversal); a request costs working days by the person's
//! Holiday calendar and is decided by their manager, by HR, or both, never by themselves.

mod common;
mod demo;
mod ledger;
mod request;
mod rules;
mod setup;
mod plan;
mod extra;
