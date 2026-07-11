#!/usr/bin/env python3
"""Build a story-level review kit that bundles scorecards, learning logs, profiles, and consistency entrypoints."""

from __future__ import annotations

import argparse
import collections
from pathlib import Path

from sentinel.stats import draft as build_draft_stats
from sentinel.reports import learning as build_review_learning_logs
from sentinel.reports import scorecard as build_review_scorecards
from sentinel.reports import profiles as build_sentence_profiles
from sentinel.reports import backlog as build_template_backlog
from sentinel import consistency as consistency_index
from sentinel.audit import draft as draft_audit
import plan_draft_alignment
from sentinel.lib.analysis import analyze_files, build_corpus_profile_for_files
from sentinel.lib.io import write_text


def review_kit_path_for(draft_path: Path) -> Path:
    return build_draft_stats.stats_path_for(draft_path).parent / "review-kit" / "SUMMARY.md"



def _assignment_key(item: dict[str, object]) -> tuple[str, str, str]:
    return (str(item.get("priority", "")), str(item.get("chapter", "")), str(item.get("title", "")))


def _assignment_sort_key(item: dict[str, object]) -> tuple[int, str, str]:
    priority_rank = {"P0": 0, "P1": 1, "P2": 2}.get(str(item.get("priority", "P2")), 3)
    source = str(item.get("source", ""))
    source_rank = {
        "scorecards": 0,
        "plan_draft_alignment": 1,
        "ending_trends": 2,
        "trend_convergence": 3,
        "consistency_index": 4,
        "template_backlog": 5,
    }.get(source, 5)
    return (priority_rank, source_rank, str(item.get("chapter", "")), str(item.get("title", "")))


def _add_assignment(
    assignments: list[dict[str, object]],
    *,
    priority: str,
    chapter: str,
    title: str,
    reason: str,
    action: str,
    source: str,
) -> None:
    item = {
        "priority": priority,
        "chapter": chapter,
        "title": title,
        "reason": reason,
        "action": action,
        "source": source,
    }
    if _assignment_key(item) not in {_assignment_key(existing) for existing in assignments}:
        assignments.append(item)


def _collect_repeated_ending_runs(
    analyses: list[tuple[Path, dict[str, object]]],
) -> list[dict[str, object]]:
    runs: list[dict[str, object]] = []
    ordered = sorted(analyses, key=lambda item: build_review_scorecards.chapter_sort_key(item[0]))
    current_label: str | None = None
    current_paths: list[Path] = []

    def flush() -> None:
        nonlocal current_label, current_paths
        if current_label is None or len(current_paths) < 2:
            current_label = None
            current_paths = []
            return
        runs.append(
            {
                "label": current_label,
                "paths": list(current_paths),
            }
        )
        current_label = None
        current_paths = []

    for draft_path, analysis in ordered:
        label = build_draft_stats.infer_ending_label(analysis)
        if label == current_label:
            current_paths.append(draft_path)
            continue
        flush()
        current_label = label
        current_paths = [draft_path]
    flush()
    return runs


def _collect_repeated_value_runs(
    rows: list[tuple[Path, str]],
    *,
    min_run: int = 2,
) -> list[dict[str, object]]:
    runs: list[dict[str, object]] = []
    ordered = sorted(rows, key=lambda item: build_review_scorecards.chapter_sort_key(item[0]))
    current_value: str | None = None
    current_paths: list[Path] = []

    def flush() -> None:
        nonlocal current_value, current_paths
        if current_value is None or len(current_paths) < min_run:
            current_value = None
            current_paths = []
            return
        runs.append({"value": current_value, "paths": list(current_paths)})
        current_value = None
        current_paths = []

    for draft_path, value in ordered:
        if value == current_value:
            current_paths.append(draft_path)
            continue
        flush()
        current_value = value
        current_paths = [draft_path]
    flush()
    return runs


