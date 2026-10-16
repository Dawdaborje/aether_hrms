# hr_insurance end-to-end checks

Plain Python 3 over HTTP, like `plugins/accounting/tests/gl_payment_e2e.py`.

## One-time setup (the `project/test` project)

```sh
cd project/test
A="../../aether/target/debug/aether -c aether.toml"
$A --load-plugins ../../plugins/base ../../plugins/hrms
$A --create-org "Insurance Test" --org-db-name ins_test --username ins_owner --email ins_owner@test.local --password Test-pass-123
$A --install-plugin hr_insurance --org ins_test
for u in ins_admin ins_rev ins_emp_a ins_emp_b; do
  $A --create-user --username $u --email $u@test.local --password Test-pass-123 --org-db-name ins_test
done
$A --grant-role hr_insurance.ins_admin --username ins_admin --org ins_test
$A --grant-role hr_insurance.ins_reviewer --username ins_rev --org ins_test
$A --grant-role hr.hr_manager --username ins_admin --org ins_test   # to hire the two employees
$A --grant-role hr_benefits.benefits_admin --username ins_admin --org ins_test   # for the migration check
$A --grant-role hr.hr_user --username ins_emp_a --org ins_test      # so each employee can link themself
$A --grant-role hr.hr_user --username ins_emp_b --org ins_test
```

Then, with the server up, hire two employees as `ins_admin` (`hr.hire_employee`) and have each of `ins_emp_a` and
`ins_emp_b` call `hr.link_me` with their employee id.
The script stops with a message if the two logins are not linked to employees.

## Running

```sh
python3 plugins/hrms/hr_insurance/tests/hr_insurance_e2e.py -v
```

It covers: eligibility (parent, over-age child), overlapping enrolments, privacy between employees, network-only
providers, preauthorisation threshold, deductible then copay then limit, no self-decisions, deciding once, computed
utilisation, idempotent payment, idempotent premium generation, the employer/employee split, two reviewers
deciding for one member at once (the limit and deductible hold), paying by id from a list button, one-line claim forms, refusals with a reason, adding a dependant later, privacy of covered people, the dashboard and the two reports, and the migration from `hr_benefits`.

The commands above use the paths of the `project/test` layout; for a scratch copy, point `-c` at your own config and pass `--base` to the script.
