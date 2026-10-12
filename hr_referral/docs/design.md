# Employee referrals (`hr_referral`)

## Purpose

Refer a candidate: state machine, one recruitment application, bonus gated on hire tenure, paid once via compensation. Rust → WASM.

## Dependencies

`hr`, `hr_recruitment`, `hr_compensation`, `currency`, `party`

## Models

| Model | Role |
|---|---|
| `ref_policy` | Bonus amount and wait days |
| `ref_referral` | Referral case with copied policy snapshot |
| `counter` | Numbering |

## Design

- Anyone working here can refer (never themselves, never twice for the same email). Accept opens one `hr_recruitment` application marked as referral.
- Bonus amount and wait days are **copied** onto the referral. Due only after hire tenure and still employed; leaving forfeits. Payment is one `hr_compensation` adjustment, never self-paid to the referrer for themselves.
- Nightly advances due bonuses / tenure checks.

## Functions, commands, events

**Handlers:** policy CRUD; submit/accept/withdraw/reject; `record_referral_hire`; release/pay/forfeit bonus; lists; `referral_nightly`.

**Schedule:** `referral-nightly` → `referral_nightly`.

**Events:** `referral_submitted`, `referral_hired`, `referral_bonus_due`.

## Roles

| Role | Use |
|---|---|
| `referral_admin` | Policy, accept, release/forfeit bonus |

## Boundaries

Does not own the hiring pipeline beyond opening one application. Bonus money is compensation, not a separate wallet.