def _collect_converging_trend_runs(
    analyses: list[tuple[Path, dict[str, object]]],
) -> list[dict[str, object]]:
    ending_runs = _collect_repeated_ending_runs(analyses)
    tone_runs = _collect_repeated_value_runs(
        [
            (path, str(analysis.get("tone_profile", {}).get("dominant_tone", "none") or "none"))
            for path, analysis in analyses
        ]
    )
    emotion_runs = _collect_repeated_value_runs(
        [
            (path, str(analysis.get("dialogue_emotions", {}).get("dominant_emotion", "neutral") or "neutral"))
            for path, analysis in analyses
        ]
    )

    convergences: list[dict[str, object]] = []
    for ending_run in ending_runs:
        ending_paths = set(ending_run["paths"])
        ending_label = build_draft_stats.ending_label_display(str(ending_run["label"]))
        for tone_run in tone_runs:
            overlap = [path for path in tone_run["paths"] if path in ending_paths]
            if len(overlap) >= 2 and tone_run["value"] != "none":
                convergences.append(
                    {
                        "kind": "ending_tone",
                        "paths": overlap,
                        "ending_label": ending_label,
                        "secondary_label": str(tone_run["value"]),
                    }
                )
        for emotion_run in emotion_runs:
            overlap = [path for path in emotion_run["paths"] if path in ending_paths]
            if len(overlap) >= 2 and emotion_run["value"] != "neutral":
                convergences.append(
                    {
                        "kind": "ending_emotion",
                        "paths": overlap,
                        "ending_label": ending_label,
                        "secondary_label": str(emotion_run["value"]),
                    }
                )
    return convergences


