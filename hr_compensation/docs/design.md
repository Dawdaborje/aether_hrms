# Compensation (`hr_compensation`)

## Purpose

Single funnel for payslip extras outside the salary structure: coded adjustments, awards, monthly withholding/release, gratuity, and the period lock payroll uses. Rust → WASM.

## Dependencies

`hr`

## Models

| Model | Role |
|---|---|
| `comp_code` | Pay codes (`adj_<code>` totals for payroll) |
| `comp_adjustment` | One-off or recurring; source + idempotency key |
| `comp_award` | Incentive/retention; second-person decision |
| `comp_withholding` / `comp_withholding_cycle` | Withhold by month; release one month at a time |
| `comp_gratuity_rule` / `comp_gratuity` | Slab rules; stored copy of rule used |
| `comp_lock` | Closed payroll periods |
| `counter` | Numbering |

## Design

- Amounts land in a period by their dates. Payroll calls `payroll_adjustments` and `lock_through` on approve so a closed period never changes; corrections are new adjustments in a later period.
- Awards: requester ≠ decider. Adjustments carry an idempotency key for safe retries from benefits/referrals/etc.
- No events declared; other plugins call in. No scheduled jobs in this plugin.

## Functions, commands, events

**Handlers:** code CRUD; `add_adjustment` / cancel/end/list; `payroll_adjustments`, `lock_through`, `get_lock`; award request/decide; gratuity rule/calculate/approve; withholding start/release/cancel.

**Events / schedule:** none.

## Roles

| Role | Use |
|---|---|
| `comp_admin` | Codes, adjustments, awards, withhold/release, gratuity |

## Boundaries

Does not calculate base salary structures (`hr_payroll`). Career pay changes write dated `employment` via `hr_career` → `hr`, and may add adjustments here when needed.
