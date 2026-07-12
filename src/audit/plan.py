#!/usr/bin/env python3
"""Audit arc/story/chapter plan files for structure drift and field misuse."""

from __future__ import annotations

import argparse
import json
import re
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

from lib import rules
from lib.cli import resolve_inputs
from lib.io import write_text


PLAN_TYPES = ("arc-plan", "story-plan", "chapter-plan")
HEADING_RE = re.compile(r"^(#{1,6})\s+(.+?)\s*$")
LABEL_RE = re.compile(r"^\s*-\s*([^：:]+)[：:]\s*(.*)$")
LIST_RE = re.compile(r"^\s*(?:-\s+|\d+\.\s+|\*\s+)(.+)$")
SCENE_TITLE_RE = re.compile(r"^Scene\s+\d+")

RULES = rules.load_rules()
PLAN_RULES = rules.mapping_at(RULES, "plan")


def _term_set(section: str) -> set[str]:
    return set(rules.tuple_list(rules.list_at(PLAN_RULES, section)))


def _term_tuple(section: str) -> tuple[str, ...]:
    return rules.tuple_list(rules.list_at(PLAN_RULES, section))


def _compiled_regex(section: str) -> re.Pattern[str]:
    raw = rules.mapping_at(PLAN_RULES, "regex", section)
    flags = re.IGNORECASE if raw.get("ignore_case") else 0
    return re.compile(str(raw["pattern"]), flags)


REQUIRED_HEADING_GROUPS = rules.heading_groups(rules.mapping_at(PLAN_RULES, "required_headings"))
SCENE_FIELD_GROUPS = [rules.tuple_list(group) for group in rules.list_at(PLAN_RULES, "scene_field_groups")]
CHAPTER_ENDING_GROUP = _term_tuple("chapter_ending_group")
PLAN_SECTIONS = rules.mapping_at(PLAN_RULES, "sections")
PLAN_THRESHOLDS = rules.mapping_at(PLAN_RULES, "thresholds")
STORY_LAYOUT_SECTION = str(PLAN_SECTIONS["story_layout"])
FORESHADOW_TABLE_SECTION = str(PLAN_SECTIONS["foreshadow_table"])
STORY_EVENTS_SECTION = str(PLAN_SECTIONS["story_events"])
STORY_LOADS_SECTION = str(PLAN_SECTIONS["story_loads"])
STORY_ROLES_SECTION = str(PLAN_SECTIONS["story_roles"])
STORY_ENVIRONMENT_SECTIONS = set(rules.tuple_list(PLAN_SECTIONS["story_environment"]))
CHAPTER_FUNCTION_SECTION = str(PLAN_SECTIONS["chapter_function"])
LOOKPOINT_SECTION = str(PLAN_SECTIONS["lookpoint"])
RHYTHM_SECTION = str(PLAN_SECTIONS["rhythm"])
STATE_CHANGE_SECTION = str(PLAN_SECTIONS["state_change"])
SCENE_FUNCTION_FIELDS = rules.tuple_list(PLAN_SECTIONS["scene_function_fields"])
STORY_LAYOUT_MIN_ITEMS = int(PLAN_THRESHOLDS["story_layout_min_items"])
FORESHADOW_MIN_IDS = int(PLAN_THRESHOLDS["foreshadow_min_ids"])
STORY_EVENTS_MIN_ITEMS = int(PLAN_THRESHOLDS["story_events_min_items"])
LOOKPOINT_SHORT_MAX_CHARS = int(PLAN_THRESHOLDS["lookpoint_short_max_chars"])
SCENE_BODY_MAX_CHARS = int(PLAN_THRESHOLDS["scene_body_max_chars"])
SCENE_PROSE_MARK_MIN = int(PLAN_THRESHOLDS["scene_prose_mark_min"])
SCENE_DIALOGUE_PROSE_MARK_MIN = int(PLAN_THRESHOLDS["scene_dialogue_prose_mark_min"])
THIN_CHANGE_MAX_CHARS = int(PLAN_THRESHOLDS["thin_change_max_chars"])
SCENE_MONOTONY_MIN_SCENES = int(PLAN_THRESHOLDS["scene_monotony_min_scenes"])
SCENE_MONOTONY_MAX_MISSING = int(PLAN_THRESHOLDS["scene_monotony_max_missing"])


