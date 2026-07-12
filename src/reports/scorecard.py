#!/usr/bin/env python3
"""Build review scorecards from draft analysis results."""

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


def scorecard_path_for(draft_path: Path) -> Path:
    return build_draft_stats.stats_path_for(draft_path).parent / "scorecards" / f"{draft_path.stem}.md"


def clamp_score(value: int) -> int:
    return max(1, min(5, value))


def axis_score(
    *,
    base: int,
    penalties: list[int],
    bonuses: list[int] | None = None,
) -> int:
    total = base - sum(penalties) + sum(bonuses or [])
    return clamp_score(total)


def build_bonus_candidates(analysis: dict[str, object]) -> list[dict[str, str]]:
    candidates: list[dict[str, str]] = []
    summary = analysis["summary"]
    dialogue = analysis["dialogue"]

    if summary["warn_sections"] <= 6 and len(analysis["style_fatigue"]) <= 3:
        candidates.append(
            {
                "name": "整体收敛较稳",
                "reason": "硬警告分区不多，句式疲劳家族数量也较少，说明这一章的文体控制相对稳定。",
            }
        )
    if 0.12 <= summary["quote_ratio"] <= 0.45 and not dialogue["dialogue_axis_gaps"]:
        candidates.append(
            {
                "name": "对白与叙述配比自然",
                "reason": "对白比例在可读区间内，且没有明显对白转轴缺口，说明场面没有塌成纯互答录音。",
            }
        )
    if 0.18 <= summary["short_sentence_ratio"] <= 0.35 and not analysis["sentence_lengths"]["warn"]:
        candidates.append(
            {
                "name": "短句节奏可保留",
                "reason": "短句比例存在但没有连发失控，更像节奏设计而不是内容写薄。",
            }
        )
    if analysis["ending"]["warn"] is False and analysis["ending"]["tail_excerpt"]:
        candidates.append(
            {
                "name": "章末收束未模板化",
                "reason": "章末没有落入现有意象/流程词模板，说明结尾功能有继续发展的空间。",
            }
        )
    if analysis["aa_bb_patterns"] and not any(item["warn"] for item in analysis["aa_bb_patterns"]):
        candidates.append(
            {
                "name": "局部排比可视作风格点缀",
                "reason": "检测到少量 AA/BB 或短分句节奏，但还没形成模板疲劳，可以先按风格候选保留。",
            }
        )
    scene_map = analysis.get("scene_map", {})
    if scene_map and not scene_map.get("warn") and int(scene_map.get("switch_count", 0)) >= 2:
        candidates.append(
            {
                "name": "场面功能有切换",
                "reason": "粗分块没有被单一对白或说明吃满，说明这一章至少在尝试做功能接力。",
            }
        )
    dialogue_emotions = analysis.get("dialogue_emotions", {})
    if (
        dialogue_emotions.get("dialogue_sentences", 0) >= 4
        and not dialogue_emotions.get("flatness_warn")
        and not dialogue_emotions.get("volatility_warn")
        and dialogue_emotions.get("shift_count", 0) >= 1
    ):
        candidates.append(
            {
                "name": "对白情绪有起伏",
                "reason": "对白情绪不是单一平推，也没有明显横跳，更像在做关系推进而不是纯互顶。",
            }
        )
    battle_profile = analysis.get("battle_profile", {})
    if battle_profile.get("sequence_count", 0) >= 1 and battle_profile.get("result_ratio", 0) >= 0.35:
        candidates.append(
            {
                "name": "动作段有后果反馈",
                "reason": "冲突段不只累计动作动词，也带出了结果、伤害或位移反馈，可以视作紧凑度候选。",
            }
        )
    return candidates[:4]


