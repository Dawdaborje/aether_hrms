# Assets in custody (`hr_assets`)

## Purpose

Equipment and vehicles held by employees, with an append-only custody ledger and odometer. Rust → WASM.

## Dependencies

`hr`

## Models

| Model | Role |
|---|---|
| `asset_category` | Asset classes |
| `asset` | Asset; state/holder derived from ledger |
| `asset_event` | Append-only custody/maintenance/odometer events |
| `counter` | Numbering |

## Design

- At most one holder. Assign, acknowledge, return (good/damaged/lost), maintenance, retire, odometer are ledger entries—never edited. Asset state follows the last entries.
- Damaged return → maintenance; lost → retire. Odometer cannot go backwards. `outstanding_assets` feeds departure checklists. Nightly flags unacknowledged assignments.

## Functions, commands, events

**Handlers:** category/asset CRUD; assign/acknowledge/return; maintenance start/finish; retire; odometer; history; `outstanding_assets`, `my_assets`; `assets_nightly`.

**Schedule:** `assets-nightly` → `assets_nightly`.

**Events:** `asset_assigned`, `asset_returned`, `asset_unacknowledged`.

## Roles

| Role | Use |
|---|---|
| `asset_admin` | Register, assign, maintain, retire |

## Boundaries

Not fixed-asset accounting/depreciation (`gl`). Onboarding reads outstanding via API; does not own departure state.
