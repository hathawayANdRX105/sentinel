#!/usr/bin/env python3
"""Build a workspace-level template/term candidate catalog from story backlogs."""

from __future__ import annotations

import argparse
import collections
import json
from pathlib import Path

from reports import scorecard as build_review_scorecards
from reports import backlog as build_template_backlog
from audit import draft as draft_audit
from lib import rules
from lib.analysis import build_corpus_profile_for_files
from lib.io import write_json, write_text


YAML_TEMPLATE_TARGET = "configs/rules/review.yaml#draft.template_rules"
YAML_TERM_TARGET = "configs/rules/review.yaml#draft.tracked_terms"



def summary_path_for(novel_dir: Path) -> Path:
    return novel_dir / "draft-stats" / "template-catalog" / "SUMMARY.md"


def json_path_for(novel_dir: Path) -> Path:
    return novel_dir / "draft-stats" / "template-catalog" / "CATALOG.json"

def load_json(path: Path) -> object:
    return json.loads(path.read_text(encoding="utf-8"))


def script_dir() -> Path:
    return Path(__file__).resolve().parent


def novel_dir_for_story_dir(story_dir: Path) -> Path | None:
    for parent in story_dir.parents:
        if parent.name == "drafts":
            return parent.parent
    return None


def build_story_payloads(files: list[Path]) -> tuple[Path, list[dict[str, object]]]:
    grouped: dict[Path, list[tuple[Path, dict[str, object]]]] = collections.defaultdict(list)
    novel_dir: Path | None = None
    corpus_profile = build_corpus_profile_for_files(files)
    for draft_path in files:
        story_dir = draft_path.parent
        if novel_dir is None:
            novel_dir = novel_dir_for_story_dir(story_dir)
        analysis = draft_audit.analyze_path(
            draft_path,
            sample_limit=6,
            corpus_profile=corpus_profile,
        )
        grouped[story_dir].append((draft_path, analysis))

    if novel_dir is None:
        raise SystemExit("Failed to resolve novel directory from draft paths.")

    payloads: list[dict[str, object]] = []
    for story_dir, items in sorted(grouped.items()):
        markdown, payload = build_template_backlog.build_story_backlog(story_dir, items)
        summary_path = build_template_backlog.backlog_path_for(items[0][0])
        candidates_path = build_template_backlog.candidates_path_for(items[0][0])
        write_text(summary_path, markdown)
        write_json(candidates_path, payload)
        payloads.append(payload)
    return novel_dir, payloads


def load_story_payloads_from_stats(novel_dir: Path) -> list[dict[str, object]]:
    payloads: list[dict[str, object]] = []
    for path in sorted((novel_dir / "draft-stats").glob("arc*/story*/template-backlog/CANDIDATES.json")):
        payload = load_json(path)
        if isinstance(payload, dict):
            payloads.append(payload)
    return payloads


def load_bank_names(key: str) -> set[str]:
    payload = rules.load_rules()
    if key == "name":
        section = rules.list_at(payload, "draft", "template_rules")
    else:
        section = rules.list_at(payload, "draft", "tracked_terms")
    names: set[str] = set()
    for item in section:
        if isinstance(item, dict) and item.get(key):
            names.add(str(item[key]))
    return names



def load_builtin_rule_template_names() -> set[str]:
    """Names already defined on draft audit rule tables (not YAML bank entries)."""
    names: set[str] = set()
    for rule_table in (
        draft_audit.PATTERN_RULES,
        draft_audit.PHRASE_RULES,
        draft_audit.TOKEN_RULES,
        draft_audit.PUNCTUATION_RULES,
        draft_audit.COMBO_RULES,
        draft_audit.MODIFIER_RULES,
    ):
        for item in rule_table:
            if item.get("label"):
                names.add(str(item["label"]))
            if item.get("name"):
                names.add(str(item["name"]))
    return names


def load_builtin_rule_term_names() -> set[str]:
    """Token/phrase/modifier names already defined on draft audit rule tables."""
    names: set[str] = set()
    for rule_table in (
        draft_audit.TOKEN_RULES,
        draft_audit.PHRASE_RULES,
        draft_audit.MODIFIER_RULES,
    ):
        for item in rule_table:
            if item.get("name"):
                names.add(str(item["name"]))
    return names