STYLE_LEAK_RE = _compiled_regex("style_leak")
ABSTRACT_LOOKPOINT_RE = _compiled_regex("abstract_lookpoint")
ANSWER_LEAK_RE = _compiled_regex("answer_leak")
SCENE_LEAK_RE = _compiled_regex("scene_leak")
PROSE_LEAK_RE = _compiled_regex("prose_leak")
JUDGEMENT_RE = _compiled_regex("judgement")

HOOKISH_RE = _compiled_regex("hookish")
FORESHADOW_ID_RE = _compiled_regex("foreshadow_id")
RHYTHM_STYLE_RE = _compiled_regex("rhythm_style_leak")
STORY_PROSE_LEAK_RE = _compiled_regex("story_prose_leak")


LOOKPOINT_WEAK_TERMS = _term_set("lookpoint_weak_terms")
LOOKPOINT_STRONG_TERMS = _term_set("lookpoint_strong_terms")
ENDING_WEAK_TERMS = _term_tuple("ending_weak_terms")
ENVIRONMENT_PRESSURE_TERMS = _term_set("environment_pressure_terms")
ENVIRONMENT_WITNESS_TERMS = _term_set("environment_witness_terms")
GENERIC_PROGRESS_TERMS = _term_set("generic_progress_terms")
ROLE_FUNCTION_TERMS = _term_set("role_function_terms")

FUNCTION_RULES = rules.mapping_at(PLAN_RULES, "function_rules")
CHAPTER_FUNCTION_RULES = rules.tuple_map(rules.mapping_at(FUNCTION_RULES, "chapter"))
ENDING_FUNCTION_RULES = rules.tuple_map(rules.mapping_at(FUNCTION_RULES, "ending"))
SCENE_FUNCTION_RULES = rules.tuple_map(rules.mapping_at(FUNCTION_RULES, "scene"))


@dataclass
class Warning:
    line_no: int
    kind: str
    message: str
    snippet: str


def detect_function_label(text: str, rules: dict[str, tuple[str, ...]], default: str = "unclear") -> str:
    hits: list[tuple[str, int]] = []
    for label, terms in rules.items():
        count = sum(text.count(term) for term in terms)
        if count > 0:
            hits.append((label, count))
    if not hits:
        return default
    hits.sort(key=lambda item: (-item[1], item[0]))
    return hits[0][0]


def detect_plan_type(path: Path, text: str) -> str:
    joined = str(path)
    for plan_type in PLAN_TYPES:
        if plan_type in joined:
            return plan_type
    if f"## {CHAPTER_FUNCTION_SECTION}" in text or "Scene 1" in text:
        return "chapter-plan"
    if f"## {STORY_EVENTS_SECTION}" in text and f"## {STORY_LOADS_SECTION}" in text:
        return "story-plan"
    return "arc-plan"


def normalize_heading(title: str) -> str:
    return title.replace("：", "").replace(":", "").strip()


def parse_headings(lines: list[str]) -> list[tuple[int, str, int]]:
    headings: list[tuple[int, str, int]] = []
    for line_no, line in enumerate(lines, start=1):
        match = HEADING_RE.match(line)
        if match:
            level = len(match.group(1))
            title = normalize_heading(match.group(2))
            headings.append((line_no, title, level))
    return headings


def heading_present(headings: list[tuple[int, str, int]], choices: tuple[str, ...]) -> bool:
    return any(title in choices for _, title, _ in headings)


def first_heading_name(choices: tuple[str, ...]) -> str:
    return choices[0]


def collect_section_lines(lines: list[str], headings: list[tuple[int, str, int]]) -> dict[str, list[tuple[int, str]]]:
    sections: dict[str, list[tuple[int, str]]] = {}
    boundaries = headings + [(len(lines) + 1, "__END__", 0)]
    for idx, (line_no, title, _level) in enumerate(headings):
        next_line_no = boundaries[idx + 1][0]
        body: list[tuple[int, str]] = []
        for body_line_no in range(line_no + 1, next_line_no):
            body.append((body_line_no, lines[body_line_no - 1]))
        sections[title] = body
    return sections


