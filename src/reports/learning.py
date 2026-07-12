#!/usr/bin/env python3
"""Build review learning logs from draft analysis results."""

from __future__ import annotations

import argparse
import collections
from pathlib import Path

from stats import draft as build_draft_stats
import consistency as consistency_index
from audit import draft as draft_audit
from lib import alignment as plan_draft_alignment
from lib.analysis import analyze_files, build_corpus_profile_for_files
from lib.io import write_text
from lib.paths import chapter_sort_key, collect_chapter_files, novel_dir_for_draft


def learning_log_path_for(draft_path: Path) -> Path:
    return build_draft_stats.stats_path_for(draft_path).parent / "learning" / f"{draft_path.stem}.md"


def collect_template_backlog(analysis: dict[str, object]) -> list[dict[str, str]]:
    grouped: dict[str, list[dict[str, str]]] = collections.defaultdict(list)
    bucket_priority = {
        "patterns": 0,
        "custom_template": 1,
        "phrases": 2,
        "tokens": 3,
        "punctuation": 4,
        "sentence_length": 5,
        "fatigue_window": 6,
        "ba_operation_context": 7,
        "tracked_term": 8,
        "learned_filter": 9,
    }
    backlog: list[dict[str, str]] = []
    for item in analysis["template_candidates"][:20]:
        candidate = {
            "bucket": str(item["type"]),
            "name": str(item["name"]),
            "reason": str(item["note"]),
            "sample": str(item.get("sample", "")).strip(),
        }
        grouped[candidate["name"]].append(candidate)
    for name in sorted(
        grouped,
        key=lambda value: min(bucket_priority.get(item["bucket"], 99) for item in grouped[value]),
    ):
        items = sorted(grouped[name], key=lambda item: (bucket_priority.get(item["bucket"], 99), item["bucket"]))
        backlog.append(items[0])
    return backlog[:12]


def collect_bonus_backlog(analysis: dict[str, object]) -> list[dict[str, str]]:
    backlog: list[dict[str, str]] = []
    if analysis["ending"]["warn"] is False and analysis["ending"]["tail_excerpt"]:
        backlog.append(
            {
                "name": "章末收束",
                "reason": "本章章末没有命中模板警告，可人工确认它是不是值得保留的收束方式。",
                "sample": analysis["ending"]["tail_excerpt"][:80],
            }
        )
    if analysis["aa_bb_patterns"] and not any(item["warn"] for item in analysis["aa_bb_patterns"]):
        first = analysis["aa_bb_patterns"][0]
        backlog.append(
            {
                "name": "轻量排比或重叠词",
                "reason": "检测到少量节奏性排比，但没有达到疲劳阈值，可人工判断是不是文气亮点。",
                "sample": str(first["samples"][0]) if first.get("samples") else "",
            }
        )
    if not analysis["dialogue"]["dialogue_axis_gaps"] and 0.12 <= analysis["summary"]["quote_ratio"] <= 0.45:
        backlog.append(
            {
                "name": "对白调度",
                "reason": "对白比例和转轴暂时正常，可人工确认角色声音是否成立。",
                "sample": "",
            }
        )
    if (
        analysis["dialogue_emotions"]["dialogue_sentences"] >= 4
        and not analysis["dialogue_emotions"]["flatness_warn"]
        and not analysis["dialogue_emotions"]["volatility_warn"]
        and analysis["dialogue_emotions"]["shift_count"] >= 1
    ):
        backlog.append(
            {
                "name": "对白情绪曲线",
                "reason": "对白情绪出现了可解释起伏，可人工确认它是不是人物关系推进而不是工具误判。",
                "sample": "",
            }
        )
    if (
        int(analysis["character_voice"]["speaker_count"]) >= 2
        and not analysis["character_voice"]["warn"]
        and analysis["character_voice"]["coverage_ratio"] >= 0.35
    ):
        backlog.append(
            {
                "name": "角色声音分化",
                "reason": "本章已有可识别说话人分布且暂未出现明显同腔，可人工确认人物声音是否真的拉开了差异。",
                "sample": "",
            }
        )
    if analysis["battle_profile"]["sequence_count"] >= 1 and analysis["battle_profile"]["result_ratio"] >= 0.35:
        backlog.append(
            {
                "name": "动作结果链",
                "reason": "冲突段不仅有动作，还有结果或伤害反馈，可人工确认是否值得沉淀成战斗模板样本。",
                "sample": "",
            }
        )
    return backlog[:6]


