# Recruitment (`hr_recruitment`)

## Purpose

Openings (seat-checked), applications on `party`, interview rounds with blind feedback, offers with second approval, and hire handoff into `hr`. Rust → WASM.

## Dependencies

`hr`, `party`, `currency`

## Models

| Model | Role |
|---|---|
| `rec_stage` | Pipeline stages with kind/sequence |
| `rec_reject_reason` | Rejection taxonomy |
| `rec_opening` | Opening for position seats + salary band |
| `rec_application` | (`party`, opening) in a stage |
| `rec_round` | Interview round + criteria |
| `rec_interview` / `rec_feedback` | Panel booking and ratings |
| `rec_offer` | Offer awaiting second approval / response |
| `counter` | Numbering |

## Design

- Openings open only when the position has free seats not already promised by other openings; seats rechecked when an offer is sent.
- One `party` per candidate; application is that party + opening (email normalised for dedupe).
- Interview outcome is a **function of feedback** (weights, min panelists); feedback stays blind until the interviewer has submitted their own.
- Offers need a second approver and stay in band unless an exception is stated. Hire calls into `hr` so holds and validation apply; no second employee invent path.

## Functions, commands, events

**Handlers (selected):** stage/round/reason seed & CRUD; opening open/hold/close; apply/move/reject/withdraw; schedule/cancel interview, `submit_feedback`, `evaluate_interview`; offer create/approve/send/respond; `complete_hire`; `recruitment_tick`; rule vars for my openings/applications/interviews.

**Schedule:** `recruitment-tick` → `recruitment_tick` (every 10m).

**Events:** `opening_opened`, `application_stage_changed`, `interview_reminder`, `interview_outcome_ready`, `offer_sent`, `offer_accepted`, `candidate_hired`.

## Roles

| Role | Use |
|---|---|
| `recruiter` | Applications, schedule, draft offers |
| `recruit_manager` | Stages/rounds/openings, approve offers, send back |
| `recruit_corrector` | Delete feedback (break-glass) |

## Boundaries

Does not create employees itself beyond calling `hr`. Staffing plans/requisitions live in `hr_workforce`. Referrals open applications via `hr_referral`.
