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
| `counter` | Numbering |

## Design

- Punches are append-only; a correction is a **new** punch that supersedes another. Odd unpaired punches and punch-on-leave are **flagged**, never dropped.
- `att_day` is pure `compute` from shift window, paired punches, calendar, and leave; recomputed when the inputs hash changes. Manager override is separate from the derivation.
- The person never decides their own correction. Nightly job settles yesterday and marks absents once the window closed.
- Overtime is weighed by day kind (work/leave/holiday/off) against rules; `payroll_overtime` / `payroll_attendance_summary` feed payroll.

## Functions, commands, events

**Handlers (selected):** setup (`create_shift`, `assign_shift`, locations, rotations, OT rules); `punch`, `import_punches`; day views/`recompute_day`/`override_attendance_day`/`lock_attendance_days`; correction request/decide; OT approve/reject; `attendance_nightly`.

**Schedule:** `attendance-nightly` → `attendance_nightly`.

**Events:** `punch_recorded`, `day_needs_review`, `correction_decided`.

## Roles

| Role | Use |
|---|---|
| `att_admin` | Shifts, locations, corrections, day overrides |
| `att_corrector` | Edit/delete punches and locked days (break-glass) |

## Boundaries

Does not grant leave or edit leave ledgers. Does not calculate payslips; only summaries for payroll. Device secrets/PIN policy are setup concerns, not a second attendance engine.
