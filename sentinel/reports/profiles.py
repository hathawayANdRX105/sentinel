#!/usr/bin/env python3
"""Build research-focused sentence profiles for draft chapters."""

from __future__ import annotations

import argparse
import collections
from pathlib import Path

from sentinel.stats import draft as build_draft_stats
from sentinel.audit import draft as draft_audit
from sentinel.lib.io import write_text
from sentinel.lib.paths import chapter_sort_key, collect_chapter_files


def profile_path_for(draft_path: Path, output_root: Path | None = None) -> Path:
    stats_path = build_draft_stats.stats_path_for(draft_path)
    if output_root is None:
        return stats_path.parent / "profiles" / f"{draft_path.stem}.md"
    try:
        idx = list(draft_path.parts).index("drafts")
    except ValueError as exc:
        raise ValueError(f"Path does not live under drafts/: {draft_path}") from exc
    novel_root = Path(*draft_path.parts[:idx])
    relative_stats_dir = stats_path.parent.relative_to(novel_root)
    return output_root / novel_root.name / relative_stats_dir / "profiles" / f"{draft_path.stem}.md"


def format_ranked_items(
    items: list[dict[str, object]],
    value_key: str,
    sample_limit: int,
    label_key: str = "phrase",
    empty_label: str = "无",
) -> list[str]:
    if not items:
        return [f"- {empty_label}"]
    lines: list[str] = []
    for item in items[:sample_limit]:
        lines.append(f"- `{item[label_key]}` x{item[value_key]}")
    return lines


def format_metric_items(
    items: list[dict[str, object]],
    sample_limit: int,
    empty_label: str = "无",
) -> list[str]:
    if not items:
        return [f"- {empty_label}"]
    lines: list[str] = []
    for item in items[:sample_limit]:
        line = f"- `{item['name']}` x{item['count']}"
        if "per_10k" in item:
            line += f" per_10k=`{item['per_10k']}`"
        if item.get("warn"):
            line += " `WARN`"
        note = str(item.get("note", "")).strip()
        if note:
            line += f"：{note}"
        lines.append(line)
    return lines


def format_hot_windows(items: list[dict[str, object]], sample_limit: int, title_key: str = "reasons") -> list[str]:
    if not items:
        return ["- 无"]
    lines: list[str] = []
    for item in items[:sample_limit]:
        reasons = "、".join(str(reason) for reason in item.get(title_key, [])) or "局部高压"
        sample = " | ".join(str(value) for value in item.get("sample", [])[:4])
        lines.append(
            f"- `S{item['start_index']}-{item['end_index']}` `L{item['start_line']}-{item['end_line']}` score=`{item['score']}`：{reasons}"
        )
        if sample:
            lines.append(f"  样例：{sample}")
    return lines


