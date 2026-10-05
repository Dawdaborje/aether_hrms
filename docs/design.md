# HRMS design, from the real source

Written after reading Odoo 19 (`inspirations/odoo-19`), ERPNext and Frappe HRMS (`inspirations/erpnext`,
`inspirations/hrms`). Each point says what was found, where, and what Aether does about it. Paths are
relative to those checkouts. Odoo is `A/` = `addons`, `B/` = `odoo/addons/base`; HRMS is `R/` = `hrms/hrms`;
ERPNext is `E/` = `erpnext/erpnext`.

## 1. Foundation (base)

| Finding | Source | Aether decision |
|---|---|---|
| ERPNext has no party table. A person is a Customer, a Supplier and an Employee, copied three times, joined by polymorphic `Dynamic Link` rows. Merge, dedupe and "which types exist" are hand-listed everywhere. | `E/selling/doctype/customer`, `E/buying/doctype/supplier`, `E/stock/doctype/company_restriction`, `E/portal/utils.py` | Keep one `party` table. A role is data, not a table: **employee, customer, supplier link to a party**. |
| Odoo splits the employee's *work contact* (visible to everyone) from *private data* (address, bank, ID), and needs an ORM cache hack plus dozens of `groups=` redeclarations to keep private fields from leaking. | `A/hr/models/hr_employee.py` (37-43, 1335-1400), `hr_employee_public.py` | Same split, done properly: private fields live in a separate model `employee_private` with its own role gate. No hack needed. |
| ERPNext's Department was global, then had to be duplicated per company with a data migration and every user permission copied. Names carry a company suffix (`Sales - ACME`). | `E/patches/v11_0/create_department_records_for_each_company`, `E/setup/doctype/department/department.py` | `department` has a `company` link from day one and no name suffix. |
| Odoo's `hr.employee` has no `parent_path`; subordinates are found by Python recursion. Departments have a real tree. | `A/hr/models/hr_employee.py:718`, `hr_department.py` | Both use Aether hierarchy links (graph edges), so subtree and chain queries are one indexed query. |
| ERPNext stores exchange rates as floats; Branch is a bare label; amounts are floats in places. | `E/setup/doctype/currency_exchange` | Decimal everywhere money is involved. `branch`/`work_location` gets address and timezone. |
| UoM categories and a reporting currency were bolted on years later with backfills. | `E/patches/v16_0/uom_category.py`, `set_reporting_currency.py` | UoM has a category and conversions from the start; company has a reporting currency field. |
| Numbering: Odoo's `ir.sequence` has `standard` (gap-prone, concurrent) and `no_gap` (row lock) and date ranges; ERPNext keeps variables in a hook list. No HR model in Odoo even uses it. | `B/models/ir_sequence.py` | `next_number` already exists (atomic). Add the pattern/reset/company-scope version to base as `number_sequence`. |
| Company holds ~90 accounting/stock links in ERPNext, and HRMS injects more with `create_custom_fields`. | `E/setup/doctype/company/company.json`, `R/setup.py` | Company stays lean. Each plugin keeps its own `<plugin>_company_settings` model linked to the company. Extend/inherit replaces custom-field injection. |
| 120 of 251 ERPNext doctypes carry a `company` link; scoping is convention. `Company Restriction` retrofits it with two hand-kept functions per rule. | `E/stock/doctype/company_restriction/company_restriction.py` | Kernel: a model flag `company_scoped`, and record rules defined once, evaluated as a filter and as a record check. |

## 2. Employee

