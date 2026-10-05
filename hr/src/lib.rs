//! HR core: the people of an organization and how they are organized.
//!
//! Departments form a tree, positions are seats in that tree (with a headcount), employees hold
//! positions and report to each other, and a dated history of employment records says on what
//! terms (the way Odoo 19 versions an employee rather than keeping a flat row plus a contract).
//! Who may see and change what is in `rules/`, enforced by the kernel. Nothing here knows about
//! any particular country or employer: another plugin extends it with its own rules.
//!
//! The rules that need no data (status changes) live in `rules`, and date arithmetic comes from
//! the SDK's `dates`; the rest reads and writes the records and calls those rules.

mod common;
mod department;
mod employee;
mod employment;
mod people;
mod position;
mod reports;
mod rules;
