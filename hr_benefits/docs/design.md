# Benefits (`hr_benefits`)

## Purpose

Benefit plans with yearly ceilings, accrual ledger, claims, and health-insurance enrolment with dependents. Rust → WASM.

## Dependencies

`hr`, `hr_compensation`, `currency`

## Models

| Model | Role |
|---|---|
| `bnf_plan` | Yearly ceiling + pay mode (accrue / claim / payroll) |
| `bnf_enrolment` / `bnf_dependent` | Enrolment window + dependents |
| `bnf_claim` | Claim against remaining balance |
| `bnf_entry` | Accrual / claim / reversal ledger (never deleted) |
| `counter` | Numbering |

## Design

- Accrual, claims, reversals are ledger entries. Claims that exceed remaining balance are refused (no one-claim-per-month workaround). Manager or benefits admin approves; never the claimant. Approved claim becomes a compensation adjustment once.
- Health insurance: dependents within plan rules, one active enrolment per plan and year, no overlapping windows.
- **Note:** leftover `ben_*` model/rule JSON files exist beside the live `bnf_*` models; access and handlers use `bnf_*` only.

## Functions, commands, events

**Handlers:** plan CRUD; enrol/end; claim/decide/cancel/reverse; `benefit_balance`; lists; `benefits_nightly`.

**Schedule:** `benefits-nightly` → `benefits_nightly`.

**Events:** `benefit_claimed`, `benefit_approved`.

## Roles

| Role | Use |
|---|---|
| `benefits_admin` | Plans, enrolments, claim approval, accrual |

## Boundaries

Pays through `hr_compensation` adjustments / payroll—does not invent payslip components. Does not own `hr.dependent` family records used elsewhere.