def collect_rule_suggestions(analysis: dict[str, object]) -> list[dict[str, str]]:
    suggestions: list[dict[str, str]] = []
    if len(analysis["sentence_patterns"]) >= 4:
        suggestions.append(
            {
                "target": "configs/rules/review.yaml#draft.template_rules",
                "reason": "句首骨架重复已经形成明确家族，可考虑把高频骨架固化成模板库条目。",
            }
        )
    if analysis["tracked_term_window_count"] >= 3:
        suggestions.append(
            {
                "target": "configs/rules/review.yaml#draft.tracked_terms",
                "reason": "局部点名密度偏高，说明某些实体或动作词值得进入跟踪词库。",
            }
        )
    if analysis["viewpoint_profile"]["warn"]:
        suggestions.append(
            {
                "target": "skills/review-guide.md",
                "reason": "视角锚点漂移已经能被脚本稳定抓到，评审指南应补“近距离视角切锚”的具体复核动作。",
            }
        )
    if analysis["battle_profile"]["warn"] or analysis["scene_map"]["warn"]:
        suggestions.append(
            {
                "target": "configs/rules/review.yaml#draft.template_rules",
                "reason": "场面功能失衡或动作链缺结果已开始出现，可继续补充对应模板与反模板样本。",
            }
        )
    if any(item["priority"] == "P1" and item["category"] == "dialogue" for item in analysis["review_reminders"]):
        suggestions.append(
            {
                "target": "skills/review-guide.md",
                "reason": "对白问题已经稳定到 P1，说明评审指南里应进一步补具体审查动作。",
            }
        )
    if analysis["character_voice"]["warn"]:
        suggestions.append(
            {
                "target": "skills/review-guide.md",
                "reason": "角色对白同质化已经能被脚本稳定提示，评审指南应补“角色声音拉差”的复核动作。",
            }
        )
    if analysis["summary"]["warn_sections"] >= 14:
        suggestions.append(
            {
                "target": "novel1/rules/draft.md",
                "reason": "当前章的问题组合已经足够重，若在同一 Story 反复出现，应沉淀为本书规则。",
            }
        )
    return suggestions[:6]


def build_consistency_suggestions(snapshot: dict[str, object]) -> list[dict[str, str]]:
    if not snapshot.get("available"):
        return []
    decision_counter = snapshot["decision_counter"]
    category_counter = snapshot["category_counter"]
    pending_rows = snapshot["pending_rows"]
    suggestions: list[dict[str, str]] = []

    if decision_counter.get("false_positive", 0) >= 2:
        suggestions.append(
            {
                "target": "consistency",
                "reason": "同一 story 已累计多条一致性误报，说明抽取逻辑该继续压噪，而不是把人工复核当常态。",
            }
        )
    if decision_counter.get("confirmed", 0) >= 2:
        suggestions.append(
            {
                "target": "novel1/rules/draft.md",
                "reason": "同一 story 已确认多条一致性问题，说明这类漂移不是偶发手误，值得沉淀为本书返工规则。",
            }
        )
    if any(name.endswith("::designed_keep") for name in category_counter):
        suggestions.append(
            {
                "target": "configs/rules/review.yaml#draft.template_rules",
                "reason": "已有一致性候选被人工判为设计性保留，说明某些重复或称谓变化应进入可保留模式样本，而不是继续当纯风险。",
            }
        )
    if pending_rows:
        suggestions.append(
            {
                "target": "novel1/research/consistency/review-feedback.jsonl",
                "reason": f"当前 story 还有 {len(pending_rows)} 条中高置信度一致性候选未复核，先补反馈再决定是否继续扩规则。",
            }
        )
    suggestions.extend(snapshot.get("global_feedback_backlog", [])[:2])
    return suggestions[:6]


