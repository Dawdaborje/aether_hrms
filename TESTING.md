# Walk-through checklist

Nothing built in the gap-closing work (payroll, leave and attendance automation, setup pages, recruitment,
performance, separation, reports) has been run on a live instance. This is the order that finds problems fastest.

## 0. Before anything
1. Start the server with `--watch`, or load by hand: `aether -c <config> --load-plugins`, then
   `--upgrade-plugin all --org <org>`. Upgrade runs `hr_payroll.seed_rule_packs`.
2. `aether -c <config> --load-plugins --dry-run` must list every hr_* package as `ok`. (It caught a bad link field in
   `hr_career` already. `erp_item` and `erp_quality` fail it for reasons outside HR.)
3. After editing a plugin: `cargo build --release --target wasm32-unknown-unknown` then `make` in that plugin.

## 1. People (hr)
- Edit an employee: set Reports to, Department, Job, a private field. Save, reload: values stay; an `employment`
  row starts today. (This was the "saved but nothing changed" bug: the installed build was stale.)
- Title bar: Save and the status tracker stay at the top with tabs (Leave, Attendance, Career…).
- Reports → Turnover shows figures and charts; Excel and PDF buttons download.

## 2. Setup (hr_config)
- Payroll: settings, components (open one, edit, save), periods (edit), tax slab (new, 3 bands), exemptions,
  Tax and contribution rules (20 packs after upgrade; Load shipped packs is a no-op the second time).
- Attendance: shifts and locations (open, edit), rotations (new), assign a department to a shift.
- Leave: periods, approvers, types. Recruitment: sources, templates, offer terms, pools.

## 3. Payroll
- Pay runs: New run → Calculate → wait → Approve. Register (Excel) downloads. Run again for the same period: refused.
- My payslips and My tax declarations as an ordinary employee (the category picker must not be empty).
- Correct a pay run: pick an approved run, one person, a reason. Calculate it, approve it (someone other than its maker).
  Approving emits `payroll_correction_approved` with `net_difference`, not `payroll_run_approved`; paying it reports the difference.
  "Correction difference (Excel)" shows original, corrected and difference per person.
- Close a period, then try a run inside it: refused. Reopen: allowed.

## 4. Leave and attendance
- Bulk allocate leave twice: the second adds nothing. Request leave as someone with no manager: goes to the department approver.
- Shift request and attendance request: submit, approve from the approvals pages. Kiosk punch with PIN.

## 5. Other plugins
- Loans: ask, approve from Loan approvals, cancel. Benefits: claim, approve. Referrals: submit, reject with a reason.
- Grievance: raise (also anonymous), dismiss with a reason. Training: enrol.
- Offboarding: Record departure; notice shortfall; Waive it; Close. Letters: issue an appointment letter, accept it as the employee.
- Recruitment: public /careers apply, honeypot, resume file. Performance: KRAs, feedback notes.
- Any file field: the eye button opens an image, a PDF, a CSV and a text file; a .docx, .xlsx and .pptx show text or the first sheet; an old .doc/.xls shows the download fallback.

## 6. Added in the parity pass
- Org chart (`/hr/org-chart`), Who is in and Where I work, Roster calendar, Mark attendance for a department.
- Approvals inbox (`hr_approvals`): items from leave, loans, expenses, travel, benefits, attendance, timesheets.
- Fleet (`hr_fleet`): vehicle, assign, log fuel (odometer cannot go down), record a service with a next-due date.
- Timesheet (`hr_timesheet`): project, log hours (24 a day cap), submit, approve as the manager, not as yourself.
- Bank file: CSV and ISO 20022 from an approved run (set the paying IBAN in payroll settings first).
- Other income, exemption sub-category with a parent cap, proof file, expense tax, training feedback, cost centre.
- `hr_payroll.simulate_package` (call it by API): net for a wage under a structure.

## 7. Payslip explainer, review before approval, My HR
- My payslips: open a payslip; each line shows its formula and the values it read. Payslips calculated before this
  change say to recalculate the run (the inputs are kept from now on).
- Pay runs: open a calculated run to see who changed more than 20%, who is new, who dropped out, who has no bank
  account (the bank check needs payroll finance access; without it that one flag is skipped).
- `/hr/home`: waiting on you, open tasks, away today, latest net pay, leave available.

## Known gaps (see the last report)
Gambia has no pack (sources disagree, see TO_CONFIRM.md); all rule packs are unverified; ERP packages `erp_item` and `erp_quality` have manifest errors.
