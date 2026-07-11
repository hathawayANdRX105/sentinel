#!/usr/bin/env python3
"""Preview and apply template candidate writeback actions.

This script supports both --dry-run (default) and --apply modes.
"""

from __future__ import annotations

import json
import collections
from pathlib import Path
from typing import Any

import yaml
from sentinel.lib import rules


YAML_TEMPLATE_TARGET = "scripts/rules.yaml#draft.template_rules"
YAML_TERM_TARGET = "scripts/rules.yaml#draft.tracked_terms"
YAML_INACTIVE_TARGET = "scripts/rules.yaml#draft.inactive_template_candidates"


def load_catalog(path: Path) -> dict[str, Any]:
    if not path.exists():
        raise SystemExit(f"Catalog not found: {path}")
    try:
        payload = yaml.safe_load(path.read_text(encoding="utf-8")) if path.suffix in {".yaml", ".yml"} else json.loads(path.read_text(encoding="utf-8"))
    except Exception as exc:
        raise SystemExit(f"Invalid catalog in {path}: {exc}") from exc
    if not isinstance(payload, dict) or "writeback_queue" not in payload or not isinstance(payload["writeback_queue"], list):
        raise SystemExit("Catalog must have a list field 'writeback_queue'.")
    return payload


def build_plan(catalog: dict[str, Any]) -> list[dict[str, Any]]:
    plan: list[dict[str, Any]] = []
    for item in catalog["writeback_queue"]:
        target = str(item.get("target", ""))
        state = str(item.get("state", ""))
        kind = str(item.get("kind", ""))

        if target.endswith("draft.template_rules") or target.endswith("draft_template_bank.json"):
            action = "template_bank"
        elif target.endswith("draft.tracked_terms") or target.endswith("draft_term_bank.json"):
            action = "term_bank"
        elif target.endswith(".md") or "/rules/" in target:
            action = "guide_or_rule"
        elif target.endswith(".py"):
            action = "script_recalibration"
        else:
            action = "manual_review"

        if state == "hardcoded":
            action = "script_recalibration"
        elif state == "bank" and action in {"template_bank", "term_bank"}:
            action = "bank_recalibration"
        elif kind.startswith("keep"):
            action = "designed_keep_review"

        plan.append({**item, "action": action})
    return plan


def render_dry_run(catalog_path: Path, catalog: dict[str, Any], plan: list[dict[str, Any]]) -> str:
    lines = ["# Template Candidate Writeback Dry Run", ""]
    lines.append(f"- catalog: `{catalog_path}`")
    lines.append(f"- novel: `{catalog.get('novel', 'unknown')}`")
    lines.append(f"- stories: `{catalog.get('stories', 0)}`")
    lines.append(f"- queue_items: `{len(plan)}`")
    lines.append("- mode: `dry-run` (no files modified)")
    lines.append("")

    by_action: dict[str, list] = collections.defaultdict(list)
    by_state: collections.Counter[str] = collections.Counter()
    for item in plan:
        by_action[item["action"]].append(item)
        by_state[item["state"]] += 1

    lines.append("## State Summary")
    for state, count in by_state.most_common():
        lines.append(f"- `{state}` x{count}")
    lines.append("")

    order = [
        "template_bank",
        "term_bank",
        "bank_recalibration",
        "designed_keep_review",
        "script_recalibration",
        "guide_or_rule",
        "manual_review",
    ]
    titles = {
        "template_bank": f"Would add to {YAML_INACTIVE_TARGET}",
        "term_bank": f"Would add to {YAML_TERM_TARGET}",
        "bank_recalibration": "Would recalibrate existing bank entries",
        "designed_keep_review": "Needs designed-keep review",
        "script_recalibration": "Needs script recalibration",
        "guide_or_rule": "Would update guide or rule docs",
        "manual_review": "Needs manual routing",
    }
    for action in order:
        items = by_action.get(action, [])
        if not items:
            continue
        lines.append(f"## {titles[action]}")
        for item in items:
            stories = item.get("stories", "")
            count = item.get("count", "")
            lines.append(
                f"- `{item['state']}` `{item['kind']}` `{item['name']}` -> `{item['target']}` stories=`{stories}` count=`{count}`"
            )
            lines.append(f"  reason: {item['reason']}")
        lines.append("")
    return "\n".join(lines).rstrip() + "\n"