| Finding | Source | Decision |
|---|---|---|
| Odoo 19 removed `hr.contract`: `hr.version` is a **dated employee record** (job, department, wage, calendar, contract dates) with a partial unique index on (employee, effective date), an overlap check and a rule that the last version cannot be deleted. | `A/hr/models/hr_version.py` | Adopt: `employee` holds identity; `employment` (dated) holds job, department, manager, type, calendar, wage, contract dates. History is free. |
| ERPNext Employee is one very wide row; address, bank, family and health are free text. Naming by full name collides. | `E/setup/doctype/employee/employee.json`, `R/overrides/employee_master.py` | Addresses and bank accounts are party child records; family is `dependent`; number comes from a sequence. |
| Employee status "Left" is refused while active reports remain; disabling an employee disables their user. | `E/setup/doctype/employee/employee.py` | Keep both rules. |
| `unique(user, company)` and auto-sync of name/photo from the user. | `A/hr/models/hr_employee.py` | Keep the user link; unique per company. |
| Odoo gives the *responsible* role automatically when a user is named leave manager; HRMS does the same for Leave Approver. | `A/hr_holidays/models/hr_employee.py:319-341`, `R/overrides/employee_master.py` | Do it: a hook grants the plugin role when a link names a user. Roles are plugin-declared (built). |

## 3. Leave (rebuild)

What I built from memory sums allocations. Both real systems show why that is not enough.

* **Odoo** replays leaves against allocations on every read (`A/hr_holidays/models/hr_employee.py:546-830`, ~280
  lines) and mutates accrual allocations in place; no history of accruals, carry-over or expiry.
* **Frappe** keeps a signed, dated **ledger** (`R/hr/doctype/leave_ledger_entry`) but implements it badly: raw SQL
  deletes on cancel, expiry rows found by `LIKE` on a timestamp, balance maths spread over six functions, plus
  denormalised totals that drift (`R/hr/doctype/leave_application/leave_application.py`, `leave_allocation`).

**Decision: an append-only ledger, simplified.** Each entry is a *bucket*: `kind` (allocation, accrual,
carry_forward, usage, expiry, encashment, adjustment, reservation), signed decimal `days`, `valid_from`,
`valid_to`, source model and id. Cancelling writes a **reversal**, never a delete. Balance on a date is one pass:
sum of non-expired buckets, consumed first-expiring-first. Pending requests are `reservation` entries, which gives
Odoo's "virtual remaining" and Frappe's "balance for consumption" (min of balance and days to expiry) for free.

Other things taken from the sources:

* Leave type as flags: `max_continuous`, `applicable_after` (service days), `allow_negative` with a cap,
  `half_days`, `count_holidays`, carry-forward cap and expiry days, encashment, earned-leave frequency and
  rounding (`R/hr/doctype/leave_type`, `A/hr_holidays/models/hr_work_entry_type.py`). Odoo also locks
  `count_days_as` and `requires_allocation` once leaves exist; do the same.
* **Policy is copied onto the request** (validation type, rules). Odoo reads it live, so editing a type changes
  the flow of existing requests.
* Approval chosen per type: none, manager, HR, both (Odoo); first and second approver recorded.
* **Accrual plan** with levels (milestone after N units of service, frequency, added value, yearly and total caps,
  carry-over with expiry) from `A/hr_holidays/models/hr_leave_accrual_plan*.py`. Run by an idempotent job that
  writes ledger entries. Earned-leave failures are recorded and retried (`R/hr/utils.py allocate_earned_leaves`).
* Holiday calendars as a **dated assignment history**: employee over branch over company, gaps filled
  (`R/utils/holiday_list.py`). Bulk resolver for many employees in one query.