def bucket_family(bucket: str) -> str:
    if bucket in {"custom_template", "patterns", "phrases", "tokens"}:
        return "template_family"
    if bucket in {"punctuation", "sentence_length", "fatigue_window", "ba_operation_context"}:
        return "narrative_signal"
    if bucket in {"learned_filter", "tracked_term"}:
        return "term_family"
    return bucket


def aggregate_candidates(
    payloads: list[dict[str, object]],
    field: str,
    count_key: str = "count",
) -> list[dict[str, object]]:
    rows: dict[tuple[str, str], dict[str, object]] = {}
    for payload in payloads:
        story = str(payload.get("story", ""))
        chapters = int(payload.get("chapters", 0))
        for item in payload.get(field, []):
            name = str(item.get("name", ""))
            bucket = str(item.get("bucket", field))
            key = (bucket, name)
            row = rows.setdefault(
                key,
                {
                    "bucket": bucket,
                    "name": name,
                    "count": 0,
                    "story_count": 0,
                    "stories": [],
                    "chapter_total": 0,
                    "sample": str(item.get("sample", "")),
                    "suggested_target": str(item.get("suggested_target", "")),
                    "reasons": [],
                    "_story_set": set(),
                },
            )
            row["count"] = int(row["count"]) + int(item.get(count_key, 0))
            story_set = row["_story_set"]
            assert isinstance(story_set, set)
            if story not in story_set:
                story_set.add(story)
                row["chapter_total"] = int(row["chapter_total"]) + chapters
            reasons = row["reasons"]
            assert isinstance(reasons, list)
            reason = str(item.get("reason", "")).strip()
            if reason and reason not in reasons:
                reasons.append(reason)
            if not row["sample"] and item.get("sample"):
                row["sample"] = str(item["sample"])
    ordered = sorted(
        rows.values(),
        key=lambda item: (-int(item["story_count"]), -int(item["count"]), str(item["name"])),
    )
    for item in ordered:
        story_set = item.pop("_story_set")
        assert isinstance(story_set, set)
        item["stories"] = sorted(story_set)
        item["story_count"] = len(story_set)
    ordered.sort(key=lambda item: (-int(item["story_count"]), -int(item["count"]), str(item["name"])))
    return ordered


def aggregate_candidate_families(candidates: list[dict[str, object]]) -> list[dict[str, object]]:
    rows: dict[str, dict[str, object]] = {}
    for item in candidates:
        name = str(item["name"])
        row = rows.setdefault(
            name,
            {
                "name": name,
                "count": 0,
                "story_count": 0,
                "stories": set(),
                "chapter_total": 0,
                "sample": str(item.get("sample", "")),
                "suggested_target": str(item.get("suggested_target", "")),
                "buckets": set(),
                "bucket_families": set(),
                "reasons": [],
            },
        )
        row["count"] = int(row["count"]) + int(item["count"])
        stories = row["stories"]
        assert isinstance(stories, set)
        stories.update(str(story) for story in item.get("stories", []))
        row["chapter_total"] = max(int(row["chapter_total"]), int(item.get("chapter_total", 0)))
        buckets = row["buckets"]
        assert isinstance(buckets, set)
        buckets.add(str(item["bucket"]))
        bucket_families = row["bucket_families"]
        assert isinstance(bucket_families, set)
        bucket_families.add(bucket_family(str(item["bucket"])))
        reasons = row["reasons"]
        assert isinstance(reasons, list)
        for reason in item.get("reasons", []):
            reason_text = str(reason).strip()
            if reason_text and reason_text not in reasons:
                reasons.append(reason_text)
        if not row["sample"] and item.get("sample"):
            row["sample"] = str(item["sample"])
    merged: list[dict[str, object]] = []
    for row in rows.values():
        stories = row.pop("stories")
        buckets = row.pop("buckets")
        bucket_families = row.pop("bucket_families")
        assert isinstance(stories, set)
        assert isinstance(buckets, set)
        assert isinstance(bucket_families, set)
        row["stories"] = sorted(stories)
        row["story_count"] = len(stories)
        row["buckets"] = sorted(buckets)
        row["bucket_families"] = sorted(bucket_families)
        merged.append(row)
    return sorted(merged, key=lambda item: (-int(item["story_count"]), -int(item["count"]), str(item["name"])))


