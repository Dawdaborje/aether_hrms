//! Attendance: shifts, punches, one derived day per person and date, and corrections.
//!
//! Punches are raw facts and are never edited: a correction is a new punch that supersedes another.
//! A day is worked out by the pure function in `rules` from the shift's window, the paired punches,
//! the holiday calendar and approved leave, and rewritten only when what it was worked out from
//! changes. Built from a reading of Odoo 19 and Frappe HRMS (see `docs/design.md`).

mod common;
mod correction;
mod day;
mod punch;
mod rules;
mod setup;
