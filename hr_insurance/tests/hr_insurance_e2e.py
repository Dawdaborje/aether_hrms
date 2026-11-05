# End-to-end checks of hr_insurance against a running kernel. See README.md next to this file for setup.
# Usage: python3 hr_insurance_e2e.py [--base http://localhost:7890] [--org ins_test] [-v]
# Every run uses fresh codes, so it can run again on the same organization.
import argparse, json, secrets, sys, urllib.error, urllib.request
from datetime import date, timedelta
from decimal import Decimal

ap = argparse.ArgumentParser()
ap.add_argument("--base", default="http://localhost:7890")
ap.add_argument("--org", default="ins_test")
ap.add_argument("--password", default="Test-pass-123")
ap.add_argument("-v", action="store_true")
args = ap.parse_args()
RUN = secrets.token_hex(3).upper()


class Failed(Exception):
    def __init__(self, status, data):
        super().__init__(f"{status}: {json.dumps(data)[:400]}")
        self.status, self.data = status, data


class Session:
    def __init__(self, username):
        self.cookies = {"aether_csrf": secrets.token_hex(16)}
        self.name = username
        self.post("/api/auth/login", {"username": username, "password": args.password})
        self.post("/api/auth/org", {"org": args.org})

    def post(self, path, body):
        r = urllib.request.Request(args.base + path, data=json.dumps(body).encode(), method="POST")
        r.add_header("content-type", "application/json")
        r.add_header("origin", args.base)
        r.add_header("x-csrf-token", self.cookies["aether_csrf"])
        r.add_header("cookie", "; ".join(f"{k}={v}" for k, v in self.cookies.items()))
        try:
            resp = urllib.request.urlopen(r)
            status, raw, headers = resp.status, resp.read(), resp.headers
        except urllib.error.HTTPError as e:
            status, raw, headers = e.code, e.read(), e.headers
        for c in headers.get_all("set-cookie") or []:
            k, _, rest = c.partition("=")
            self.cookies[k.strip()] = rest.split(";")[0]
        try:
            data = json.loads(raw) if raw else None
        except ValueError:
            data = {"_raw": raw.decode(errors="replace")}
        if status >= 400:
            raise Failed(status, data)
        return data

    def call(self, fn, body=None, plugin="hr_insurance"):
        out = self.post(f"/api/plugins/{plugin}/{fn}", body or {})
        if args.v:
            print(f"    {self.name} {plugin}.{fn} -> {json.dumps(out)[:300]}")
        return out


results = []


def check(name, cond, detail=""):
    results.append((name, bool(cond)))
    print(("  ok    " if cond else "  FAIL  ") + name + ("" if cond else f"  [{detail}]"))


def refused(name, action, words):
    try:
        action()
    except Failed as f:
        msg = json.dumps(f.data)
        check(name, any(w in msg for w in words), msg[:300])
        return
    check(name, False, "was allowed")


def d(v):
    return Decimal(str(v)) if v is not None else Decimal(0)


admin, rev, emp_a, emp_b = (Session(u) for u in ("ins_admin", "ins_rev", "ins_emp_a", "ins_emp_b"))
me_a = emp_a.call("my_employee", plugin="hr")
me_b = emp_b.call("my_employee", plugin="hr")
if not me_a or not me_b:
    sys.exit("ins_emp_a and ins_emp_b must each be linked to an employee (hr.link_user): see README.md")
today = date.today()
Y0, Y1 = date(today.year, 1, 1), date(today.year, 12, 31)