def aggregate_deposition_targets(payloads: list[dict[str, object]]) -> list[dict[str, object]]:
    counter: collections.Counter[str] = collections.Counter()
    for payload in payloads:
        for item in payload.get("deposition_targets", []):
            counter[str(item.get("target", ""))] += int(item.get("count", 0))
    return [{"target": target, "count": count} for target, count in counter.most_common()]


def build_learning_anchors(
    template_families: list[dict[str, object]],
    term_candidates: list[dict[str, object]],
    keep_candidates: list[dict[str, object]],
) -> list[dict[str, object]]:
    anchors: list[dict[str, object]] = []
    for item in template_families:
        if int(item["story_count"]) >= 2 and int(item["count"]) >= 10:
            anchors.append(
                {
                    "kind": "template",
                    "name": str(item["name"]),
                    "bucket": "/".join(str(bucket) for bucket in item.get("buckets", [])),
                    "count": int(item["count"]),
                    "story_count": int(item["story_count"]),
                    "suggested_target": str(item["suggested_target"]),
                    "reason": "跨 Story 反复出现，值得判断它是硬模板、结构模式，还是应改抽取定义。",
                }
            )
    for item in term_candidates:
        if int(item["story_count"]) >= 2 and int(item["count"]) >= 8:
            anchors.append(
                {
                    "kind": "term",
                    "name": str(item["name"]),
                    "bucket": str(item["bucket"]),
                    "count": int(item["count"]),
                    "story_count": int(item["story_count"]),
                    "suggested_target": str(item["suggested_target"]),
                    "reason": "跨 Story 稳定偏高，适合正式进词库或改成更细的 phrase 规则。",
                }
            )
    for item in keep_candidates:
        if int(item["story_count"]) >= 2:
            anchors.append(
                {
                    "kind": "keep",
                    "name": str(item["name"]),
                    "bucket": str(item["bucket"]),
                    "count": int(item["count"]),
                    "story_count": int(item["story_count"]),
                    "suggested_target": str(item["suggested_target"]),
                    "reason": "多条 Story 都在把它当加分或保留候选，说明审查不该一刀切地误杀这类文笔设计。",
                }
            )
    return sorted(anchors, key=lambda item: (-int(item["story_count"]), -int(item["count"]), str(item["name"])))[:16]


