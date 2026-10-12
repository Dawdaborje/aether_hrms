# Business travel (`hr_travel`)

## Purpose

Trip requests with legs and costs, per-diem policy snapshots, two-level approval, advances and expense-report handoff. Rust → WASM.

## Dependencies

`hr`, `hr_expense`, `currency`

## Models

| Model | Role |
|---|---|
| `trv_policy` | Per diem, lodging cap, approval limit, advance share |
| `trv_trip` | Trip header with copied policy figures |
| `trv_leg` / `trv_cost` | Ordered legs and cost lines |
| `counter` | Numbering |

## Design

- Draft → submit against policy → manager approval; travel admin second-approves when estimate exceeds policy limit. Policy figures are **copied** onto the trip.
- Legs must lie inside trip dates and follow one another; a person cannot be on two overlapping trips. Approved trips may request one advance and open one expense report in `hr_expense`. Nightly flags unsettled trips.

## Functions, commands, events

**Handlers:** policy CRUD; trip create/legs/costs; submit/decide/cancel; `request_trip_advance`, `open_trip_report`, `complete_trip`; lists; `travel_nightly`.

**Schedule:** `travel-nightly` → `travel_nightly`.

**Events:** `trip_submitted`, `trip_approved`, `trip_unsettled`.

## Roles

| Role | Use |
|---|---|
| `travel_admin` | Policies, second approval, see all trips |

## Boundaries

Does not reimburse; that is `hr_expense` (+ optional `gl_hrms`). Does not book flights/hotels externally.