def build_learning_log(draft_path: Path, analysis: dict[str, object], consistency_snapshot: dict[str, object]) -> str:
    novel_dir = novel_dir_for_draft(draft_path)
    alignment = (
        plan_draft_alignment.build_plan_draft_alignment(draft_path, novel_dir, analysis)
        if novel_dir is not None
        else {"available": False, "reason": "novel_dir_not_resolved"}
    )
    template_backlog = collect_template_backlog(analysis)
    bonus_backlog = collect_bonus_backlog(analysis)
    rule_suggestions = collect_rule_suggestions(analysis) + build_consistency_suggestions(consistency_snapshot)
    if alignment.get("available") and alignment["mismatch_count"] >= 1:
        rule_suggestions = rule_suggestions + [
            {
                "target": str(alignment["chapter_plan_path"]),
                "reason": "正文功能与施工图存在漂移，这一轮复核不要只改句子，先确认 `本章功能 / 章节收尾` 是否需要回修。",
            }
        ]

    lines = [f"# {draft_path.stem} Review Learning Log", ""]
    lines.append(f"- source: `{draft_path}`")
    lines.append("- usage: 这不是最终审查结论，而是为‘确认 / 驳回 / 沉淀’准备的学习清单。")
    lines.append("")

    lines.append("## Manual Decisions")
    lines.append("- confirmed_issues: `TODO`")
    lines.append("- false_positives: `TODO`")
    lines.append("- design_repeats_to_keep: `TODO`")
    lines.append("- missing_checks: `TODO`")
    lines.append("")

    lines.append("## Template Backlog")
    if template_backlog:
        for item in template_backlog:
            lines.append(f"- `[pending]` `{item['bucket']}` `{item['name']}`：{item['reason']}")
            if item["sample"]:
                lines.append(f"  样例：{item['sample']}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Bonus Candidates")
    if bonus_backlog:
        for item in bonus_backlog:
            lines.append(f"- `[pending]` `{item['name']}`：{item['reason']}")
            if item["sample"]:
                lines.append(f"  样例：{item['sample']}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Rule / Bank Suggestions")
    if rule_suggestions:
        for item in rule_suggestions:
            lines.append(f"- `[pending]` `{item['target']}`：{item['reason']}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Consistency Feedback Snapshot")
    if consistency_snapshot.get("available"):
        lines.append(f"- story: `{consistency_snapshot['story']}`")
        lines.append(f"- feedback_log: `{consistency_snapshot['feedback_path']}`")
        lines.append(f"- confirmed=`{consistency_snapshot['decision_counter'].get('confirmed', 0)}`")
        lines.append(f"- false_positive=`{consistency_snapshot['decision_counter'].get('false_positive', 0)}`")
        lines.append(f"- designed_keep=`{consistency_snapshot['decision_counter'].get('designed_keep', 0)}`")
        lines.append(f"- watch=`{consistency_snapshot['decision_counter'].get('watch', 0)}`")
        lines.append(f"- pending=`{len(consistency_snapshot['pending_rows'])}`")
        lines.append(f"- review_queue: `{consistency_snapshot['review_queue_command']}`")
        lines.append(f"- feedback_summary: `{consistency_snapshot['feedback_summary_command']}`")
        if consistency_snapshot["facet_counter"]:
            lines.append("- facets:")
            for name, count in consistency_snapshot["facet_counter"].most_common(6):
                lines.append(f"  - `{name}` x{count}")
        pending_rows = consistency_snapshot["pending_rows"][:4]
        if pending_rows:
            lines.append("- pending rows:")
            for row in pending_rows:
                lines.append(
                    f"  - `{row['category']}` `{row['title']}` confidence=`{row.get('confidence', '')}` {row['summary']}"
                )
        pending_actions = consistency_snapshot.get("pending_actions", [])
        if pending_actions:
            lines.append("- pending actions:")
            for item in pending_actions[:3]:
                lines.append(
                    f"  - `{item['category']}` `{item['title']}` confidence=`{item['confidence']}`：{item['focus']}"
                )
                lines.append(f"  - command: `{item['command']}`")
    else:
        lines.append("- 无一致性反馈快照；可能还没建立本地索引库。")
    lines.append("")

    lines.append("## Plan Alignment Review")
    if alignment.get("available"):
        lines.append(f"- chapter_plan: `{alignment['chapter_plan_path']}`")
        lines.append(
            f"- chapter_function: plan=`{alignment['plan_chapter_function']}` draft=`{alignment['draft_chapter_function']}` match=`{alignment['chapter_match']}`"
        )
        lines.append(
            f"- ending_function: plan=`{alignment['plan_ending_function']}` draft=`{alignment['draft_ending_function']}` match=`{alignment['ending_match']}`"
        )
        lines.append(f"- alignment_status=`{alignment.get('alignment_status', 'unknown')}`")
        lines.append(f"- recommended_action=`{alignment.get('recommended_action', 'manual_review')}`")
        if alignment.get("drift_types"):
            lines.append("- drift_types: " + ", ".join(f"`{name}`" for name in alignment.get("drift_types", [])))
        if alignment["mismatch_count"] >= 1:
            lines.append("- review_focus:")
            lines.append(f"  - {alignment.get('review_note', '先判断该修正文，还是回修 chapter-plan。')}")
            lines.append("  - 如果正文更好，应回改施工图；如果施工图更对，应压回正文结构，而不是只做字词去重。")
    else:
        lines.append("- 无 plan-draft 对齐快照")
    lines.append("")

    lines.append("## Ending Signal Review")
    lines.append(
        f"- ending_signal=`{build_draft_stats.ending_label_display(build_draft_stats.infer_ending_label(analysis))}`"
    )
    if analysis["ending"]["warn"]:
        lines.append("- 当前章末仍有模板化风险；复盘时先判断它是坏重复，还是真有新的后果落点。")
    else:
        lines.append("- 当前章末未触发模板化警告；复盘时优先判断它是否在承担新的后果，而不是只因为不重复就直接加分。")
    lines.append("")

    lines.append("## Feedback-Derived Backlog")
    global_backlog = consistency_snapshot.get("global_feedback_backlog", [])
    if global_backlog:
        for item in global_backlog[:6]:
            lines.append(f"- `{item['target']}`：{item['reason']}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Reminder Snapshot")
    if analysis["review_reminders"]:
        for item in analysis["review_reminders"][:8]:
            lines.append(f"- `{item['priority']}` `{item['category']}` {item['title']}：{item['reason']}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Next Review Questions")
    lines.append("- 这些重复里，哪些其实承担了人物声音、压迫感、节奏或讽刺功能？")
    lines.append("- 这章真正要沉淀的是模板、词库、规则，还是只是一个局部问题？")
    lines.append("- 如果这类问题再次出现，下一轮脚本应该如何更早抓到它？")
    lines.append("")

    return "\n".join(lines) + "\n"


