#!/usr/bin/env python3
"""Apply monitoring/monitors.json to Cloudflare's alerting API (ADR-015).

    python3 monitoring/apply.py apply        # create, update and prune; idempotent
    python3 monitoring/apply.py plan         # print what apply would do
    python3 monitoring/apply.py export NAME  # print a dashboard-made policy as a monitors.json entry

Policies whose name starts with PREFIX belong to this file: apply creates the
missing ones, rewrites the existing ones and deletes those no longer listed.
Anything else on the account, such as the auto-created budget alert, is left
alone.

Env: CLOUDFLARE_API_TOKEN (Notifications Read/Write), CLOUDFLARE_ACCOUNT_ID,
ALERTS_SLACK_WEBHOOK_URL (every policy), ALERTS_EMAIL (comma-separated,
added to "page" policies; optional).
"""

import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path

PREFIX = "data-proxy: "
WEBHOOK_NAME = "data-proxy alerts (Slack)"
SEVERITIES = ("page", "notify")
# Fields a policy is defined by; everything else the API returns (id,
# mechanisms, timestamps) is derived or account state.
POLICY_FIELDS = ("name", "alert_type", "description", "enabled", "alert_interval", "filters")
DEFINITIONS = Path(__file__).with_name("monitors.json")


def load_definitions(path=DEFINITIONS):
    policies = json.loads(path.read_text())["policies"]
    names = [p.get("name") for p in policies]
    for p in policies:
        if not isinstance(p.get("name"), str) or not p["name"].startswith(PREFIX):
            raise SystemExit(f"{p.get('name')!r}: policy names must start with {PREFIX!r}")
        if p.get("severity") not in SEVERITIES:
            raise SystemExit(f"{p['name']!r}: severity must be one of {SEVERITIES}")
    if len(names) != len(set(names)):
        raise SystemExit("policy names must be unique")
    return policies


def plan(definitions, existing):
    """Actions that bring `existing` (name -> id, ours only) in line with `definitions`."""
    wanted = {d["name"] for d in definitions}
    actions = [("update", d, existing[d["name"]]) if d["name"] in existing else ("create", d, None) for d in definitions]
    actions += [("delete", {"name": name}, pid) for name, pid in sorted(existing.items()) if name not in wanted]
    return actions


def body_for(definition, webhook_id, emails):
    body = {k: definition[k] for k in POLICY_FIELDS if k in definition}
    body.setdefault("enabled", True)
    mechanisms = {"webhooks": [{"id": webhook_id}]}
    if definition["severity"] == "page" and emails:
        mechanisms["email"] = [{"id": e} for e in emails]
    body["mechanisms"] = mechanisms
    return body


class Cloudflare:
    def __init__(self, token, account_id):
        self.token = token
        self.base = f"https://api.cloudflare.com/client/v4/accounts/{account_id}/alerting/v3"

    def __call__(self, method, path, body=None):
        req = urllib.request.Request(
            self.base + path,
            method=method,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Authorization": f"Bearer {self.token}", "Content-Type": "application/json"},
        )
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                return json.load(resp).get("result")
        except urllib.error.HTTPError as e:
            raise SystemExit(f"{method} {path}: HTTP {e.code}\n{e.read().decode()}")

    def our_policies(self):
        ours = [p for p in self("GET", "/policies") or [] if p["name"].startswith(PREFIX)]
        names = [p["name"] for p in ours]
        dupes = sorted({n for n in names if names.count(n) > 1})
        if dupes:
            # A name -> id map would silently keep one and leave the rest
            # firing forever, untouched by apply.
            raise SystemExit(f"duplicate owned policies on the account, delete the extras: {dupes}")
        return {p["name"]: p["id"] for p in ours}

    def upsert_webhook(self, url):
        hooks = {h["name"]: h["id"] for h in self("GET", "/destinations/webhooks") or []}
        body = {"name": WEBHOOK_NAME, "url": url}
        if WEBHOOK_NAME in hooks:
            # The URL is write-only, so it can't be compared; rewriting it is idempotent.
            self("PUT", f"/destinations/webhooks/{hooks[WEBHOOK_NAME]}", body)
            return hooks[WEBHOOK_NAME]
        return self("POST", "/destinations/webhooks", body)["id"]


def main(argv):
    cmd = argv[1] if len(argv) > 1 else ""
    if cmd not in ("apply", "plan", "export"):
        raise SystemExit(__doc__)
    definitions = load_definitions()
    cf = Cloudflare(os.environ["CLOUDFLARE_API_TOKEN"], os.environ["CLOUDFLARE_ACCOUNT_ID"])

    if cmd == "export":
        name = " ".join(argv[2:])
        policy = next((p for p in cf("GET", "/policies") or [] if p["name"] == name), None)
        if policy is None:
            raise SystemExit(f"no policy named {name!r}")
        entry = {k: policy[k] for k in POLICY_FIELDS if k in policy}
        entry["name"] = name if name.startswith(PREFIX) else PREFIX + name
        entry["severity"] = "notify"
        print(json.dumps(entry, indent=2))
        return

    actions = plan(definitions, cf.our_policies())
    for action, definition, _ in actions:
        print(f"{action:7} {definition['name']}")
    if cmd == "plan":
        return

    webhook_id = cf.upsert_webhook(os.environ["ALERTS_SLACK_WEBHOOK_URL"])
    emails = [e.strip() for e in os.environ.get("ALERTS_EMAIL", "").split(",") if e.strip()]
    for action, definition, policy_id in actions:
        if action == "create":
            cf("POST", "/policies", body_for(definition, webhook_id, emails))
        elif action == "update":
            cf("PUT", f"/policies/{policy_id}", body_for(definition, webhook_id, emails))
        else:
            cf("DELETE", f"/policies/{policy_id}")


if __name__ == "__main__":
    main(sys.argv)
