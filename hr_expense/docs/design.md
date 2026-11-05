# Expenses (`hr_expense`)

## Purpose

Expense reports (lines, receipts, policies), advances, and reimbursement recording. Accounting and payroll receive events; this plugin never posts ledger entries. Rust → WASM.

## Dependencies

`hr`, `currency`

## Models

| Model | Role |
|---|---|
| `expense_category` | Category taxonomy |
| `expense_policy` | warn / justify / block / cap for a date |
| `expense_report` / `expense_line` | Report + lines with stored FX rate |
| `expense_advance` / `expense_advance_use` | Advances and allocation against reports |
| `expense_payment` | Reimbursement / payment records |
| `counter` | Numbering |

## Design

- Exact decimals; FX rate and date stored on the line; convert once; report total = sum of lines.
- Policy resolved against employment on the **expense date**. Receipt and duplicate rules enforced; approver acknowledges flags.
- Approver is the manager chain, **frozen at submit**; self-approval impossible. Status moves only by transitions or inbound payment events.
- Advances settle in their own currency up to unclaimed. Nightly flags overdue advances.

## Functions, commands, events

**Handlers (selected):** category/policy CRUD; report draft → submit → approve/reject → reimburse; line CRUD, `attach_receipt`; advance request/decide/issue/return; `expense_nightly`.

**Schedule:** `expense-nightly` → `expense_nightly`.

**Events:** `report_approved`, `report_rejected`, `report_closed`, `advance_requested`, `advance_approved`, `advance_overdue`.

## Roles

| Role | Use |
|---|---|
| `expense_admin` | Categories and policies |
| `expense_finance` | Escalated decisions, reimbursements, advances paid |
| `expense_proxy` | Create reports for others |
| `expense_corrector` | Delete reports / edit payments (break-glass) |

## Boundaries

No GL posting. Install `accounting/gl_hrms` to forward `report_approved` into `gl`. Travel opens reports via `hr_travel` once per trip.
