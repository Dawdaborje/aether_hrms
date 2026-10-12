# Onboarding and offboarding (`hr_onboarding`)

## Purpose

One template engine for joining and leaving: dated checklist tasks, hire holds, departures with return items and settlement, confidential exit interview, and final settlement builder.2 adds exit interview + settlement pull). Rust → WASM.

## Dependencies

`hr`, `party`, `calendar`, `hr_loan`, `hr_assets`, `hr_leave`, `hr_compensation`

## Models

| Model | Role |
|---|---|
| `ob_template` / `ob_step` | Checklist template; steps with anchor + signed offset |
| `ob_run` / `ob_task` | Instantiated run and tasks (never deleted; cancel voids) |
| `ob_departure_reason` / `ob_departure` | Leaving case |
| `ob_return_item` / `ob_settlement_line` | Clearance blockers |
| `ob_exit_interview` | One confidential interview per departure |
| `counter` | Numbering |

## Design

- Due dates = anchor (start / last day / notice) + offset, rolled to a working day via `calendar`. Assignee resolution (person, manager, manager’s manager, user, role, owner) records which fallback was used.
- Hire steps can place an `hr` hire hold until done or waived. Departure closes only when blocking tasks, returns, and settlements are clear (or waived with reason), then ends employment through `hr`.
- `build_final_settlement` pulls open loans, outstanding assets, encashable leave, and gratuity into lines/items. Tasks are voided on cancel, never hard-deleted.
- Hourly tick moves departures into clearance and announces overdue tasks.

## Functions, commands, events

**Handlers (selected):** template/step CRUD; `start_onboarding`, task complete/waive/reopen/reassign, `cancel_run`, `move_start_day`; departure begin/confirm/close + return/settlement helpers; exit interview schedule/complete; `build_final_settlement`; `onboarding_tick`.

**Schedule:** `onboarding-tick` → `onboarding_tick` (every 1h).

**Events:** `onboarding_run_started`, `onboarding_task_done`, `onboarding_run_completed`, `onboarding_task_overdue`, `departure_clearance_due`, `employee_departed`.

## Roles

| Role | Use |
|---|---|
| `ob_officer` | Start onboarding, manage departures/checklists, reassign |
| `ob_manager` | Templates, waive, close departures |

## Boundaries

Does not invent payroll math; pulls balances from loan/assets/leave/compensation. Does not own asset custody or loan ledgers.
