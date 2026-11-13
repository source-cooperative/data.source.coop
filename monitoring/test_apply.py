"""Offline checks for apply.py: `python3 monitoring/test_apply.py`."""

import json
import tempfile
from pathlib import Path

from apply import PREFIX, body_for, load_definitions, plan


def test_plan_creates_updates_and_prunes_only_ours():
    defs = [{"name": PREFIX + "a"}, {"name": PREFIX + "b"}]
    existing = {PREFIX + "a": "id-a", PREFIX + "stale": "id-s"}
    assert plan(defs, existing) == [
        ("update", defs[0], "id-a"),
        ("create", defs[1], None),
        ("delete", {"name": PREFIX + "stale"}, "id-s"),
    ]
    assert plan(defs, {PREFIX + "a": "id-a", PREFIX + "b": "id-b"}) == [
        ("update", defs[0], "id-a"),
        ("update", defs[1], "id-b"),
    ]


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
    with tempfile.TemporaryDirectory() as d:
        bad = Path(d, "m.json")
        bad.write_text(json.dumps({"policies": [{"name": "no prefix", "severity": "page"}]}))
        try:
            load_definitions(bad)
        except SystemExit:
            return
    raise AssertionError("a name without the prefix must be rejected")


if __name__ == "__main__":
    for name, fn in list(globals().items()):
        if name.startswith("test_"):
            fn()
    print("ok")
