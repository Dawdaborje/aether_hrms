# Enrollment (`hr_enrollment`)

## Purpose

HR starts an enrollment with a name, email and the job. The new hire fills in their own details from an emailed link (no account), HR reviews and approves, and only then is the employee created through `hr`. Rust → WASM.

## Dependencies

`hr`, `party`

## Models

| Model | Role |
|---|---|
| `enrollment` | The person, the proposed job terms, status, link expiry, SHA-256 of the link token, resulting employee |
| `enrollment_section` | One per (enrollment, kind): `personal`, `emergency`, `identity`, `bank`, `acknowledgement`; JSON data, status, reviewer comment |
| `enr_setting` | Site address used in invitation emails, link validity in days |

## Flow and states

`draft → invited → in_progress → submitted → approved`; `submitted → changes_requested → submitted`; `expired` and `cancelled` from any open state; `expired → invited` by inviting again.

## Link token

- Made from the kernel's random request id plus a hash mix; 64 characters.
- Only its SHA-256 is stored (`token_hash`). A database leak does not leak links.
- A new invitation or send-back makes a new token; the old one stops working.
- Cleared on approve, cancel and expiry. `enrollment_tick` (hourly) expires stale links.
- Every failure on the public side gives one message, so a guess learns nothing. The kernel rate-limits anonymous clients per address.
- A link reaches the person by email (`invite_enrollment`) or, when HR chooses, by HR copying a fresh one (`copy_enrollment_link`) to send another way, such as a message. Copying makes a new link and ends the earlier one, needs an HR role, and returns the link to HR only; the stored token is a hash, so an old link can never be shown again. Whoever holds a link can fill in the form, so HR treats a copied link like a password.

## Public surface

`public_functions = enroll_get, enroll_submit`. Both need a valid token. The public page is `/enroll/{token}`. A honeypot field `website` is ignored by the page and discards a bot's submission. Public code touches only `enrollment` and `enrollment_section`.

## Hire

`approve_enrollment` merges the enrollment's job terms with every section's fields and calls `hr.hire_employee`. Section fields use HR's private-detail names, so `hr` stores them on `employee_private`. A hire hold or any HR check refuses with its own reason and the enrollment stays `submitted`.

## Not done yet

- Required sections are fixed in code (`rules.rs`), not a per-employment-type template.
- Attached documents stay on the enrollment (`files`); they are not copied to the employee.
- Identity numbers use HR's passport and visa fields; `hr_documents` lists are not filled.
- No email reminder before a link expires; no resend from the new hire's side.
- The HR "copy link" button exists (`copy_enrollment_link`); it replaces the earlier link each time it is used.

## Roles

Uses HR's `hr.hr_user` and `hr.hr_manager`. Settings need `hr_manager`.