def section_text(section: list[tuple[int, str]]) -> str:
    return "\n".join(line for _, line in section)


def find_section(sections: dict[str, list[tuple[int, str]]], choices: tuple[str, ...]) -> tuple[str | None, list[tuple[int, str]]]:
    for name in choices:
        if name in sections:
            return name, sections[name]
    return None, []


def find_label_blocks(section: list[tuple[int, str]]) -> dict[str, list[tuple[int, str]]]:
    blocks: dict[str, list[tuple[int, str]]] = {}
    current_label: str | None = None
    for line_no, line in section:
        match = LABEL_RE.match(line)
        if match:
            current_label = match.group(1).strip()
            content = match.group(2).strip()
            blocks.setdefault(current_label, [])
            if content:
                blocks[current_label].append((line_no, content))
            continue
        if current_label and line.strip().startswith("- "):
            blocks[current_label].append((line_no, line.strip()[2:].strip()))
    return blocks


def bullet_lines(section: list[tuple[int, str]]) -> list[tuple[int, str]]:
    items: list[tuple[int, str]] = []
    for line_no, line in section:
        match = LIST_RE.match(line.strip())
        if match:
            items.append((line_no, match.group(1).strip()))
    return items


def contains_any(text: str, terms: set[str]) -> bool:
    return any(term in text for term in terms)


def dominant_generic_terms(items: list[tuple[int, str]]) -> list[tuple[str, int]]:
    counts: dict[str, int] = {}
    for _line_no, text in items:
        for term in GENERIC_PROGRESS_TERMS:
            if term in text:
                counts[term] = counts.get(term, 0) + 1
    return sorted(counts.items(), key=lambda item: (-item[1], item[0]))


def audit_required(plan_type: str, headings: list[tuple[int, str, int]]) -> list[Warning]:
    warnings: list[Warning] = []
    for group in REQUIRED_HEADING_GROUPS.get(plan_type, []):
        if not heading_present(headings, group):
            warnings.append(Warning(0, "missing_heading", f"缺少必备标题：{first_heading_name(group)}", ""))
    return warnings


def audit_arc_plan(sections: dict[str, list[tuple[int, str]]]) -> list[Warning]:
    warnings: list[Warning] = []
    _, story_layout = find_section(sections, (STORY_LAYOUT_SECTION,))
    story_bullets = bullet_lines(story_layout)
    story_table_rows = [line for _, line in story_layout if line.strip().startswith("|")]
    if len(story_bullets) + len(story_table_rows) < STORY_LAYOUT_MIN_ITEMS:
        warnings.append(Warning(0, "thin_story_layout", "Story 排布条目过少，Arc 推进骨架偏薄", ""))
    for line_no, line in story_bullets:
        if SCENE_LEAK_RE.search(line):
            warnings.append(Warning(line_no, "layer_drift", "Arc 层不应写 Scene/章节拆分", line))

    _, foil_table = find_section(sections, (FORESHADOW_TABLE_SECTION,))
    foil_text = section_text(foil_table)
    if len(FORESHADOW_ID_RE.findall(foil_text)) < FORESHADOW_MIN_IDS:
        warnings.append(Warning(0, "foreshadow_sparse", "伏笔表里的编号偏少，建议统一编号并补齐", ""))
    return warnings