def collect_review_assignments(
    story_dir: Path,
    analyses: list[tuple[Path, dict[str, object]]],
    snapshots: dict[Path, dict[str, object]],
) -> list[dict[str, object]]:
    """Translate review-kit inputs into concrete P1/P2 tasks for a human reviewer."""
    assignments: list[dict[str, object]] = []
    template_counter: collections.Counter[str] = collections.Counter()
    consistency_added = False

    for draft_path, analysis in sorted(analyses, key=lambda item: build_review_scorecards.chapter_sort_key(item[0])):
        chapter = draft_path.stem
        snapshot = snapshots.get(draft_path, {"available": False})
        novel_dir = build_review_scorecards.novel_dir_for_draft(draft_path)
        alignment = (
            plan_draft_alignment.build_plan_draft_alignment(draft_path, novel_dir, analysis)
            if novel_dir is not None
            else {"available": False}
        )
        axes = build_review_scorecards.build_axes(analysis, snapshot, alignment)
        gate, priority, recommendation = build_review_scorecards.decide_gate(analysis, axes)

        if gate in {"WATCH", "FAIL"}:
            _add_assignment(
                assignments,
                priority="P1" if gate == "FAIL" else priority,
                chapter=chapter,
                title=f"复核 `{chapter}` 的 {gate} scorecard",
                reason=(
                    f"gate=`{gate}` recommendation=`{recommendation}` warn_sections=`{analysis['summary']['warn_sections']}` "
                    f"hard_flags=`{len(analysis['hard_flags'])}`"
                ),
                action="先读 `scorecards/` 对应章节，再按 P1 reminders 和 hard flags 定位局部重写点。",
                source="scorecards",
            )

        for reminder in analysis.get("review_reminders", []):
            if reminder.get("priority") not in {"P1", "P2"}:
                continue
            if reminder.get("priority") == "P2" and len(assignments) >= 10:
                continue
            _add_assignment(
                assignments,
                priority=str(reminder.get("priority", "P2")),
                chapter=chapter,
                title=f"复核 `{chapter}`：{reminder.get('title', '审查提醒')}",
                reason=str(reminder.get("reason", "")),
                action=str(reminder.get("action", reminder.get("check", "按报告样例回查正文。"))),
                source=f"review_reminders/{reminder.get('category', 'general')}",
            )

        if alignment.get("available") and int(alignment.get("mismatch_count", 0)) >= 1:
            _add_assignment(
                assignments,
                priority="P1",
                chapter=chapter,
                title=f"复核 `{chapter}` 的 plan-draft 漂移",
                reason=(
                    f"status=`{alignment.get('alignment_status', 'unknown')}` action=`{alignment.get('recommended_action', 'manual_review')}`；"
                    f"chapter `{alignment.get('plan_chapter_function')}`→`{alignment.get('draft_chapter_function')}`；"
                    f"ending `{alignment.get('plan_ending_function')}`→`{alignment.get('draft_ending_function')}`"
                ),
                action=str(alignment.get("review_note", "先判断该修正文落点，还是回修 chapter-plan / story-plan；不要只当文风问题处理。")),
                source="plan_draft_alignment",
            )

        if snapshot.get("available") and snapshot.get("pending_rows") and not consistency_added:
            pending_count = len(snapshot.get("pending_rows", []))
            story = snapshot.get("story", story_dir.name)
            _add_assignment(
                assignments,
                priority="P1",
                chapter=str(story),
                title=f"复核 `{story}` 的一致性 pending",
                reason=f"当前一致性快照仍有 `{pending_count}` 条候选未判定。",
                action=f"跑 `{snapshot.get('review_queue_command', '')}`，复核后用 `feedback-add` 写回判定。",
                source="consistency_index",
            )
            consistency_added = True

        for item in build_review_learning_logs.collect_template_backlog(analysis):
            template_counter[f"{item['bucket']}::{item['name']}"] += 1

    for run in _collect_repeated_ending_runs(analyses):
        paths = [path.stem for path in run["paths"]]
        chapter_span = f"{paths[0]}-{paths[-1]}" if len(paths) >= 2 else paths[0]
        ending_label = build_draft_stats.ending_label_display(str(run["label"]))
        priority = "P1" if len(paths) >= 3 else "P2"
        _add_assignment(
            assignments,
            priority=priority,
            chapter=story_dir.name,
            title=f"复核 `{chapter_span}` 的同类章末连发",
            reason=f"连续 `{len(paths)}` 章落在 `{ending_label}`，跨章读感可能开始同质化。",
            action="对照 pairs / triples 的 `endings=` 和 `repeated=`，确认这些结尾是在推进不同后果，还是只是在重复同一种收束手势。",
            source="ending_trends",
        )

    for item in _collect_converging_trend_runs(analyses):
        paths = [path.stem for path in item["paths"]]
        chapter_span = f"{paths[0]}-{paths[-1]}" if len(paths) >= 2 else paths[0]
        if item["kind"] == "ending_tone":
            title = f"复核 `{chapter_span}` 的章末-色调合流"
            reason = (
                f"连续 `{len(paths)}` 章同时落在章末 `{item['ending_label']}` 和色调 `{item['secondary_label']}`，"
                "跨章读感可能开始同温度、同收束。"
            )
            action = "先看 scorecards / profiles 的 tone 与 ending trend，再判断这些章节是在持续累积压迫，还是已经写成同一种章末氛围模板。"
        else:
            title = f"复核 `{chapter_span}` 的章末-对白情绪合流"
            reason = (
                f"连续 `{len(paths)}` 章同时落在章末 `{item['ending_label']}` 和对白情绪 `{item['secondary_label']}`，"
                "跨章关系推进可能开始同质化。"
            )
            action = "先看 learning / profiles 的 dialogue emotion 与 ending trend，再判断这些结尾是不是一直在用同一种情绪温度收束关系。"
        _add_assignment(
            assignments,
            priority="P1" if len(paths) >= 3 else "P2",
            chapter=story_dir.name,
            title=title,
            reason=reason,
            action=action,
            source="trend_convergence",
        )

    for name, count in template_counter.most_common(4):
        if count < 2:
            continue
        _add_assignment(
            assignments,
            priority="P2",
            chapter=story_dir.name,
            title=f"判断跨章模板候选 `{name}` 是否该沉淀",
            reason=f"该候选在本 Story 中出现 `{count}` 次，已经值得人工区分坏重复、词库项或设计性保留。",
            action="读 `template-backlog/SUMMARY.md` 和 `CANDIDATES.json`，决定写回模板库、词库、规则，还是标记为 keep。",
            source="template_backlog",
        )

    ordered = sorted(assignments, key=_assignment_sort_key)
    p1_items = [item for item in ordered if item.get("priority") in {"P0", "P1"}]
    p2_items = [item for item in ordered if item.get("priority") == "P2"]
    if p2_items and len(p1_items) >= 12:
        return p1_items[:12] + p2_items[:2]
    return ordered[:14]