* Half days: a start-half and an end-half flag (clearer than Frappe's single `half_day_date`); two half-day
  leaves on one day may coexist.
* Mandatory/stress days scoped by department and job (`A/hr_holidays/models/hr_leave_mandatory_day.py`).
* Block periods with an allow-list (`R/hr/doctype/leave_block_list`).
* Real rules neither system has: **self-approval refused** (Frappe makes it a setting that a workflow bypasses; I
  found no check in Odoo's leave code), a **minimum notice** and a **sandwich rule** per type, and **the decider
  must be the designated approver** (Frappe never checks), with the approver resolved up the department tree
  (Frappe only reads the direct department).
* Transitions: reopening an approved leave is an explicit revert with a reason that writes reversals (Odoo lets
  officers flip nearly any state).
* Country rules out of the engine (Odoo hard-codes Belgian payroll codes in `_get_leaves_on_public_holiday`).
* Duration is **one pure function** (calendar intervals, holidays, window → days and hours), rounded once with a
  documented policy; Odoo's `_get_durations` is a 180-line branch tree.

## 4. Permissions

* Odoo 19 merged access and rules into one record: an operation string, an optional group, an optional domain.
  Records with a group are permissions (OR); records without are restrictions (AND). State-dependent rules are
  normal: an employee edits or deletes their own request only before approval. (`B/models/ir_access.py`,
  `A/hr_holidays/security/ir.access.csv`)
* Frappe has role × doctype × permlevel, one `if_owner` rule in all of ERPNext, no row rules in HRMS code; it
  substitutes sharing the document with the approver and a user permission on Employee
  (`R/hr/utils.py share_doc_with_approver`, `R/setup.py` user types).
* **Decision for the kernel (step E):** one rule definition = operations + roles + optional domain
  (with `$user.employee`, `$subordinates`), compiled to a query filter *and* a single-record check from the same
  source. Field-level rules are first class (`status` writable only by approvers; private fields readable only by
  HR), declared on the model. Self-service is "rows whose employee link is the caller's employee".

## 5. Build order

1. Kernel: record rules + field rules, company scoping flag, extend/inherit, dependency enforcement, workflow.
2. Base: company (lean), party roles, number_sequence, holiday calendar + assignment history, uom, exchange_rate.
3. hr: employee / employee_private / employment (dated), department, designation, work_location, onboarding
   templates.
4. hr_leave: rebuild on the ledger as above.
5. Then attendance & shifts, recruitment, expenses, appraisal, in that order, each after a study of the matching
   modules (`R/hr/doctype/attendance*`, `shift_*`, `A/hr_attendance`, `A/hr_recruitment`, `A/hr_expense`).
6. Payroll consumes: leave type pay flags, attendance counts, holiday dates per employee, join/leave dates,
   encashment entries, and a "payroll locked through date" signal that leave checks.

## Status

Built and verified on a scratch kernel (`projects/cspr/BREAKS.md` has the findings):

* **base/calendar 0.1.0** holiday calendars (weekly days off on the calendar, public/optional/company holidays,
  half days, recurring), assigned over time with the most specific subject winning and the earliest assignment
  filling gaps (Frappe). 24 checks.
* **hr 0.2.0** dated employment history (Odoo 19), private data split off, rules and roles. 39 checks.
* **hr_leave 0.2.0** the ledger, per-type rules copied onto each request, two-step approval, no self-approval,
  notice, blocks, sandwich, earned leave, yearly grants with pro rata and carry-forward with expiry, nightly
  accrual / lapse record / status roll, explicit revert. About 55 checks, plus 13 native tests of the ledger,
  calendar arithmetic and earned-leave maths.

Deliberately not built yet, from the studies: accrual plans with milestone levels and yearly caps (Odoo),
leave policy assignments (Frappe), encashment and compensatory leave, mandatory/stress days, the approver
resolved up the department tree (today: the direct manager or a leave administrator), start-half/end-half flags,
attachments, and a payroll-locked-through-date signal.

## Second round of studies (attendance, recruitment, expenses, performance)

Same method: the real Odoo 19 and Frappe HRMS source, with the findings that change the design. Where a feature
was not in a tree it says so; nothing here comes from memory.

### Attendance and shifts (`hr_attendance`)

* **Odoo 19** turned attendance into a *time record* (check-in, check-out, break, state) and moved overtime, night
  and holiday premiums into a rule engine (`hr_work_entry/models/hr_time_rule.py`, about 1000 lines of interval
  algebra that splits and retypes the records). Powerful, hard to verify: copy the **data shape** (sequence, weekday
  flags, hour window, holiday flag, tolerance, output type, pay rate), not the algebra. Its invariants are worth
  keeping: one open record per person, no overlaps, break only after check-out and never longer than the span, the
  date taken from the check-in in the person's time zone.
* **Frappe** keeps raw punches (`Employee Checkin`) apart from a daily verdict (`Attendance`), stamps the shift
  window onto every punch so a replay does not depend on later configuration, groups by (employee, shift start),
  and has a watermark (`last_sync_of_checkin`) so a day is processed only once its window has closed. Its batch
  isolates failures per group with a savepoint. Weak: an odd trailing punch is dropped silently, overtime ignores
  breaks, the leave side writes attendance with `db_set` bypassing validation, uniqueness is only a `validate`
  check, and the bulk marker swallows every exception.
* **Neither** blocks self-approval of a regularisation. Odoo's kiosk key is a bearer secret that can also assign
  badges and create employees; PINs are compared in plain equality with no lockout; coordinates are trusted from the
  client and there is no geofence. Frappe has a haversine geofence per assigned location.
* **Aether design:** raw punches immutable and append-only (a correction is a new punch that supersedes another);
  one derived day row per person and date, recomputable and diffed by an inputs hash, manager overrides kept
  separate; a pure Rust `compute_day` (window, pairing, breaks, late/early, verdict, calendar overlay) unit-tested
  without a database; odd punches and punch-on-leave conflicts are **flagged for review**, never dropped or hidden;
  absent is marked a day late and only for people employed, scheduled, not on leave and not on a holiday;
  regularisation requests whose decider can never be the person (resolved up the manager chain); per-device
  credentials, not one company-wide secret; PIN lockout; geofence as record or enforce.

### Recruitment, onboarding, offboarding (`hr_recruitment`)

* **Vacancy control is weak in both.** Odoo keeps one integer that silently floors at zero. Frappe's check counts
  offers (rejected ones included) and openings, not seats, behind a setting that is off by default, and a
  requisition does not tie to a staffing plan. **Aether:** seats are checked against the position's headcount, less
  the people who hold it and the open seats of other openings, when an opening opens and again when an offer is sent.
* **Candidate identity:** Frappe uses the email as the applicant's primary key; Odoo copies identity onto every
  application. **Aether:** one `party` per human, deduplicated at intake; an application is (party, opening).
* **Interviews:** Odoo has no rounds or feedback model. Frappe's feedback rules are good (must be a panelist, one per
  interviewer, not before the interview, ratings per expected skill) but the result is never aggregated, `Under
  Review` is never set so the feedback reminder never fires, and reminders compare dates but not times. **Aether:**
  the outcome is a function of submitted feedback; feedback is blind until each panelist submits their own.
* **Hire conversion:** Frappe gates employee creation on onboarding tasks flagged `required_for_employee_creation`,
  checked at the conversion action *and again at employee validation*, so a manual create cannot bypass it (but
  `Employee.after_insert` force-accepts offers, and cancelling onboarding deletes the tasks). Odoo has no gate and no
  duplicate-employee check. **Aether:** the gate lives in `hr`'s create validation; an existing employee for the
  party gets a new dated employment record instead of a second employee; tasks are never deleted on cancel.
* **One template engine for onboarding and offboarding:** anchor date, offsets, holiday roll-forward, assignee by
  user, role, manager (walking up the chain when a manager has no user, with loop detection), tasks that block
  employee creation, first day, last day or settlement. Departure has dismissal, last-day and archive dates, a nightly
  job that applies it, and an undo (Odoo); settlement lines and returned assets block closing (Frappe).

### Expenses (`hr_expense`)

* **Odoo** posts the accounting entry in the same step as approval, derives the expense state from the move and
  payment state, unlinks the expense when a move is reversed, and silently self-approves when nobody can approve
  (skipping the duplicate check). Its job-position limit is advisory and uses the *current* job. It has no advances.
* **Frappe** has the useful settlement model: a per-line `sanctioned <= claimed` rule (rejection zeroes it), and
  advance allocation rules (row allocation <= unclaimed - returned, total <= sanctioned + taxes, same employee and
  currency). It has no receipts, no duplicate detection, a travel request with no status and no link to an advance,
  an approver that is never validated, self-approval only as a setting a workflow bypasses, and a suspected
  double-count in its exchange gain/loss loop (read, not run).
* **Aether design:** exact decimals; the rate and its date stored; each line converted once and rounded with the
  report total equal to the sum of its lines; policy resolved against the employment on the *expense date* with
  `warn`, `justify`, `block` or `cap`; receipt and duplicate rules enforced (hash and exact key, acknowledged by the
  approver, recorded); the approver resolved from the manager chain and frozen at submission, self-approval
  impossible by construction; status stored and moved only by transitions or incoming events; **accounting and
  payroll only receive events** (`expense.report.approved` with the lines) and send reimbursement events back,
  idempotent by reference.

### Performance (`hr_performance`)

* **Odoo 19 has no appraisals, 360 feedback or training** in the community tree; only skills. Its skills are
  *versioned, not edited* (archive the old row, create a new one, one active level per skill, certification windows)
  and completions of courses and surveys write resume lines automatically.
* **Frappe** has cycles, appraisals, goals, KRAs, feedback, training and grievances, but: the final score can be a
  user-supplied Python expression (escapable sandbox, whole Employee record exposed, one typo breaks every appraisal);
  a missing component counts as zero and still divides by three; scales are hard-coded to 5 in places and read from
  metadata in others; "self-appraisal pending" is `score == 0`; any reviewer can be named for anyone; anyone who can
  read an appraisal sees who gave each piece of feedback; its skill map is unused data; a training-result bug sets the
  wrong field; there are no reminder jobs.
* **Aether design:** a declarative scorer (component weights that sum to exactly 100 as decimals, a scale object, an
  explicit missing-data policy, a minimum number of peer responses before a role's score is shown), a pure Rust
  function that stores its full breakdown and freezes at publication; reviewer eligibility and peer nomination
  enforced in data; blind feedback; weighted goal roll-up with bounded depth and an append-only check-in log; skills
  versioned with a position-requirement table for gap analysis and training that grants skills by event; a real cycle
  calendar with reminders and locking. Grievances are case management and belong in their own plugin.

### Order of building

1. `hr_attendance` (this next), 2. `hr_expense`, 3. `hr_recruitment` with onboarding/offboarding, 4. `hr_performance`.
Kernel needs that keep coming up: an **attachment field** (receipts, resumes, evidence), **role implication**,
a way to write the **public projection** of private data (Odoo's `hr.employee.public`), and a rule operator that
walks hierarchy edges (`under`) to remove the 500-item cap on team rules.

### Status of the second round

* **hr_attendance 0.1.0** built and verified (about 65 checks on a scratch kernel, 13 native tests of the day maths):
  immutable punches with supersede-style corrections; shift windows with overnight shifts; ordered pairing that flags
  what it cannot pair; recorded breaks versus the usual break; late / early with grace; thresholds; the calendar and
  leave overlay (leave wins, conflict flagged); inputs-hash recompute that keeps overrides and never touches locked
  days; absent marked a day late only for the scheduled and employed; corrections never decided by the person;
  geofence (record or enforce) with the distance stored; idempotent imports; period lock for payroll.
* Deferred from the proposal: the overtime **rule table** (v1 reports overtime as minutes beyond the schedule, all of
  it on days off), paired-span bands for payroll, devices with their own credentials and PIN lockout, rotating shift
  patterns, flexible/undefined calendars, and a fixed UTC offset per shift (no daylight-saving changes).

* **hr_expense 0.1.0** built and verified (about 90 checks on a scratch kernel, 8 native tests of the money rules):
  reports of lines; each line converted **once** to the report's currency at a cross rate stored with it (exact
  decimals, the report is the sum of its rounded lines); mileage and per diem as quantity times the category rate
  copied onto the line; policies looked up for the **expense date** (a job-scoped limit applies from the day the job
  was held, not before) with warn / justify / block / cap by day, item, report or month; receipts required over a
  threshold (or a note saying why), stored with a content hash; duplicates by exact key and by receipt hash, flagged
  and acknowledged by the approver; the approver is the first active manager, fixed at submission, never the person
  (finance decides for someone with nobody above them); approval writes approved amounts and the advances used in one
  transaction and announces `report_approved` with its lines; advances approved, paid out and returned by reference
  (idempotent), used only in their own currency up to what is unclaimed; reimbursements applied by reference, over-
  payment refused, a report paid in full closed. **No accounting entry is posted and no status is derived from one.**
* Deferred: travel requests, reopening an approved report (needs reversal rows for advance uses), per-diem rates by
  country, receipts as a real attachment field (v1 keeps storage keys on the line), employee-chosen payer rules.

### `hr_recruitment` 0.1.0 (built, about 65 checks green)

* Stages (sourcing, screening, interview, offer, hired, rejected) forward only for recruiters, any way for managers;
  *hired* only through hiring and *rejected* only with a reason. Rejecting or withdrawing winds down offers and
  interviews instead of leaving them live.
* Openings hold seats; a manager other than the requester opens them, after a check against the position's headcount
  less its holders and the seats other open openings promised. Sending an offer checks again, counting offers already out.
* One party per human, matched by normalised email at intake; one application per (person, opening).
* Rounds carry weighted criteria (weights total 100, ratings 1 to 5). Feedback comes only from panelists, once, after
  the interview starts, and is blind until the recruiter evaluates; the outcome (advance / reject / pending) is computed
  from the mean and the `strong_no` veto, and then everyone's feedback is revealed. A panelist cannot be booked twice.
* Offers: drafted, approved by a manager who did not draft, sent, answered; a salary outside the opening's band needs
  a written reason. `complete_hire` calls `hr.hire_employee` or `hr.rehire_employee`, so hire holds apply.
* `recruitment_tick` (every ten minutes) marks interviews held, evaluates, reminds, closes expired openings and offers.
* Kernel changes this needed: `id` in filters and rules, `lt` no longer matches a missing value, forward `db::related`
  returns ids when the other side is another plugin's model (BREAKS 27 to 29).
* Deferred: onboarding/offboarding (`hr_onboarding`), talent pool and duplicate-candidate merge, scheduling against
  calendars, applicant-facing portal, attachments (resume) pending the kernel `file` field.

## `hr_onboarding` design (onboarding and offboarding), from the sources

Studied: Odoo 19 `mail/models/mail_activity_plan_template.py`, `mail/wizard/mail_activity_schedule.py`,
`hr/models/hr_employee_departure.py`, `hr/models/hr_departure_reason.py`, `hr/wizard/mail_activity_schedule.py`;
Frappe HRMS `controllers/employee_boarding_controller.py`, `hr/doctype/employee_onboarding*`, `employee_separation*`,
`exit_interview`, `full_and_final_statement`, `overrides/employee_master.py`.

What they do and where they fail:

| Source | Behaviour | Weakness | Aether |
|---|---|---|---|
| Odoo plan | One `plan_date` anchor, signed day/week/month offset, responsible by user / role / manager (walks up to the first manager with a user, reports a loop) / employee | Launched by hand; offsets ignore weekends and holidays; launching twice duplicates tasks; dates never follow a changed anchor | Tasks keyed (run, step), so a run is idempotent; offsets roll to a working day through the calendar plugin; changing the anchor rebases open tasks |
| Odoo departure | Dismissal / departure / archive dates, reason, cron applies it, archives the user only if no other active employee uses it | Equipment is unassigned silently at archive; future versions deleted and not restored by cancel; one not-yet-due record aborts the batch | Departure is a state machine with a clearance checklist (returns, settlement) that blocks closing; each employee applies on its own; undo restores |
| Frappe boarding | Template rows copied to a document, tasks in a Project, status from project percent, `required_for_employee_creation` gate | Copy happens in the browser, so an API call does not expand it; role assignees are a snapshot; status written around events; cancel hard-deletes; dates roll forward only and can pass the end | Template expanded server side; assignee resolved at creation and recorded with warnings; status computed from tasks; cancel voids, never deletes; roll direction explicit, start never after end |
| Frappe separation | Tasks only; nothing sets Left or the relieving date; exit interview is free text | The three documents are unconnected | The departure drives the employee's status through `hr.change_employee_status`, and holds the interview and settlement |
| Frappe final statement | Payables are placeholder rows, assets counted by movement parity | Manual amounts; transfers miscount | Settlement lines with a source reference, settled flag and an override reason; return items are explicit records; no payroll maths here (events only) |

Decisions:
* One engine for both kinds. A **template** has steps; a step has an anchor (`start` for onboarding; `last_day` or
  `notice_date` for offboarding), a signed day offset, a duration, a roll rule (next / previous working day / none)
  and an assignee rule (employee, manager, manager's manager, a user, a role, the recruiter or HR owner of the run).
  Assignee resolution is a pure function over the management chain with a bound and loop detection, returning
  `user`, `warning` or `error`; the fallback used is stored on the task.
* **Gates**: a step may carry a gate. `hire` gates place an `hr` hire hold (holder `hr_onboarding`, key = task id) so
  the person cannot be hired until the task is done or waived; `access` and `settlement` gates block closing a departure.
  The gate lives where it is enforced (hr for hiring; the departure for closing), as agreed in the earlier study.
* **Status computed** from tasks, weighted, waived and void tasks excluded, never stored by hand.
* **Waiving** a task needs a manager and a reason; **cancelling** a run voids its tasks and releases its holds.
* **Departure**: draft, notice, clearance, closed, cancelled. Closing needs no open blocking task, no return item still
  owed, no unsettled line unless a manager overrides with a reason; then it calls `hr.change_employee_status` (which
  already refuses while people report to them), emits `employee_departed`, and keeps what the employee had before so
  an undo inside a manager's window can restore it. Each departure is applied on its own in the nightly tick.
* Deferred: exit interview forms (reuses the recruitment feedback shape), retention and anonymising of private data,
  linking user deactivation (needs a kernel call), asset custody ledger (an inventory plugin's job), accounting lines.

### `hr_onboarding` 0.1.0 (built, about 75 checks green)

* Templates and steps (anchor start / last_day / notice_date, signed offset, duration, roll next / previous / none,
  assignee employee / manager / manager2 / user / role / owner, gate none / hire / access / settlement, weight).
  Best template chosen by department and job (the more specific wins).
* Runs make one task per step, unique per (run, step); due dates follow the employee's calendar; the assignee
  fallback is written on the task; status is computed (waived and void tasks excluded). Cancelling voids.
* Hire-gated tasks hold the hire in `hr` until done or waived; assignees without an HR role can release them because
  `hr` now trusts calls that come through other plugins (kernel `via:*`, BREAKS 31).
* Departure: draft, notice, clearance, closed, cancelled; return items and settlement lines (net shown); closing
  needs a manager, a clear checklist (an open settlement line needs a written reason), and then ends the
  employment through hr. A tick moves departures to clearance and announces late tasks once, item by item.
* Deferred: exit interview, retention and anonymising, user deactivation, asset custody ledger, accounting lines.

## `hr_performance` design (appraisals, goals, skills), from the sources

Studied: Odoo 19 `hr_skills/models/{hr_skill_type,hr_skill,hr_skill_level,hr_individual_skill_mixin,hr_employee_skill,hr_job_skill}.py`,
`gamification/models/{gamification_goal,gamification_goal_definition,gamification_challenge}.py` (Odoo community has no
appraisal, no 360 feedback and no employee goals: those are Enterprise); Frappe HRMS `hr/doctype/{appraisal_cycle,
appraisal,appraisal_template,employee_performance_feedback,goal,employee_skill_map,employee_grievance}` and
`mixins/appraisal.py`.

| Source | Behaviour | Weakness | Aether |
|---|---|---|---|
| Frappe `appraisal.py` | Self, reviewer and goal scores; final = mean of the three or a `safe_eval` formula | Missing self ratings or feedback count as 0 and drag the mean down silently; a KRA with no goals scores 0; star count read from metadata in one place and hard-coded 5 in another; weights must equal 100 exactly | One scale per cycle (`scale_max`), every component normalised to 0 to 100 with exact decimals; a declarative weighted mean over named components; an explicit `missing_policy` (exclude and renormalise / count as zero / block) shown on the appraisal |
| Frappe `goal.py` | Parent progress is the unweighted mean of children; every save re-saves the appraisal | No weight, target or history on a goal; progress overwritten | Leaf progress from `current / target` with a check-in log; parent progress a weighted mean of children (one pure function); KRA completion a weighted mean of root goals |
| Frappe cycle | Not Started / In Progress / Completed; completing locks everything; no reminders; template read live | One global lock; template edits change old appraisals | Phases (goal setting, self review, manager review, calibration, published, closed), each writing only in its phase; template snapshotted onto the appraisal; tick reminds once per phase deadline |
| Frappe feedback | Anyone with the role reads reviewers' text and scores; any reviewer | No assignment, no anonymity, no calibration | Reviewers assigned by role (manager, peer); blind until publication; a minimum number of peers; calibration is one recorded change with a reason, on the appraisal |
| Odoo skills | Level ladder per skill type; employee skills versioned by `valid_from` / `valid_to`; removal expires, not deletes | Levels self-asserted; ladder not versioned; no job targets compared | Append-only ratings with a reason and who rated; job requirements with a target level and weight; a gap report |

Decisions:
* **Scale**: ratings are whole numbers 1 to `scale_max` (3 to 10). Component score = sum(rating x weight) / scale_max
  for weights adding to 100, giving 0 to 100; shown on the scale as score x scale_max / 100.
* **Components** and their cycle weights (adding to 100): `goal`, `self`, `manager`, `peer`. Peer needs at least
  `min_peers` answers, else it counts as missing. The final score is the weighted mean of the components present
  (exclude), or with absent ones as 0 (zero), or publication is refused (block). Calibration may replace the final
  score once, with a reason, and keeps the computed one beside it.
* **Visibility** by phase, through rules: an employee reads their own appraisal during self review and again once
  published, never in between; their manager reads their reports'; reviewers see only the feedback form they owe.
* **Goals** per employee per cycle, in a tree (graph hierarchy), a KRA name from the template, a weight among
  siblings; progress is stored and recomputed up the tree in the same call that changes a child.
* Deferred: grievances (own plugin), training and skill-gap-driven development plans, 360 anonymity beyond blindness,
  automatic skill changes from appraisals.

### `hr_performance` 0.1.0 (built, about 85 checks green)

* Templates (key result areas and criteria, weights adding to 100), cycles (scale 3 to 10, component weights adding
  to 100, `missing_policy` exclude / zero / block, `min_peers`, a deadline per phase), appraisals with the template
  copied onto them.
* Phases: draft, goal setting, self review, manager review, calibration, published, closed, forward one at a time with
  checks (appraisals exist; scoring possible under the policy; every appraisal has a final score). Goals are written in
  goal setting and self review, progress reported until the managers' review ends, scores calibrated in calibration.
* Scores exact on 0 to 100 and shown on the cycle's scale; goal component from the goal tree (leaf: current / target
  with a check-in log; group: weighted mean of children); manager and peer from submitted reviews; calibration replaces
  the final score once with a reason and keeps the computed one.
* Blind by rules: the employee reads their appraisal in self review and after publication only; reviewers see the
  appraisal only while they owe a review; feedback rows are never readable by the person reviewed.
* Skills: levels per skill scale, appended with who rated and why (a change on the same day corrects the entry);
  job requirements with targets and weights; a gap report.
* Deferred: grievances (own plugin), training links and development plans, anonymous peer aggregation beyond
  blindness, automatic skill changes from appraisals, goal metric definitions.
