//! HR core: the people of an organization and how they are organized.
//!
//! Departments form a tree, positions are seats in that tree (with a headcount), employees hold
//! Positions and report to each other, and a dated history of employment records says on what
//! Terms.
//! Who may see and change what is in `rules/`, enforced by the kernel. Nothing here knows about
//! Any particular country or employer: another plugin extends it with its own rules.
//!
//! The rules that need no data (status changes) live in `rules`, and date arithmetic comes from
//! The SDK's `dates`; the rest reads and writes the records and calls those rules.

mod common;
mod department;
mod employee;
mod employment;
mod people;
mod position;
mod reports;
mod rules;
