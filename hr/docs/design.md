# Human Resources (`hr`)

## Purpose

Core people and organization: department tree, jobs, positions (seats with headcount), employees linked to `party`, dependents, skills, private details, work locations, hire holds, and a dated `employment` history. Country- and employer-neutral; other HRMS plugins build on it. Rust → WASM.

## Dependencies

`party`, `country`, `currency`, `company`

## Models

| Model | Role |
|---|---|
| `department` | Hierarchy of org units (company-scoped) |
| `job` | Job title / designation |
| `position` | Seat with headcount under a department/job |
| `employee` | Working identity; links `party`, status, reporting line |
| `employee_private` | Private fields (own role gate; not on `employee`) |
| `employment` | Dated terms (job, dept, manager, wage, calendar, contract) |
| `dependent` | Family members |
| `skill` | Skill catalog + employee skill links |
| `work_location` | Site / branch with address and timezone (catalog in HR Configuration) |
| `cost_center` | Cost centre catalog picked on people and departments |
| `hire_hold` | Blocks hire until another plugin releases |
| `hr_setting` | Organisation defaults: probation days, notice days, retirement age; applied when a hire gives none |
| `employee_number_rule` | Pattern rules for hire numbers (department / type scopes); managed in HR Configuration |
| `counter` | Numbering series (including scoped employee-number keys) |

## Design

- Identity is `party`; `employee` is the working role. Private data is a separate model with its own rules.
- Terms live on dated `employment` rows, not a flat contract overwrite. Nightly `apply_due_employments` makes future terms current.
- Positions have headcount; vacancies = headcount − holders. Subordinates use hierarchy walks, not recursive Python.
- Hire holds let onboarding/recruitment gate `hire_employee` without bypassing HR validation.
- Status transitions are rule-checked (`next_statuses`); leave/rehire/rehire paths emit events.
- Employee numbers come from `employee_number_rule` patterns (`{number:N}`, `{year}`, `{department_code}`, …). The most specific active rule wins; otherwise `EMP-{number:5}`.

## Functions, commands, events

**Handlers (selected):** `hire_employee`, `rehire_employee`, `change_employee_status`, `update_employee`, `add_employment` / `employment_history` / `employment_snapshot`, dept/position/job CRUD, `vacancies`, `place_hire_hold` / `release_hire_hold`, private/dependent/skill helpers, `my_employee`, `rule_var_employee` / `rule_var_subordinates`, employee number rules (`create_employee_number_rule`, `preview_employee_number`, …), reports (`headcount_report`, `probation_ending_soon`, `upcoming_birthdays`).

**Commands:** `seed_employee_number_rules` (install/upgrade) seeds a default `EMP-{number:5}` rule.

**Schedule:** `apply-employment` → `apply_due_employments` (`10 0 * * *`).

**Events:** `employee_hired`, `employee_rehired`, `employee_status_changed`.

## Roles

| Role | Use |
|---|---|
| `hr_user` | HR officer: hire/update people, org, terms |
| `hr_manager` | Everything an officer can, and sees pay |

## Boundaries

No leave, attendance, payroll, or recruiting. Does not post accounting. Government/employer packs extend via separate plugins.