def build_story_summary(
    story_dir: Path,
    analyses: list[tuple[Path, dict[str, object]]],
    snapshots: dict[Path, dict[str, object]],
) -> str:
    template_counter: collections.Counter[str] = collections.Counter()
    suggestion_counter: collections.Counter[str] = collections.Counter()
    consistency_decisions: collections.Counter[str] = collections.Counter()
    consistency_categories: collections.Counter[str] = collections.Counter()
    consistency_facets: collections.Counter[str] = collections.Counter()
    alignment_counter: collections.Counter[str] = collections.Counter()
    ending_alignment_counter: collections.Counter[str] = collections.Counter()
    ending_signal_counter: collections.Counter[str] = collections.Counter()
    ending_signal_flow: list[str] = []

    lines = ["# Review Learning Summary", ""]
    lines.append(f"- story: `{story_dir}`")
    lines.append(f"- chapters: `{len(analyses)}`")
    lines.append("")

    lines.append("## Chapters")
    for path, analysis in sorted(analyses, key=lambda item: chapter_sort_key(item[0])):
        template_backlog = collect_template_backlog(analysis)
        snapshot = snapshots.get(path, {"available": False})
        novel_dir = novel_dir_for_draft(path)
        alignment = (
            plan_draft_alignment.build_plan_draft_alignment(path, novel_dir, analysis)
            if novel_dir is not None
            else {"available": False}
        )
        for item in template_backlog:
            template_counter[f"{item['bucket']}::{item['name']}"] += 1
        for item in collect_rule_suggestions(analysis) + build_consistency_suggestions(snapshot):
            suggestion_counter[item["target"]] += 1
        if alignment.get("available"):
            alignment_counter[
                f"{alignment['plan_chapter_function']}->{alignment['draft_chapter_function']}"
            ] += 1
            ending_alignment_counter[
                f"{alignment['plan_ending_function']}->{alignment['draft_ending_function']}"
            ] += 1
        ending_signal = build_draft_stats.infer_ending_label(analysis)
        ending_signal_counter[ending_signal] += 1
        ending_signal_flow.append(ending_signal)
        if snapshot.get("available"):
            for decision, count in snapshot["decision_counter"].items():
                consistency_decisions[decision] += int(count)
            for category, count in snapshot["category_counter"].items():
                consistency_categories[category] += int(count)
            for facet, count in snapshot["facet_counter"].items():
                consistency_facets[facet] += int(count)
        lines.append(
            f"- `{path.name}` template_backlog=`{len(template_backlog)}` reminders=`{len(analysis['review_reminders'])}` warn_sections=`{analysis['summary']['warn_sections']}` ending_signal=`{build_draft_stats.ending_label_display(ending_signal)}`"
        )
    lines.append("")

    lines.append("## Repeated Template Candidates")
    if template_counter:
        for name, count in template_counter.most_common(12):
            lines.append(f"- `{name}` x{count}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Plan-Draft Alignment")
    if alignment_counter:
        for name, count in alignment_counter.most_common(8):
            lines.append(f"- `chapter {name}` x{count}")
        for name, count in ending_alignment_counter.most_common(8):
            lines.append(f"- `ending {name}` x{count}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Ending Trend Signals")
    if ending_signal_counter:
        for name, count in ending_signal_counter.most_common(8):
            lines.append(f"- `{build_draft_stats.ending_label_display(name)}` x{count}")
        lines.append(f"- flow=`{build_draft_stats.ending_flow_text(ending_signal_flow)}`")
        runs = build_draft_stats.summarize_runs(ending_signal_flow)
        lines.append(f"- repeated=`{' | '.join(runs) if runs else '无'}`")
        if runs:
            lines.append("- review_focus: 连续同类章末时，优先判断这些结尾是在推进不同后果，还是只是在复用同一种收束手势。")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Suggested Deposition Targets")
    if suggestion_counter:
        for name, count in suggestion_counter.most_common():
            lines.append(f"- `{name}` x{count}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Consistency Feedback")
    if consistency_decisions:
        for decision in ("confirmed", "false_positive", "designed_keep", "watch"):
            lines.append(f"- `{decision}` x{consistency_decisions.get(decision, 0)}")
        if consistency_categories:
            lines.append("- categories:")
            for name, count in consistency_categories.most_common(8):
                lines.append(f"  - `{name}` x{count}")
        if consistency_facets:
            lines.append("- facets:")
            for name, count in consistency_facets.most_common(8):
                lines.append(f"  - `{name}` x{count}")
    else:
        lines.append("- 无")
    lines.append("")
    return "\n".join(lines) + "\n"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Build review learning logs from draft analysis.")
    parser.add_argument("paths", nargs="+", help="Draft chapter files or directories")
    parser.add_argument("--sample-limit", type=int, default=6, help="Sample limit for draft analysis")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    files = collect_chapter_files(args.paths)
    if not files:
        raise SystemExit("No draft chapter files found.")

    corpus_profile = build_corpus_profile_for_files(files)
    snapshots: dict[Path, dict[str, object]] = {}
    analyses = analyze_files(
        files,
        sample_limit=args.sample_limit,
        corpus_profile=corpus_profile,
    )
    for draft_path, _analysis in analyses:
        snapshot = consistency_index.build_story_conflict_snapshot_from_path(draft_path)
        snapshots[draft_path] = snapshot
        out_path = learning_log_path_for(draft_path)
        write_text(out_path, build_learning_log(draft_path, _analysis, snapshot))
        print(out_path)

    grouped: dict[Path, list[tuple[Path, dict[str, object]]]] = collections.defaultdict(list)
    for path, analysis in analyses:
        grouped[path.parent].append((path, analysis))
    for story_dir, items in sorted(grouped.items()):
        summary_path = learning_log_path_for(items[0][0]).parent / "SUMMARY.md"
        write_text(summary_path, build_story_summary(story_dir, items, snapshots))
        print(summary_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
