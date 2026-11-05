# Payroll (`hr_payroll`)

## Purpose

Salary structures (exact-decimal Swift payroll formulas), parallel batched pay runs, second-person approval, and payslips. Announces component totals for the ledger; does not post GL. Rust → WASM.

## Dependencies

`hr`, `currency`, `hr_leave`, `hr_attendance`, `hr_compensation`

## Models

| Model | Role |
|---|---|
| `pay_structure` | Immutable component list + formulas (new version = new structure) |
| `pay_assignment` | Who is on which structure |
| `pay_input` | One-off inputs per run (OT, bonus, …) |
| `pay_run` / `pay_plan` / `pay_batch` | Run planning and parallel batches |
| `payslip` / `pay_error` | Results and per-employee failures |
| `pay_rule_pack` | Importable rule packs |
| `pay_setting` | Org payroll settings |
| `pay_component` | Reusable earning/deduction/employer/info building block; `compose_structure` turns a list of them into an immutable structure |
| `pay_period` | Payroll calendar; with `require_open_period` a run must fall inside an open one |
| `pay_tax_slab` | Dated yearly bands (+ standard deduction) that become the `bracket()` constant tables of a composed structure |
| `pay_exemption_category` / `pay_exemption_declaration` | Employee tax exemptions: declare → proof → verify; feeds the input `exemptions_annual` |
| `counter` | Numbering |

## Design

- Structures are never mutated in place; payslip stores structure id + hash. Formulas are exact decimal via the Swift payroll engine.
- Runs are planned in pages and calculated in parallel background batches (fixed work budget per call); every step is idempotent/safe to retry. `watch_pay_runs` recovers stuck batches.
- Inputs: employment terms from `hr`, days employed in period, leave/attendance summaries, `hr_compensation.payroll_adjustments`. Approval refuses runs with calculation errors; approver ≠ maker. Approval locks the compensation period and emits totals for accounting.
- Employees read own payslips only after approval.

## Library

- `compose_structure` reads components in the order given; a pro-rata component is scaled by `days_worked / period_days`; taxable earnings carry the tag `taxable`; slabs become constants (`<name>` and `<name>_standard_deduction`).
- `pay_setting.periods_per_year` replaces the fixed 12; `exemptions_annual` is filled only for structures that declare it.
- `bulk_assign_structure` assigns up to 100 people per call and reports failures per person.
- Extra pay (incentive, retention, additional salary, arrears) is a `hr_compensation` adjustment/award, not a payroll model.

## Functions, commands, events

**Handlers (selected):** structure/assignment/rule-pack/settings; `create_pay_run` → plan/calculate/finalise → `approve_pay_run` / `cancel_pay_run` / `record_pay_run_payment`; `set_run_input`; `my_payslips`; `watch_pay_runs`.

**Schedule:** `watch-pay-runs` → `watch_pay_runs` (every 1m).

**Events:** `payroll_run_calculated`, `payroll_run_approved`, `payroll_run_paid`, `payroll_run_failed`, `payroll_run_restarted`.

## Roles

| Role | Use |
|---|---|
| `payroll_admin` | Structures, assignments; full clerk+finance powers |
| `payroll_clerk` | Runs, inputs, calculate, cancel |
| `payroll_approver` | Approve calculated runs (not ones they made) |
| `payroll_finance` | Mark approved runs paid |

## Boundaries

No GL. Install `accounting/gl_hrms` for `payroll_run_approved` → `gl`. Country tax packs are separate; loan deductions come from `hr_loan` plan/report APIs.
