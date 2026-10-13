# Leave (`hr_leave`)

## Purpose

Leave types with per-type rules, an append-only day ledger, requests with manager/HR approval, blocks, accrual plans, encashment and compensatory claims. Rust → WASM.

## Dependencies

`hr`, `calendar`, `hr_compensation`

## Models

| Model | Role |
|---|---|
| `leave_type` | Rules: approval, notice, sandwich, half days, negative, earned/yearly, carry-forward |
| `leave_ledger` | Append-only buckets (grant, accrual, carry, hold, spend, reversal, …) |
| `leave_request` | Request with policy snapshot and approval state |
| `leave_block` | Blocked periods (allow-list) |
| `leave_plan` / `leave_plan_level` / `leave_plan_assignment` | Accrual plans by service milestone |
| `leave_encashment` | Encashment of leave days |
| `leave_comp_claim` | Compensatory leave claims |
| `counter` | Numbering |

## Design

- Days are never edited or deleted: cancel writes a **reversal**. Balance is a pass over non-expired buckets (first-expiring-first); pending requests are reservations.
- Type rules are **copied onto the request** so later type edits do not rewrite in-flight flow.
- Duration is working days from the person’s holiday calendar (pure function). Self-approval is refused.
- Approval: none / manager / HR / both per type. Nightly job accrues, lapses carry-over, and rolls leave status; yearly job grants annual days.

## Functions, commands, events

**Handlers (selected):** type/block/plan CRUD; `request_leave`, `approve_leave`, `reject_leave`, `cancel_leave`, `revert_leave`; `allocate_leave`, `adjust_leave`, `allocate_yearly`, `leave_balances`, `leave_ledger_entries`; encashment and compensatory claim flows; `payroll_leave_summary`, `leave_on_date`, `preview_leave_days`, `who_is_away_between`; `leave_nightly`.

**Schedule:** `leave-nightly` → `leave_nightly`; `leave-yearly` → `allocate_yearly`.

**Events:** `leave_requested`, `leave_approved`, `leave_rejected`.

## Roles

| Role | Use |
|---|---|
| `leave_admin` | Types, grants, HR decisions, block overrides |
| `ledger_corrector` | Edit/delete ledger (break-glass; not for normal use) |

## Pages

| Route | Who | Purpose |
|---|---|---|
| `/hr/leave` | Members | Own requests |
| `/hr/leave/new` | Members | Ask for leave (picker: active types via `list_leave_types`) |
| `/hr/leave/approvals` | `leave_admin` | Requests waiting for a decision |

**Leave types** (create/edit kinds of leave) live in the HRMS **Configurations**
app (`hr_config`): `/hr/config/leave-types`, not in this app’s sidebar. Handlers
`list_all_leave_types` / `get_leave_type` / `create_leave_type` / `update_leave_type`
remain here; Configurations calls them.

Org **Settings** / **Users** in the shell Navigate menu are separate: they need
`org_admin` / `user_manager`, not `leave_admin`.

## Boundaries

Does not own the holiday calendar (`calendar`). Does not pay encashment; hands totals to compensation/payroll. Attendance overlays leave via `hr_leave` queries, not by writing leave rows.
