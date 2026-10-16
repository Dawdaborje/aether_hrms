# Attendance (`hr_attendance`)

## Purpose

Shifts, locations, rotations, immutable punches, derived daily attendance, corrections, and overtime. Rust → WASM.

## Dependencies

`hr`, `calendar`, `hr_leave`

## Models

| Model | Role |
|---|---|
| `att_location` | Punch location / geofence |
| `att_shift` | Window, tolerances, breaks |
| `att_shift_assignment` | Dated shift for a person |
| `att_rotation` / `att_rotation_step` | Rotating schedules |
| `att_punch` | Raw punch (immutable; corrections supersede) |
| `att_day` | One derived day per person/date |
| `att_correction` | Regularisation request |
| `att_overtime_rule` / `att_overtime` | Overtime bands and settled OT |
| `att_shift_request` | Another shift for some days; approval makes a dated temporary `att_shift_assignment` (`source_request`), the standing one resumes after |
| `att_request` | Work from home / on duty / half day; approval overrides each day (future days applied by the nightly job) |
| `att_kiosk_pin` | Hashed PIN per person for kiosk punching; five wrong tries lock it until an administrator unlocks |
| `counter` | Numbering |

## Design

- Punches are append-only; a correction is a **new** punch that supersedes another. Odd unpaired punches and punch-on-leave are **flagged**, never dropped.
- `att_day` is pure `compute` from shift window, paired punches, calendar, and leave; recomputed when the inputs hash changes. Manager override is separate from the derivation.
- The person never decides their own correction. Nightly job settles yesterday and marks absents once the window closed.
- Overtime is weighed by day kind (work/leave/holiday/off) against rules; `payroll_overtime` / `payroll_attendance_summary` feed payroll.

## Functions, commands, events

**Handlers (selected):** setup (`create_shift`, `assign_shift`, locations, rotations, OT rules); `punch`, `import_punches`; day views/`recompute_day`/`override_attendance_day`/`lock_attendance_days`; correction request/decide; OT approve/reject; `attendance_nightly`.

**Requests:** `request_shift`, `approve_shift_request`, `reject_shift_request`, `request_attendance`, `approve_attendance_request`, …; `bulk_assign_shift`. Decided by the manager or an administrator, never the person.

**Kiosk:** `kiosk_punch` (role `att_kiosk`: employee number + PIN; wrong tries return an answer, not an error, so the count is kept), `set_kiosk_pin`, `unlock_kiosk_pin`.

**Schedule:** `attendance-nightly` → `attendance_nightly`.

**Events:** `punch_recorded`, `day_needs_review`, `correction_decided`, `shift_request_decided`, `attendance_request_decided`.

## Roles

| Role | Use |
|---|---|
| `att_admin` | Shifts, locations, corrections, day overrides |
| `att_kiosk` | A shared device account that punches people in by number and PIN |
| `att_corrector` | Edit/delete punches and locked days (break-glass) |

## Boundaries

Does not grant leave or edit leave ledgers. Does not calculate payslips; only summaries for payroll. Device secrets/PIN policy are setup concerns, not a second attendance engine.
