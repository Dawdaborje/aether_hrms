# Performance (`hr_performance`)

## Purpose

Appraisal cycles with phased writes, weighted goals, blind reviews, calibration, and skills with history vs job requirements. Rust → WASM.

## Dependencies

`hr`

## Models

| Model | Role |
|---|---|
| `perf_template` | Weighted criteria for reviews |
| `perf_cycle` | Phased cycle (goal → self → manager → calibrate → publish) |
| `perf_appraisal` | Per-person appraisal in a cycle |
| `perf_feedback` | Self / manager / peer ratings |
| `perf_goal` / `perf_checkin` | Goal tree + progress check-ins |
| `perf_skill` / `perf_skill_rating` / `perf_skill_requirement` | Skills, rated history, job targets |

## Design

- Cycle advances one phase at a time; each phase allows its own writes. Scores use one scale: template-weighted criteria, goals rolled up by weight, final score with an explicit missing-data rule (omit / zero / refuse).
- Reviews are blind: employees see nothing between self review and publication.
- Skills are versioned history (who, level, why); training may raise a level via `hr_performance` calls, never lower it from training.
- Hourly tick announces deadlines / phase pressure.

## Functions, commands, events

**Handlers (selected):** template/cycle CRUD, `open_cycle_appraisals`, `advance_cycle`, self/peer/manager submit, `calibrate_appraisal`, `refresh_appraisal`; goal CRUD/progress; skill credit/rate/requirements/gaps; `performance_tick`.

**Schedule:** `performance-tick` → `performance_tick` (every 1h).

**Events:** `performance_phase_changed`, `appraisal_published`, `performance_deadline`.

## Roles

| Role | Use |
|---|---|
| `perf_admin` | Templates, cycles, open appraisals, peers, calibration |

## Boundaries

No compensation decisions (those are `hr_compensation` / `hr_career`). Training certificates raise skills through `hr_training` → this plugin’s skill APIs.