def audit_story_plan(sections: dict[str, list[tuple[int, str]]]) -> list[Warning]:
    warnings: list[Warning] = []
    _, events_section = find_section(sections, (STORY_EVENTS_SECTION,))
    events = bullet_lines(events_section)
    if len(events) < STORY_EVENTS_MIN_ITEMS:
        warnings.append(Warning(0, "thin_events", "核心事件少于 3 条，推进骨架偏薄", ""))
    generic_events = dominant_generic_terms(events)
    if generic_events and generic_events[0][1] >= max(2, len(events) - 1):
        warnings.append(
            Warning(
                0,
                "event_monotony",
                "核心事件过度依赖同一类推进动词，后续章节容易全部写成‘继续调查/继续解释’",
                f"{generic_events[0][0]} x{generic_events[0][1]}",
            )
        )

    _, loads_section = find_section(sections, (STORY_LOADS_SECTION,))
    load_lines = bullet_lines(loads_section)
    if not load_lines:
        warnings.append(Warning(0, "missing_loads", "粗章节负载没有明确章序或分段", ""))
    for line_no, line in load_lines:
        if SCENE_LEAK_RE.search(line):
            warnings.append(Warning(line_no, "scene_leak", "Story 层不应直接拆 Scene", line))
        if line.count("：") + line.count(":") > 1:
            warnings.append(Warning(line_no, "over_detailed_load", "粗章节负载像在偷写章节施工图", line))

    _, role_section = find_section(sections, (STORY_ROLES_SECTION,))
    role_lines = bullet_lines(role_section)
    if len(role_lines) < 2:
        warnings.append(Warning(0, "thin_roles", "主要角色与功能过少，角色承载面不清", ""))
    elif not any(contains_any(text, ROLE_FUNCTION_TERMS) for _line_no, text in role_lines):
        warnings.append(
            Warning(
                0,
                "thin_role_functions",
                "主要角色与功能更像点名名单，没有明确写出谁在推动、阻拦、见证或施压",
                "",
            )
        )

    story_text = "\n".join(
        section_text(section)
        for name, section in sections.items()
        if name in STORY_ENVIRONMENT_SECTIONS
    )
    if contains_any(story_text, ENVIRONMENT_PRESSURE_TERMS) and not contains_any(story_text, ENVIRONMENT_WITNESS_TERMS):
        warnings.append(
            Warning(
                0,
                "missing_environment_witness",
                "Story 在讨论制度/阶级/城市压力，但没有明确安排配角、小事故、手续或环境证词来承接",
                "",
            )
        )
    return warnings


def audit_lookpoint(line_no: int, items: list[tuple[int, str]]) -> list[Warning]:
    warnings: list[Warning] = []
    values = [text for _, text in items if text]
    if not values:
        warnings.append(Warning(line_no, "thin_lookpoint", "看点为空，只有字段名没有戏", "看点"))
        return warnings
    joined = " ".join(values)
    weak_hits = [term for term in LOOKPOINT_WEAK_TERMS if term in joined]
    strong_hits = [term for term in LOOKPOINT_STRONG_TERMS if term in joined]
    if weak_hits and not strong_hits:
        warnings.append(Warning(line_no, "abstract_lookpoint", "看点更像气氛词或人物态度，缺少可拍出来的戏", joined))
    if len(values) == 1 and len(values[0]) <= LOOKPOINT_SHORT_MAX_CHARS:
        warnings.append(Warning(line_no, "thin_lookpoint", "看点过短，像标签不是内容", values[0]))
    return warnings