def build_writeback_queue(
    template_families: list[dict[str, object]],
    term_candidates: list[dict[str, object]],
    keep_candidates: list[dict[str, object]],
    template_bank_names: set[str],
    term_bank_names: set[str],
    builtin_template_names: set[str],
    builtin_term_names: set[str],
) -> list[dict[str, object]]:
    queue: list[dict[str, object]] = []
    for item in template_families:
        name = str(item["name"])
        in_bank = name in template_bank_names
        in_builtin = name in builtin_template_names
        story_count = int(item["story_count"])
        count = int(item["count"])
        if story_count < 2:
            continue
        if in_bank and count >= 12:
            queue.append(
                {
                    "kind": "template_recalibration",
                    "name": name,
                    "target": YAML_TEMPLATE_TARGET,
                    "stories": story_count,
                    "count": count,
                    "state": "bank",
                    "reason": "模板已在库中，但跨 Story 仍高频命中，应回看 pattern、阈值或说明是否过宽。",
                }
            )
        elif in_builtin and count >= 12:
            queue.append(
                {
                    "kind": "rule_recalibration",
                    "name": name,
                    "target": "audit.draft",
                    "stories": story_count,
                    "count": count,
                    "state": "builtin",
                    "reason": "这条规则已经写在审查脚本里，高频命中更像阈值、分类或说明需要回调，而不是简单再加一条 bank。",
                }
            )
        elif not in_bank and count >= 12:
            queue.append(
                {
                    "kind": "template_add",
                    "name": name,
                    "target": YAML_TEMPLATE_TARGET,
                    "stories": story_count,
                    "count": count,
                    "state": "new",
                    "reason": "同名家族跨 Story 稳定出现，适合进入模板库或至少进入候选审阅清单。",
                }
            )
    for item in term_candidates:
        name = str(item["name"])
        in_bank = name in term_bank_names
        in_builtin = name in builtin_term_names
        story_count = int(item["story_count"])
        count = int(item["count"])
        if story_count < 2:
            continue
        if in_bank and count >= 12:
            queue.append(
                {
                    "kind": "term_recalibration",
                    "name": name,
                    "target": YAML_TERM_TARGET,
                    "stories": story_count,
                    "count": count,
                    "state": "bank",
                    "reason": "词项已在库中却仍跨 Story 偏高，应调阈值、说明，或拆成更细 phrase 规则。",
                }
            )
        elif in_builtin and count >= 8:
            queue.append(
                {
                    "kind": "rule_recalibration",
                    "name": name,
                    "target": "audit.draft",
                    "stories": story_count,
                    "count": count,
                    "state": "builtin",
                    "reason": "这条词项已经在审查脚本基础规则里，高频命中说明更适合调阈值或拆分类，而不是重复入库。",
                }
            )
        elif not in_bank and count >= 8:
            queue.append(
                {
                    "kind": "term_add",
                    "name": name,
                    "target": YAML_TERM_TARGET,
                    "stories": story_count,
                    "count": count,
                    "state": "new",
                    "reason": "词项跨 Story 稳定偏高，适合做第一轮真实学习回写。",
                }
            )
    for item in keep_candidates:
        if int(item["story_count"]) >= 3:
            queue.append(
                {
                    "kind": "keep_rule",
                    "name": str(item["name"]),
                    "target": "skills/review-guide.md",
                    "stories": int(item["story_count"]),
                    "count": int(item["count"]),
                    "state": "new",
                    "reason": "这类候选在多条 Story 都被视作可保留，应补“设计性重复保留”口径。",
                }
            )
    return sorted(queue, key=lambda item: (-int(item["stories"]), -int(item["count"]), str(item["name"])))[:16]


def build_catalog_markdown(
    novel_dir: Path,
    payloads: list[dict[str, object]],
    template_candidates: list[dict[str, object]],
    template_families: list[dict[str, object]],
    term_candidates: list[dict[str, object]],
    keep_candidates: list[dict[str, object]],
    deposition_targets: list[dict[str, object]],
    learning_anchors: list[dict[str, object]],
    writeback_queue: list[dict[str, object]],
) -> str:
    lines = ["# Template Candidate Catalog", ""]
    lines.append(f"- novel: `{novel_dir.name}`")
    lines.append(f"- stories: `{len(payloads)}`")
    lines.append(f"- source: `{novel_dir / 'draft-stats'}`")
    lines.append("")

    lines.append("## Learning Anchors")
    if learning_anchors:
        for item in learning_anchors:
            lines.append(
                f"- `{item['kind']}` `{item['bucket']}::{item['name']}` stories=`{item['story_count']}` total=`{item['count']}` -> `{item['suggested_target']}`"
            )
            lines.append(f"  说明：{item['reason']}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Merged Families")
    if template_families:
        for item in template_families[:16]:
            lines.append(
                f"- `{item['name']}` stories=`{item['story_count']}` total=`{item['count']}` buckets=`{' / '.join(item['buckets'])}`"
            )
            if item["sample"]:
                lines.append(f"  样例：{item['sample']}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Template Candidates")
    if template_candidates:
        for item in template_candidates[:20]:
            lines.append(
                f"- `{item['bucket']}::{item['name']}` stories=`{item['story_count']}` total=`{item['count']}` target=`{item['suggested_target']}`"
            )
            if item["sample"]:
                lines.append(f"  样例：{item['sample']}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Term Candidates")
    if term_candidates:
        for item in term_candidates[:20]:
            lines.append(
                f"- `{item['bucket']}::{item['name']}` stories=`{item['story_count']}` total=`{item['count']}` target=`{item['suggested_target']}`"
            )
            if item["sample"]:
                lines.append(f"  样例：{item['sample']}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Keep Candidates")
    if keep_candidates:
        for item in keep_candidates[:12]:
            lines.append(
                f"- `{item['bucket']}::{item['name']}` stories=`{item['story_count']}` total=`{item['count']}` target=`{item['suggested_target']}`"
            )
            reasons = item["reasons"]
            if reasons:
                lines.append(f"  说明：{' / '.join(reasons[:2])}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Deposition Targets")
    if deposition_targets:
        for item in deposition_targets:
            lines.append(f"- `{item['target']}` x{item['count']}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Writeback Queue")
    if writeback_queue:
        for item in writeback_queue:
            lines.append(
                f"- `{item['kind']}` `{item['name']}` stories=`{item['stories']}` total=`{item['count']}` state=`{item['state']}` -> `{item['target']}`"
            )
            lines.append(f"  说明：{item['reason']}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Next Actions")
    lines.append("1. 先看 `Merged Families` 和 `Writeback Queue`，避免同名异桶重复判断。")
    lines.append("2. 再核 `Keep Candidates`，把设计性重复和局部节奏从纯负向规则里拆出来。")
    lines.append("3. 最后按 `Deposition Targets` 分流到模板库、词库、评审指南或本书规则。")
    lines.append("")
    return "\n".join(lines) + "\n"