def build_profile_report(
    draft_path: Path,
    analysis: dict[str, object],
    sample_limit: int,
) -> str:
    summary = analysis["summary"]
    dialogue = analysis["dialogue"]
    profile = analysis["corpus_profile"]

    lines = [f"# {draft_path.stem} Sentence Profile", ""]
    lines.append(f"- source: `{draft_path}`")
    lines.append(f"- chars: `{summary['chars']}`")
    lines.append(f"- sentences: `{summary['sentences']}`")
    lines.append(f"- paragraphs: `{summary['paragraphs']}`")
    lines.append(f"- avg_sentence_chars: `{summary['avg_sentence_chars']}`")
    lines.append(f"- short_ratio: `{summary['short_sentence_ratio']}`")
    lines.append(f"- quote_ratio: `{summary['quote_ratio']}`")
    lines.append("")

    lines.append("## Chapter Fingerprint")
    top_fatigue = ", ".join(
        f"{item['family']} x{item['count']}"
        for item in analysis["style_fatigue"]
        if item["status"] == "WARN"
    ) or "无"
    top_templates = ", ".join(
        f"{item['name']} x{item['count']}" for item in analysis["template_candidates"][:5]
    ) or "无"
    lines.append(f"- fatigue: {top_fatigue}")
    lines.append(f"- template_candidates: {top_templates}")
    lines.append(
        f"- dialogue: axis_gaps=`{len(dialogue['dialogue_axis_gaps'])}` short_quote_runs=`{len(dialogue['short_quote_runs'])}` ping_pong=`{len(dialogue['quote_ping_pong'])}`"
    )
    lines.append(
        f"- windows: fatigue=`{analysis['fatigue_window_count']}` tracked_terms=`{analysis['tracked_term_window_count']}`"
    )
    lines.append(
        f"- narrative: scene_blocks=`{analysis['scene_map']['block_count']}` tone=`{analysis['tone_profile']['dominant_tone']}` battle_sequences=`{analysis['battle_profile']['sequence_count']}` viewpoint_anchor=`{analysis['viewpoint_profile']['dominant_anchor'] or 'none'}`"
    )
    lines.append(
        f"- character_voice: speakers=`{analysis['character_voice']['speaker_count']}` dominant=`{analysis['character_voice']['dominant_speaker'] or 'none'}` coverage=`{analysis['character_voice']['coverage_ratio']}` warn=`{analysis['character_voice']['warn']}`"
    )
    lines.append("")

    sections: list[tuple[str, list[str]]] = [
        ("Sentence Skeletons", format_ranked_items(analysis["sentence_patterns"], "count", sample_limit)),
        ("Sentence Starts", format_ranked_items(analysis["sentence_starts"], "count", sample_limit)),
        ("Subject Leads", format_ranked_items(analysis["subject_leads"], "count", sample_limit)),
        ("Paragraph Leads", format_ranked_items(analysis["paragraph_leads"], "count", sample_limit)),
        ("Clause Prefixes", format_ranked_items(analysis["clause_prefixes"], "count", sample_limit)),
        ("Parallel Clauses", format_ranked_items(analysis["parallel_clauses"], "count", sample_limit)),
        ("Judgement Endings", format_ranked_items(analysis["judgement_endings"], "count", sample_limit)),
        ("Structural Phrases", format_ranked_items(analysis["short_phrases"], "count", sample_limit, label_key="term")),
        ("Frequent Fragments", format_ranked_items(analysis["terms"], "count", sample_limit, label_key="term")),
        ("Tracked Terms", format_metric_items(analysis["tracked_terms"], sample_limit)),
        ("Template Bank Hits", format_metric_items(analysis["custom_templates"], sample_limit)),
        ("Learned Filters", format_metric_items(analysis["learned_filters"], sample_limit)),
        ("AA/BB Patterns", format_metric_items(analysis["aa_bb_patterns"], sample_limit)),
        ("Hot Fatigue Windows", format_hot_windows(analysis["fatigue_windows"], sample_limit)),
        ("Tracked Term Windows", format_hot_windows(analysis["tracked_term_windows"], sample_limit)),
    ]

    for title, block_lines in sections:
        lines.append(f"## {title}")
        lines.extend(block_lines)
        lines.append("")

    lines.append("## Narrative Signals")
    scene_map = analysis["scene_map"]
    role_counts = scene_map.get("role_counts", {})
    role_summary = "，".join(f"{name} x{count}" for name, count in role_counts.items()) or "无"
    lines.append(
        f"- scene_blocks=`{scene_map['block_count']}` dominant_role=`{scene_map['dominant_role']}` dominance_ratio=`{scene_map['dominance_ratio']}` switches=`{scene_map['switch_count']}` warn=`{scene_map['warn']}`"
    )
    lines.append(f"- scene_roles: {role_summary}")
    for item in scene_map.get("blocks", [])[:sample_limit]:
        lines.append(
            f"- block `P{item['start_paragraph']}-{item['end_paragraph']}` `L{item['start_line']}-{item['end_line']}` role=`{item['role']}` chars=`{item['chars']}`"
        )

    dialogue_emotions = analysis["dialogue_emotions"]
    emotion_counts = dialogue_emotions.get("emotion_counts", {})
    emotion_summary = "，".join(f"{name} x{count}" for name, count in emotion_counts.items()) or "无"
    lines.append(
        f"- dialogue_emotion dominant=`{dialogue_emotions['dominant_emotion']}` ratio=`{dialogue_emotions['dominant_ratio']}` shifts=`{dialogue_emotions['shift_count']}` flat_warn=`{dialogue_emotions['flatness_warn']}` volatility_warn=`{dialogue_emotions['volatility_warn']}`"
    )
    lines.append(f"- emotion_counts: {emotion_summary}")
    for item in dialogue_emotions.get("samples", [])[:sample_limit]:
        lines.append(f"- `L{item['line_no']}` `{item['label']}` {item['text']}")

    character_voice = analysis["character_voice"]
    lines.append(
        f"- character_voice dominant=`{character_voice['dominant_speaker'] or 'none'}` speakers=`{character_voice['speaker_count']}` identified=`{character_voice['identified_lines']}` unknown=`{character_voice['unknown_lines']}` coverage=`{character_voice['coverage_ratio']}` warn=`{character_voice['warn']}`"
    )
    for item in character_voice.get("speakers", [])[:sample_limit]:
        lines.append(
            f"- speaker `{item['speaker']}` lines=`{item['lines']}` avg=`{item['avg_chars']}` q=`{item['question_ratio']}` short=`{item['short_ratio']}` judgement=`{item['judgement_ratio']}` emotion=`{item['dominant_emotion']}`"
        )
    for item in character_voice.get("homogenized_pairs", [])[:sample_limit]:
        lines.append(f"- homogenized `{item}`")

    tone_profile = analysis["tone_profile"]
    tone_counts = tone_profile.get("tone_counts", {})
    tone_summary = "，".join(f"{name} x{count}" for name, count in tone_counts.items()) or "无"
    lines.append(
        f"- tone dominant=`{tone_profile['dominant_tone']}` stable_ratio=`{tone_profile['stable_ratio']}` switches=`{tone_profile['switch_count']}` warn=`{tone_profile['warn']}`"
    )
    lines.append(f"- tone_counts: {tone_summary}")

    battle_profile = analysis["battle_profile"]
    lines.append(
        f"- battle sequences=`{battle_profile['sequence_count']}` max_run=`{battle_profile['max_sequence_sentences']}` action=`{battle_profile['action_hits']}` result=`{battle_profile['result_hits']}` damage=`{battle_profile['damage_hits']}` ratio=`{battle_profile['result_ratio']}` warn=`{battle_profile['warn']}`"
    )
    for item in battle_profile.get("samples", [])[:sample_limit]:
        lines.append(
            f"- battle `S{item['start_index']}-{item['end_index']}` `L{item['start_line']}-{item['end_line']}` action=`{item['action_hits']}` result=`{item['result_hits']}` damage=`{item['damage_hits']}`"
        )

    viewpoint_profile = analysis["viewpoint_profile"]
    anchor_counts = viewpoint_profile.get("anchor_counts", {})
    anchor_summary = "，".join(f"{name} x{count}" for name, count in anchor_counts.items()) or "无"
    lines.append(
        f"- viewpoint dominant=`{viewpoint_profile['dominant_anchor'] or 'none'}` switches=`{viewpoint_profile['switch_count']}` overlaps=`{viewpoint_profile['overlap_count']}` warn=`{viewpoint_profile['warn']}`"
    )
    lines.append(f"- viewpoint_anchors: {anchor_summary}")
    for item in viewpoint_profile.get("overlaps", [])[:sample_limit]:
        lines.append(f"- `L{item['line_no']}` `{','.join(item['anchors'])}` {item['text']}")
    lines.append("")

    lines.append("## Style Fatigue")
    if analysis["style_fatigue"]:
        for item in analysis["style_fatigue"]:
            evidence = "；".join(str(value) for value in item["evidence"][:3]) or "无"
            lines.append(
                f"- `{item['status']}` `{item['family']}` x{item['count']}：{item['risk']} 建议：{item['reduce']} 证据：{evidence}"
            )
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Review Reminders")
    if analysis["review_reminders"]:
        for item in analysis["review_reminders"][:sample_limit]:
            lines.append(
                f"- `{item['priority']}` `{item['category']}` {item['title']}：{item['reason']} 动作：{item['action']}"
            )
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## Corpus Baseline")
    if profile["enabled"]:
        baseline = profile["sentence_length_baseline"]
        lines.append(
            f"- sources=`{profile['source_count']}` chars=`{profile['chars']}` draft_chars=`{profile['draft_chars']}`"
        )
        if baseline:
            lines.append(
                f"- sentence_length: p10=`{baseline['p10_chars']}` p25=`{baseline['p25_chars']}` median=`{baseline['median_chars']}` avg=`{baseline['avg_chars']}` short_ratio=`{baseline['short_ratio']}`"
            )
        learned_leads = ", ".join(
            f"{item['phrase']} x{item['count']}" for item in profile["learned_sentence_leads"][:sample_limit]
        ) or "无"
        lines.append(f"- learned_sentence_leads: {learned_leads}")
    else:
        lines.append("- 未启用")
    lines.append("")

    return "\n".join(lines) + "\n"


