#!/usr/bin/env python3
"""Build concept/plan/draft audits and write a single workspace dashboard."""

from __future__ import annotations

import argparse
import collections
import re
from pathlib import Path

from sentinel.stats import concept as build_concept_stats
from sentinel import consistency as consistency_index
from sentinel.stats import draft as build_draft_stats
from sentinel.stats import plan as build_plan_stats
from sentinel.reports import kit as build_review_kit
from sentinel.reports import scorecard as build_review_scorecards
from sentinel.reports import catalog as build_template_candidate_catalog
from sentinel.reports import backlog as build_template_backlog
from sentinel.audit import concept as concept_audit
from sentinel.audit import draft as draft_audit
from sentinel.audit import plan as plan_audit
from sentinel.lib import alignment as plan_draft_alignment
from sentinel.lib.analysis import analyze_files
from sentinel.lib.paths import collect_chapter_files


PLAN_DIR_NAMES = ("arc-plan", "story-plan", "chapter-plan")
DRIFT_SAMPLE_LIMIT = 6
CHAPTER_ID_RE = re.compile(r"ch(\d+)", re.IGNORECASE)


def summarize_counter(counter: collections.Counter[str], limit: int = 3) -> str:
    return " ".join(f"{name} x{count}" for name, count in counter.most_common(limit)) or "无"


def summarize_runs(labels: list[str], min_run: int = 3, limit: int = 4) -> list[str]:
    if not labels:
        return []
    runs: list[str] = []
    current = labels[0]
    start = 1
    length = 1
    for index, label in enumerate(labels[1:], start=2):
        if label == current:
            length += 1
            continue
        if current not in {"unclear", "missing", "none", "neutral"} and length >= min_run:
            runs.append(f"{current} x{length} (ch{start:02d}-ch{index - 1:02d})")
        current = label
        start = index
        length = 1
    if current not in {"unclear", "missing", "none", "neutral"} and length >= min_run:
        runs.append(f"{current} x{length} (ch{start:02d}-ch{start + length - 1:02d})")
    return runs[:limit]


def summarize_story_convergences(
    ending_signal_flow: list[str],
    tone_flow: list[str],
    emotion_flow: list[str],
    limit: int = 4,
) -> list[str]:
    items: list[str] = []
    for idx in range(len(ending_signal_flow) - 1):
        if ending_signal_flow[idx] != ending_signal_flow[idx + 1]:
            continue
        if tone_flow[idx] == tone_flow[idx + 1] and tone_flow[idx] != "none":
            items.append(
                f"{build_draft_stats.ending_label_display(ending_signal_flow[idx])}+tone:{tone_flow[idx]} x2"
            )
        if emotion_flow[idx] == emotion_flow[idx + 1] and emotion_flow[idx] != "neutral":
            items.append(
                f"{build_draft_stats.ending_label_display(ending_signal_flow[idx])}+emotion:{emotion_flow[idx]} x2"
            )
    seen: list[str] = []
    for item in items:
        if item not in seen:
            seen.append(item)
    return seen[:limit]


def build_story_trajectory_summary(consistency: dict[str, object]) -> list[dict[str, object]]:
    by_story: dict[str, dict[str, collections.Counter[str] | list[str]]] = {}

    def bucket(story: str) -> dict[str, collections.Counter[str] | list[str]]:
        return by_story.setdefault(
            story,
            {
                "state": collections.Counter(),
                "goal": collections.Counter(),
                "relationship": collections.Counter(),
                "samples": [],
            },
        )

    for item in consistency.get("story_tension", []):
        story = str(item["story"])
        entry = bucket(story)
        state_counter = entry["state"]
        assert isinstance(state_counter, collections.Counter)
        state_counter[str(item["title"])] += 1
        samples = entry["samples"]
        assert isinstance(samples, list)
        if len(samples) < 6:
            parts: list[str] = []
            if item.get("injury_negative") and item.get("injury_stable"):
                parts.append(f"injury={item['injury_negative']}->{item['injury_stable']}")
            if item.get("equipment_damaged") and item.get("equipment_active"):
                parts.append(f"equipment={item['equipment_damaged']}->{item['equipment_active']}")
            samples.append(f"state `{item['title']}` {' ; '.join(parts)}")

    for item in consistency.get("story_goal_tension", []):
        story = str(item["story"])
        entry = bucket(story)
        goal_counter = entry["goal"]
        assert isinstance(goal_counter, collections.Counter)
        goal_counter[str(item["title"])] += 1
        samples = entry["samples"]
        assert isinstance(samples, list)
        if len(samples) < 6:
            parts: list[str] = []
            if item.get("goal_assigned"):
                parts.append(f"assigned={item['goal_assigned']}")
            if item.get("goal_changed"):
                parts.append(f"changed={item['goal_changed']}")
            if item.get("goal_completed"):
                parts.append(f"completed={item['goal_completed']}")
            samples.append(f"goal `{item['title']}` {' ; '.join(parts)}")

    for item in consistency.get("story_relationship_tension", []):
        story = str(item["story"])
        entry = bucket(story)
        relationship_counter = entry["relationship"]
        assert isinstance(relationship_counter, collections.Counter)
        relationship_counter[str(item["title"])] += 1
        samples = entry["samples"]
        assert isinstance(samples, list)
        if len(samples) < 6:
            samples.append(
                f"relationship `{item['title']}` close={item['relationship_close']} ; distant={item['relationship_distant']}"
            )

    rows: list[dict[str, object]] = []
    for story, entry in sorted(by_story.items()):
        state_counter = entry["state"]
        goal_counter = entry["goal"]
        relationship_counter = entry["relationship"]
        samples = entry["samples"]
        assert isinstance(state_counter, collections.Counter)
        assert isinstance(goal_counter, collections.Counter)
        assert isinstance(relationship_counter, collections.Counter)
        assert isinstance(samples, list)
        rows.append(
            {
                "story": story,
                "state_summary": summarize_counter(state_counter, 3),
                "goal_summary": summarize_counter(goal_counter, 3),
                "relationship_summary": summarize_counter(relationship_counter, 3),
                "samples": samples[:4],
            }
        )
    return rows


def chapter_order_from_path(path_text: str, novel_dir: Path) -> tuple[int, str]:
    path = Path(path_text)
    try:
        _doc_type, _arc, _story, chapter = consistency_index.classify_document(path, novel_dir)
    except Exception:
        chapter = None
    chapter_name = chapter or path.stem
    match = CHAPTER_ID_RE.search(chapter_name)
    if match:
        return int(match.group(1)), chapter_name
    return 9999, chapter_name


