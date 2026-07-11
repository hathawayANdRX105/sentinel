#!/usr/bin/env python3
"""Shared helpers for chapter-plan and draft alignment signals."""

from __future__ import annotations

import collections
from pathlib import Path

from sentinel.audit import plan as plan_audit


DRAFT_FUNCTION_RULES = {
    "conflict": ("枪", "火力", "埋伏", "子弹", "追", "拦", "伤", "血", "打", "炸"),
    "investigation": ("线索", "证据", "坐标", "记录", "名单", "异常", "确认", "查", "归档"),
    "relationship": ("对视", "沉默", "笑", "嘴硬", "护住", "搭档", "信任", "回嘴", "安慰"),
    "procedure": ("安检", "排队", "窗口", "手续", "通道", "账单", "登记", "权限"),
    "exposition": ("解释", "说明", "分析", "知道", "明白", "讨论", "复盘", "判断"),
    "movement": ("出城", "回城", "进城", "上车", "下车", "抵达", "离开", "赶到", "入口"),
}

DRAFT_ENDING_RULES = {
    "action_aftershock": ("撤", "压上", "挡在", "补位", "继续追", "转移", "收拢"),
    "new_info": ("线索", "名单", "坐标", "名字", "短码", "短讯", "消息", "记录"),
    "external_threat": ("异响", "危险", "热源", "追兵", "枪火", "火力", "报警", "封锁"),
    "relationship_turn": ("沉默", "看了她一眼", "护住", "松口", "翻脸", "笑了笑", "回嘴"),
    "procedure_pressure": ("安检", "窗口", "账单", "手续", "权限", "通报", "资格", "登记"),
    "foreshadow_flash": ("不该", "熟悉", "异常", "多了一处", "旧标记", "像是", "闪了一下"),
    "self_realization": ("意识到", "明白", "知道", "想起", "终于懂"),
}


def count_rule_hits(text: str, rules: dict[str, tuple[str, ...]]) -> collections.Counter[str]:
    counter: collections.Counter[str] = collections.Counter()
    for label, terms in rules.items():
        counter[label] = sum(text.count(term) for term in terms)
    return counter


def pick_top_label(counter: collections.Counter[str], default: str = "unclear") -> str:
    positive = [(name, count) for name, count in counter.items() if count > 0]
    if not positive:
        return default
    positive.sort(key=lambda item: (-item[1], item[0]))
    return positive[0][0]


def chapter_plan_path_for_draft(draft_path: Path, novel_dir: Path) -> Path | None:
    try:
        relative = draft_path.relative_to(novel_dir / "drafts")
    except ValueError:
        return None
    parts = relative.parts
    if len(parts) < 3:
        return None
    arc = parts[0]
    story = parts[1]
    chapter = draft_path.name
    if story == "story1":
        return novel_dir / "chapter-plan" / arc / chapter
    return novel_dir / "chapter-plan" / arc / f"{story}-{chapter}"


def collect_plan_signals(chapter_plan_path: Path) -> dict[str, str]:
    if not chapter_plan_path.exists():
        return {"available": False, "chapter_function": "missing", "ending_function": "missing"}
    text = chapter_plan_path.read_text(encoding="utf-8")
    lines = text.splitlines()
    headings = plan_audit.parse_headings(lines)
    sections = plan_audit.collect_section_lines(lines, headings)
    _function_name, function_section = plan_audit.find_section(sections, ("本章功能",))
    function_text = " ".join(
        text for _line_no, text in plan_audit.bullet_lines(function_section)
    ) or plan_audit.section_text(function_section)
    _ending_name, ending_section = plan_audit.find_section(sections, plan_audit.CHAPTER_ENDING_GROUP)
    ending_text = " ".join(
        text for _line_no, text in plan_audit.bullet_lines(ending_section)
    ) or plan_audit.section_text(ending_section)
    return {
        "available": True,
        "chapter_function": plan_audit.detect_function_label(function_text, plan_audit.CHAPTER_FUNCTION_RULES),
        "ending_function": plan_audit.detect_function_label(ending_text, plan_audit.ENDING_FUNCTION_RULES),
        "path": str(chapter_plan_path),
    }