def audit_chapter_plan(sections: dict[str, list[tuple[int, str]]], headings: list[tuple[int, str, int]]) -> list[Warning]:
    warnings: list[Warning] = []
    scene_titles = [(line_no, title) for line_no, title, _ in headings if SCENE_TITLE_RE.match(title)]
    chapter_function_name, chapter_function_section = find_section(sections, (CHAPTER_FUNCTION_SECTION,))
    chapter_function_label = "unclear"
    if not chapter_function_name:
        warnings.append(Warning(0, "missing_chapter_function", "缺少本章功能，施工图不知道这一章到底主推进什么", ""))
    else:
        function_items = bullet_lines(chapter_function_section)
        function_text = " ".join(text for _line_no, text in function_items) or section_text(chapter_function_section)
        if len(function_text.strip()) < 8:
            warnings.append(Warning(0, "thin_chapter_function", "本章功能过短，像标签不是施工指令", function_text.strip()))
        chapter_function_label = detect_function_label(function_text, CHAPTER_FUNCTION_RULES)
        if chapter_function_label == "unclear":
            warnings.append(
                Warning(
                    0,
                    "unclear_chapter_function",
                    "本章功能没有落到冲突/调查/关系/手续/转场等主功能，后续容易一章承担过多杂事",
                    function_text.strip()[:120],
                )
            )
    if not scene_titles:
        warnings.append(Warning(0, "missing_scene", "章节规划没有 Scene 拆分", ""))
        return warnings

    scene_function_counts: dict[str, int] = {}
    for scene_line_no, scene_title in scene_titles:
        body = sections.get(scene_title, [])
        labels = find_label_blocks(body)
        body_text = section_text(body)
        for group in SCENE_FIELD_GROUPS:
            if not any(choice in labels for choice in group):
                warnings.append(Warning(scene_line_no, "scene_field", f"{scene_title} 缺少字段：{first_heading_name(group)}", scene_title))
        if len(body_text) > SCENE_BODY_MAX_CHARS:
            warnings.append(Warning(scene_line_no, "scene_overwrite", f"{scene_title} 内容过长，疑似写成半正文", scene_title))
        prose_marks = body_text.count("。") + body_text.count("！") + body_text.count("？")
        if prose_marks >= SCENE_PROSE_MARK_MIN:
            warnings.append(Warning(scene_line_no, "scene_prose_density", f"{scene_title} 句号/感叹/问号过多，像半正文", scene_title))
        if PROSE_LEAK_RE.search(body_text) and prose_marks >= SCENE_DIALOGUE_PROSE_MARK_MIN:
            warnings.append(Warning(scene_line_no, "scene_dialogue_leak", f"{scene_title} 混入成句对白或正文化句子", scene_title))

        lookpoint_items = labels.get(LOOKPOINT_SECTION, [])
        warnings.extend(audit_lookpoint(scene_line_no, lookpoint_items))

        if RHYTHM_SECTION in labels:
            joined_rhythm = " ".join(text for _, text in labels[RHYTHM_SECTION])
            if RHYTHM_STYLE_RE.search(joined_rhythm):
                warnings.append(Warning(scene_line_no, "rhythm_style_leak", "节奏字段混入写法要求", joined_rhythm))

        if STATE_CHANGE_SECTION in labels:
            joined_change = " ".join(text for _, text in labels[STATE_CHANGE_SECTION])
            if JUDGEMENT_RE.search(joined_change) and len(joined_change) <= THIN_CHANGE_MAX_CHARS:
                warnings.append(Warning(scene_line_no, "thin_change", "状态变化太抽象，缺少局势或关系上的具体变化", joined_change))

        scene_function_text = " ".join(
            text
            for field in SCENE_FUNCTION_FIELDS
            for _line_no, text in labels.get(field, [])
        )
        scene_function = detect_function_label(scene_function_text, SCENE_FUNCTION_RULES)
        scene_function_counts[scene_function] = scene_function_counts.get(scene_function, 0) + 1

    generic_scene_terms: dict[str, list[str]] = {}
    environment_scene_hits = 0
    witness_scene_hits = 0
    for _scene_line_no, scene_title in scene_titles:
        body = sections.get(scene_title, [])
        labels = find_label_blocks(body)
        joined_scene = " ".join(
            text
            for field in SCENE_FUNCTION_FIELDS
            for _line_no, text in labels.get(field, [])
        )
        for term in GENERIC_PROGRESS_TERMS:
            if term in joined_scene:
                generic_scene_terms.setdefault(term, []).append(scene_title)
        if contains_any(joined_scene, ENVIRONMENT_PRESSURE_TERMS):
            environment_scene_hits += 1
        if contains_any(joined_scene, ENVIRONMENT_WITNESS_TERMS):
            witness_scene_hits += 1

    for term, titles in sorted(generic_scene_terms.items()):
        if len(titles) >= 2:
            warnings.append(
                Warning(
                    0,
                    "repeated_scene_function",
                    f"多个 Scene 都在围绕“{term}”推进，施工图可能过于单调",
                    " / ".join(titles[:4]),
                )
            )
            break

    effective_scene_counts = {name: count for name, count in scene_function_counts.items() if name != "unclear"}
    if len(scene_titles) >= SCENE_MONOTONY_MIN_SCENES and effective_scene_counts:
        dominant_scene_function, dominant_scene_count = sorted(
            effective_scene_counts.items(),
            key=lambda item: (-item[1], item[0]),
        )[0]
        if dominant_scene_count >= max(2, len(scene_titles) - SCENE_MONOTONY_MAX_MISSING):
            warnings.append(
                Warning(
                    0,
                    "scene_function_monotony",
                    f"多个 Scene 都在承担 `{dominant_scene_function}` 功能，章节施工图的功能切换偏少",
                    f"{dominant_scene_function} x{dominant_scene_count}/{len(scene_titles)}",
                )
            )

    if environment_scene_hits >= 1 and witness_scene_hits == 0:
        warnings.append(
            Warning(
                0,
                "missing_environment_witness",
                "本章想承载城市/制度压力，但 Scene 里看不见配角、手续、交易、窗口或环境证词",
                "",
            )
        )

    ending_name, ending_section = find_section(sections, CHAPTER_ENDING_GROUP)
    if not ending_name:
        warnings.append(Warning(0, "missing_ending", "缺少章节收尾或章节钩子", ""))
    else:
        ending_items = bullet_lines(ending_section)
        if not ending_items:
            warnings.append(Warning(0, "thin_ending", f"{ending_name} 为空", ending_name))
            ending_function_label = "unclear"
        for line_no, line in ending_items:
            if HOOKISH_RE.search(line):
                warnings.append(Warning(line_no, "hook_meta", "收尾字段里写了元话术，不是具体收尾动作", line))
            if any(term in line for term in ENDING_WEAK_TERMS):
                warnings.append(Warning(line_no, "template_ending", "收尾疑似落回主角总结或城市意象模板", line))
        ending_text = " ".join(text for _line_no, text in ending_items)
        ending_function_label = detect_function_label(ending_text, ENDING_FUNCTION_RULES)
        if ending_text and ending_function_label == "unclear":
            warnings.append(
                Warning(
                    0,
                    "unclear_ending_function",
                    "章节收尾没有落到新信息/外部威胁/关系转向/手续压力等功能类型，钩子作用不清",
                    ending_text[:120],
                )
            )
        if chapter_function_label != "unclear" and ending_function_label == chapter_function_label and len(scene_titles) >= 3:
            warnings.append(
                Warning(
                    0,
                    "flat_chapter_curve",
                    "本章功能和收尾功能落在同一类型，可能从头到尾都在做同一种事，缺少章末转向",
                    f"chapter={chapter_function_label} ending={ending_function_label}",
                )
            )
    return warnings


