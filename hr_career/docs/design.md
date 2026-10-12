# Career (`hr_career`)

## Purpose

Promotions, demotions, transfers, grade and pay changes as approved change sets; grade ladders; letter templates. Rust → WASM.

## Dependencies

`hr`, `hr_compensation`

## Models

| Model | Role |
|---|---|
| `career_grade` | Grade ladder + wage band |
| `career_change` | Requested change set awaiting decision |
| `career_letter_template` | Letter body with required placeholders |
| `counter` | Numbering |

## Design

- A move is a **request**, not an edit. Approver ≠ requester and never the subject. Approval writes dated `employment` in `hr` (history kept). Future pending changes can be withdrawn.
- Grade ladder enforces promotions go “up” and wages stay in band unless an exception is stated.
- Letters: missing placeholder values are errors, not blanks.

## Functions, commands, events

**Handlers:** `request_career_change`, `approve_career_change`, `reject_career_change`, `withdraw_career_change`, list/get; grade CRUD; letter template CRUD, `make_career_letter`.

**Events:** `career_change_approved`.

**Schedule:** none.

## Roles

| Role | Use |
|---|---|
| `career_admin` | Grades, templates, request and decide changes |

## Boundaries

Does not overwrite employment in place. Does not run payroll; pay effects appear as new employment terms (and optional compensation adjustments).