def infer_draft_signals(draft_path: Path, analysis: dict[str, object]) -> dict[str, object]:
    text = draft_path.read_text(encoding="utf-8")
    function_counter = count_rule_hits(text, DRAFT_FUNCTION_RULES)
    ending_counter = count_rule_hits(str(analysis["ending"]["tail_excerpt"]), DRAFT_ENDING_RULES)
    scene_role = str(analysis["scene_map"]["dominant_role"])
    if scene_role == "battle":
        function_counter["conflict"] += 3
    elif scene_role == "dialogue":
        function_counter["relationship"] += 2
    elif scene_role == "environment":
        function_counter["movement"] += 1
        function_counter["procedure"] += 1
    elif scene_role == "action":
        function_counter["movement"] += 2
    elif scene_role == "mixed":
        function_counter["exposition"] += 1
    if analysis["battle_profile"]["sequence_count"] >= 1:
        function_counter["conflict"] += int(analysis["battle_profile"]["sequence_count"])
    if analysis["dialogue_emotions"]["shift_count"] >= 1:
        function_counter["relationship"] += 1
    if analysis["tracked_term_window_count"] >= 1 or any(
        term["name"] in {"线索面板词", "Pi竖线状态栏"} for term in analysis["patterns"] if term.get("count", 0) > 0
    ):
        function_counter["investigation"] += 1
    if analysis["ending"]["warn"]:
        ending_counter["foreshadow_flash"] += 1
    if analysis["dialogue_emotions"]["shift_count"] >= 1:
        ending_counter["relationship_turn"] += 1
    if analysis["battle_profile"]["sequence_count"] >= 1:
        ending_counter["action_aftershock"] += 1
        ending_counter["external_threat"] += 1
    return {
        "chapter_function": pick_top_label(function_counter),
        "ending_function": pick_top_label(ending_counter),
        "chapter_counter": function_counter,
        "ending_counter": ending_counter,
    }



def classify_alignment_status(
    plan_chapter: str,
    draft_chapter: str,
    plan_ending: str,
    draft_ending: str,
    chapter_match: bool,
    ending_match: bool,
) -> dict[str, object]:
    """Classify whether mismatch should revise draft, update plan, or both."""
    plan_unknown = {"missing", "unclear"}
    draft_unknown = {"missing", "unclear"}
    mismatches: list[str] = []
    if not chapter_match:
        mismatches.append("chapter_function")
    if not ending_match:
        mismatches.append("ending_function")

    if not mismatches:
        return {
            "alignment_status": "implemented",
            "recommended_action": "retain_alignment",
            "drift_types": [],
            "review_note": "正文功能和章末收束都已接住施工图，优先只做正文局部润色。",
        }

    if (plan_chapter in plan_unknown or plan_ending in plan_unknown) and (
        draft_chapter not in draft_unknown or draft_ending not in draft_unknown
    ):
        return {
            "alignment_status": "plan_needs_update",
            "recommended_action": "update_chapter_plan",
            "drift_types": mismatches,
            "review_note": "施工图功能含糊或缺失，而正文已有较明确落点；优先判断正文新增是否有效，有效则回修 chapter-plan。",
        }

    if (draft_chapter in draft_unknown or draft_ending in draft_unknown) and (
        plan_chapter not in plan_unknown or plan_ending not in plan_unknown
    ):
        return {
            "alignment_status": "missing_anchor",
            "recommended_action": "revise_draft",
            "drift_types": mismatches,
            "review_note": "施工图有明确功能，但正文没有接出足够清晰的功能锚点；优先回修正文落点。",
        }

    if len(mismatches) == 1:
        return {
            "alignment_status": "weak_implementation",
            "recommended_action": "revise_draft_or_plan_section",
            "drift_types": mismatches,
            "review_note": "正文只接住一部分施工图；先看偏移处是否更好，再决定改正文还是局部回修 plan。",
        }

    return {
        "alignment_status": "draft_drift",
        "recommended_action": "compare_plan_and_draft",
        "drift_types": mismatches,
        "review_note": "正文主功能和章末收束都偏离施工图；不要只修句子，先重判本章结构目标。",
    }


def build_plan_draft_alignment(
    draft_path: Path,
    novel_dir: Path,
    analysis: dict[str, object],
) -> dict[str, object]:
    chapter_plan_path = chapter_plan_path_for_draft(draft_path, novel_dir)
    if chapter_plan_path is None:
        return {"available": False, "reason": "chapter_plan_not_resolved"}
    plan_signals = collect_plan_signals(chapter_plan_path)
    draft_signals = infer_draft_signals(draft_path, analysis)
    chapter_match = plan_signals["chapter_function"] == draft_signals["chapter_function"]
    ending_match = plan_signals["ending_function"] == draft_signals["ending_function"]
    status = classify_alignment_status(
        plan_signals["chapter_function"],
        draft_signals["chapter_function"],
        plan_signals["ending_function"],
        draft_signals["ending_function"],
        chapter_match,
        ending_match,
    )
    return {
        "available": bool(plan_signals.get("available")),
        "chapter_plan_path": chapter_plan_path,
        "plan_chapter_function": plan_signals["chapter_function"],
        "draft_chapter_function": draft_signals["chapter_function"],
        "plan_ending_function": plan_signals["ending_function"],
        "draft_ending_function": draft_signals["ending_function"],
        "chapter_match": chapter_match,
        "ending_match": ending_match,
        "mismatch_count": int(not chapter_match) + int(not ending_match),
        **status,
        "chapter_counter": draft_signals["chapter_counter"],
        "ending_counter": draft_signals["ending_counter"],
    }