print("setup")
insurer = admin.call("create_insurer", {"code": f"INS{RUN}", "name": f"Insurer {RUN}"})
plan = admin.call("create_insurance_plan", {
    "code": f"PL{RUN}", "name": f"Plan {RUN}", "insurer": insurer["id"], "currency": "USD", "member_limit": "1000.00",
    "deductible": "100.00", "copay_percent": "10", "premium_amount": "1200.00", "premium_period": "annual",
    "employer_share_percent": "75", "preauth_threshold": "800.00", "max_children": 2, "child_max_age": 25, "network_only": True,
})
refused("a reviewer cannot make plans for a missing insurer", lambda: rev.call("create_insurance_plan", {"code": "X", "name": "x", "insurer": "nope", "currency": "USD", "member_limit": "1", "premium_amount": "1"}), ["insurer", "no "])
net = admin.call("create_insurance_provider", {"name": f"Network clinic {RUN}", "is_network": True})
out = admin.call("create_insurance_provider", {"name": f"Outside clinic {RUN}", "is_network": False})
policy = admin.call("create_policy", {"number": f"POL-{RUN}", "plan": plan["id"], "start_date": Y0.isoformat(), "end_date": Y1.isoformat()})
policy = admin.call("activate_policy", {"id": policy["id"]})
check("policy is active", policy["state"] == "active")

print("enrolment")
refused("employees do not enrol themselves", lambda: emp_a.call("enrol_in_policy", {"policy": policy["id"], "employee": me_a["id"], "start_date": Y0.isoformat()}), ["insurance", "role", "staff"])
refused("a parent is not covered", lambda: admin.call("enrol_in_policy", {"policy": policy["id"], "employee": me_a["id"], "start_date": Y0.isoformat(),
        "members": [{"relationship": "parent", "name": "P"}]}), ["covers"])
refused("an over-age child is refused", lambda: admin.call("enrol_in_policy", {"policy": policy["id"], "employee": me_a["id"], "start_date": Y0.isoformat(),
        "members": [{"relationship": "child", "name": "Old", "date_of_birth": "1980-01-01"}]}), ["over"])
enr_a = admin.call("enrol_in_policy", {"policy": policy["id"], "employee": me_a["id"], "start_date": Y0.isoformat()})
enr_b = admin.call("enrol_in_policy", {"policy": policy["id"], "employee": me_b["id"], "start_date": Y0.isoformat()})
refused("no overlapping enrolment on a policy", lambda: admin.call("enrol_in_policy", {"policy": policy["id"], "employee": me_a["id"], "start_date": Y0.isoformat()}), ["already"])
full_a = emp_a.call("get_insurance_enrolment", {"id": enr_a["id"]})
members = full_a.get("members") or []
check("the employee is covered as a member", len(members) == 1 and members[0]["relationship"] == "self", members)
refused("another employee cannot read it", lambda: emp_b.call("get_insurance_enrolment", {"id": enr_a["id"]}), ["another", "denied", "not"])

print("claims")
svc = (today - timedelta(days=3)).isoformat()
refused("a non-network provider is refused", lambda: emp_a.call("submit_insurance_claim", {
    "enrolment": enr_a["id"], "provider": out["id"], "service_date": svc, "lines": [{"description": "Visit", "amount": "300.00"}]}), ["network"])
refused("a claim of 1000 needs a preauthorisation", lambda: emp_a.call("submit_insurance_claim", {
    "enrolment": enr_a["id"], "provider": net["id"], "service_date": svc, "lines": [{"description": "Surgery", "amount": "900.00"}]}), ["preauth"])
pre = emp_a.call("request_preauthorisation", {"enrolment": enr_a["id"], "provider": net["id"], "procedure": "Surgery", "estimated_amount": "1000.00"})
refused("employees cannot decide a preauthorisation", lambda: emp_a.call("decide_preauthorisation", {"id": pre["id"], "approve": True, "approved_amount": "1000.00", "valid_until": Y1.isoformat()}), ["insurance", "role", "staff"])
pre = rev.call("decide_preauthorisation", {"id": pre["id"], "approve": True, "approved_amount": "1000.00", "valid_until": Y1.isoformat()})
check("preauthorisation approved", pre["state"] == "approved")
claim = emp_a.call("submit_insurance_claim", {"enrolment": enr_a["id"], "provider": net["id"], "preauth": pre["id"], "service_date": svc,
                                              "lines": [{"description": "Visit", "amount": "600.00"}, {"description": "Drugs", "amount": "400.00"}]})
