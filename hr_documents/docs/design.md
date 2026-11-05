# Employee documents (`hr_documents`)

## Purpose

Lists of identity documents (with expiry), emergency contacts, and bank accounts with a salary split—instead of flattening one of each onto the person. Rust → WASM.

## Dependencies

`hr`

## Models

| Model | Role |
|---|---|
| `doc_identity` | Passport, national id, visa, work permit, licence, … + expiry |
| `doc_emergency` | Emergency contacts (exactly one primary) |
| `doc_bank` | Bank accounts with salary share % |

## Design

- Each kind is a **list**, not a single private field. Nightly warns at 30 days, 7 days, and on expiry day (`document_expiring`).
- Emergency contacts: one primary. Bank shares of net salary must sum to **exactly 100**.
- Self-service for the employee; `documents_admin` sees everyone. Complements (does not replace) `employee_private` for other private fields.

## Functions, commands, events

**Handlers:** identity/emergency/bank add/update/remove + my/list; `salary_split`; `expiring_documents`; `documents_nightly`.

**Schedule:** `documents-nightly` → `documents_nightly`.

**Events:** `document_expiring`.

## Roles

| Role | Use |
|---|---|
| `documents_admin` | All employees’ documents, contacts, banks |

## Boundaries

Not a DMS/file vault beyond stored fields/attachments the models allow. Payroll reading of bank split is a consumer concern; this plugin only enforces the 100% rule.
