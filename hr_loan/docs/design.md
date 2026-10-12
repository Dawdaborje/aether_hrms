# Staff loans (`hr_loan`)

## Purpose

Loan products, requests with full schedules, append-only money ledger, capped payroll deductions, foreclosure, and reversal. Designed from first principles. Rust → WASM.

## Dependencies

`hr`, `currency`

## Models

| Model | Role |
|---|---|
| `loan_product` | Caps, rate, method (reducing/flat), tenure, payroll share |
| `loan` | Request/loan header |
| `loan_instalment` | Full schedule at approval |
| `loan_entry` | Disbursement / repayment / reversal (append-only) |
| `counter` | Numbering |

## Design

- Request checked against product; schedule materialised at once. Approver ≠ borrower; disburser ≠ approver and ≠ borrower.
- Money moves only as ledger entries. Payroll asks `payroll_loan_plan` (capped by available pay) then reports what it took once per run. Foreclosure quote charges no interest on not-yet-due instalments. Undo = reversal entry, never edit.
- Nightly marks overdue instalments. `leaver_loan_balance` feeds offboarding settlement.

## Functions, commands, events

**Handlers:** product CRUD; request/decide/cancel; disburse; repayment/foreclose/reverse; payroll plan/report; statement; `loan_nightly`.

**Schedule:** `loan-nightly` → `loan_nightly`.

**Events:** `loan_requested`, `loan_disbursed`, `loan_closed`, `loan_instalment_overdue`.

## Roles

| Role | Use |
|---|---|
| `loan_admin` | Products, approve, disburse, repayments/reversals |

## Boundaries

Does not run payroll; only supplies deduction plans. No GL posting of its own.