def build_story_review_kit(
    story_dir: Path,
    analyses: list[tuple[Path, dict[str, object]]],
    snapshots: dict[Path, dict[str, object]],
) -> str:
    ordered = sorted(analyses, key=lambda item: build_review_scorecards.chapter_sort_key(item[0]))
    scorecard_summary_path = build_review_scorecards.scorecard_path_for(ordered[0][0]).parent / "SUMMARY.md"
    learning_summary_path = build_review_learning_logs.learning_log_path_for(ordered[0][0]).parent / "SUMMARY.md"
    profile_summary_path = build_sentence_profiles.profile_path_for(ordered[0][0]).parent / "SUMMARY.md"
    template_backlog_summary_path = build_template_backlog.backlog_path_for(ordered[0][0])
    template_backlog_candidates_path = build_template_backlog.candidates_path_for(ordered[0][0])

    gate_counter: collections.Counter[str] = collections.Counter()
    recommendation_counter: collections.Counter[str] = collections.Counter()
    pending_rows = 0
    consistency_story = ""
    review_queue_command = ""
    feedback_summary_command = ""
    template_counter: collections.Counter[str] = collections.Counter()

    for draft_path, analysis in ordered:
        snapshot = snapshots.get(draft_path, {"available": False})
        if snapshot.get("available"):
            pending_rows = max(pending_rows, len(snapshot["pending_rows"]))
            consistency_story = str(snapshot["story"])
            review_queue_command = str(snapshot.get("review_queue_command", review_queue_command))
            feedback_summary_command = str(snapshot.get("feedback_summary_command", feedback_summary_command))
        novel_dir = build_review_scorecards.novel_dir_for_draft(draft_path)
        alignment = (
            plan_draft_alignment.build_plan_draft_alignment(draft_path, novel_dir, analysis)
            if novel_dir is not None
            else {"available": False}
        )
        axes = build_review_scorecards.build_axes(analysis, snapshot, alignment)
        gate, _priority, recommendation = build_review_scorecards.decide_gate(analysis, axes)
        gate_counter[gate] += 1
        recommendation_counter[recommendation] += 1
        for item in build_review_learning_logs.collect_template_backlog(analysis):
            template_counter[f"{item['bucket']}::{item['name']}"] += 1

    lines = ["# Review Kit", ""]
    lines.append(f"- story: `{story_dir}`")
    lines.append(f"- chapters: `{len(ordered)}`")
    lines.append(f"- scorecards: `{scorecard_summary_path}`")
    lines.append(f"- learning: `{learning_summary_path}`")
    lines.append(f"- profiles: `{profile_summary_path}`")
    lines.append(f"- template_backlog: `{template_backlog_summary_path}`")
    lines.append(f"- template_candidates: `{template_backlog_candidates_path}`")
    if review_queue_command:
        lines.append(f"- review_queue: `{review_queue_command}`")
    if feedback_summary_command:
        lines.append(f"- feedback_summary: `{feedback_summary_command}`")
    lines.append("")

    lines.append("## Current State")
    for name, count in gate_counter.items():
        lines.append(f"- gate `{name}` x{count}")
    for name, count in recommendation_counter.items():
        lines.append(f"- recommendation `{name}` x{count}")
    lines.append(f"- pending_consistency_rows: `{pending_rows}`")
    if consistency_story:
        lines.append(f"- consistency_story: `{consistency_story}`")
    lines.append("")

    assignments = collect_review_assignments(story_dir, ordered, snapshots)
    lines.append("## Review Assignments")
    if assignments:
        for item in assignments:
            lines.append(
                f"- {item['priority']}: {item['title']}（source=`{item['source']}`）"
                f"：{item['reason']} 动作：{item['action']}"
            )
    else:
        lines.append("- P2: 暂无硬派单；按 Suggested Flow 做常规抽查，重点确认可保留重复不要被误杀。")
    lines.append("")

    lines.append("## Suggested Flow")
    lines.append("1. 先读 `Review Assignments`，按 P1/P2 处理可指派复审任务。")
    lines.append("2. 再读 `scorecards/SUMMARY.md`，确认这条 Story 当前是 `PASS / WATCH / FAIL` 哪一侧。")
    lines.append("3. 再读 `learning/SUMMARY.md`，确认重复模板、沉淀目标和 plan-draft 漂移是否集中。")
    lines.append("4. 再读 `profiles/SUMMARY.md`，确认句式骨架、人物声音和场景色调是不是同一类问题反复出现。")
    lines.append("5. 再读 `template-backlog/SUMMARY.md`，把坏模式候选和可保留风格候选拆开看。")
    if review_queue_command:
        lines.append(f"6. 跑 `{review_queue_command}`，逐条复核一致性 pending。")
    if feedback_summary_command:
        lines.append(f"7. 每做完一轮判定后，跑 `{feedback_summary_command}` 看这条 Story 是否开始收敛。")
    lines.append("8. 最后再决定这轮该沉淀模板、词库、规则，还是回修正文 / chapter-plan / story-plan。")
    lines.append("")

    lines.append("## Template Hotspots")
    if template_counter:
        for name, count in template_counter.most_common(8):
            lines.append(f"- `{name}` x{count}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Chapter Paths")
    for draft_path, _analysis in ordered:
        lines.append(f"- `{draft_path}`")
    lines.append("")

    return "\n".join(lines) + "\n"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Build a bundled story-level review kit.")
    parser.add_argument("paths", nargs="+", help="Draft chapter files or directories")
    parser.add_argument("--sample-limit", type=int, default=6, help="Sample limit for draft analysis")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    files = build_review_scorecards.collect_chapter_files(args.paths)
    if not files:
        raise SystemExit("No draft chapter files found.")

    corpus_profile = build_corpus_profile_for_files(files)
    snapshots: dict[Path, dict[str, object]] = {}
    analyses = analyze_files(
        files,
        sample_limit=args.sample_limit,
        corpus_profile=corpus_profile,
    )

    for draft_path, analysis in analyses:
        snapshot = consistency_index.build_story_conflict_snapshot_from_path(draft_path)
        snapshots[draft_path] = snapshot

        build_review_scorecards.write_text(
            build_review_scorecards.scorecard_path_for(draft_path),
            build_review_scorecards.build_scorecard_report(draft_path, analysis, snapshot),
        )
        build_review_learning_logs.write_text(
            build_review_learning_logs.learning_log_path_for(draft_path),
            build_review_learning_logs.build_learning_log(draft_path, analysis, snapshot),
        )
        build_sentence_profiles.write_text(
            build_sentence_profiles.profile_path_for(draft_path),
            build_sentence_profiles.build_profile_report(draft_path, analysis, args.sample_limit),
        )

    grouped: dict[Path, list[tuple[Path, dict[str, object]]]] = collections.defaultdict(list)
    for draft_path, analysis in analyses:
        grouped[draft_path.parent].append((draft_path, analysis))

    for story_dir, items in sorted(grouped.items()):
        score_summary_path = build_review_scorecards.scorecard_path_for(items[0][0]).parent / "SUMMARY.md"
        build_review_scorecards.write_text(
            score_summary_path,
            build_review_scorecards.build_story_summary(story_dir, items, snapshots),
        )
        print(score_summary_path)

        learning_summary_path = build_review_learning_logs.learning_log_path_for(items[0][0]).parent / "SUMMARY.md"
        build_review_learning_logs.write_text(
            learning_summary_path,
            build_review_learning_logs.build_story_summary(story_dir, items, snapshots),
        )
        print(learning_summary_path)

        profile_summary_path = build_sentence_profiles.profile_path_for(items[0][0]).parent / "SUMMARY.md"
        build_sentence_profiles.write_text(
            profile_summary_path,
            build_sentence_profiles.build_story_summary(story_dir, items, args.sample_limit),
        )
        print(profile_summary_path)

        template_backlog_markdown, template_backlog_payload = build_template_backlog.build_story_backlog(story_dir, items)
        template_backlog_summary_path = build_template_backlog.backlog_path_for(items[0][0])
        template_backlog_candidates_path = build_template_backlog.candidates_path_for(items[0][0])
        build_template_backlog.write_text(template_backlog_summary_path, template_backlog_markdown)
        build_template_backlog.write_json(template_backlog_candidates_path, template_backlog_payload)
        print(template_backlog_summary_path)
        print(template_backlog_candidates_path)

        kit_path = review_kit_path_for(items[0][0])
        write_text(kit_path, build_story_review_kit(story_dir, items, snapshots))
        print(kit_path)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