def audit_content(plan_type: str, sections: dict[str, list[tuple[int, str]]]) -> list[Warning]:
    warnings: list[Warning] = []
    for section_title, body in sections.items():
        for line_no, line in body:
            stripped = line.strip()
            if not stripped:
                continue

            if STYLE_LEAK_RE.search(stripped):
                warnings.append(Warning(line_no, "style_leak", "规划字段里混入写法建议或读者效果", stripped))
            if ANSWER_LEAK_RE.search(stripped):
                warnings.append(Warning(line_no, "answer_leak", "规划阶段疑似提前把答案写穿", stripped))
            if plan_type == "arc-plan" and SCENE_LEAK_RE.search(stripped):
                warnings.append(Warning(line_no, "layer_drift", "Arc 层混入 Scene/章节粒度内容", stripped))
            if plan_type == "story-plan" and STORY_PROSE_LEAK_RE.search(stripped):
                warnings.append(Warning(line_no, "layer_drift", "Story 层不应讨论完整正文写法", stripped))
            if section_title.startswith("Scene ") and ABSTRACT_LOOKPOINT_RE.search(stripped):
                warnings.append(Warning(line_no, "abstract_lookpoint", "Scene 字段疑似滑回抽象气氛描述", stripped))
            if JUDGEMENT_RE.search(stripped) and stripped.startswith("-") and len(stripped) <= 18:
                warnings.append(Warning(line_no, "thin_judgement", "字段内容像判断句，不像推进点", stripped))
    return warnings