def build_fact_timeline(
    conn,
    novel_dir: Path,
    story: str,
    title: str,
    fact_types: tuple[str, ...],
    limit: int = 8,
) -> list[str]:
    rows = consistency_index.query_story_tension_evidence(
        conn,
        story,
        title,
        limit * 4,
        fact_types=fact_types,
    )
    grouped: dict[tuple[int, str], list[str]] = collections.defaultdict(list)
    for row in rows:
        order = chapter_order_from_path(str(row["path"]), novel_dir)
        grouped[order].append(f"{row['fact_type']}:{row['cue']}")
    timeline: list[str] = []
    for (_order, chapter_name), events in sorted(grouped.items(), key=lambda item: item[0]):
        deduped: list[str] = []
        seen: set[str] = set()
        for event in events:
            if event in seen:
                continue
            seen.add(event)
            deduped.append(event)
        timeline.append(f"{chapter_name} {'/'.join(deduped[:3])}")
        if len(timeline) >= limit:
            break
    return timeline


def build_story_trajectory_details(conn, novel_dir: Path, consistency: dict[str, object]) -> list[dict[str, object]]:
    by_story: dict[str, list[dict[str, object]]] = collections.defaultdict(list)

    for item in consistency.get("story_tension", []):
        fact_types: tuple[str, ...] = ()
        if item.get("injury_negative") and item.get("injury_stable"):
            fact_types = ("injury_negative", "injury_stable")
        elif item.get("equipment_damaged") and item.get("equipment_active"):
            fact_types = ("equipment_damaged", "equipment_active")
        if not fact_types:
            continue
        by_story[str(item["story"])].append(
            {
                "title": str(item["title"]),
                "kind": "state",
                "summary": (
                    f"injury={item['injury_negative']}->{item['injury_stable']}"
                    if item.get("injury_negative") and item.get("injury_stable")
                    else f"equipment={item['equipment_damaged']}->{item['equipment_active']}"
                ),
                "timeline": build_fact_timeline(conn, novel_dir, str(item["story"]), str(item["title"]), fact_types),
            }
        )

    for item in consistency.get("story_goal_tension", []):
        by_story[str(item["story"])].append(
            {
                "title": str(item["title"]),
                "kind": "goal",
                "summary": " ; ".join(
                    part
                    for part in (
                        f"assigned={item['goal_assigned']}" if item.get("goal_assigned") else "",
                        f"changed={item['goal_changed']}" if item.get("goal_changed") else "",
                        f"completed={item['goal_completed']}" if item.get("goal_completed") else "",
                    )
                    if part
                ),
                "timeline": build_fact_timeline(
                    conn,
                    novel_dir,
                    str(item["story"]),
                    str(item["title"]),
                    ("goal_assigned", "goal_changed", "goal_completed"),
                ),
            }
        )

    for item in consistency.get("story_relationship_tension", []):
        by_story[str(item["story"])].append(
            {
                "title": str(item["title"]),
                "kind": "relationship",
                "summary": f"close={item['relationship_close']} ; distant={item['relationship_distant']}",
                "timeline": build_fact_timeline(
                    conn,
                    novel_dir,
                    str(item["story"]),
                    str(item["title"]),
                    ("relationship_close", "relationship_distant"),
                ),
            }
        )

    rows: list[dict[str, object]] = []
    for story, items in sorted(by_story.items()):
        rows.append(
            {
                "story": story,
                "items": items[:8],
            }
        )
    return rows


def build_narrative_trajectory_rows(
    drafts: dict[str, object],
    consistency: dict[str, object],
) -> list[dict[str, object]]:
    draft_rows = {
        str(item["story"]).split("/", 1)[-1]: item
        for item in drafts.get("stories", [])
    }
    consistency_rows = {
        str(item["story"]): item
        for item in consistency.get("story_trajectories", [])
    }
    detail_rows = {
        str(item["story"]): item
        for item in consistency.get("story_trajectory_details", [])
    }

    story_keys = sorted(set(draft_rows) | set(consistency_rows) | set(detail_rows))
    rows: list[dict[str, object]] = []
    for story in story_keys:
        draft = draft_rows.get(story, {})
        consistency_summary = consistency_rows.get(story, {})
        consistency_detail = detail_rows.get(story, {})
        trajectory_parts: list[str] = []
        if draft.get("speaker_summary") and draft.get("speaker_summary") != "无":
            trajectory_parts.append(f"speakers={draft['speaker_summary']}")
        if draft.get("tone_runs"):
            trajectory_parts.append(f"tone_runs={' | '.join(draft['tone_runs'][:2])}")
        if draft.get("emotion_runs"):
            trajectory_parts.append(f"emotion_runs={' | '.join(draft['emotion_runs'][:2])}")
        if draft.get("chapter_runs"):
            trajectory_parts.append(f"chapter_runs={' | '.join(draft['chapter_runs'][:2])}")
        if draft.get("voice_drifts"):
            trajectory_parts.append(f"voice={' | '.join(draft['voice_drifts'][:2])}")
        if consistency_summary.get("goal_summary") and consistency_summary.get("goal_summary") != "无":
            trajectory_parts.append(f"goal={consistency_summary['goal_summary']}")
        if consistency_summary.get("relationship_summary") and consistency_summary.get("relationship_summary") != "无":
            trajectory_parts.append(f"relationship={consistency_summary['relationship_summary']}")
        if consistency_summary.get("state_summary") and consistency_summary.get("state_summary") != "无":
            trajectory_parts.append(f"state={consistency_summary['state_summary']}")
        detail_samples: list[str] = []
        for item in consistency_detail.get("items", [])[:3]:
            detail_samples.append(
                f"{item['kind']}:{item['title']} {' -> '.join(item.get('timeline', [])[:2])}"
            )
        rows.append(
            {
                "story": story,
                "summary": " ; ".join(trajectory_parts) or "无",
                "details": detail_samples,
            }
        )
    return rows


def build_relationship_pair_trajectories(
    novel_dir: Path,
    pair_rows: list[object],
    limit_per_story: int = 4,
) -> list[dict[str, object]]:
    grouped: dict[str, dict[str, list[object]]] = collections.defaultdict(lambda: collections.defaultdict(list))
    for row in pair_rows:
        story = str(row["story"])
        pair = f"{row['left_title']}~{row['right_title']}"
        grouped[story][pair].append(row)

    results: list[dict[str, object]] = []
    for story, pairs in sorted(grouped.items()):
        items: list[dict[str, object]] = []
        for pair, rows in sorted(pairs.items()):
            ordered_rows = sorted(rows, key=lambda item: chapter_order_from_path(str(item["path"]), novel_dir))
            timeline: list[str] = []
            close_count = 0
            distant_count = 0
            for row in ordered_rows:
                chapter_name = chapter_order_from_path(str(row["path"]), novel_dir)[1]
                parts: list[str] = []
                if row["close_cues"]:
                    close_count += 1
                    parts.append(f"close={row['close_cues']}")
                if row["distant_cues"]:
                    distant_count += 1
                    parts.append(f"distant={row['distant_cues']}")
                if parts:
                    timeline.append(f"{chapter_name} {' ; '.join(parts)}")
            if not timeline:
                continue
            items.append(
                {
                    "pair": pair,
                    "summary": f"close_hits={close_count} distant_hits={distant_count}",
                    "timeline": timeline[:4],
                }
            )
        if items:
            results.append({"story": story, "items": items[:limit_per_story]})
    return results


