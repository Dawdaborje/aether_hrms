# Insurance (`hr_insurance`)

## Purpose

Group health insurance for employees: insurers, plans, policies by plan year, enrolments with covered members,
preauthorisations, claims with lines, premiums through payroll, payments, and events for the ledger. Rust → WASM.
Built so that policy years, utilisation and money cannot drift: terms live on plans, years on policies, nothing is stored that can be derived.

## Dependencies

`hr`, `hr_compensation`, `hr_payroll` (only to hear `payroll_run_approved`), `hr_benefits` (migration), `currency`.

## Models

| Model | Role |
|---|---|
| `ins_insurer`, `ins_plan` | Who insures; the terms: member limit, deductible, copay %, network-only, preauth threshold, premium and employer share, eligibility |
| `ins_policy` | A plan for a plan year; a renewal is a new policy (`renewal_of`) |
| `ins_enrolment` + `ins_member` (child rows) | An employee on a policy for a window, with the people covered; many per employee over the years |
| `ins_provider` | Health facility, with a network flag |
| `ins_preauth` | Approved amount valid until a date, consumed by claims |
| `ins_claim` + `ins_claim_line` (child rows) | Claim; the claimed amount is computed from the lines |
| `ins_premium` | One per enrolment and month: employer and employee share |
| `ins_payment` | Claim reimbursement, premium remittance |
| `ins_decision` | Unique decision numbers per member: the optimistic lock |
| `counter` | Numbering |

## Design

- **One claim engine** (`rules::settle`): deductible left → copay on the rest → capped by the member's limit left.
  Deductible/limit used are summed from approved and paid claims of the same policy and member at decision time;
  utilisation is never stored, so it cannot drift.
- **Separation of duties:** the claimant never decides or pays their own claim. State, money and decision fields are
  writable only by this plugin's functions (`via:self`), so nothing is approved by editing a status.
- **Employees see their own and, as managers, their team's** records (covered people carry the employee, so the same rule applies to them); reviewers/administrators see all.
- **Renewals and years:** a policy is per plan year; enrolments never overlap on a policy; an employee has one per year.
- **Payroll:** the employee premium share and approved reimbursements are `hr_compensation` adjustments keyed by the
  premium / claim (so each exists once). `payroll_run_approved` marks premiums deducted.
- **Ledger:** `claim_submitted`, `claim_approved`, `claim_paid`, `claim_refunded`, `premium_accrued`, `premium_paid`,
  `policy_expiring` are announced, and `claim_pending_review` nightly for claims waiting over a week; `gl_hrms` posting rules are a follow-up.
- **Idempotency:** premiums by `prem:<enrolment>:<YYYY-MM>`; payments by claim/premium.

## Screens

| Route | For |
|---|---|
| `/hr/insurance` | My claims; `New claim` takes one line flat (category, amount, description), several lines through the function |
| `/hr/insurance/preauths` | Ask for approval of planned care; reviewers decide at `/hr/insurance/preauths/decide` |
| `/hr/insurance/overview` | Reviewer dashboard: claims and requests waiting, policies, people covered |
| `/hr/insurance/review`, `/reject` | Claims waiting; approve in the list, refuse with a reason on the form |
| `/hr/insurance/pay` | Approved claims; paying makes the payroll reimbursement once |
| `/hr/insurance/insurers`, `plans`, `policies`, `providers` | Setup |
| `/hr/insurance/enrolments`, `members/new` | Enrol an employee; cover a spouse, child or parent later (eligibility is checked again) |
| `/hr/insurance/premiums`, `payments` | Monthly premiums (create a month, remit to the insurer) and the payment log |
| `/hr/insurance/reports/coverage`, `reports/premiums` | Who is covered; premiums against approved claims per policy |

The employee's Insurance tab (own record and team's) shows limit, used and left per covered person.

## Concurrency

Each decision on a member takes the next number in `ins_decision` (key `enrolment:member:seq`, unique) in the same
transaction as the claim update. Two reviewers deciding for one member at once compute the same number; the second
write is refused and that decision is worked out again against what the first one used.

## Migration from `hr_benefits`

`migrate_from_benefits` (command, also callable) copies `bnf_enrolment` rows that have a carrier or policy number,
with their `bnf_dependent` rows, into insurers/plans/policies/enrolments. The old records are left in place and a row
already moved is skipped (`BNF-<old id>`). Run it as someone who may read benefits dependents (`hr_benefits.benefits_admin`
or the `via:hr_insurance` rule on `bnf_dependent`).

## Known limits

- Claim events other than premiums are announced but `gl_hrms` listens only to the premium events (see its design notes).
- `hr_compensation`'s `comp_code` rule now lets other plugins create pay codes (`via:*`); the plugin relies on it.
- Role grants need a server restart to take effect in a running server (observed in the dev server).

## Roles

| Role | Use |
|---|---|
| `ins_admin` | Insurers, plans, policies, enrolments, premiums, payments |
| `ins_reviewer` | Claims and preauthorisation decisions |