def _load_rules_yaml(path: Path) -> dict[str, Any]:
    payload = yaml.safe_load(path.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise SystemExit(f"Invalid review rules YAML: {path}")
    return payload


def _write_rules_yaml(path: Path, payload: dict[str, Any]) -> None:
    path.write_text(
        yaml.safe_dump(payload, allow_unicode=True, sort_keys=False),
        encoding="utf-8",
    )
    rules._load_rules_cached.cache_clear()


def apply_to_template_bank(item: dict[str, Any]) -> None:
    """Add new template under inactive candidates when no concrete regex is available."""
    rules_path = rules.DEFAULT_RULES_PATH
    payload = _load_rules_yaml(rules_path)
    draft = payload.setdefault("draft", {})
    if not isinstance(draft, dict):
        raise SystemExit("Invalid draft section in review rules YAML")
    inactive = draft.setdefault("inactive_template_candidates", [])
    if not isinstance(inactive, list):
        raise SystemExit("Invalid inactive_template_candidates section")

    name = str(item.get("name", ""))
    existing = next((entry for entry in inactive if isinstance(entry, dict) and entry.get("name") == name), None)
    if existing is not None:
        if "note" in item or "reason" in item:
            existing["note"] = item.get("reason", item.get("note", existing.get("note", "")))
    else:
        inactive.append(
            {
                "name": name,
                "pattern": "",
                "note": item.get("reason", ""),
                "max_per_10k": float(item.get("max_per_10k", 5.0)),
                "category": str(item.get("category", "auto_generated")),
                "enabled": False,
            }
        )
    _write_rules_yaml(rules_path, payload)
    print(f"Added to {YAML_INACTIVE_TARGET}: {name}")


def apply_to_term_bank(item: dict[str, Any]) -> None:
    """Add or update tracked terms in rules.yaml."""
    rules_path = rules.DEFAULT_RULES_PATH
    payload = _load_rules_yaml(rules_path)
    draft = payload.setdefault("draft", {})
    if not isinstance(draft, dict):
        raise SystemExit("Invalid draft section in review rules YAML")
    terms = draft.setdefault("tracked_terms", [])
    if not isinstance(terms, list):
        raise SystemExit("Invalid tracked_terms section")

    name = str(item.get("name", ""))
    existing = next((entry for entry in terms if isinstance(entry, dict) and entry.get("term") == name), None)
    if existing is not None:
        if "note" in item or "reason" in item:
            existing["note"] = item.get("reason", item.get("note", existing.get("note", "")))
        if "max_per_10k" in item:
            existing["max_per_10k"] = float(item["max_per_10k"])
        if "category" in item:
            existing["category"] = str(item["category"])
    else:
        terms.append(
            {
                "term": name,
                "category": str(item.get("category", "auto_generated")),
                "max_per_10k": float(item.get("max_per_10k", 10.0)),
                "note": item.get("reason", item.get("note", "")),
            }
        )
    _write_rules_yaml(rules_path, payload)
    print(f"Added to {YAML_TERM_TARGET}: {name}")


def apply_writeback(plan: list[dict[str, Any]]) -> None:
    for item in plan:
        if item["action"] == "template_bank":
            apply_to_template_bank(item)
        elif item["action"] == "term_bank":
            apply_to_term_bank(item)
        else:
            print(f"Skipping action: {item['action']} for {item.get('name', '')} (not implemented)")


def main() -> int:
    parser = argparse.ArgumentParser(description="Preview and apply template candidate writeback actions.")
    parser.add_argument("catalog", help="Path to template-catalog/CATALOG.json")
    parser.add_argument("--dry-run", action="store_true", help="Preview actions without modifying files")
    parser.add_argument("--apply", action="store_true", help="Apply the writeback actions")
    args = parser.parse_args()

    if not (args.dry_run or args.apply):
        raise SystemExit("Either --dry-run or --apply must be specified.")

    catalog = load_catalog(Path(args.catalog))
    plan = build_plan(catalog)

    if args.apply:
        print("Applying writeback actions...")
        apply_writeback(plan)
        print("Applied successfully.")
    else:
        print(render_dry_run(Path(args.catalog), catalog, plan))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