def audit_file(path: Path) -> tuple[str, list[Warning]]:
    text = path.read_text(encoding="utf-8")
    lines = text.splitlines()
    plan_type = detect_plan_type(path, text)
    headings = parse_headings(lines)
    sections = collect_section_lines(lines, headings)

    warnings: list[Warning] = []
    warnings.extend(audit_required(plan_type, headings))
    warnings.extend(audit_content(plan_type, sections))

    if plan_type == "arc-plan":
        warnings.extend(audit_arc_plan(sections))
    elif plan_type == "story-plan":
        warnings.extend(audit_story_plan(sections))
    elif plan_type == "chapter-plan":
        warnings.extend(audit_chapter_plan(sections, headings))

    return plan_type, warnings


def format_report(path: Path, plan_type: str, warnings: list[Warning]) -> str:
    status = "WARN" if warnings else "OK"
    lines = [
        f"# {path.name}",
        "",
        f"- type: `{plan_type}`",
        f"- status: `{status}`",
        f"- warnings: `{len(warnings)}`",
        "",
    ]
    if not warnings:
        lines.append("无警告。")
        return "\n".join(lines)

    lines.append("## Warnings")
    for warning in warnings:
        location = f"L{warning.line_no}" if warning.line_no else "global"
        lines.append(f"- `{location}` `{warning.kind}` {warning.message}")
        if warning.snippet:
            lines.append(f"  - `{warning.snippet}`")
    return "\n".join(lines)


def _json_report(path: Path, plan_type: str, warnings: list[Warning]) -> dict[str, object]:
    return {
        "source": str(path),
        "type": plan_type,
        "status": "WARN" if warnings else "OK",
        "warnings": [asdict(warning) for warning in warnings],
    }


def _write_reports(
    reports: list[tuple[Path, str, list[Warning]]],
    output_format: str,
    output: str | None,
) -> None:
    if output is None:
        if output_format == "json":
            payload = [_json_report(path, plan_type, warnings) for path, plan_type, warnings in reports]
            print(json.dumps(payload, ensure_ascii=False, indent=2))
            return
        for path, plan_type, warnings in reports:
            print(format_report(path, plan_type, warnings))
            print()
        return

    out_path = Path(output)
    if output_format == "json":
        payload = [_json_report(path, plan_type, warnings) for path, plan_type, warnings in reports]
        write_text(out_path, json.dumps(payload, ensure_ascii=False, indent=2) + "\n")
        return

    suffix = ".md" if output_format == "markdown" else ".txt"
    if len(reports) == 1:
        path, plan_type, warnings = reports[0]
        write_text(out_path, format_report(path, plan_type, warnings) + "\n")
        return

    out_path.mkdir(parents=True, exist_ok=True)
    for path, plan_type, warnings in reports:
        write_text(out_path / f"{path.stem}{suffix}", format_report(path, plan_type, warnings) + "\n")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Audit plan markdown files for structural drift.")
    parser.add_argument("paths", nargs="*", help="Plan files or directories to inspect")
    parser.add_argument("-i", "--input", action="append", dest="inputs", help="Input plan file or directory; may be repeated")
    parser.add_argument("--format", choices=["text", "json", "markdown"], default="markdown", help="Report output format")
    parser.add_argument("--fail-on-warn", action="store_true", help="Exit 1 when any warning appears")
    parser.add_argument("-o", "--output", help="Output file or directory")
    return parser.parse_args()


def iter_targets(raw_paths: list[str]) -> list[Path]:
    targets: list[Path] = []
    for raw in raw_paths:
        path = Path(raw)
        if path.is_dir():
            targets.extend(sorted(p for p in path.rglob("*.md") if p.is_file() and p.name.lower() != "readme.md"))
        elif path.is_file():
            if path.name.lower() == "readme.md":
                continue
            targets.append(path)
    return targets


def main() -> int:
    args = parse_args()
    targets = iter_targets(resolve_inputs(args.paths, args.inputs))
    if not targets:
        raise SystemExit("No plan files found.")

    reports: list[tuple[Path, str, list[Warning]]] = []
    warned = False
    for path in targets:
        plan_type, warnings = audit_file(path)
        reports.append((path, plan_type, warnings))
        if warnings:
            warned = True

    _write_reports(reports, args.format, args.output)
    return 1 if args.fail_on_warn and warned else 0



if __name__ == "__main__":
    raise SystemExit(main())
