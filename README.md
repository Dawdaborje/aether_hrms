# Aether HRMS

Human Resource Management for Aether: people, time, pay-adjacent processes, and talent flows.
**Rust → WASM** plugins.
Each plugin’s design lives next to its code: [`<plugin>/docs/design.md`](./hr/docs/design.md).

## Features

- Dated employment history, private data split, departments and seat-based positions
- Append-only leave ledger; shift punches with derived days; expense reports that never post GL
- Recruitment with seat checks; onboarding/offboarding checklists and final settlement
- Performance cycles; career change sets; workforce plans and requisitions
- Payroll runs (Swift engine, parallel batches) plus compensation funnel (adjustments, awards, gratuity)
- Training, grievances, travel, custody assets, staff loans, benefits, referrals, documents

## Depends on base

Install from the **base** workspace first (or let dependency install pull them):

`currency` → `country` → `party` → `company` (and `calendar` / `number_sequence` where linked).

HR core also needs those for party identity, company scoping, and money/dates.

## Plugins (19)

| Plugin | Role | Design |
|---|---|---|
| [`hr`](./hr) | People, org tree, positions, dated employment | [docs/design.md](./hr/docs/design.md) |
| [`hr_leave`](./hr_leave) | Leave types, ledger, requests, plans | [docs/design.md](./hr_leave/docs/design.md) |
| [`hr_attendance`](./hr_attendance) | Shifts, punches, days, corrections, OT | [docs/design.md](./hr_attendance/docs/design.md) |
| [`hr_expense`](./hr_expense) | Expense reports and advances (events out) | [docs/design.md](./hr_expense/docs/design.md) |
| [`hr_recruitment`](./hr_recruitment) | Openings, applications, interviews, offers | [docs/design.md](./hr_recruitment/docs/design.md) |
| [`hr_onboarding`](./hr_onboarding) | Join/leave checklists, exit interview, settlement | [docs/design.md](./hr_onboarding/docs/design.md) |
| [`hr_performance`](./hr_performance) | Cycles, goals, blind reviews, skills | [docs/design.md](./hr_performance/docs/design.md) |
| [`hr_payroll`](./hr_payroll) | Structures, batched pay runs, payslips | [docs/design.md](./hr_payroll/docs/design.md) |
| [`hr_compensation`](./hr_compensation) | Adjustments, awards, withholding, gratuity, period lock | [docs/design.md](./hr_compensation/docs/design.md) |
| [`hr_career`](./hr_career) | Promotions, transfers, grades, letters | [docs/design.md](./hr_career/docs/design.md) |
| [`hr_workforce`](./hr_workforce) | Staffing plans and requisitions | [docs/design.md](./hr_workforce/docs/design.md) |
| [`hr_training`](./hr_training) | Courses, sessions, certificates, budgets | [docs/design.md](./hr_training/docs/design.md) |
| [`hr_grievance`](./hr_grievance) | Confidential cases, SLA, append-only log | [docs/design.md](./hr_grievance/docs/design.md) |
| [`hr_travel`](./hr_travel) | Trips, policy, advance → expense report | [docs/design.md](./hr_travel/docs/design.md) |
| [`hr_assets`](./hr_assets) | Custody ledger for equipment/vehicles | [docs/design.md](./hr_assets/docs/design.md) |
| [`hr_loan`](./hr_loan) | Staff loans, schedule, payroll deductions | [docs/design.md](./hr_loan/docs/design.md) |
| [`hr_benefits`](./hr_benefits) | Plans, accrual ledger, claims, insurance | [docs/design.md](./hr_benefits/docs/design.md) |
| [`hr_referral`](./hr_referral) | Referrals and tenure-gated bonus | [docs/design.md](./hr_referral/docs/design.md) |
| [`hr_documents`](./hr_documents) | Identity docs, emergency contacts, bank split | [docs/design.md](./hr_documents/docs/design.md) |

## Handoff to accounting

`hr_payroll` and `hr_expense` **emit** approval events; they do not post journals.

In the **accounting** workspace, [`gl_hrms`](../accounting/gl_hrms) listens to
`hr_payroll.payroll_run_approved` and `hr_expense.report_approved` and calls `gl.on_posting_event`.
Install `gl` + `gl_hrms` (and posting rules) when both suites are in use. Design:
[gl_hrms/docs/design.md](../accounting/gl_hrms/docs/design.md).

## Load order

Dependencies decide install order. A workable spine:

```text
base: currency → country → party → company (+ calendar as needed)

hr
hr_compensation
hr_leave          (needs hr, calendar, hr_compensation)
hr_attendance     (needs hr, calendar, hr_leave)
hr_documents
hr_assets
hr_loan
hr_benefits       (needs hr_compensation)
hr_performance
hr_career         (needs hr_compensation)
hr_expense
hr_recruitment
hr_referral       (needs recruitment + compensation)
hr_workforce      (needs recruitment)
hr_training       (needs performance)
hr_grievance
hr_travel         (needs expense)
hr_onboarding     (needs loan, assets, leave, compensation)
hr_payroll        (needs leave, attendance, compensation)
```

Workspace members are declared in [`workspace.toml`](./workspace.toml).
There is no workspace-level `docs/` folder; use each plugin’s `docs/design.md`.