def build_catalog_payload(novel_dir: Path, payloads: list[dict[str, object]]) -> dict[str, object]:
    template_bank_names = load_bank_names("name")
    term_bank_names = load_bank_names("term")
    builtin_template_names = load_builtin_rule_template_names()
    builtin_term_names = load_builtin_rule_term_names()
    template_candidates = aggregate_candidates(payloads, "template_bank_candidates")
    template_families = aggregate_candidate_families(template_candidates)
    term_candidates = aggregate_candidates(payloads, "term_bank_candidates")
    keep_candidates = aggregate_candidates(payloads, "keep_candidates")
    deposition_targets = aggregate_deposition_targets(payloads)
    learning_anchors = build_learning_anchors(template_families, term_candidates, keep_candidates)
    writeback_queue = build_writeback_queue(
        template_families,
        term_candidates,
        keep_candidates,
        template_bank_names,
        term_bank_names,
        builtin_template_names,
        builtin_term_names,
    )
    return {
        "novel": novel_dir.name,
        "stories": len(payloads),
        "story_paths": [str(payload.get("story", "")) for payload in payloads],
        "template_candidates": template_candidates,
        "template_families": template_families,
        "term_candidates": term_candidates,
        "keep_candidates": keep_candidates,
        "deposition_targets": deposition_targets,
        "learning_anchors": learning_anchors,
        "writeback_queue": writeback_queue,
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Build a workspace-level template candidate catalog.")
    parser.add_argument("paths", nargs="+", help="Novel dir, draft directories, or draft chapter files")
    return parser.parse_args()


def resolve_payloads(paths: list[str]) -> tuple[Path, list[dict[str, object]]]:
    path_objects = [Path(path) for path in paths]
    if len(path_objects) == 1 and path_objects[0].is_dir() and (path_objects[0] / "draft-stats").exists():
        novel_dir = path_objects[0]
        payloads = load_story_payloads_from_stats(novel_dir)
        if payloads:
            return novel_dir, payloads

    files = build_review_scorecards.collect_chapter_files(paths)
    if not files:
        raise SystemExit("No draft chapter files found.")
    return build_story_payloads(files)


def main() -> int:
    args = parse_args()
    novel_dir, payloads = resolve_payloads(args.paths)
    if not payloads:
        raise SystemExit("No story candidate payloads found.")

    catalog = build_catalog_payload(novel_dir, payloads)
    markdown = build_catalog_markdown(
        novel_dir,
        payloads,
        catalog["template_candidates"],
        catalog["template_families"],
        catalog["term_candidates"],
        catalog["keep_candidates"],
        catalog["deposition_targets"],
        catalog["learning_anchors"],
        catalog["writeback_queue"],
    )
    summary_path = summary_path_for(novel_dir)
    catalog_path = json_path_for(novel_dir)
    write_text(summary_path, markdown)
    write_json(catalog_path, catalog)
    print(summary_path)
    print(catalog_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
