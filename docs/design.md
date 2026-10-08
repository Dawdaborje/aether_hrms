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

## `hr_payroll` (payroll on the Swift engine)

Engine: `swift_projects/swift_payroll_engine` (`sp_engine`, built for WebAssembly without threads). Compared with
the sources: Odoo computes a payslip by running Python rules in `sequence` order over rule categories; ERPNext by
evaluating earning and deduction rows with formulas that may name each other. Both depend on the order the rows
are written in and use floating point. Here a salary structure is data, compiled once per batch into dependency
order, in exact decimals, and a payslip records which structure and which hash of it produced it.

What it does:
* **Structures** are saved once and never changed (a change is a new version); every payslip stores the code, version
  and hash. Rounding must be two digits so a payslip can store what the engine computed.
* **Assignments** put a structure on a person for dates, with their own inputs (allowances, a loan); payroll fills in
  `base_salary` (from the person's HR employment terms on the last day of the period), `period_days`, `days_worked`
  (hire and last day) and `periods_per_year`. Pay in another currency than the structure's is an error for that person.
* **Runs** (`create_pay_run` -> `start_pay_run`) are planned in pages of 100 assignments, calculated in parallel batches
  of 20 people (`aether_sdk::parallel`), and finalised by adding the batch totals in pages. A person who cannot be
  calculated gets an error row (not a payslip) and a run with an error cannot be approved. One-off inputs
  (`set_run_input`) override per person for one run.
* **Attempts**: each calculation writes its own payslips; an earlier attempt's stay as the record of what was
  calculated then. A batch writes its payslips, its errors and its report in one transaction, so repeating it is safe.
* **Approval** by someone other than the person who made the run; the event `payroll_run_approved` carries the totals
  by component for the ledger (`gl` turns it into one entry through a posting rule with `where` on the component kind).
  Employees read their own payslips only once the run is approved. A finance role records the payment.
* **Failure handling**: a job that fails for good leaves nothing in the run, so `watch_pay_runs` (every minute) polls
  the jobs of runs still calculating; a failed batch restarts the run with half the batch size, twice; a run still
  calculating after 20 minutes is marked failed.

Limits found by running it (see `projects/cspr/BREAKS.md` 39 to 45): one call may spend 50 million instructions and
10 seconds. With the demo structure 25 people per call fit and 50 do not; a call that reads 700 records fits and one
that reads 1000 does not. Hence batches of 20 (never more than 40) and pages of 100. The first big run found a bug no
small test could: batch numbers collided between planning pages, so most batches of a multi-page run were skipped
as "already done".

Rule packs and inputs (added after the first version):
* **Rule packs** (tax, social security) are stored in `pay_rule_pack` through `import_rule_pack`: the pack's own tests
  must pass, an edition (`gm.paye.demo`, later `gm.paye.2025`) is never changed. A structure names the packs it uses
  and which formulas feed each pack's inputs; they are expanded into the structure when it is saved, so the stored
  structure (and its hash) is everything a payslip needs, and its `packs` list says which editions and whether each
  was verified.
* **Unverified packs cannot pay anyone**: approving a run whose structure uses a pack with `verified: false` is
  refused unless the organisation's `allow_unverified_rules` setting (admin only, off by default) is on. The
  Gambian packs in `swift_tax_rules/countries/example/` are demonstrations with invented figures.
* **Leave and attendance**: a structure that declares `unpaid_leave_days` and/or `overtime_hours` gets them from
  `hr_leave.payroll_leave_summary` (unpaid days inside the period, by the person's calendar; a request that crosses an
  edge is recounted for the days inside) and `hr_attendance.payroll_attendance_summary` (overtime minutes as hours).
  A clerk can correct them for one person in one run; the other four standard inputs cannot be changed.

Not built: arrears and retro pay, payslip PDF and bank file, several currencies in one run, loans as their own
ledger, a Gambian pack from the real schedule.

---

# Completing the HRMS: plan and findings from the second round of source studies

Studied (read-only): Frappe HRMS `hr/doctype/*` and `payroll/doctype/*`, Odoo 19 `hr`, `hr_holidays`, `hr_holidays_attendance`,
`hr_attendance`, `hr_work_entry`, `hr_presence`, `hr_skills_*`, `hr_maintenance`, `hr_fleet`, `maintenance`, `hr_expense`.

## What the sources got wrong, collected (what every new plugin avoids)

* **Floats and silent rounding everywhere** (`flt`, `rounded()`, a per-day rate rounded before it is multiplied).
* **Status set by hand or never moved**: Frappe's FnF "Settled" and "Returned" ticks, an exit interview that never
  changes the employee, a referral whose status resets on every save, a training result that writes `status` where the
  field is `event_status`, a requisition marked Filled when one of five seats is hired.
* **Counts instead of ledgers**: asset custody by counting movements, headcount recomputed live, leave state kept on
  the allocation row (Odoo `lastcall`/`nextcall`), a vehicle odometer cancelled by subtraction.
* **No guards**: two open promotions, two overlapping staffing plans, a result for an absent trainee, an approver who is
  the requester, a deduction that takes net pay below zero, a loan deduction with no cap.
* **Revert by overwriting** (promotion/transfer cancel) and **side effects while drafting** (a draft slip accrues loan interest).

## The plugins (all Rust, all in this workspace) and the order they are built in

| # | Plugin | What | Built from |
|---|---|---|---|
| 1 | `hr_compensation` | The one funnel for extra pay: adjustments (one-off, recurring, override), incentives, retention bonuses, salary withholding, gratuity rules and calculation, arrears and payroll corrections as signed deltas, period lock | Frappe `additional_salary`, `arrear`, `payroll_correction`, `retention_bonus`, `employee_incentive`, `salary_withholding`, `gratuity*` |
| 2 | `hr_leave` 0.3 | Leave periods, policies and assignments, accrual plans with tenure levels and caps, encashment, compensatory leave lots, adjustments, approver chain by department | Frappe `leave_*`, Odoo `hr.leave.accrual.plan` |
| 3 | `hr_attendance` 0.2 | Overtime rule table and day-level approval, attendance (regularization) requests, shift requests, rotations, device keys and PIN lockout | Odoo `hr.time.rule`, Frappe `overtime_*`, `shift_*`, `attendance_request` |
| 4 | `hr_career` | Promotion, transfer and grade change as typed, approval-gated change sets that write dated employment; appointment letters from templates; grade ladders | Frappe `employee_promotion/transfer`, Odoo `hr.version` |
| 5 | `hr_workforce` | Staffing plans by period and position, job requisitions that carry seats and fill counts, tied to `hr_recruitment` openings | Frappe `staffing_plan`, `job_requisition` |
| 6 | `hr_training` | Course catalog linked to skills, sessions, enrolments, results that append skill ratings, certification expiry, budgets | Frappe `training_*`, Odoo `hr_skills_*` |
| 7 | `hr_grievance` | Case management: state machine, investigator conflict check, SLA and escalation, confidentiality, append-only case log | Frappe `employee_grievance` (extended) |
| 8 | `hr_travel` | Travel requests, itinerary, costing, per diem policy, advance on approval, link to expense reports | Frappe `travel_request` (extended) |
| 9 | `hr_assets` | Append-only custody ledger for equipment and vehicles (odometer ledger), return items for departures | Odoo `hr_maintenance`, `hr_fleet` |
| 10 | `hr_loan` | Loan products, schedules, ledger events, deductions through payroll with a cap, foreclosure quote, reversal | designed from first principles (neither source has an engine) |
| 11 | `hr_benefits` | Benefit plans and ceilings, claims, accrual ledger, health insurance enrolment | Frappe `employee_benefit_*`, `employee_health_insurance` |
| 12 | `hr_referral` | Referral state machine with a rule-defined bonus gated on tenure | Frappe `employee_referral` (fixed) |
| 13 | `hr_documents` | Identification documents with expiry sweeps, emergency contacts, bank accounts with salary split | Odoo `hr` private fields |
| 14 | `hr_onboarding` 0.2 | Exit interview (one per departure, confidential), final settlement lines generated from loans, leave encashment, gratuity and asset returns | Frappe `exit_interview`, `full_and_final_statement` |

Glue: `gl_hrms` (done) hands payroll and expense events to the ledger.

## How extra amounts reach payroll (decision)

Frappe funnels everything through one doctype, Additional Salary, overloaded by `ref_doctype`. Here `hr_compensation`
owns `pay_adjustment` (employee, code, kind earning or deduction, amount, one-off date or recurring from/to, a source
plugin and reference with an idempotency key, an explicit `override` flag) and is the **only** thing payroll asks
(`payroll_adjustments`, like leave and attendance). Loans, benefits, referrals, leave encashment, travel advances and
gratuity push adjustments into it by source. A structure picks them up with inputs named `adj_<code>`. Payroll locks
a period in `hr_compensation` when it approves a run, so an adjustment dated inside an approved period cannot change:
a correction is a new adjustment in a later period.

* **hr_compensation 0.1.0** adjustment codes, one-off and recurring adjustments (idempotent by source, refused on or
  before the payroll lock), the forward-only payroll lock, award requests with a different approver, withholding,
  gratuity with a stored rule snapshot. Loaded on a scratch kernel with `hr`: idempotent replay, payroll summary
  (`adj_bonus`) and the lock refusal verified over HTTP. Withholding and gratuity paths are compiled and unit
  tested but not yet exercised on a kernel. `hr_payroll` now calls `payroll_adjustments` and `lock_through`; that
  edit is **not compile-checked** because the external `sp_engine` path crate is absent on this machine.
* **hr_leave 0.3.1** accrual plans with service levels (the level in force on each grant day, yearly and total caps,
  idempotent keys per assignment and period, no overlapping plan for one type), encashment (type limits, days leave
  the ledger and the money goes to `hr_compensation` by source key; cancelling cancels the payment first, refused once
  payroll closed the period) and compensatory leave claims (only real days off on the person's calendar count, no
  overlap, claim window, decided by someone else, lapsing grants). Verified over HTTP on a scratch kernel; 19 native
  tests. Still not built: mandatory/stress days, approver up the department tree, policy assignments by grade,
  start-half/end-half flags. A claim is not yet checked against attendance (hr_attendance 0.2 will feed it).
* **hr_attendance 0.2.0** overtime bands per kind of day (a table of from/to minutes and a multiplier, overlapping
  bands refused, uncovered minutes count once), one overtime claim per person and day that is pending until the
  manager or an administrator decides it (never the person), sent back to pending with a note if the day is later
  recomputed to different minutes; payroll summary now also returns approved weighted, approved and pending
  overtime. Shift rotations (a cycle of shifts and days off from an anchor day, assigned like a shift). Verified over
  HTTP: 360 minutes on a weekly off cut 240 at 1.5 and 120 at 2 = 600 weighted; a rotation gave Day, Night and an
  off day on the right dates; 17 native tests. 0.2.1 fixed two limits: a rotation day off counts as a weekly off (so overtime is banded as one), and on a day with no shift a punch that belongs to a neighbouring night shift's window is not counted again (no false `odd_punches`). A person who works on such a day still shows status `unscheduled` (they had no shift), with all minutes as overtime. Not built: shift-change requests, devices with their own keys, PIN lockout.
* **hr_career 0.1.0** promotion, demotion, transfer, grade change and pay change as requests (by the person's manager or
  a career administrator, never for oneself), checked against the current terms and a grade ladder (a promotion must
  go up, a demotion down, a pay change touches only the wage, a wage must sit in its grade's band unless the
  exception is stated), decided by a career administrator who is neither the person nor the requester, and written on
  approval as a **dated employment record** in `hr` (history kept; a change not yet in force can be withdrawn and its
  record removed; one in force is corrected by a new change). Refuses dates on or before the payroll lock. Letter
  templates with `{{placeholders}}` that fail on a missing value. `hr` 0.1.2 grants `via:hr_career` access to
  employment and its pay fields. Verified over HTTP with two administrators; 4 native tests.
* **hr_workforce 0.1.0** staffing plans (period, positions, planned hires, cost per hire; draft until a second planner
  approves; no two approved plans cover one position on the same day) and hiring requisitions: growth must fit the
  seats the position has free net of seats other approved requisitions hold, and the plan line covering its date
  (no plan: allowed, marked `unplanned`); replacement and temporary skip both checks; decided by someone other than
  the requester; `start_hiring` creates the draft opening in `hr_recruitment`; withdrawing closes that opening;
  fills and time-to-fill are read back from the opening nightly. Verified over HTTP (overlap refused, plan room used
  up, self-decision refused, opening created and closed); 3 native tests. The read-back of a real fill (hire completed
  in recruitment) was not exercised. Not built: budgets rolled up the department tree (Frappe's parent-company cap).
  Also fixed: several plugins shared one `counter` model id, which the kernel refuses; recruitment, onboarding and
  payroll now get fresh ids on sync.
* **hr_training 0.1.0** courses (skill and level they build, pass score, certificate validity, seat price, mandatory),
  sessions with capacity, enrolment by the person, their manager or a planner, a waiting list promoted oldest first when
  a seat frees (someone the budget cannot cover keeps their place), no two overlapping bookings, no second seat while a
  certificate is valid and not about to lapse; seat price copied at booking and charged to the person's department
  against a yearly budget that refuses overspending unless a planner states a reason; results recorded once by a
  planner and never for themselves; a pass issues one certificate (expiry from the course) and credits the skill in
  `hr_performance` (new `credit_skill` in hr_performance 0.1.1: raise-only, for perf or training admins); expiring
  certificates warned at 30, 7 and 0 days; mandatory gaps listed. Verified over HTTP (waitlist and promotion, budget
  refusal and override, pass and fail, skill level 3 credited, gaps shrinking); 5 native tests. Not built: prerequisites
  between courses, trainer feedback forms, a failed skill credit is recorded on the certificate but not retried.
* **hr_grievance 0.1.0** (new): confidential cases. States submitted, investigating, findings submitted, resolved,
  appealed, closed, withdrawn (a table of moves, nothing else is allowed). The investigator may not be a party, nor
  the manager or report of one, nor have investigated the case before; a case manager who is a party is recused;
  the investigator proposes findings and a different case manager decides. Category fixes the time limit, copied to
  the case; nightly job escalates overdue cases once per level (1 overdue, 2 overdue twice the limit) and closes
  decided cases after the 14-day appeal window; one appeal, to someone new. The log is append-only (no rule allows
  edit or delete), numbered on the case, internal or shared; an anonymous complainant is hidden from the
  investigator, in the case and in the log. Verified over HTTP; 4 native tests. Not built: witnesses, attachments,
  escalation contacts by department (the event is emitted; nothing routes it yet).
* **hr_travel 0.1.0** (new): trips with legs and cost lines, a policy per destination class (daily allowance, meal
  deduction, lodging cap, approval limit, advance share) whose figures are copied onto the trip. Dates frame the
  legs; no two live trips for one person on a day; lodging over the cap needs a reason; the manager approves, and a
  travel administrator other than the first approver gives a second approval above the limit; never one's own trip.
  An approved trip asks hr_expense for an advance (a share of the estimate, once) and opens its expense report
  (once, from the first day); completion needs the report or a statement of no expenses; unsettled trips are
  flagged nightly. Verified over HTTP (allowance 175 with two meal days, estimate 1035, advance 621, overlap and
  second-approval refusals); 6 native tests. Not built: foreign-currency trips (one currency per trip), checking
  approved leave against the dates, booking integrations.
* **hr_assets 0.1.0** (new): equipment and vehicles in custody. One ledger (`asset_event`, never edited, numbered on
  the asset) records created, assigned, acknowledged, returned (good, damaged, lost), maintenance, retired and
  odometer; the asset's state and holder are the result of the moves (a table of moves, 2 tests). One holder at a
  time, who must be working and who acknowledges receipt (unacknowledged for a week: flagged once); a damaged
  return goes to maintenance, a lost one retires the asset; an odometer never goes backwards and the history
  reports distance under the current holder; `outstanding_assets` lists what a leaver must hand back, for the
  departure checklist. Verified over HTTP. Not built: scheduled maintenance, fuel and service costs, insurance
  and licence expiry on vehicles, wiring the departure list into hr_onboarding (next, with 0.2).
* **hr_loan 0.1.0** (new): products (limit, term, rate, reducing or flat, tenure, payroll cap), requests with the whole
  schedule made at once, approval by manager or loan admin (never the borrower), disbursement by a third person.
  Money moves only as ledger entries (disbursement, repayment, reversal), each applied once per reference; a repayment
  is spread over the oldest unpaid instalments and records exactly what it did, so a reversal undoes exactly that
  (including a foreclosure's waived interest). `payroll_loan_plan` says what to deduct, capped by the pay available,
  the rest stays due; `payroll_loan_report` records what payroll took, once per run and loan. Foreclosure quote
  charges no interest on instalments not yet due (922.05 against 942.35 scheduled in the test). `leaver_loan_balance`
  feeds the final settlement. Verified over HTTP; 7 native tests (schedules to the cent, allocation, cap,
  foreclosure). Not built: wiring into hr_payroll (its crate cannot be built here), loans in mixed currencies for one
  person, rescheduling, top-ups, a guarantor.
* **hr_benefits 0.1.0** (new): plans with a yearly ceiling and a way of paying (accrue then claim, claim against the
  ceiling, or payroll); an append-only ledger of accrual, claims and reversals, remaining never below zero and never
  above the ceiling. A claim that would go over is refused (no one-claim-per-month workaround). Manager or benefits
  admin approves, never the claimant; an approved claim becomes an hr_compensation adjustment once. Nightly accrual
  is one entry per person, plan and month (idempotent). Health insurance is an enrolment with a window of dependents
  (spouse, children), one active window per plan, no overlap. Verified over HTTP (ceiling 1200, claim 400 leaving
  800, accrue October to 1000, reverse restoring 1200); 5 native tests. Not built: receipts as attachments, prorating
  the ceiling for a mid-year join, family claims against the employee's pot, a carrier API.
* **hr_referral 0.1.0** (new): refer a candidate (never yourself; email unique), accept copies the bonus policy onto
  the referral and opens one application in hr_recruitment (`source: referral`, via:hr_referral create). States:
  submitted → in_process → hired → bonus_due → paid (or rejected / withdrawn / forfeited). The bonus waits the
  policy's days after hire and is forfeited if the hire leaves; payment is an hr_compensation adjustment once.
  `record_referral_hire` and nightly catch-up from the application; Frappe reset status to Pending and never paid.
  Verified over HTTP (accept → application, 90-day gate, pay 500.00 as adj_refbonus, forfeit, reject); 3 native
  tests. hr_recruitment 0.1.1: referral apply without recruiter when source=referral. Not built: listening to
  candidate_hired instead of polling, different bonuses by job, a public referral form.
* **Not started:** documents.
