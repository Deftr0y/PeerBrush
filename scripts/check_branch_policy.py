"""Validate PeerBrush branch names and pull-request promotion destinations."""

import argparse
import json
import re
from pathlib import Path

VERSION = r"[0-9]+\.[0-9]+(?:\.[0-9]+)?"
TOPIC = r"[a-z0-9]+(?:[.-][a-z0-9]+)*(?:/[a-z0-9]+(?:[.-][a-z0-9]+)*)*"


def validate_name(name):
    if name.lower().split("/", 1)[0] == "codex":
        raise ValueError("Branches under codex/ are prohibited; use feature/<version>/<topic> or fix/<version>/<topic>.")
    if name in {"main", "dev", "site-content"}:
        return
    if re.fullmatch(rf"version/{VERSION}", name):
        return
    if re.fullmatch(rf"(?:feature|fix)/{VERSION}/{TOPIC}", name):
        return
    if re.fullmatch(rf"maintenance/{TOPIC}", name):
        return
    raise ValueError(f"Invalid branch name: {name}")


def validate_pull_request(head, base):
    validate_name(head)
    validate_name(base)
    if base == "main" and head == "dev":
        return
    if base == "dev" and re.fullmatch(rf"version/{VERSION}", head):
        return
    if base.startswith("version/"):
        version = base.removeprefix("version/")
        if head.startswith((f"feature/{version}/", f"fix/{version}/")):
            return
    if base == "site-content" and head.startswith("maintenance/site-"):
        return
    raise ValueError("Incorrect PR destination; promote feature/fix → matching version → dev → main, or maintenance/site-* → site-content.")


def validate_event(event, ref):
    if "pull_request" in event:
        pr = event["pull_request"]
        validate_pull_request(pr["head"]["ref"], pr["base"]["ref"])
    elif ref.startswith("refs/heads/"):
        validate_name(ref.removeprefix("refs/heads/"))
    elif ref.startswith("refs/tags/"):
        # Release tagging has separate exact-source and publication gates.
        return
    else:
        raise ValueError("Expected a branch push, tag push or pull request event.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--event", required=True, type=Path)
    parser.add_argument("--ref", required=True)
    args = parser.parse_args()
    try:
        validate_event(json.loads(args.event.read_text(encoding="utf-8")), args.ref)
    except (ValueError, KeyError) as error:
        parser.exit(1, f"Branch policy failed: {error}\n")
    print("Branch naming and promotion policy passed.")


if __name__ == "__main__":
    main()
