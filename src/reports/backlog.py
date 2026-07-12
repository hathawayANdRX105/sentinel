#!/usr/bin/env python3
"""Build a cross-story template backlog from draft audit results."""

from __future__ import annotations

import argparse
import collections
from pathlib import Path

from stats import draft as build_draft_stats
from reports import learning as build_review_learning_logs
from reports import scorecard as build_review_scorecards
from audit import draft as draft_audit
from lib.analysis import analyze_files, build_corpus_profile_for_files
from lib.io import write_json, write_text


def backlog_path_for(draft_path: Path) -> Path:
    return build_draft_stats.stats_path_for(draft_path).parent / "template-backlog" / "SUMMARY.md"


def candidates_path_for(draft_path: Path) -> Path:
    return build_draft_stats.stats_path_for(draft_path).parent / "template-backlog" / "CANDIDATES.json"

def infer_template_candidate(
    key: str,
    count: int,
    sample: str,
) -> dict[str, object]:
    bucket, name = key.split("::", 1)
    return {
        "bucket": bucket,
        "name": name,
        "count": count,
        "sample": sample,
        "suggested_target": "configs/rules/review.yaml#draft.template_rules",
        "reason": "跨章重复出现，优先作为模板库候选继续人工筛选。",
    }


def infer_term_candidate(
    key: str,
    count: int,
    sample: str,
) -> dict[str, object]:
    _bucket, name = key.split("::", 1)
    return {
        "bucket": "learned_filter",
        "name": name,
        "count": count,
        "sample": sample,
        "suggested_target": "configs/rules/review.yaml#draft.tracked_terms",
        "reason": "跨章重复出现，更像词项或短语，需要进词库观察。",
    }


def infer_keep_candidate(
    name: str,
    count: int,
    reason: str,
) -> dict[str, object]:
    return {
        "name": name,
        "count": count,
        "reason": reason,
        "suggested_target": "configs/rules/review.yaml#draft.template_rules",
        "action": "designed_keep_review",
    }


def build_story_backlog(
    story_dir: Path,
    analyses: list[tuple[Path, dict[str, object]]],
) -> tuple[str, dict[str, object]]:
    template_counter: collections.Counter[str] = collections.Counter()
    template_samples: dict[str, str] = {}
    bonus_counter: collections.Counter[str] = collections.Counter()
    bonus_reasons: dict[str, str] = {}
    bank_target_counter: collections.Counter[str] = collections.Counter()

    for _path, analysis in sorted(analyses, key=lambda item: build_review_scorecards.chapter_sort_key(item[0])):
        for item in build_review_learning_logs.collect_template_backlog(analysis):
            key = f"{item['bucket']}::{item['name']}"
            template_counter[key] += 1
            if key not in template_samples and item.get("sample"):
                template_samples[key] = str(item["sample"])
        for item in build_review_scorecards.build_bonus_candidates(analysis):
            bonus_counter[str(item["name"])] += 1
            bonus_reasons.setdefault(str(item["name"]), str(item["reason"]))
        for suggestion in build_review_learning_logs.collect_rule_suggestions(analysis):
            bank_target_counter[str(suggestion["target"])] += 1

    lines = ["# Template Backlog", ""]
    lines.append(f"- story: `{story_dir}`")
    lines.append(f"- chapters: `{len(analyses)}`")
    lines.append("")

    lines.append("## Repeat Candidates")
    if template_counter:
        for name, count in template_counter.most_common(16):
            lines.append(f"- `{name}` x{count}")
            sample = template_samples.get(name, "")
            if sample:
                lines.append(f"  样例：{sample}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Keep Candidates")
    if bonus_counter:
        for name, count in bonus_counter.most_common(12):
            lines.append(f"- `{name}` x{count}：{bonus_reasons.get(name, '')}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Deposition Targets")
    if bank_target_counter:
        for name, count in bank_target_counter.most_common():
            lines.append(f"- `{name}` x{count}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Next Actions")
    lines.append("1. 先看 `Repeat Candidates` 里跨章反复出现的家族，判断它该进模板库、词库，还是只算局部问题。")
    lines.append("2. 再看 `Keep Candidates`，避免把本来应保留的节奏、章末收束或动作后果误杀。")
    lines.append("3. 最后按 `Deposition Targets` 决定写回 `configs/rules/review.yaml`（`draft.template_rules` / `draft.tracked_terms`）、`skills/review-guide.md` 还是本书规则。")
    lines.append("")

    template_bank_candidates: list[dict[str, object]] = []
    term_bank_candidates: list[dict[str, object]] = []
    keep_candidates: list[dict[str, object]] = []

    for key, count in template_counter.most_common(16):
        sample = template_samples.get(key, "")
        if key.startswith(("learned_filter::", "tracked_term::")):
            term_bank_candidates.append(infer_term_candidate(key, count, sample))
        else:
            template_bank_candidates.append(infer_template_candidate(key, count, sample))

    for name, count in bonus_counter.most_common(12):
        keep_candidates.append(infer_keep_candidate(name, count, bonus_reasons.get(name, "")))

    payload = {
        "story": story_dir.as_posix(),
        "chapters": len(analyses),
        "template_bank_candidates": template_bank_candidates,
        "term_bank_candidates": term_bank_candidates,
        "keep_candidates": keep_candidates,
        "deposition_targets": [
            {"target": name, "count": count}
            for name, count in bank_target_counter.most_common()
        ],
    }

    return "\n".join(lines) + "\n", payload


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Build a cross-story template backlog.")
    parser.add_argument("paths", nargs="+", help="Draft chapter files or directories")
    parser.add_argument("--sample-limit", type=int, default=6, help="Sample limit for draft analysis")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    files = build_review_scorecards.collect_chapter_files(args.paths)
    if not files:
        raise SystemExit("No draft chapter files found.")

    corpus_profile = build_corpus_profile_for_files(files)
    analyses = analyze_files(
        files,
        sample_limit=args.sample_limit,
        corpus_profile=corpus_profile,
    )

    grouped: dict[Path, list[tuple[Path, dict[str, object]]]] = collections.defaultdict(list)
    for draft_path, analysis in analyses:
        grouped[draft_path.parent].append((draft_path, analysis))

    for story_dir, items in sorted(grouped.items()):
        markdown, payload = build_story_backlog(story_dir, items)
        out_path = backlog_path_for(items[0][0])
        json_path = candidates_path_for(items[0][0])
        write_text(out_path, markdown)
        write_json(json_path, payload)
        print(out_path)
        print(json_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
