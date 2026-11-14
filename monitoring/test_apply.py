"""Offline checks for apply.py: `python3 monitoring/test_apply.py`."""

import json
import tempfile
from pathlib import Path

from apply import MAX_SQL_MONITORS, PREFIX,body_for, load_definitions, plan


A, B, STALE = PREFIX + "a", PREFIX + "b", PREFIX + "stale"
BODY = {
    "name": A,
    "alert_type": "incident_alert",
    "description": "d",
    "enabled": True,
    "filters": {"incident_impact": ["MAJOR", "CRITICAL"]},
    "mechanisms": {"webhooks": [{"id": "w"}], "email": [{"id": "ops@x"}]},
}
# How the API returns the same policy: extra keys, reordered values, empty mechanisms.
API_COPY = dict(
    BODY,
    id="id-a",
    created="2026-11-13T00:00:00Z",
    alert_interval="1h",
    filters={"incident_impact": ["CRITICAL", "MAJOR"], "zones": []},
    mechanisms={"email": [{"id": "ops@x"}], "webhooks": [{"id": "w", "name": "data-proxy alerts (Slack)"}], "pagerduty": []},
)


def test_plan_creates_updates_and_prunes_only_ours():
    stale = {"id": "id-s", "name": STALE}
    assert plan({A: BODY, B: dict(BODY, name=B)}, {A: API_COPY, STALE: stale}) == [
        ("unchanged", A, "id-a"),
        ("create", B, None),
        ("delete", STALE, "id-s"),
    ]


def test_plan_ignores_api_noise_but_not_real_changes():
    assert plan({A: BODY}, {A: API_COPY}) == [("unchanged", A, "id-a")]
    for changed in (
        {"description": "other"},
        {"enabled": False},
        {"filters": {"incident_impact": ["MAJOR"]}},
        {"mechanisms": {"webhooks": [{"id": "new-webhook"}]}},
        {"alert_interval": "24h"},
    ):
        assert plan({A: dict(BODY, **changed)}, {A: API_COPY}) == [("update", A, "id-a")], changed


def test_email_only_on_page_and_severity_never_sent():
    page = {"name": PREFIX + "p", "severity": "page", "alert_type": "t", "filters": {}}
    notify = dict(page, severity="notify")
    assert body_for(page, "w", ["ops@x"])["mechanisms"] == {"webhooks": [{"id": "w"}], "email": [{"id": "ops@x"}]}
    assert body_for(notify, "w", ["ops@x"])["mechanisms"] == {"webhooks": [{"id": "w"}]}
    assert "severity" not in body_for(page, "w", [])
    assert body_for(page, "w", [])["enabled"] is True


def test_repo_definitions_load():
    assert load_definitions()


def test_definitions_are_validated():
    ok = {"name": PREFIX + "a", "severity": "page"}
    cases = {
        "a name without the prefix": [dict(ok, name="no prefix")],
        "a missing name": [{"severity": "page"}],
        "an unknown severity": [dict(ok, severity="urgent")],
        "a duplicate name": [ok, dict(ok)],
        "more SQL Monitors than Cloudflare allows": [
            dict(ok, name=f"{PREFIX}{i}", alert_type="custom_analytics") for i in range(MAX_SQL_MONITORS + 1)
        ],
    }
    with tempfile.TemporaryDirectory() as d:
        bad = Path(d, "m.json")
        for case, policies in cases.items():
            bad.write_text(json.dumps({"policies": policies}))
            try:
                load_definitions(bad)
            except SystemExit:
                continue
            raise AssertionError(f"{case} must be rejected")


if __name__ == "__main__":
    for name, fn in list(globals().items()):
        if name.startswith("test_"):
            fn()
    print("ok")
