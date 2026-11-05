# Training (`hr_training`)

## Purpose

Courses, sessions with seats and waiting lists, department budgets, results, certificates, and skill raises into `hr_performance`. Rust → WASM.

## Dependencies

`hr`, `hr_performance`

## Models

| Model | Role |
|---|---|
| `trn_course` | Course; optional skill + expiry |
| `trn_session` | Scheduled instance with seat count |
| `trn_enrolment` | Enrolment or waitlist |
| `trn_certificate` | Issued certificate (optional expiry) |
| `trn_budget` | Yearly department budget |
| `counter` | Numbering |

## Design

- Full sessions wait-list; first in line promotes when a seat frees. Seat cost is the course cost at booking time, charged to the person’s department budget (overspend needs an explicit override).
- Nobody records their own result. A pass issues a certificate and may raise skill level in `hr_performance` (never lowers). Nightly lists expiring/missing mandatory certificates.

## Functions, commands, events

**Handlers:** course/session/budget CRUD; enrol/cancel; `record_training_result`, `revoke_certificate`; `mandatory_training_gaps`, `training_nightly`.

**Schedule:** `training-nightly` → `training_nightly`.

**Events:** `certificate_issued`, `certificate_expiring`.

## Roles

| Role | Use |
|---|---|
| `training_admin` | Courses, sessions, budgets, results, revoke |

## Boundaries

Does not own the skill catalog (`hr_performance`). Not an LMS content platform.