def build_consistency_bonus_candidates(snapshot: dict[str, object]) -> list[dict[str, str]]:
    if not snapshot.get("available"):
        return []
    candidates: list[dict[str, str]] = []
    if snapshot["decision_counter"].get("designed_keep", 0) >= 1:
        candidates.append(
            {
                "name": "一致性例外已沉淀",
                "reason": "同一 story 已有候选被人工判为设计性保留，说明工具开始学会区分“可保留的变化”和“真漂移”。",
            }
        )
    if snapshot["decision_counter"].get("false_positive", 0) >= 1 and not snapshot["pending_rows"]:
        candidates.append(
            {
                "name": "一致性复核收敛",
                "reason": "这一条 story 的一致性候选已有复核反馈，且当前没有遗留待判项，说明复审闭环在起作用。",
            }
        )
    return candidates[:2]


def build_axes(
    analysis: dict[str, object],
    consistency_snapshot: dict[str, object] | None = None,
    alignment_snapshot: dict[str, object] | None = None,
    story_trend_snapshot: dict[str, object] | None = None,
) -> list[dict[str, object]]:
    summary = analysis["summary"]
    dialogue = analysis["dialogue"]
    fatigue_warn_count = sum(1 for item in analysis["style_fatigue"] if item["status"] == "WARN")
    reminder_p1_count = sum(1 for item in analysis["review_reminders"] if item["priority"] == "P1")
    reminder_p2_count = sum(1 for item in analysis["review_reminders"] if item["priority"] == "P2")
    dialogue_gap_count = len(dialogue["dialogue_axis_gaps"])
    ping_pong_count = len(dialogue["quote_ping_pong"]) + len(dialogue["question_ping_pong"])
    scene_map = analysis.get("scene_map", {})
    dialogue_emotions = analysis.get("dialogue_emotions", {})
    character_voice = analysis.get("character_voice", {})
    tone_profile = analysis.get("tone_profile", {})
    battle_profile = analysis.get("battle_profile", {})
    viewpoint_profile = analysis.get("viewpoint_profile", {})
    consistency_pending = 0
    consistency_confirmed = 0
    consistency_false_positive = 0
    alignment_mismatch_count = 0
    if consistency_snapshot and consistency_snapshot.get("available"):
        consistency_pending = len(consistency_snapshot["pending_rows"])
        consistency_confirmed = int(consistency_snapshot["decision_counter"].get("confirmed", 0))
        consistency_false_positive = int(consistency_snapshot["decision_counter"].get("false_positive", 0))
    if alignment_snapshot and alignment_snapshot.get("available"):
        alignment_mismatch_count = int(alignment_snapshot.get("mismatch_count", 0))
    convergence_kinds: list[str] = []
    if story_trend_snapshot:
        convergence_kinds = list(story_trend_snapshot.get("convergence_kinds", []))

    axes: list[dict[str, object]] = []

    repetition_score = axis_score(
        base=5,
        penalties=[
            min(len(analysis["hard_flags"]) // 6, 3),
            min(fatigue_warn_count, 2),
            1 if analysis["tracked_term_window_count"] >= 3 else 0,
            1 if len(analysis["sentence_patterns"]) >= 4 else 0,
        ],
        bonuses=[1 if summary["warn_sections"] <= 4 else 0],
    )
    axes.append(
        {
            "name": "重复控制",
            "score": repetition_score,
            "reason": "综合硬警告、句式疲劳、局部点名密度与句首骨架重复。",
        }
    )

    sentence_score = axis_score(
        base=5,
        penalties=[
            1 if analysis["sentence_lengths"]["warn"] else 0,
            1 if len(analysis["clause_prefixes"]) >= 4 else 0,
            1 if len(analysis["parallel_clauses"]) >= 3 else 0,
            1 if any(item["warn"] for item in analysis["aa_bb_patterns"]) else 0,
        ],
        bonuses=[1 if 0.18 <= summary["short_sentence_ratio"] <= 0.35 else 0],
    )
    axes.append(
        {
            "name": "句式弹性",
            "score": sentence_score,
            "reason": "观察短句、并列分句、AA/BB 节奏和分句前缀，判断是节奏还是手癖。",
        }
    )

    dialogue_score = axis_score(
        base=5,
        penalties=[
            min(dialogue_gap_count, 2),
            1 if ping_pong_count >= 2 else 0,
            1 if dialogue["dense_quote_run_count"] >= 2 else 0,
            1 if summary["quote_ratio"] > 0.55 else 0,
            1 if dialogue_emotions.get("flatness_warn") else 0,
            1 if dialogue_emotions.get("volatility_warn") else 0,
            1 if character_voice.get("warn") else 0,
            1 if "ending_emotion" in convergence_kinds else 0,
        ],
        bonuses=[
            1 if 0.12 <= summary["quote_ratio"] <= 0.45 and dialogue_gap_count == 0 else 0,
            1 if dialogue_emotions.get("shift_count", 0) >= 1 and not dialogue_emotions.get("volatility_warn") else 0,
            1 if not character_voice.get("warn") and int(character_voice.get("speaker_count", 0)) >= 2 else 0,
        ],
    )
    axes.append(
        {
            "name": "对白情感与转轴",
            "score": dialogue_score,
            "reason": "看对白是否有动作、环境、第三方或设备转轴，而不是长时间互顶。",
        }
    )

    tone_score = axis_score(
        base=4,
        penalties=[
            1 if analysis["ending"]["warn"] else 0,
            1 if any(item["warn"] for item in analysis["modifier_pressure"]) else 0,
            1 if len(analysis["fatigue_windows"]) >= 4 else 0,
            1 if tone_profile.get("warn") else 0,
            1 if scene_map.get("warn") else 0,
            1 if "ending_tone" in convergence_kinds else 0,
        ],
        bonuses=[
            1 if not analysis["ending"]["warn"] else 0,
            1 if tone_profile.get("stable_ratio", 0) >= 0.35 and tone_profile.get("dominant_tone", "none") != "none" else 0,
        ],
    )
    axes.append(
        {
            "name": "场景色调稳定",
            "score": tone_score,
            "reason": "暂时用章末模板、修饰压力和局部疲劳窗口做代理指标，后续再接更细的色调分类。",
        }
    )

    tension_score = axis_score(
        base=4,
        penalties=[
            1 if summary["short_sentence_ratio"] > 0.42 else 0,
            1 if ping_pong_count >= 2 else 0,
            1 if dialogue_gap_count >= 2 else 0,
            1 if battle_profile.get("warn") else 0,
        ],
        bonuses=[
            1 if summary["avg_sentence_chars"] >= 14 and summary["avg_sentence_chars"] <= 28 else 0,
            1 if battle_profile.get("sequence_count", 0) >= 1 and battle_profile.get("result_ratio", 0) >= 0.35 else 0,
        ],
    )
    axes.append(
        {
            "name": "张力与紧凑度",
            "score": tension_score,
            "reason": "用句长、对白互顶和转轴缺口粗看战斗/冲突段是否只是快而不紧。",
        }
    )

    viewpoint_score = axis_score(
        base=4,
        penalties=[
            1 if len(analysis["judgement_contexts"]) >= 3 else 0,
            1 if len(analysis["learned_filters"]) >= 4 else 0,
            1 if reminder_p1_count >= 3 else 0,
            1 if viewpoint_profile.get("warn") else 0,
        ],
        bonuses=[
            1 if reminder_p1_count == 0 else 0,
            1 if not viewpoint_profile.get("warn") and viewpoint_profile.get("dominant_anchor") else 0,
        ],
    )
    axes.append(
        {
            "name": "视角与判断稳定",
            "score": viewpoint_score,
            "reason": "当前主要用判断句上下文、语料偏移和高优先提醒做代理，先拦旁白抢跑与说明过重。",
        }
    )

    consistency_score = axis_score(
        base=4,
        penalties=[
            1 if analysis["tracked_term_window_count"] >= 4 else 0,
            1 if len(analysis["learned_filters"]) >= 5 else 0,
            1 if summary["warn_sections"] >= 12 else 0,
            1 if consistency_pending >= 1 else 0,
            1 if consistency_confirmed >= 2 else 0,
            1 if alignment_mismatch_count >= 2 else 0,
        ],
        bonuses=[
            1 if analysis["corpus_profile"]["enabled"] else 0,
            1 if consistency_false_positive >= 1 and consistency_pending == 0 else 0,
            1 if alignment_mismatch_count == 0 and alignment_snapshot and alignment_snapshot.get("available") else 0,
        ],
    )
    axes.append(
        {
            "name": "一致性准备度",
            "score": consistency_score,
            "reason": "看当前章与同书语料的偏离程度，以及当前 story 的一致性候选是否已被复核、确认或仍待处理。",
        }
    )

    structure_score = axis_score(
        base=4,
        penalties=[
            1 if reminder_p1_count >= 2 else 0,
            1 if len(analysis["fatigue_windows"]) >= 5 else 0,
            1 if summary["warn_sections"] >= 14 else 0,
            1 if scene_map.get("warn") else 0,
            1 if alignment_mismatch_count >= 1 else 0,
        ],
        bonuses=[
            1 if reminder_p1_count == 0 and reminder_p2_count <= 2 else 0,
            1 if int(scene_map.get("switch_count", 0)) >= 2 and not scene_map.get("warn") else 0,
            1 if alignment_mismatch_count == 0 and alignment_snapshot and alignment_snapshot.get("available") else 0,
        ],
    )
    axes.append(
        {
            "name": "结构完成度",
            "score": structure_score,
            "reason": "暂用高优先提醒、局部高压窗口和总体告警量做代理，后续再接 Scene/章末功能分析。",
        }
    )
    return axes


def decide_gate(analysis: dict[str, object], axes: list[dict[str, object]]) -> tuple[str, str, str]:
    p1_count = sum(1 for item in analysis["review_reminders"] if item["priority"] == "P1")
    avg_score = sum(int(item["score"]) for item in axes) / max(len(axes), 1)
    hard_count = len(analysis["hard_flags"])
    warn_sections = int(analysis["summary"]["warn_sections"])

    if p1_count >= 4 or warn_sections >= 16 or avg_score < 2.4 or hard_count >= 22:
        return "FAIL", "P0", "targeted_rewrite"
    if p1_count >= 2 or warn_sections >= 10 or avg_score < 3.4 or hard_count >= 12:
        return "WATCH", "P1", "light_revise"
    return "PASS", "P2", "retain"


def collect_repeated_value_runs(
    rows: list[tuple[Path, str]],
    *,
    min_run: int = 2,
) -> list[dict[str, object]]:
    runs: list[dict[str, object]] = []
    ordered = sorted(rows, key=lambda item: chapter_sort_key(item[0]))
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


def build_story_trend_snapshots(
    analyses: list[tuple[Path, dict[str, object]]],
) -> dict[Path, dict[str, object]]:
    snapshots: dict[Path, dict[str, object]] = {}
    ending_runs = []
    ordered = sorted(analyses, key=lambda item: chapter_sort_key(item[0]))
    current_label: str | None = None
    current_paths: list[Path] = []

    def flush_endings() -> None:
        nonlocal current_label, current_paths
        if current_label is not None and len(current_paths) >= 2:
            ending_runs.append({"label": current_label, "paths": list(current_paths)})
        current_label = None
        current_paths = []

    for draft_path, analysis in ordered:
        label = build_draft_stats.infer_ending_label(analysis)
        if label == current_label:
            current_paths.append(draft_path)
            continue
        flush_endings()
        current_label = label
        current_paths = [draft_path]
    flush_endings()

    tone_runs = collect_repeated_value_runs(
        [
            (path, str(analysis.get("tone_profile", {}).get("dominant_tone", "none") or "none"))
            for path, analysis in analyses
        ]
    )
    emotion_runs = collect_repeated_value_runs(
        [
            (path, str(analysis.get("dialogue_emotions", {}).get("dominant_emotion", "neutral") or "neutral"))
            for path, analysis in analyses
        ]
    )

    for ending_run in ending_runs:
        ending_paths = set(ending_run["paths"])
        ending_label = build_draft_stats.ending_label_display(str(ending_run["label"]))
        for tone_run in tone_runs:
            overlap = [path for path in tone_run["paths"] if path in ending_paths]
            if len(overlap) >= 2 and tone_run["value"] != "none":
                for path in overlap:
                    snapshot = snapshots.setdefault(path, {"convergence_kinds": [], "notes": []})
                    snapshot["convergence_kinds"].append("ending_tone")
                    snapshot["notes"].append(f"{ending_label}+tone:{tone_run['value']}")
        for emotion_run in emotion_runs:
            overlap = [path for path in emotion_run["paths"] if path in ending_paths]
            if len(overlap) >= 2 and emotion_run["value"] != "neutral":
                for path in overlap:
                    snapshot = snapshots.setdefault(path, {"convergence_kinds": [], "notes": []})
                    snapshot["convergence_kinds"].append("ending_emotion")
                    snapshot["notes"].append(f"{ending_label}+emotion:{emotion_run['value']}")
    return snapshots


def recommendation_note(recommendation: str) -> str:
    mapping = {
        "retain": "当前章可保留主体结构，优先微调个别硬项，不要为了清零统计把文气磨平。",
        "light_revise": "优先处理报告里的硬警告和高优先提醒，重点改局部句法和对白转轴，不必整章推倒。",
        "targeted_rewrite": "这一章已出现多轴失衡，先按窗口和提醒重写重点段，再回查上游施工图是否诱发了这些问题。",
        "structural_rework": "需要回退到章节结构或 Story 负载层重做。",
        "rollback_to_plan": "当前章问题主要来自上游规划，应先修大纲再修正文。",
    }
    return mapping[recommendation]


def build_scorecard_report(
    draft_path: Path,
    analysis: dict[str, object],
    consistency_snapshot: dict[str, object] | None = None,
    story_trend_snapshot: dict[str, object] | None = None,
) -> str:
    novel_dir = novel_dir_for_draft(draft_path)
    alignment = (
        plan_draft_alignment.build_plan_draft_alignment(draft_path, novel_dir, analysis)
        if novel_dir is not None
        else {"available": False, "reason": "novel_dir_not_resolved"}
    )
    axes = build_axes(analysis, consistency_snapshot, alignment, story_trend_snapshot)
    gate, priority, recommendation = decide_gate(analysis, axes)
    convergence_kinds = list((story_trend_snapshot or {}).get("convergence_kinds", []))
    if gate == "PASS" and convergence_kinds:
        gate, priority, recommendation = "WATCH", "P1", "light_revise"
    elif gate == "WATCH" and len(convergence_kinds) >= 2 and int(analysis["summary"]["warn_sections"]) >= 8:
        gate, priority, recommendation = "FAIL", "P0", "targeted_rewrite"
    bonus_candidates = build_bonus_candidates(analysis) + build_consistency_bonus_candidates(consistency_snapshot or {})
    p1_items = [item for item in analysis["review_reminders"] if item["priority"] == "P1"][:6]
    hard_flags = analysis["hard_flags"][:10]

    lines = [f"# {draft_path.stem} Review Scorecard", ""]
    lines.append(f"- source: `{draft_path}`")
    lines.append(f"- gate: `{gate}`")
    lines.append(f"- priority: `{priority}`")
    lines.append(f"- recommendation: `{recommendation}`")
    lines.append(f"- note: {recommendation_note(recommendation)}")
    lines.append("")

    lines.append("## Axis Scores")
    lines.append("| 维度 | 分数 | 说明 |")
    lines.append("|---|---:|---|")
    for axis in axes:
        lines.append(f"| {axis['name']} | `{axis['score']}` | {axis['reason']} |")
    lines.append("")

    lines.append("## Hard Gates")
    lines.append(f"- warn_sections=`{analysis['summary']['warn_sections']}`")
    lines.append(f"- hard_flags=`{len(analysis['hard_flags'])}`")
    lines.append(f"- P1 reminders=`{sum(1 for item in analysis['review_reminders'] if item['priority'] == 'P1')}`")
    lines.append(f"- fatigue_windows=`{analysis['fatigue_window_count']}`")
    lines.append(f"- tracked_term_windows=`{analysis['tracked_term_window_count']}`")
    lines.append("")

    lines.append("## Bonus Candidates")
    if bonus_candidates:
        for item in bonus_candidates:
            lines.append(f"- `{item['name']}`：{item['reason']}")
    else:
        lines.append("- 暂无明显可直接保留的设计性重复候选；这不代表没有亮点，只代表脚本暂未捕捉到。")
    lines.append("")

    scene_map = analysis.get("scene_map", {})
    dialogue_emotions = analysis.get("dialogue_emotions", {})
    tone_profile = analysis.get("tone_profile", {})
    battle_profile = analysis.get("battle_profile", {})
    viewpoint_profile = analysis.get("viewpoint_profile", {})
    character_voice = analysis.get("character_voice", {})

    lines.append("## Narrative Signals")
    lines.append(
        f"- scene_blocks=`{scene_map.get('block_count', 0)}` dominant_role=`{scene_map.get('dominant_role', 'mixed')}` dominance_ratio=`{scene_map.get('dominance_ratio', 0)}` switches=`{scene_map.get('switch_count', 0)}`"
    )
    lines.append(
        f"- dialogue_emotion=`{dialogue_emotions.get('dominant_emotion', 'neutral')}` ratio=`{dialogue_emotions.get('dominant_ratio', 0)}` shifts=`{dialogue_emotions.get('shift_count', 0)}`"
    )
    lines.append(
        f"- character_voice=`{character_voice.get('dominant_speaker', '') or 'none'}` speakers=`{character_voice.get('speaker_count', 0)}` coverage=`{character_voice.get('coverage_ratio', 0)}` warn=`{character_voice.get('warn', False)}`"
    )
    lines.append(
        f"- tone=`{tone_profile.get('dominant_tone', 'none')}` stable_ratio=`{tone_profile.get('stable_ratio', 0)}` tone_switches=`{tone_profile.get('switch_count', 0)}`"
    )
    lines.append(
        f"- battle_sequences=`{battle_profile.get('sequence_count', 0)}` result_ratio=`{battle_profile.get('result_ratio', 0)}`"
    )
    lines.append(
        f"- viewpoint_anchor=`{viewpoint_profile.get('dominant_anchor', '') or 'none'}` switches=`{viewpoint_profile.get('switch_count', 0)}` overlaps=`{viewpoint_profile.get('overlap_count', 0)}`"
    )
    lines.append(
        f"- ending_signal=`{build_draft_stats.ending_label_display(build_draft_stats.infer_ending_label(analysis))}`"
    )
    if convergence_kinds:
        lines.append("- trend_convergence: " + ", ".join(f"`{name}`" for name in convergence_kinds))
        for note in (story_trend_snapshot or {}).get("notes", [])[:3]:
            lines.append(f"  - `{note}`")
    lines.append("")

    lines.append("## Consistency Snapshot")
    if consistency_snapshot and consistency_snapshot.get("available"):
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
        if consistency_snapshot.get("pending_actions"):
            lines.append("- pending actions:")
            for item in consistency_snapshot["pending_actions"][:2]:
                lines.append(
                    f"  - `{item['category']}` `{item['title']}` confidence=`{item['confidence']}`：{item['focus']}"
                )
                lines.append(f"  - command: `{item['command']}`")
    else:
        lines.append("- 无一致性反馈快照")
    lines.append("")

    lines.append("## Plan Alignment Snapshot")
    if alignment.get("available"):
        lines.append(f"- chapter_plan: `{alignment['chapter_plan_path']}`")
        lines.append(
            f"- chapter_function: plan=`{alignment['plan_chapter_function']}` draft=`{alignment['draft_chapter_function']}` match=`{alignment['chapter_match']}`"
        )
        lines.append(
            f"- ending_function: plan=`{alignment['plan_ending_function']}` draft=`{alignment['draft_ending_function']}` match=`{alignment['ending_match']}`"
        )
        lines.append(f"- mismatch_count=`{alignment['mismatch_count']}`")
        lines.append(f"- alignment_status=`{alignment.get('alignment_status', 'unknown')}`")
        lines.append(f"- recommended_action=`{alignment.get('recommended_action', 'manual_review')}`")
        if alignment.get("drift_types"):
            lines.append("- drift_types: " + ", ".join(f"`{name}`" for name in alignment.get("drift_types", [])))
        if alignment.get("review_note"):
            lines.append(f"- note: {alignment['review_note']}")
    else:
        lines.append("- 无 plan-draft 对齐快照")
    lines.append("")

    lines.append("## Priority Fixes")
    if p1_items:
        for item in p1_items:
            lines.append(
                f"- `{item['category']}` {item['title']}：{item['reason']} 检查：{item['check']} 动作：{item['action']}"
            )
    else:
        lines.append("- 无 P1 项")
    lines.append("")

    lines.append("## Top Hard Flags")
    if hard_flags:
        for item in hard_flags:
            line = f"- `{item['section']}` `{item['name']}` x{item['count']}：{item['note']}"
            if item["sample"]:
                line += f" 样例：{item['sample']}"
            lines.append(line)
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Follow-up")
    lines.append("- 如果是 `WATCH` 或 `FAIL`，先读对应 `profiles/` 目录里的句式画像，再决定是删词、拆句还是重写段落。")
    lines.append("- 如果同一角色、地点、装备或称谓在这章显得摇摆，先跑本章快照里的 `review_queue`，再决定是否补 `feedback-add`。")
    lines.append("- 如果 `recommendation` 已接近 `targeted_rewrite`，先回查 `chapter-plan` 和 `story-plan`，不要只在正文层补丁。")
    if convergence_kinds:
        lines.append("- 如果本章处在跨章合流里，优先确认它是不是在重复同一种章末温度，而不是只修单章字词。")
    lines.append("")

    return "\n".join(lines) + "\n"


def build_story_summary(
    story_dir: Path,
    scorecards: list[tuple[Path, dict[str, object]]],
    snapshots: dict[Path, dict[str, object]] | None = None,
) -> str:
    ordered = sorted(scorecards, key=lambda item: chapter_sort_key(item[0]))
    story_trend_snapshots = build_story_trend_snapshots(ordered)
    gate_counter: collections.Counter[str] = collections.Counter()
    recommendation_counter: collections.Counter[str] = collections.Counter()
    axis_totals: collections.Counter[str] = collections.Counter()
    feedback_counter: collections.Counter[str] = collections.Counter()
    feedback_facets: collections.Counter[str] = collections.Counter()
    alignment_counter: collections.Counter[str] = collections.Counter()
    ending_alignment_counter: collections.Counter[str] = collections.Counter()
    ending_signal_counter: collections.Counter[str] = collections.Counter()
    ending_signal_flow: list[str] = []
    story_backlog: list[dict[str, str]] = []

    lines = ["# Review Scorecard Summary", ""]
    lines.append(f"- story: `{story_dir}`")
    lines.append(f"- chapters: `{len(ordered)}`")
    lines.append("")

    lines.append("## Chapters")
    for path, analysis in ordered:
        snapshot = snapshots.get(path, {}) if snapshots else {}
        novel_dir = novel_dir_for_draft(path)
        alignment = (
            plan_draft_alignment.build_plan_draft_alignment(path, novel_dir, analysis)
            if novel_dir is not None
            else {"available": False}
        )
        trend_snapshot = story_trend_snapshots.get(path)
        axes = build_axes(analysis, snapshot, alignment, trend_snapshot)
        gate, priority, recommendation = decide_gate(analysis, axes)
        convergence_kinds = list((trend_snapshot or {}).get("convergence_kinds", []))
        if gate == "PASS" and convergence_kinds:
            gate, priority, recommendation = "WATCH", "P1", "light_revise"
        elif gate == "WATCH" and len(convergence_kinds) >= 2 and int(analysis["summary"]["warn_sections"]) >= 8:
            gate, priority, recommendation = "FAIL", "P0", "targeted_rewrite"
        gate_counter[gate] += 1
        recommendation_counter[recommendation] += 1
        for axis in axes:
            axis_totals[str(axis["name"])] += int(axis["score"])
        if snapshot and snapshot.get("available"):
            for decision, count in snapshot["decision_counter"].items():
                feedback_counter[decision] += int(count)
            for facet, count in snapshot["facet_counter"].items():
                feedback_facets[facet] += int(count)
            if not story_backlog:
                story_backlog = list(snapshot.get("global_feedback_backlog", []))
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
        lines.append(
            f"- `{path.name}` gate=`{gate}` priority=`{priority}` recommendation=`{recommendation}` warn_sections=`{analysis['summary']['warn_sections']}` hard_flags=`{len(analysis['hard_flags'])}` ending_signal=`{build_draft_stats.ending_label_display(ending_signal)}`"
        )
        if trend_snapshot and trend_snapshot.get("notes"):
            lines.append(f"  - convergence=`{' | '.join(trend_snapshot['notes'][:2])}`")
    lines.append("")

    lines.append("## Gate Distribution")
    for name, count in gate_counter.items():
        lines.append(f"- `{name}` x{count}")
    lines.append("")

    lines.append("## Recommendation Distribution")
    for name, count in recommendation_counter.items():
        lines.append(f"- `{name}` x{count}")
    lines.append("")

    lines.append("## Average Axis Scores")
    for name, total in axis_totals.items():
        avg = round(total / max(len(ordered), 1), 2)
        lines.append(f"- `{name}` avg=`{avg}`")
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
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Consistency Feedback")
    if feedback_counter:
        for decision in ("confirmed", "false_positive", "designed_keep", "watch"):
            lines.append(f"- `{decision}` x{feedback_counter.get(decision, 0)}")
        if feedback_facets:
            lines.append("- facets:")
            for name, count in feedback_facets.most_common(8):
                lines.append(f"  - `{name}` x{count}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Consistency Backlog")
    if story_backlog:
        for item in story_backlog[:6]:
            lines.append(f"- `{item['target']}` {item['reason']}")
    else:
        lines.append("- 无")
    lines.append("")
    return "\n".join(lines) + "\n"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Build draft review scorecards.")
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

    grouped: dict[Path, list[tuple[Path, dict[str, object]]]] = collections.defaultdict(list)
    for path, analysis in analyses:
        grouped[path.parent].append((path, analysis))
    for story_dir, items in sorted(grouped.items()):
        story_trend_snapshots = build_story_trend_snapshots(items)
        for draft_path, analysis in items:
            out_path = scorecard_path_for(draft_path)
            write_text(
                out_path,
                build_scorecard_report(
                    draft_path,
                    analysis,
                    snapshots.get(draft_path),
                    story_trend_snapshots.get(draft_path),
                ),
            )
            print(out_path)
        summary_path = scorecard_path_for(items[0][0]).parent / "SUMMARY.md"
        write_text(summary_path, build_story_summary(story_dir, items, snapshots))
        print(summary_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
