# Grievances (`hr_grievance`)

## Purpose

Confidential grievance cases with conflict-checked investigators, SLA escalation, appeal, and an append-only log. Rust → WASM.

## Dependencies

`hr`

## Models

| Model | Role |
|---|---|
| `grv_category` | Case kinds with SLA |
| `grv_case` | Case state machine |
| `grv_log` | Append-only log (internal or shared visibility) |
| `counter` | Numbering |

## Design

- States: filed → investigated → findings → decided → (appeal) → closed. Investigator cannot be complainant, respondent, or in a direct reporting line with either; a case manager who is a party is recused.
- Investigator proposes findings; a **different** person decides. Overdue cases escalate by level once each (logged). Nothing in the log is edited or deleted. Anonymous complainants are hidden from the investigator.
- Appeal once within 14 days to a new investigator; otherwise auto-close.

## Functions, commands, events

**Handlers:** category CRUD; `file_grievance`, triage, assign, notes, findings, decide, appeal, withdraw, dismiss, close; get/list/log; `grievance_nightly`.

**Schedule:** `grievance-nightly` → `grievance_nightly`.

**Events:** `grievance_filed`, `grievance_escalated`, `grievance_decided`.

## Roles

| Role | Use |
|---|---|
| `grievance_admin` | Triage, assign, decide, close; recused when a party |

## Boundaries

Not a general ticketing system. No HRIS edits to employment as a side effect of a decision (those remain explicit in other plugins).