def write_text(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def build_template_research_report(
    novel_dir: Path,
    grouped: dict[Path, list[tuple[Path, dict[str, object]]]],
    workspace_counter: collections.Counter[str],
    deposition_counter: collections.Counter[str],
) -> str:
    lines = ["# Template Research", ""]
    lines.append(f"- novel: `{novel_dir.name}`")
    lines.append("")

    lines.append("## Workspace Candidates")
    if workspace_counter:
        for name, count in workspace_counter.most_common(20):
            lines.append(f"- `{name}` x{count}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Deposition Targets")
    if deposition_counter:
        for name, count in deposition_counter.most_common():
            lines.append(f"- `{name}` x{count}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## By Story")
    for story_dir, items in sorted(grouped.items()):
        template_counter: collections.Counter[str] = collections.Counter()
        for _path, analysis in items:
            seen: set[str] = set()
            for candidate in analysis["template_candidates"][:20]:
                key = f"{candidate['type']}::{candidate['name']}"
                if key in seen:
                    continue
                seen.add(key)
                template_counter[key] += 1
        lines.append(f"- `{story_dir}`")
        if template_counter:
            for name, count in template_counter.most_common(8):
                lines.append(f"  - `{name}` x{count}")
        else:
            lines.append("  - 无")
    lines.append("")
    return "\n".join(lines) + "\n"


def infer_template_deposition_target(candidate_type: str, candidate_name: str) -> str:
    if candidate_type in {"dialogue", "dialogue_axis_gap", "dialogue_emotion"}:
        return "skills/review-guide.md"
    if candidate_type in {"tracked_term", "tracked_term_window", "learned_filter"}:
        return "scripts/rules.yaml#draft.tracked_terms"
    if candidate_type in {"scene_map", "battle_profile", "viewpoint_profile"}:
        return "scripts/rules.yaml#draft.template_rules"
    if candidate_type in {"sentence_pattern", "short_phrase", "aa_bb_pattern", "custom_template"}:
        return "scripts/rules.yaml#draft.template_rules"
    if candidate_type == "ending":
        return "novel1/rules/draft.md"
    if candidate_name in {"场面功能失衡", "动作链缺结果", "视角锚点漂移"}:
        return "scripts/rules.yaml#draft.template_rules"
    return "scripts/rules.yaml#draft.template_rules"


def collect_concept_section(novel_dir: Path) -> dict[str, object]:
    cards_dir = novel_dir / "concept" / "cards"
    if not cards_dir.exists():
        return {"exists": False}

    files = build_concept_stats.collect_targets([str(cards_dir)], include_templates=False)
    reports = build_concept_stats.build_single_reports(files)
    summary_paths = build_concept_stats.build_directory_summaries(reports)
    grouped: dict[str, list[tuple[Path, list[concept_audit.Warning]]]] = collections.defaultdict(list)
    for path, warnings in reports:
        grouped[path.parent.name].append((path, warnings))

    categories: list[dict[str, object]] = []
    for category, items in sorted(grouped.items()):
        ordered = sorted(items, key=lambda item: (-len(item[1]), item[0].name))
        total_warnings = sum(len(warnings) for _, warnings in items)
        top_file, top_warnings = ordered[0]
        top_kinds = ", ".join(
            f"{kind} x{count}"
            for kind, count in collections.Counter(w.kind for w in top_warnings).most_common(3)
        ) or "无"
        categories.append(
            {
                "category": category,
                "cards": len(items),
                "warnings": total_warnings,
                "top_file": top_file.name,
                "top_count": len(top_warnings),
                "top_kinds": top_kinds,
            }
        )

    return {
        "exists": True,
        "files": len(files),
        "warnings": sum(len(warnings) for _, warnings in reports),
        "summary_paths": summary_paths,
        "categories": categories,
    }


def collect_plan_section(novel_dir: Path) -> dict[str, object]:
    plan_dirs = [novel_dir / name for name in PLAN_DIR_NAMES if (novel_dir / name).exists()]
    if not plan_dirs:
        return {"exists": False}

    files = build_plan_stats.collect_targets([str(path) for path in plan_dirs])
    reports = build_plan_stats.build_single_reports(files)
    summary_paths = build_plan_stats.build_directory_summaries(reports)

    grouped: dict[str, list[tuple[Path, str, list[plan_audit.Warning]]]] = collections.defaultdict(list)
    chapter_function_counter: collections.Counter[str] = collections.Counter()
    ending_function_counter: collections.Counter[str] = collections.Counter()
    chapter_plan_by_story: dict[str, list[tuple[Path, str, str]]] = collections.defaultdict(list)
    for path, plan_type, warnings in reports:
        grouped[plan_type].append((path, plan_type, warnings))
        if plan_type == "chapter-plan":
            text = path.read_text(encoding="utf-8")
            lines = text.splitlines()
            headings = plan_audit.parse_headings(lines)
            sections = plan_audit.collect_section_lines(lines, headings)
            _name, chapter_function_section = plan_audit.find_section(sections, ("本章功能",))
            chapter_function_text = " ".join(
                text for _line_no, text in plan_audit.bullet_lines(chapter_function_section)
            ) or plan_audit.section_text(chapter_function_section)
            chapter_function = plan_audit.detect_function_label(
                chapter_function_text,
                plan_audit.CHAPTER_FUNCTION_RULES,
            )
            chapter_function_counter[chapter_function] += 1

            _ending_name, ending_section = plan_audit.find_section(sections, plan_audit.CHAPTER_ENDING_GROUP)
            ending_text = " ".join(text for _line_no, text in plan_audit.bullet_lines(ending_section)) or plan_audit.section_text(ending_section)
            ending_function = plan_audit.detect_function_label(
                ending_text,
                plan_audit.ENDING_FUNCTION_RULES,
            )
            ending_function_counter[ending_function] += 1
            _doc_type, _arc, story, chapter = consistency_index.classify_document(path, novel_dir)
            story_key = story or "story1"
            chapter_key = chapter or path.stem
            chapter_plan_by_story[story_key].append((path, chapter_function, ending_function))

    plan_types: list[dict[str, object]] = []
    for plan_type, items in sorted(grouped.items()):
        ordered = sorted(items, key=lambda item: (-len(item[2]), item[0].name))
        total_warnings = sum(len(warnings) for _, _, warnings in items)
        top_file, _, top_warnings = ordered[0]
        top_kinds = ", ".join(
            f"{kind} x{count}"
            for kind, count in collections.Counter(w.kind for w in top_warnings).most_common(3)
        ) or "无"
        plan_types.append(
            {
                "plan_type": plan_type,
                "files": len(items),
                "warnings": total_warnings,
                "top_file": top_file.name,
                "top_count": len(top_warnings),
                "top_kinds": top_kinds,
            }
        )

    story_trends: list[dict[str, object]] = []
    for story, rows in sorted(chapter_plan_by_story.items()):
        ordered_rows = sorted(rows, key=lambda item: item[0].name)
        chapter_labels = [item[1] for item in ordered_rows]
        ending_labels = [item[2] for item in ordered_rows]
        story_trends.append(
            {
                "story": story,
                "chapter_flow": " -> ".join(chapter_labels[:8]) if chapter_labels else "无",
                "ending_flow": " -> ".join(ending_labels[:8]) if ending_labels else "无",
                "chapter_runs": summarize_runs(chapter_labels),
                "ending_runs": summarize_runs(ending_labels),
            }
        )

    return {
        "exists": True,
        "files": len(files),
        "warnings": sum(len(warnings) for _, _, warnings in reports),
        "summary_paths": summary_paths,
        "plan_types": plan_types,
        "chapter_function_distribution": summarize_counter(chapter_function_counter, 6),
        "ending_function_distribution": summarize_counter(ending_function_counter, 6),
        "story_trends": story_trends,
    }


def collect_draft_section(novel_dir: Path, sample_limit: int, window_sizes: list[int]) -> dict[str, object]:
    drafts_dir = novel_dir / "drafts"
    if not drafts_dir.exists():
        return {"exists": False}

    files = build_draft_stats.collect_chapter_files([str(drafts_dir)])
    if not files:
        return {"exists": False}

    corpus_profile = draft_audit.build_corpus_profile(draft_audit.corpus_paths_for_targets(files))
    analyses = analyze_files(files, sample_limit=sample_limit, corpus_profile=corpus_profile)
    build_draft_stats.build_single_reports(
        files,
        sample_limit,
        corpus_profile,
        chapter_analyses=analyses,
    )
    written = build_draft_stats.build_group_reports(
        files,
        sample_limit,
        window_sizes,
        corpus_profile,
        chapter_analyses=analyses,
    )

    grouped: dict[Path, list[tuple[Path, dict[str, object]]]] = collections.defaultdict(list)
    workspace_template_counter: collections.Counter[str] = collections.Counter()
    deposition_counter: collections.Counter[str] = collections.Counter()
    alignment_counter: collections.Counter[str] = collections.Counter()
    alignment_mismatches: list[dict[str, object]] = []
    for path, analysis in analyses:
        grouped[path.parent].append((path, analysis))
        alignment = plan_draft_alignment.build_plan_draft_alignment(path, novel_dir, analysis)
        if alignment.get("available"):
            alignment_counter[f"chapter::{alignment['plan_chapter_function']}->{alignment['draft_chapter_function']}"] += 1
            alignment_counter[f"ending::{alignment['plan_ending_function']}->{alignment['draft_ending_function']}"] += 1
            if alignment["mismatch_count"] >= 1 and len(alignment_mismatches) < DRIFT_SAMPLE_LIMIT * 4:
                alignment_mismatches.append(
                    {
                        "draft": path.relative_to(drafts_dir).as_posix(),
                        "chapter": f"{alignment['plan_chapter_function']}->{alignment['draft_chapter_function']}",
                        "ending": f"{alignment['plan_ending_function']}->{alignment['draft_ending_function']}",
                        "score": int(alignment["mismatch_count"]),
                    }
                )
        seen_templates: set[str] = set()
        for candidate in analysis["template_candidates"][:20]:
            key = f"{candidate['type']}::{candidate['name']}"
            if key in seen_templates:
                continue
            seen_templates.add(key)
            workspace_template_counter[key] += 1
            deposition_counter[infer_template_deposition_target(str(candidate["type"]), str(candidate["name"]))] += 1

    stories: list[dict[str, object]] = []
    for story_dir, items in sorted(grouped.items()):
        ordered = sorted(
            items,
            key=lambda item: (
                -int(item[1]["summary"]["warn_sections"]),
                -int(item[1]["summary"]["chars"]),
                item[0].name,
            ),
        )
        top_path, top_analysis = ordered[0]
        top_templates = ", ".join(
            f"{candidate['name']} x{candidate['count']}"
            for candidate in top_analysis["template_candidates"][:3]
        ) or "无"
        top_fatigue = ", ".join(
            f"{item['family']} x{item['count']}"
            for item in top_analysis.get("style_fatigue", [])
            if item["status"] == "WARN"
        ) or "无"
        scene_role_counter: collections.Counter[str] = collections.Counter()
        tone_counter: collections.Counter[str] = collections.Counter()
        emotion_counter: collections.Counter[str] = collections.Counter()
        speaker_counter: collections.Counter[str] = collections.Counter()
        template_counter: collections.Counter[str] = collections.Counter()
        story_alignment_counter: collections.Counter[str] = collections.Counter()
        story_alignment_mismatches: list[str] = []
        voice_drifts: list[str] = []
        gates: collections.Counter[str] = collections.Counter()
        recommendations: collections.Counter[str] = collections.Counter()
        axis_totals: collections.Counter[str] = collections.Counter()
        chapter_flow: list[str] = []
        ending_flow: list[str] = []
        ending_signal_counter: collections.Counter[str] = collections.Counter()
        ending_signal_flow: list[str] = []
        tone_flow: list[str] = []
        emotion_flow: list[str] = []
        for _path, analysis in items:
            axes = build_review_scorecards.build_axes(analysis)
            gate, _priority, recommendation = build_review_scorecards.decide_gate(analysis, axes)
            gates[gate] += 1
            recommendations[recommendation] += 1
            for axis in axes:
                axis_totals[str(axis["name"])] += int(axis["score"])
            scene_role_counter[str(analysis["scene_map"]["dominant_role"])] += 1
            dominant_tone = str(analysis["tone_profile"]["dominant_tone"])
            if dominant_tone and dominant_tone != "none":
                tone_counter[dominant_tone] += 1
            tone_flow.append(dominant_tone or "none")
            dominant_emotion = str(analysis["dialogue_emotions"]["dominant_emotion"])
            if dominant_emotion and dominant_emotion != "neutral":
                emotion_counter[dominant_emotion] += 1
            for speaker in analysis["character_voice"].get("speakers", [])[:4]:
                speaker_counter[str(speaker["speaker"])] += int(speaker["lines"])
            if analysis["character_voice"].get("warn"):
                voice_drifts.extend(str(item) for item in analysis["character_voice"].get("homogenized_pairs", [])[:2])
            emotion_flow.append(dominant_emotion or "neutral")
            ending_signal = build_draft_stats.infer_ending_label(analysis)
            ending_signal_counter[ending_signal] += 1
            ending_signal_flow.append(ending_signal)
            alignment = plan_draft_alignment.build_plan_draft_alignment(_path, novel_dir, analysis)
            if alignment.get("available"):
                story_alignment_counter[
                    f"{alignment['plan_chapter_function']}->{alignment['draft_chapter_function']}"
                ] += 1
                chapter_flow.append(str(alignment["draft_chapter_function"]))
                ending_flow.append(str(alignment["draft_ending_function"]))
                if alignment["mismatch_count"] >= 1 and len(story_alignment_mismatches) < 4:
                    story_alignment_mismatches.append(
                        f"{_path.name} chapter={alignment['plan_chapter_function']}->{alignment['draft_chapter_function']} ending={alignment['plan_ending_function']}->{alignment['draft_ending_function']}"
                    )
            seen_templates: set[str] = set()
            for candidate in analysis["template_candidates"][:20]:
                key = f"{candidate['type']}::{candidate['name']}"
                if key in seen_templates:
                    continue
                seen_templates.add(key)
                template_counter[key] += 1
            scorecard_path = build_review_scorecards.scorecard_path_for(_path)
            build_review_scorecards.write_text(scorecard_path, build_review_scorecards.build_scorecard_report(_path, analysis))
        scorecard_summary_path = build_review_scorecards.scorecard_path_for(items[0][0]).parent / "SUMMARY.md"
        build_review_scorecards.write_text(scorecard_summary_path, build_review_scorecards.build_story_summary(story_dir, items))
        review_kit_summary_path = build_review_kit.review_kit_path_for(items[0][0])
        template_backlog_summary_path = build_template_backlog.backlog_path_for(items[0][0])
        avg_axis = ", ".join(
            f"{name} {round(total / max(len(items), 1), 2)}"
            for name, total in sorted(axis_totals.items())
        ) or "无"
        scene_summary = summarize_counter(scene_role_counter, 3)
        tone_summary = summarize_counter(tone_counter, 3)
        emotion_summary = summarize_counter(emotion_counter, 3)
        speaker_summary = summarize_counter(speaker_counter, 4)
        template_summary = summarize_counter(template_counter, 4)
        alignment_summary = summarize_counter(story_alignment_counter, 3)
        ending_signal_summary = summarize_counter(
            collections.Counter(
                {
                    build_draft_stats.ending_label_display(name): count
                    for name, count in ending_signal_counter.items()
                }
            ),
            4,
        )
        stories.append(
            {
                "story": story_dir.relative_to(drafts_dir).as_posix(),
                "chapters": len(items),
                "warnings": sum(int(item[1]["summary"]["warn_sections"]) for item in items),
                "top_file": top_path.name,
                "top_count": int(top_analysis["summary"]["warn_sections"]),
                "top_templates": top_templates,
                "top_fatigue": top_fatigue,
                "gate_summary": " ".join(f"{name} x{count}" for name, count in sorted(gates.items())) or "无",
                "recommendation_summary": " ".join(
                    f"{name} x{count}" for name, count in sorted(recommendations.items())
                ) or "无",
                "avg_axis": avg_axis,
                "scene_summary": scene_summary,
                "tone_summary": tone_summary,
                "emotion_summary": emotion_summary,
                "speaker_summary": speaker_summary,
                "template_summary": template_summary,
                "alignment_summary": alignment_summary,
                "ending_signal_summary": ending_signal_summary,
                "alignment_mismatches": story_alignment_mismatches,
                "voice_drifts": voice_drifts[:4],
                "chapter_runs": summarize_runs(chapter_flow),
                "ending_runs": summarize_runs(ending_flow),
                "ending_signal_flow": build_draft_stats.ending_flow_text(ending_signal_flow),
                "ending_signal_runs": build_draft_stats.summarize_runs(ending_signal_flow),
                "trend_convergences": summarize_story_convergences(
                    ending_signal_flow,
                    tone_flow,
                    emotion_flow,
                ),
                "tone_runs": summarize_runs(tone_flow),
                "emotion_runs": summarize_runs(emotion_flow),
                "scorecard_summary_path": scorecard_summary_path,
                "review_kit_summary_path": review_kit_summary_path,
                "template_backlog_summary_path": template_backlog_summary_path,
            }
        )

    story_payloads = [
        payload
        for story_dir, items in sorted(grouped.items())
        for _markdown, payload in [build_template_backlog.build_story_backlog(story_dir, items)]
    ]
    template_research_path = novel_dir / "draft-stats" / "TEMPLATE_RESEARCH.md"
    write_text(
        template_research_path,
        build_template_research_report(
            novel_dir,
            grouped,
            workspace_template_counter,
            deposition_counter,
        ),
    )
    template_catalog = build_template_candidate_catalog.build_catalog_payload(
        novel_dir,
        story_payloads,
    )
    template_catalog_summary_path = build_template_candidate_catalog.summary_path_for(novel_dir)
    template_catalog_json_path = build_template_candidate_catalog.json_path_for(novel_dir)
    build_template_candidate_catalog.write_text(
        template_catalog_summary_path,
        build_template_candidate_catalog.build_catalog_markdown(
            novel_dir,
            story_payloads,
            template_catalog["template_candidates"],
            template_catalog["template_families"],
            template_catalog["term_candidates"],
            template_catalog["keep_candidates"],
            template_catalog["deposition_targets"],
            template_catalog["learning_anchors"],
            template_catalog["writeback_queue"],
        ),
    )
    build_template_candidate_catalog.write_json(template_catalog_json_path, template_catalog)

    return {
        "exists": True,
        "files": len(files),
        "warnings": sum(int(analysis["summary"]["warn_sections"]) for _, analysis in analyses),
        "written": written,
        "stories": stories,
        "workspace_templates": summarize_counter(workspace_template_counter, 6),
        "template_targets": summarize_counter(deposition_counter, 4),
        "template_research_path": template_research_path,
        "template_catalog_summary_path": template_catalog_summary_path,
        "template_catalog_json_path": template_catalog_json_path,
        "template_learning_anchors": template_catalog["learning_anchors"][:4],
        "template_writeback_queue": template_catalog["writeback_queue"][:5],
        "alignment_summary": summarize_counter(alignment_counter, 6),
        "alignment_mismatches": alignment_mismatches[:DRIFT_SAMPLE_LIMIT],
    }


def collect_consistency_section(novel_dir: Path) -> dict[str, object]:
    db_path = novel_dir / "research" / "consistency" / "consistency.sqlite3"
    feedback_path = novel_dir / "research" / "consistency" / "review-feedback.jsonl"
    consistency_index.build_index(novel_dir, db_path)
    conn = consistency_index.open_db(db_path)
    trajectory_summary: list[dict[str, object]] = []
    trajectory_details: list[dict[str, object]] = []
    relationship_pair_trajectories: list[dict[str, object]] = []
    try:
        alignment_rows = consistency_index.query_story_alignment_rows(conn, 200)
        tension_rows = consistency_index.query_story_tension_rows(conn, 200)
        goal_tension_rows = consistency_index.query_story_goal_tension_rows(conn, 200)
        relationship_tension_rows = consistency_index.query_story_relationship_tension_rows(conn, 200)
        relationship_pair_rows = consistency_index.query_story_relationship_pair_rows(conn, 800)
        conflict_rows = consistency_index.collect_conflict_rows(conn, 200, feedback_path)
        feedback_summary = consistency_index.summarize_feedback(conn, feedback_path, 200)
        trajectory_summary = build_story_trajectory_summary(
            {
                "story_tension": [
                    {
                        "story": row["story"],
                        "title": row["title"],
                        "category": row["category"],
                        "injury_negative": row["injury_negative"] or "",
                        "injury_stable": row["injury_stable"] or "",
                        "equipment_damaged": row["equipment_damaged"] or "",
                        "equipment_active": row["equipment_active"] or "",
                    }
                    for row in tension_rows
                ],
                "story_goal_tension": [
                    {
                        "story": row["story"],
                        "title": row["title"],
                        "category": row["category"],
                        "goal_assigned": row["goal_assigned"] or "",
                        "goal_changed": row["goal_changed"] or "",
                        "goal_completed": row["goal_completed"] or "",
                    }
                    for row in goal_tension_rows
                ],
                "story_relationship_tension": [
                    {
                        "story": row["story"],
                        "title": row["title"],
                        "category": row["category"],
                        "relationship_close": row["relationship_close"] or "",
                        "relationship_distant": row["relationship_distant"] or "",
                    }
                    for row in relationship_tension_rows
                ],
            }
        )
        trajectory_details = build_story_trajectory_details(
            conn,
            novel_dir,
            {
                "story_tension": [
                    {
                        "story": row["story"],
                        "title": row["title"],
                        "category": row["category"],
                        "injury_negative": row["injury_negative"] or "",
                        "injury_stable": row["injury_stable"] or "",
                        "equipment_damaged": row["equipment_damaged"] or "",
                        "equipment_active": row["equipment_active"] or "",
                    }
                    for row in tension_rows
                ],
                "story_goal_tension": [
                    {
                        "story": row["story"],
                        "title": row["title"],
                        "category": row["category"],
                        "goal_assigned": row["goal_assigned"] or "",
                        "goal_changed": row["goal_changed"] or "",
                        "goal_completed": row["goal_completed"] or "",
                    }
                    for row in goal_tension_rows
                ],
                "story_relationship_tension": [
                    {
                        "story": row["story"],
                        "title": row["title"],
                        "category": row["category"],
                        "relationship_close": row["relationship_close"] or "",
                        "relationship_distant": row["relationship_distant"] or "",
                    }
                    for row in relationship_tension_rows
                ],
            },
        )
        relationship_pair_trajectories = build_relationship_pair_trajectories(
            novel_dir,
            relationship_pair_rows,
        )
    finally:
        conn.close()

    story_alignment = [
        {
            "story": row["story"],
            "plan_entities": row["plan_entities"],
            "draft_entities": row["draft_entities"],
            "plan_only_entities": row["plan_only_entities"] or "",
            "draft_only_entities": row["draft_only_entities"] or "",
        }
        for row in alignment_rows
    ]
    story_tension = [
        {
            "story": row["story"],
            "title": row["title"],
            "category": row["category"],
            "injury_negative": row["injury_negative"] or "",
            "injury_stable": row["injury_stable"] or "",
            "equipment_damaged": row["equipment_damaged"] or "",
            "equipment_active": row["equipment_active"] or "",
        }
        for row in tension_rows
    ]
    story_conflicts = [
        {
            "category": row["category"],
            "story": row["story"],
            "title": row["title"],
            "entity_category": row["entity_category"],
            "confidence": row.get("confidence", "low"),
            "support_note": row.get("support_note", ""),
            "summary": row["summary"],
        }
        for row in conflict_rows
    ]
    story_goal_tension = [
        {
            "story": row["story"],
            "title": row["title"],
            "category": row["category"],
            "goal_assigned": row["goal_assigned"] or "",
            "goal_changed": row["goal_changed"] or "",
            "goal_completed": row["goal_completed"] or "",
        }
        for row in goal_tension_rows
    ]
    story_relationship_tension = [
        {
            "story": row["story"],
            "title": row["title"],
            "category": row["category"],
            "relationship_close": row["relationship_close"] or "",
            "relationship_distant": row["relationship_distant"] or "",
        }
        for row in relationship_tension_rows
    ]
    return {
        "db_path": db_path,
        "feedback_path": feedback_path,
        "feedback_entries": len(feedback_summary["entries"]),
        "feedback_decisions": list(feedback_summary["decision_counter"].most_common()),
        "feedback_facets": list(feedback_summary["facet_counter"].most_common(6)),
        "feedback_backlog": feedback_summary["backlog"],
        "feedback_pending": len(feedback_summary["unresolved"]),
        "feedback_pending_samples": feedback_summary["unresolved"][:6],
        "story_alignment": story_alignment,
        "story_tension": story_tension,
        "story_goal_tension": story_goal_tension,
        "story_relationship_tension": story_relationship_tension,
        "story_conflicts": story_conflicts,
        "story_trajectories": trajectory_summary,
        "story_trajectory_details": trajectory_details,
        "relationship_pair_trajectories": relationship_pair_trajectories,
    }


def build_dashboard(
    novel_dir: Path,
    concept: dict[str, object],
    plans: dict[str, object],
    drafts: dict[str, object],
    consistency: dict[str, object],
) -> str:
    narrative_trajectories = build_narrative_trajectory_rows(drafts, consistency)
    lines = ["# AUDIT DASHBOARD", ""]
    lines.append(f"- novel: `{novel_dir.name}`")
    lines.append(f"- concept cards: `{concept['files'] if concept.get('exists') else 0}`")
    lines.append(f"- plan files: `{plans['files'] if plans.get('exists') else 0}`")
    lines.append(f"- draft chapters: `{drafts['files'] if drafts.get('exists') else 0}`")
    lines.append("")

    lines.append("## Concept")
    if not concept.get("exists"):
        lines.append("- 无概念卡目录")
    else:
        lines.append(f"- total warnings: `{concept['warnings']}`")
        for item in concept["categories"]:
            lines.append(
                f"- `{item['category']}` cards=`{item['cards']}` warnings=`{item['warnings']}` top=`{item['top_file']}` (`{item['top_count']}` | {item['top_kinds']})"
            )
        lines.append("- summary files:")
        for path in concept["summary_paths"]:
            lines.append(f"  - `{path}`")
    lines.append("")

    lines.append("## Plans")
    if not plans.get("exists"):
        lines.append("- 无大纲目录")
    else:
        lines.append(f"- total warnings: `{plans['warnings']}`")
        lines.append(f"- chapter functions: {plans['chapter_function_distribution']}")
        lines.append(f"- ending functions: {plans['ending_function_distribution']}")
        if plans["story_trends"]:
            lines.append("- story function trends:")
            for item in plans["story_trends"][:8]:
                lines.append(
                    f"  - `{item['story']}` chapter_flow=`{item['chapter_flow']}` ending_flow=`{item['ending_flow']}`"
                )
                if item["chapter_runs"] or item["ending_runs"]:
                    run_parts: list[str] = []
                    if item["chapter_runs"]:
                        run_parts.append(f"chapter_runs={' | '.join(item['chapter_runs'])}")
                    if item["ending_runs"]:
                        run_parts.append(f"ending_runs={' | '.join(item['ending_runs'])}")
                    lines.append(f"    trend=`{'; '.join(run_parts)}`")
        for item in plans["plan_types"]:
            lines.append(
                f"- `{item['plan_type']}` files=`{item['files']}` warnings=`{item['warnings']}` top=`{item['top_file']}` (`{item['top_count']}` | {item['top_kinds']})"
            )
        lines.append("- summary files:")
        for path in plans["summary_paths"]:
            lines.append(f"  - `{path}`")
    lines.append("")

    lines.append("## Drafts")
    if not drafts.get("exists"):
        lines.append("- 无草稿目录")
    else:
        lines.append(f"- total warn sections: `{drafts['warnings']}`")
        lines.append(f"- workspace templates: {drafts['workspace_templates']}")
        lines.append(f"- deposition targets: {drafts['template_targets']}")
        lines.append(f"- plan-draft alignment: {drafts['alignment_summary']}")
        for item in drafts["stories"]:
            lines.append(
                f"- `{item['story']}` chapters=`{item['chapters']}` warn_sections=`{item['warnings']}` top=`{item['top_file']}` (`{item['top_count']}` | {item['top_templates']}) fatigue=`{item['top_fatigue']}`"
            )
            lines.append(
                f"  gate=`{item['gate_summary']}` recommendation=`{item['recommendation_summary']}` avg_axes=`{item['avg_axis']}`"
            )
            lines.append(
                f"  narrative=`scene:{item['scene_summary']} tone:{item['tone_summary']} emotion:{item['emotion_summary']} speakers:{item['speaker_summary']}` templates=`{item['template_summary']}` alignment=`{item['alignment_summary']}` ending_signals=`{item['ending_signal_summary']}`"
            )
            if item["alignment_mismatches"]:
                lines.append(f"  drift=`{' | '.join(item['alignment_mismatches'][:2])}`")
            if item["voice_drifts"]:
                lines.append(f"  voice=`{' | '.join(item['voice_drifts'][:2])}`")
            if item["chapter_runs"] or item["ending_runs"] or item["ending_signal_runs"] or item["tone_runs"] or item["emotion_runs"] or item["trend_convergences"]:
                trend_parts: list[str] = []
                if item["chapter_runs"]:
                    trend_parts.append(f"chapter_runs={' | '.join(item['chapter_runs'])}")
                if item["ending_runs"]:
                    trend_parts.append(f"ending_runs={' | '.join(item['ending_runs'])}")
                if item["ending_signal_runs"]:
                    trend_parts.append(f"ending_signal_runs={' | '.join(item['ending_signal_runs'])}")
                if item["trend_convergences"]:
                    trend_parts.append(f"convergence={' | '.join(item['trend_convergences'])}")
                if item["tone_runs"]:
                    trend_parts.append(f"tone_runs={' | '.join(item['tone_runs'])}")
                if item["emotion_runs"]:
                    trend_parts.append(f"emotion_runs={' | '.join(item['emotion_runs'])}")
                lines.append(f"  trend=`{'; '.join(trend_parts)}`")
                lines.append(f"  ending_signal_flow=`{item['ending_signal_flow']}`")
        lines.append("- mirror stats:")
        lines.append(f"  - `{novel_dir / 'draft-stats'}`")
        lines.append("- scorecard summaries:")
        for item in drafts["stories"]:
            lines.append(f"  - `{item['scorecard_summary_path']}`")
        lines.append("- review kits:")
        for item in drafts["stories"]:
            lines.append(f"  - `{item['review_kit_summary_path']}`")
        lines.append("- template backlogs:")
        for item in drafts["stories"]:
            lines.append(f"  - `{item['template_backlog_summary_path']}`")
        lines.append("- template research:")
        lines.append(f"  - `{drafts['template_research_path']}`")
        lines.append("- template catalog:")
        lines.append(f"  - `{drafts['template_catalog_summary_path']}`")
        lines.append(f"  - `{drafts['template_catalog_json_path']}`")
        if drafts["template_learning_anchors"]:
            lines.append("- learning anchors:")
            for item in drafts["template_learning_anchors"]:
                lines.append(
                    f"  - `{item['kind']}` `{item['bucket']}::{item['name']}` stories=`{item['story_count']}` total=`{item['count']}`"
                )
        if drafts["template_writeback_queue"]:
            lines.append("- writeback queue:")
            for item in drafts["template_writeback_queue"]:
                lines.append(
                    f"  - `{item['kind']}` `{item['name']}` stories=`{item['stories']}` total=`{item['count']}` -> `{item['target']}`"
                )
        if drafts["alignment_mismatches"]:
            lines.append("- plan-draft drift samples:")
            for item in drafts["alignment_mismatches"]:
                lines.append(
                    f"  - `{item['draft']}` chapter=`{item['chapter']}` ending=`{item['ending']}`"
                )
    lines.append("")

    lines.append("## Consistency")
    lines.append(f"- index: `{consistency['db_path']}`")
    lines.append(f"- feedback log: `{consistency['feedback_path']}` entries=`{consistency['feedback_entries']}`")
    if consistency["feedback_decisions"]:
        decision_summary = " ".join(f"`{name}` x{count}" for name, count in consistency["feedback_decisions"])
        lines.append(f"- feedback decisions: {decision_summary}")
    else:
        lines.append("- feedback decisions: 无")
    if consistency["feedback_facets"]:
        facet_summary = " ".join(f"`{name}` x{count}" for name, count in consistency["feedback_facets"])
        lines.append(f"- feedback facets: {facet_summary}")
    else:
        lines.append("- feedback facets: 无")
    lines.append(f"- pending review: `{consistency['feedback_pending']}`")
    if consistency["feedback_backlog"]:
        lines.append("- feedback backlog:")
        for item in consistency["feedback_backlog"][:4]:
            lines.append(f"  - `{item['target']}` {item['reason']}")
    else:
        lines.append("- feedback backlog: 无")
    if consistency["feedback_pending_samples"]:
        lines.append("- pending samples:")
        for row in consistency["feedback_pending_samples"]:
            lines.append(
                f"  - `{row['story']}` `{row['category']}` `{row['title']}` confidence=`{row.get('confidence', '')}`"
            )
    else:
        lines.append("- pending samples: 无")
    if consistency["story_alignment"]:
        lines.append("- story alignment:")
        for item in consistency["story_alignment"][:12]:
            line = (
                f"  - `{item['story']}` plan_entities=`{item['plan_entities']}` draft_entities=`{item['draft_entities']}`"
            )
            if item["plan_only_entities"]:
                line += f" plan_only=`{item['plan_only_entities']}`"
            if item["draft_only_entities"]:
                line += f" draft_only=`{item['draft_only_entities']}`"
            lines.append(line)
    else:
        lines.append("- story alignment: 无")
    if narrative_trajectories:
        lines.append("- narrative trajectories:")
        for item in narrative_trajectories[:8]:
            lines.append(f"  - `{item['story']}` {item['summary']}")
            if item["details"]:
                lines.append(f"    detail=`{' | '.join(item['details'][:2])}`")
    else:
        lines.append("- narrative trajectories: 无")
    if consistency["story_trajectories"]:
        lines.append("- story trajectories:")
        for item in consistency["story_trajectories"][:8]:
            lines.append(
                f"  - `{item['story']}` state=`{item['state_summary']}` goal=`{item['goal_summary']}` relationship=`{item['relationship_summary']}`"
            )
            if item["samples"]:
                lines.append(f"    sample=`{' | '.join(item['samples'][:2])}`")
    else:
        lines.append("- story trajectories: 无")
    if consistency["story_trajectory_details"]:
        lines.append("- trajectory details:")
        for item in consistency["story_trajectory_details"][:6]:
            lines.append(f"  - `{item['story']}`")
            for detail in item["items"][:4]:
                line = f"    - `{detail['kind']}` `{detail['title']}` {detail['summary']}"
                if detail["timeline"]:
                    line += f" timeline=`{' -> '.join(detail['timeline'][:3])}`"
                lines.append(line)
    else:
        lines.append("- trajectory details: 无")
    if consistency["relationship_pair_trajectories"]:
        lines.append("- relationship pair trajectories:")
        for item in consistency["relationship_pair_trajectories"][:6]:
            lines.append(f"  - `{item['story']}`")
            for detail in item["items"][:4]:
                line = f"    - `{detail['pair']}` {detail['summary']}"
                if detail["timeline"]:
                    line += f" timeline=`{' -> '.join(detail['timeline'][:3])}`"
                lines.append(line)
    else:
        lines.append("- relationship pair trajectories: 无")
    if consistency["story_tension"]:
        lines.append("- state tension:")
        for item in consistency["story_tension"][:12]:
            parts: list[str] = []
            if item["injury_negative"] and item["injury_stable"]:
                parts.append(f"injury={item['injury_negative']} -> {item['injury_stable']}")
            if item["equipment_damaged"] and item["equipment_active"]:
                parts.append(f"equipment={item['equipment_damaged']} -> {item['equipment_active']}")
            lines.append(f"  - `{item['story']}` `{item['title']}` ({item['category']}) {' ; '.join(parts)}")
    else:
        lines.append("- state tension: 无")
    if consistency["story_goal_tension"]:
        lines.append("- goal tension:")
        for item in consistency["story_goal_tension"][:12]:
            parts: list[str] = []
            if item["goal_assigned"]:
                parts.append(f"assigned={item['goal_assigned']}")
            if item["goal_changed"]:
                parts.append(f"changed={item['goal_changed']}")
            if item["goal_completed"]:
                parts.append(f"completed={item['goal_completed']}")
            lines.append(f"  - `{item['story']}` `{item['title']}` ({item['category']}) {' ; '.join(parts)}")
    else:
        lines.append("- goal tension: 无")
    if consistency["story_relationship_tension"]:
        lines.append("- relationship tension:")
        for item in consistency["story_relationship_tension"][:12]:
            lines.append(
                f"  - `{item['story']}` `{item['title']}` ({item['category']}) close={item['relationship_close']} ; distant={item['relationship_distant']}"
            )
    else:
        lines.append("- relationship tension: 无")
    if consistency["story_conflicts"]:
        lines.append("- conflict candidates:")
        for item in consistency["story_conflicts"][:12]:
            lines.append(
                f"  - `{item['story']}` `{item['category']}` `{item['title']}` ({item['entity_category']}) confidence=`{item['confidence']}` support=`{item['support_note']}` {item['summary']}"
            )
    else:
        lines.append("- conflict candidates: 无")
    lines.append("")

    lines.append("## Suggested Order")
    lines.append("1. 先修 `draft` 里 `gate=FAIL`、`recommendation=targeted_rewrite` 的章节，再看 `pairs / triples`")
    lines.append("2. 再修 `chapter-plan` 与 `story-plan` 的字段错位和空字段")
    lines.append("3. 如果某章评分里 `一致性准备度` 明显偏低，先跑 `consistency_index.py suspects` 再决定是否只是局部误写")
    lines.append("4. 如果已经锁定某条 Story，要逐条复核一致性候选，直接跑 `python3 scripts/consistency_index.py review-queue novel1 --story storyN`")
    lines.append("5. 做完一轮局部复核后，立刻跑 `python3 scripts/consistency_index.py feedback-summary novel1 --story storyN` 看这一条 Story 是否开始收敛")
    lines.append("6. 最后补 `concept` 缺口，避免下游继续空转")
    return "\n".join(lines) + "\n"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Build concept/plan/draft audits and a single dashboard.")
    parser.add_argument("novel_dir", help="Novel directory, for example novel1")
    parser.add_argument("--sample-limit", type=int, default=3, help="Max sample lines per rule")
    parser.add_argument(
        "--window-sizes",
        nargs="*",
        type=int,
        default=[2, 3],
        help="Rolling chapter window sizes for draft stats",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    novel_dir = Path(args.novel_dir)
    if not novel_dir.exists():
        raise SystemExit(f"Novel directory not found: {novel_dir}")

    concept = collect_concept_section(novel_dir)
    plans = collect_plan_section(novel_dir)
    drafts = collect_draft_section(novel_dir, args.sample_limit, args.window_sizes)
    consistency = collect_consistency_section(novel_dir)

    dashboard = build_dashboard(novel_dir, concept, plans, drafts, consistency)
    out_path = novel_dir / "AUDIT.md"
    write_text(out_path, dashboard)
    print(out_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
