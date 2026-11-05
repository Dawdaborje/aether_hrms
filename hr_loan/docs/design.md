# Staff loans (`hr_loan`)

## Purpose

Loan products, requests with full schedules, append-only money ledger, capped payroll deductions, foreclosure, and reversal. Designed from first principles. Rust → WASM.

## Dependencies

`hr`, `currency`, `hr_assets`

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

## Hire purchase

- A product with `hire_purchase` set makes the loan buy one company asset (`loan.asset`). The request must name a free asset (`asset_for_hire_purchase`), the amount may not exceed its cost (the difference is stored as `down_payment`), and no other open loan may cover it.
- Disbursement calls `hr_assets.finance_asset`: the asset is assigned to the borrower and marked financed in one write; the company keeps ownership. `hr_assets`' rules grant `via:hr_loan`, so paying out needs no asset role. The two plugins share no transaction: if the disbursement entry then fails, `hr_assets.unfinance_asset` takes the asset back. `finance_asset` is idempotent for the same loan, so a payout can be retried.
- The request form lists free assets through `hr_loan.assets_for_hire_purchase` (and the request checks one through `hr_assets.asset_for_hire_purchase`), so an employee who cannot read the asset register can still choose one.
- When the last instalment is paid (payroll, repayment or foreclosure) `hr_assets.transfer_asset_ownership` makes the holder the owner. If that call fails the repayment still stands; `complete_hire_purchase` or the nightly run repeats the transfer.
- Tax benefit: a product may state a `market_bps`, copied onto each loan. `loan_interest_benefit` (a person for themselves, a loan administrator for anyone) returns, for a period, the interest not paid on loans charging below that rate: per instalment due, the scheduled balance before it times the rate gap divided by 12. Early closure stops it. It only reports; payroll decides whether and how it is taxed.
- A repayment cannot be reversed once the asset has been transferred.
- Open financed assets still show in the leaver's outstanding list: settlement either pays the loan off (`leaver_loan_balance` now includes `asset`) or the asset is returned.

## Functions, commands, events

**Handlers:** product CRUD; request/decide/cancel; disburse; repayment/foreclose/reverse; payroll plan/report; `complete_hire_purchase`; statement; `loan_nightly`.

**Schedule:** `loan-nightly` → `loan_nightly`.

**Events:** `loan_requested`, `loan_disbursed`, `loan_closed`, `loan_instalment_overdue`.

## Roles

| Role | Use |
|---|---|
| `loan_admin` | Products, approve, disburse, repayments/reversals |

## Boundaries

Does not run payroll; only supplies deduction plans. No GL posting of its own.