check("claim waits for a decision", claim["state"] == "submitted")
refused("employees cannot approve", lambda: emp_a.call("approve_insurance_claim", {"id": claim["id"]}), ["insurance", "role", "staff"])
decided = rev.call("approve_insurance_claim", {"id": claim["id"]})
# 1000 - 100 deductible = 900; 10% copay = 90; approved 810 (limit 1000 not reached)
check("deductible then copay applied", d(decided["approved_amount"]) == Decimal("810.00") and d(decided["deductible_applied"]) == Decimal("100.00"), decided)
refused("deciding twice is refused", lambda: rev.call("approve_insurance_claim", {"id": claim["id"]}), ["waiting"])
u = emp_a.call("insurance_utilisation", {"enrolment": enr_a["id"], "member": members[0]["id"]})
check("utilisation is computed", d(u["used"]) == Decimal("810.00") and d(u["remaining"]) == Decimal("190.00"), u)
second = emp_a.call("submit_insurance_claim", {"enrolment": enr_a["id"], "provider": net["id"], "service_date": svc, "lines": [{"description": "More", "amount": "500.00"}]})
capped = rev.call("approve_insurance_claim", {"id": second["id"]})
consumed = [x for x in rev.call("list_preauthorisations") if x["id"] == pre["id"]]
check("the preauthorisation was consumed by the approved amount", consumed and d(consumed[0]["consumed_amount"]) == Decimal("810.00"), consumed)
check("the member limit caps the second claim", d(capped["approved_amount"]) == Decimal("190.00"), capped)

print("payment")
pay = admin.call("pay_insurance_claim", {"claim": claim["id"]})
again = admin.call("pay_insurance_claim", {"claim": claim["id"]})
check("paying twice is one payment", pay["id"] == again["id"] and pay["state"] == "confirmed")
refused("an unapproved claim cannot be paid", lambda: admin.call("pay_insurance_claim", {"claim": emp_b.call("submit_insurance_claim", {
    "enrolment": enr_b["id"], "provider": net["id"], "service_date": svc, "lines": [{"description": "Visit", "amount": "50.00"}]})["id"]}), ["approved"])

print("screens and reports")
pay2 = admin.call("pay_insurance_claim", {"id": second["id"]})
check("a list button can pay by id", pay2["state"] == "confirmed")
check("paid claims leave the to-pay list", all(c["id"] not in (claim["id"], second["id"]) for c in admin.call("insurance_claims_to_pay")))
flat = emp_a.call("submit_insurance_claim", {"enrolment": enr_a["id"], "provider": net["id"], "service_date": svc,
                                              "category": "pharmacy", "description": "Drugs", "amount": "40.00"})
check("a form sends one flat line", flat["state"] == "submitted" and d(flat["claimed_amount"]) == Decimal("40.00"), flat)
refused("employees cannot refuse", lambda: emp_a.call("reject_insurance_claim", {"id": flat["id"], "note": "no"}), ["insurance", "role", "staff"])
refused("a refusal needs a reason", lambda: rev.call("reject_insurance_claim", {"id": flat["id"]}), ["why"])
refused_claim = rev.call("reject_insurance_claim", {"id": flat["id"], "note": "Not covered"})
check("a refused claim keeps its reason", refused_claim["state"] == "rejected" and refused_claim["rejection_reason"] == "Not covered", refused_claim)
refused("a parent is not covered later either", lambda: admin.call("add_insurance_member", {"enrolment": enr_a["id"], "relationship": "parent", "name": "P"}), ["covers"])
refused("employees do not add people", lambda: emp_a.call("add_insurance_member", {"enrolment": enr_a["id"], "relationship": "spouse", "name": "S"}), ["insurance", "role", "staff"])
spouse = admin.call("add_insurance_member", {"enrolment": enr_a["id"], "relationship": "spouse", "name": f"Spouse {RUN}"})
check("a spouse is added to the enrolment", spouse["relationship"] == "spouse" and spouse["employee"] == me_a["id"], spouse)
refused("a second spouse is refused", lambda: admin.call("add_insurance_member", {"enrolment": enr_a["id"], "relationship": "spouse", "name": "Again"}), ["spouse", "covers", "one"])
check("the employee sees the people they cover", {m["name"] for m in emp_a.call("my_insurance_members")} >= {f"Spouse {RUN}"})
check("another employee does not see them", all(m["name"] != f"Spouse {RUN}" for m in emp_b.call("my_insurance_members")))
refused("employees do not see the dashboard", lambda: emp_a.call("insurance_dashboard"), ["insurance", "role", "staff"])
board = rev.call("insurance_dashboard")
check("the dashboard counts people and policies", board["active_policies"] >= 1 and board["covered_people"] >= 3, board)
check("the coverage report lists the spouse", any(r["member"] == f"Spouse {RUN}" for r in admin.call("insurance_dependants_coverage")))
util = [r for r in admin.call("insurance_premium_utilisation") if r["policy"] == f"POL-{RUN}"]
check("claims are set against premiums per policy", util and d(util[0]["claims_paid"]) == Decimal("1000.00"), util)

