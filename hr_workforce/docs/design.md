# Workforce planning (`hr_workforce`)

## Purpose

Staffing plans by period and position, and hiring requisitions that draw on them and can open recruitment. Rust → WASM.

## Dependencies

`hr`, `hr_recruitment`

## Models

| Model | Role |
|---|---|
| `wf_plan` / `wf_plan_line` | Period plan: headcount + budget per position |
| `wf_requisition` | Request for seats; may open an opening |
| `counter` | Numbering |

## Design

- Requisitions must fit free seats on the position (net of other holds). Counted against the plan covering their date; outside any plan is allowed but marked **unplanned**.
- Approver ≠ requester. Approval can `start_hiring` into `hr_recruitment`. Fills and time-to-fill are read back from the opening (nightly sync), never typed.
- Approved plans for one position cannot overlap.

## Functions, commands, events

**Handlers:** plan create/lines/approve/close/status; requisition request/approve/reject/withdraw/`start_hiring`/`sync_requisition`; `workforce_nightly`.

**Schedule:** `workforce-nightly` → `workforce_nightly`.

**Events:** `requisition_approved`, `requisition_filled`.

## Roles

| Role | Use |
|---|---|
| `workforce_admin` | Plans and requisition decisions |

## Boundaries

Does not own openings or applications (`hr_recruitment`). Does not hire into `hr` directly.