def build_story_summary(
    story_dir: Path,
    chapter_analyses: list[tuple[Path, dict[str, object]]],
    sample_limit: int,
) -> str:
    ordered = sorted(chapter_analyses, key=lambda item: chapter_sort_key(item[0]))

    counter_map: dict[str, collections.Counter[str]] = {
        "sentence_patterns": collections.Counter(),
        "sentence_starts": collections.Counter(),
        "judgement_endings": collections.Counter(),
        "short_phrases": collections.Counter(),
        "terms": collections.Counter(),
    }
    fatigue_counter: collections.Counter[str] = collections.Counter()
    scene_role_counter: collections.Counter[str] = collections.Counter()
    tone_counter: collections.Counter[str] = collections.Counter()
    dialogue_emotion_counter: collections.Counter[str] = collections.Counter()
    speaker_counter: collections.Counter[str] = collections.Counter()
    speaker_warn_counter: collections.Counter[str] = collections.Counter()

    lines = ["# Sentence Profile Summary", ""]
    lines.append(f"- story: `{story_dir}`")
    lines.append(f"- chapters: `{len(ordered)}`")
    lines.append("")

    lines.append("## Chapters")
    for path, analysis in ordered:
        top_pattern = analysis["sentence_patterns"][0]["phrase"] if analysis["sentence_patterns"] else "无"
        top_fragment = analysis["terms"][0]["term"] if analysis["terms"] else "无"
        lines.append(
            f"- `{path.name}` warn_sections=`{analysis['summary']['warn_sections']}` top_pattern=`{top_pattern}` top_fragment=`{top_fragment}`"
        )
        scene_role_counter[str(analysis["scene_map"]["dominant_role"])] += 1
        dominant_tone = str(analysis["tone_profile"]["dominant_tone"])
        if dominant_tone and dominant_tone != "none":
            tone_counter[dominant_tone] += 1
        dominant_emotion = str(analysis["dialogue_emotions"]["dominant_emotion"])
        if dominant_emotion and dominant_emotion != "neutral":
            dialogue_emotion_counter[dominant_emotion] += 1
        for item in analysis["character_voice"].get("speakers", [])[:sample_limit]:
            speaker_counter[str(item["speaker"])] += int(item["lines"])
        if analysis["character_voice"].get("warn"):
            for item in analysis["character_voice"].get("homogenized_pairs", [])[:sample_limit]:
                speaker_warn_counter[str(item)] += 1
        for item in analysis["style_fatigue"]:
            if item["status"] == "WARN":
                fatigue_counter[str(item["family"])] += int(item["count"])

        for key, label_key in (
            ("sentence_patterns", "phrase"),
            ("sentence_starts", "phrase"),
            ("judgement_endings", "phrase"),
            ("short_phrases", "term"),
            ("terms", "term"),
        ):
            for item in analysis[key][:sample_limit]:
                counter_map[key][str(item[label_key])] += int(item["count"])
    lines.append("")

    def add_counter_section(title: str, counter: collections.Counter[str]) -> None:
        lines.append(f"## {title}")
        if not counter:
            lines.append("- 无")
        else:
            for name, count in counter.most_common(sample_limit * 2):
                lines.append(f"- `{name}` total=`{count}`")
        lines.append("")

    add_counter_section("Story-Wide Sentence Skeletons", counter_map["sentence_patterns"])
    add_counter_section("Story-Wide Sentence Starts", counter_map["sentence_starts"])
    add_counter_section("Story-Wide Judgement Endings", counter_map["judgement_endings"])
    add_counter_section("Story-Wide Structural Phrases", counter_map["short_phrases"])
    add_counter_section("Story-Wide Frequent Fragments", counter_map["terms"])
    add_counter_section("Story-Wide Style Fatigue", fatigue_counter)
    add_counter_section("Story-Wide Scene Roles", scene_role_counter)
    add_counter_section("Story-Wide Tone Signals", tone_counter)
    add_counter_section("Story-Wide Dialogue Emotions", dialogue_emotion_counter)
    add_counter_section("Story-Wide Character Voice", speaker_counter)
    add_counter_section("Story-Wide Voice Drift Signals", speaker_warn_counter)
    return "\n".join(lines) + "\n"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Build research-oriented sentence profiles for draft chapters.")
    parser.add_argument("paths", nargs="+", help="Draft chapter files or directories")
    parser.add_argument("--sample-limit", type=int, default=8, help="Max items per section")
    parser.add_argument(
        "--output-root",
        help="Optional root directory for generated profiles; defaults to mirrored draft-stats directories",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    files = collect_chapter_files(args.paths)
    if not files:
        raise SystemExit("No draft chapter files found.")

    corpus_profile = draft_audit.build_corpus_profile(draft_audit.corpus_paths_for_targets(files))
    analyses: list[tuple[Path, dict[str, object]]] = []
    output_root = Path(args.output_root) if args.output_root else None

    for draft_path in files:
        analysis = draft_audit.analyze_path(
            draft_path,
            sample_limit=args.sample_limit,
            corpus_profile=corpus_profile,
        )
        analyses.append((draft_path, analysis))
        out_path = profile_path_for(draft_path, output_root)
        write_text(out_path, build_profile_report(draft_path, analysis, args.sample_limit))
        print(out_path)

    grouped: dict[Path, list[tuple[Path, dict[str, object]]]] = collections.defaultdict(list)
    for path, analysis in analyses:
        grouped[path.parent].append((path, analysis))

    for story_dir, items in sorted(grouped.items()):
        summary_path = profile_path_for(items[0][0], output_root).parent / "SUMMARY.md"
        write_text(summary_path, build_story_summary(story_dir, items, args.sample_limit))
        print(summary_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