print("premiums")
made = admin.call("generate_insurance_premiums", {})
again = admin.call("generate_insurance_premiums", {})
check("premiums are created once", d(made["created"]) >= 2 and d(again["created"]) == 0, (made, again))
prem = [p for p in admin.call("list_insurance_premiums") if p["enrolment"] == enr_a["id"]]
check("employee pays 25% of 100 per month", prem and d(prem[0]["employee_share"]) == Decimal("25.00") and d(prem[0]["employer_share"]) == Decimal("75.00"), prem)

print("concurrency")
import threading
c1 = emp_b.call("submit_insurance_claim", {"enrolment": enr_b["id"], "provider": net["id"], "service_date": svc, "lines": [{"description": "A", "amount": "790.00"}]})
c2 = emp_b.call("submit_insurance_claim", {"enrolment": enr_b["id"], "provider": net["id"], "service_date": svc, "lines": [{"description": "B", "amount": "790.00"}]})
outcome = {}


def decide(who, claim_id):
    try:
        outcome[claim_id] = who.call("approve_insurance_claim", {"id": claim_id})
    except Failed as f:
        outcome[claim_id] = f


threads = [threading.Thread(target=decide, args=(rev, c1["id"])), threading.Thread(target=decide, args=(admin, c2["id"]))]
[t.start() for t in threads]
[t.join() for t in threads]
done = [v for v in outcome.values() if isinstance(v, dict)]
check("both concurrent decisions finish", len(done) == 2, outcome)
check("together they never exceed the member limit", sum(d(v["approved_amount"]) for v in done) <= Decimal("1000.00"), [v["approved_amount"] for v in done])
check("the deductible is used once", sum(d(v["deductible_applied"]) for v in done) == Decimal("100.00"), [v["deductible_applied"] for v in done])

print("migration from hr_benefits")
bplan = admin.call("create_benefit_plan", {"code": f"MG{RUN}", "name": f"Migrated {RUN}", "kind": "claim_against_ceiling", "currency": "USD",
                                           "yearly_ceiling": "5000.00", "max_dependents": 2, "allow_spouse": True, "allow_children": True}, plugin="hr_benefits")
benr = admin.call("enrol_in_benefit", {"plan": bplan["id"], "employee": me_b["id"], "start_date": f"{today.year}-01-01", "policy_number": f"MIG-{RUN}",
                                       "carrier": f"Acme Health {RUN}", "premium": "900.00",
                                       "dependents": [{"name": "Spouse", "relation": "spouse"}, {"name": "Kid", "relation": "child", "born": "2015-05-05"}]}, plugin="hr_benefits")
moved = admin.call("migrate_from_benefits")
check("the enrolment was moved", d(moved["moved"]) >= 1, moved)
again = admin.call("migrate_from_benefits")
check("moving again moves nothing", d(again["moved"]) == 0 and d(again["already_moved"]) >= 1, again)
mine = [e for e in admin.call("list_insurance_enrolments") if e["reference"] == f"BNF-{benr['id']}"]
check("it is an enrolment with its covered people", len(mine) == 1)
full = admin.call("get_insurance_enrolment", {"id": mine[0]["id"]}) if mine else {}
check("employee, spouse and child are members", sorted(m["relationship"] for m in full.get("members", [])) == ["child", "self", "spouse"], full.get("members"))

bad = [n for n, ok in results if not ok]
print(f"\n{len(results) - len(bad)}/{len(results)} checks passed")
sys.exit(1 if bad else 0)
