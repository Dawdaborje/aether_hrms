//! Attendance: shifts, punches, one derived day per person and date, and corrections.
//!
//! Punches are raw facts and are never edited: a correction is a new punch that supersedes another.
//! A day is worked out by the pure function in `rules` from the shift's window, the paired punches,
//! the holiday calendar and approved leave, and rewritten only when what it was worked out from
//! changes.

mod common;
mod correction;
mod day;
mod demo;
mod kiosk;
mod punch;
mod requests;
mod presence;
mod rules;
mod setup;
mod overtime;
