#!/usr/bin/env python3
"""Generate mirrored markdown stats for draft chapters and rolling chapter windows."""

from __future__ import annotations

import argparse
import collections
from pathlib import Path

from audit import draft as draft_audit
from lib import rules
from lib.cli import resolve_inputs
from lib.analysis import analyze_files, build_corpus_profile_for_files
from lib.io import write_text
from lib.paths import (
    chapter_sort_key,
    collect_chapter_files,
    stats_path_for as default_stats_path_for,
)


ENDING_LABEL_CONFIG = rules.mapping_at(draft_audit.DRAFT_RULES, "ending_labels")
ENDING_LABEL_RULES = rules.tuple_map(rules.mapping_at(ENDING_LABEL_CONFIG, "rules"))
ENDING_LABEL_DISPLAY = {
    str(label): str(display)
    for label, display in rules.mapping_at(ENDING_LABEL_CONFIG, "display").items()
}


ChapterAnalysis = tuple[Path, dict[str, object]]


def stats_path_for(draft_path: Path, output_root: Path | None = None) -> Path:
    stats_path = default_stats_path_for(draft_path)
    if output_root is None:
        return stats_path

    parts = list(draft_path.parts)
    try:
        idx = parts.index("drafts")
    except ValueError as exc:
        raise ValueError(f"Path does not live under drafts/: {draft_path}") from exc
    if idx == 0:
        return output_root / stats_path
    novel_root = Path(*parts[:idx])
    return output_root / novel_root.name / stats_path.relative_to(novel_root)


def ending_label_display(label: str) -> str:
    return ENDING_LABEL_DISPLAY.get(label, label)


def infer_ending_label(analysis: dict[str, object]) -> str:
    ending = analysis.get("ending", {})
    tail_excerpt = str(ending.get("tail_excerpt") or "")
    flow_terms = " ".join(str(item.get("term", "")) for item in ending.get("flow_terms", []))
    image_terms = " ".join(str(item.get("term", "")) for item in ending.get("image_terms", []))
    text = f"{tail_excerpt} {flow_terms} {image_terms}"

    scores: collections.Counter[str] = collections.Counter()
    for label, terms in ENDING_LABEL_RULES.items():
        scores[label] = sum(text.count(term) for term in terms)

    if image_terms.strip():
        scores["imagery_coda"] += len(list(ending.get("image_terms", [])))
    if flow_terms.strip():
        scores["procedure_pressure"] += len(list(ending.get("flow_terms", [])))

    ranked = [(label, count) for label, count in scores.items() if count > 0]
    if not ranked:
        return "unclear"
    ranked.sort(key=lambda item: (-item[1], item[0]))
    top_label, top_count = ranked[0]
    if len(ranked) > 1 and ranked[1][1] == top_count and ranked[1][0] != top_label:
        return "mixed"
    return top_label


def summarize_runs(labels: list[str], min_run: int = 2, limit: int = 4) -> list[str]:
    if not labels:
        return []
    runs: list[str] = []
    current = labels[0]
    count = 1
    for label in labels[1:]:
        if label == current:
            count += 1
            continue
        if count >= min_run:
            runs.append(f"{ending_label_display(current)} x{count}")
        current = label
        count = 1
    if count >= min_run:
        runs.append(f"{ending_label_display(current)} x{count}")
    return runs[:limit]


def ending_flow_text(labels: list[str], limit: int = 8) -> str:
    if not labels:
        return "无"
    return " -> ".join(ending_label_display(label) for label in labels[:limit])


def analyze_chapters(
    files: list[Path],
    sample_limit: int,
    corpus_profile: draft_audit.CorpusProfile | None = None,
) -> list[ChapterAnalysis]:
    return analyze_files(files, sample_limit=sample_limit, corpus_profile=corpus_profile)


def build_single_reports(
    files: list[Path],
    sample_limit: int,
    corpus_profile: draft_audit.CorpusProfile | None = None,
    output_root: Path | None = None,
    chapter_analyses: list[ChapterAnalysis] | None = None,
    single_output: Path | None = None,
) -> list[Path]:
    written: list[Path] = []
    analyses = chapter_analyses or analyze_chapters(files, sample_limit, corpus_profile)
    for draft_path, analysis in analyses:
        report = draft_audit.format_markdown_report(analysis, title=draft_path.stem)
        if single_output is not None:
            out_path = single_output
        else:
            out_path = stats_path_for(draft_path, output_root)
        write_text(out_path, report if report.endswith("\n") else report + "\n")
        written.append(out_path)
    return written


def analyze_paths(
    paths: list[Path],
    sample_limit: int,
    corpus_profile: draft_audit.CorpusProfile | None = None,
    template_bank: list[draft_audit.TemplateRule] | None = None,
    term_bank: list[draft_audit.TrackedTerm] | None = None,
) -> dict[str, object]:
    combined_text = "\n\n".join(path.read_text(encoding="utf-8") for path in paths)
    return draft_audit.analyze_text(
        combined_text,
        sample_limit=sample_limit,
        source=" | ".join(str(path) for path in paths),
        template_bank=template_bank
        if template_bank is not None
        else draft_audit.load_template_bank(draft_audit.DEFAULT_REVIEW_RULES_PATH),
        term_bank=term_bank if term_bank is not None else draft_audit.load_term_bank(draft_audit.DEFAULT_REVIEW_RULES_PATH),
        corpus_profile=corpus_profile,
    )


def build_window_report(
    paths: list[Path],
    sample_limit: int,
    window_name: str,
    corpus_profile: draft_audit.CorpusProfile | None = None,
    output_root: Path | None = None,
    analysis: dict[str, object] | None = None,
    template_bank: list[draft_audit.TemplateRule] | None = None,
    term_bank: list[draft_audit.TrackedTerm] | None = None,
) -> tuple[Path, str, dict[str, object]]:
    title = f"{paths[0].stem}-{paths[-1].stem}"
    window_analysis = analysis or analyze_paths(
        paths,
        sample_limit,
        corpus_profile,
        template_bank=template_bank,
        term_bank=term_bank,
    )
    parent_stats_dir = stats_path_for(paths[0], output_root).parent
    out_path = parent_stats_dir / window_name / f"{title}.md"
    report = draft_audit.format_markdown_report(window_analysis, title=title)
    return out_path, report, window_analysis


def build_group_reports(
    files: list[Path],
    sample_limit: int,
    window_sizes: list[int],
    corpus_profile: draft_audit.CorpusProfile | None = None,
    output_root: Path | None = None,
    chapter_analyses: list[ChapterAnalysis] | None = None,
) -> list[Path]:
    written: list[Path] = []
    groups: dict[Path, list[Path]] = {}
    for path in files:
        groups.setdefault(path.parent, []).append(path)

    analyses_by_path = dict(chapter_analyses or analyze_chapters(files, sample_limit, corpus_profile))
    template_bank = draft_audit.load_template_bank(draft_audit.DEFAULT_REVIEW_RULES_PATH)
    term_bank = draft_audit.load_term_bank(draft_audit.DEFAULT_REVIEW_RULES_PATH)

    for _, group_files in groups.items():
        ordered = sorted(group_files, key=chapter_sort_key)
        chapter_analyses = [(item, analyses_by_path[item]) for item in ordered]
        summary_lines = ["# SUMMARY", "", "## Chapters"]
        for item, analysis in chapter_analyses:
            summary_lines.append(
                f"- `{item.name}` status=`{'WARN' if analysis['warned'] else 'OK'}` warn_sections=`{analysis['summary']['warn_sections']}` chars=`{analysis['summary']['chars']}`"
            )
        summary_lines.append("")

        hard_flag_counter: collections.Counter[tuple[str, str]] = collections.Counter()
        style_fatigue_counter: collections.Counter[tuple[str, str]] = collections.Counter()
        sentence_pattern_counter: collections.Counter[str] = collections.Counter()
        short_phrase_counter: collections.Counter[str] = collections.Counter()
        term_counter: collections.Counter[str] = collections.Counter()
        ending_label_counter: collections.Counter[str] = collections.Counter()
        ending_labels_in_order: list[str] = []

        for _item, analysis in chapter_analyses:
            for flag in analysis["hard_flags"]:
                hard_flag_counter[(str(flag["section"]), str(flag["name"]))] += int(flag["count"])
            for item in analysis.get("style_fatigue", []):
                if str(item["status"]) != "WARN":
                    continue
                style_fatigue_counter[(str(item["status"]), str(item["family"]))] += int(item["count"])
            for pattern in analysis["sentence_patterns"]:
                sentence_pattern_counter[str(pattern["phrase"])] += int(pattern["count"])
            for phrase in analysis["short_phrases"][:20]:
                short_phrase_counter[str(phrase["term"])] += int(phrase["count"])
            for term in analysis["terms"][:20]:
                term_counter[str(term["term"])] += int(term["count"])
            ending_label = infer_ending_label(analysis)
            ending_label_counter[ending_label] += 1
            ending_labels_in_order.append(ending_label)

        summary_lines.append("## Story-Wide Hard Flags")
        if hard_flag_counter:
            for (section, name), count in hard_flag_counter.most_common(15):
                summary_lines.append(f"- `{section}` `{name}` total=`{count}`")
        else:
            summary_lines.append("- 无")
        summary_lines.append("")

        summary_lines.append("## Story-Wide Style Fatigue")
        if style_fatigue_counter:
            for (status, family), count in style_fatigue_counter.most_common(15):
                summary_lines.append(f"- `{status}` `{family}` total=`{count}`")
        else:
            summary_lines.append("- 无")
        summary_lines.append("")

        summary_lines.append("## Story-Wide Sentence Skeletons")
        if sentence_pattern_counter:
            for name, count in sentence_pattern_counter.most_common(12):
                summary_lines.append(f"- `{name}` total=`{count}`")
        else:
            summary_lines.append("- 无")
        summary_lines.append("")

        summary_lines.append("## Story-Wide Structural Phrases")
        if short_phrase_counter:
            for name, count in short_phrase_counter.most_common(15):
                summary_lines.append(f"- `{name}` total=`{count}`")
        else:
            summary_lines.append("- 无")
        summary_lines.append("")

        summary_lines.append("## Story-Wide Repeated Terms")
        if term_counter:
            for name, count in term_counter.most_common(15):
                summary_lines.append(f"- `{name}` total=`{count}`")
        else:
            summary_lines.append("- 无")
        summary_lines.append("")

        summary_lines.append("## Story-Wide Ending Functions")
        if ending_label_counter:
            for label, count in ending_label_counter.most_common(10):
                summary_lines.append(f"- `{ending_label_display(label)}` total=`{count}`")
            summary_lines.append(f"- flow=`{ending_flow_text(ending_labels_in_order)}`")
            runs = summarize_runs(ending_labels_in_order)
            summary_lines.append(f"- repeated=`{' | '.join(runs) if runs else '无'}`")
        else:
            summary_lines.append("- 无")
        summary_lines.append("")

        summary_lines.append("## Priority")
        for item, analysis in sorted(
            chapter_analyses,
            key=lambda pair: (
                -int(pair[1]["summary"]["warn_sections"]),
                -int(pair[1]["summary"]["chars"]),
                pair[0].name,
            ),
        )[:5]:
            top_templates = ", ".join(
                f"{candidate['name']} x{candidate['count']}"
                for candidate in analysis["template_candidates"][:3]
            ) or "无"
            top_fatigue = ", ".join(
                f"{fatigue['family']} x{fatigue['count']}"
                for fatigue in analysis.get("style_fatigue", [])
                if fatigue["status"] == "WARN"
            ) or "无"
            ending_label = ending_label_display(infer_ending_label(analysis))
            summary_lines.append(
                f"- `{item.name}` warn_sections=`{analysis['summary']['warn_sections']}` ending=`{ending_label}` top=`{top_templates}` fatigue=`{top_fatigue}`"
            )
        summary_lines.append("")

        for size in window_sizes:
            window_name = "pairs" if size == 2 else "triples" if size == 3 else f"window-{size}"
            summary_lines.append(f"## {window_name}")
            built_any = False
            window_summaries: list[tuple[str, dict[str, object], list[str]]] = []
            for idx in range(len(ordered) - size + 1):
                chunk = ordered[idx : idx + size]
                out_path, report, analysis = build_window_report(
                    chunk,
                    sample_limit,
                    window_name,
                    corpus_profile,
                    output_root=output_root,
                    template_bank=template_bank,
                    term_bank=term_bank,
                )
                chunk_labels = [
                    infer_ending_label(item_analysis)
                    for item_path, item_analysis in chapter_analyses
                    if item_path in chunk
                ]
                write_text(out_path, report if report.endswith("\n") else report + "\n")
                written.append(out_path)
                summary_lines.append(
                    f"- `{out_path.name}` status=`{'WARN' if analysis['warned'] else 'OK'}` warn_sections=`{analysis['summary']['warn_sections']}` chars=`{analysis['summary']['chars']}` endings=`{ending_flow_text(chunk_labels, limit=size)}`"
                )
                window_summaries.append((out_path.name, analysis, chunk_labels))
                built_any = True
            if not built_any:
                summary_lines.append("- 无")
            summary_lines.append("")
            if built_any:
                summary_lines.append(f"### {window_name}-priority")
                for name, analysis, chunk_labels in sorted(
                    window_summaries,
                    key=lambda pair: (
                        -int(pair[1]["summary"]["warn_sections"]),
                        -int(pair[1]["summary"]["chars"]),
                        pair[0],
                    ),
                )[:3]:
                    top_templates = ", ".join(
                        f"{candidate['name']} x{candidate['count']}"
                        for candidate in analysis["template_candidates"][:3]
                    ) or "无"
                    top_fatigue = ", ".join(
                        f"{fatigue['family']} x{fatigue['count']}"
                        for fatigue in analysis.get("style_fatigue", [])
                        if fatigue["status"] == "WARN"
                    ) or "无"
                    ending_runs = summarize_runs(chunk_labels)
                    summary_lines.append(
                        f"- `{name}` warn_sections=`{analysis['summary']['warn_sections']}` endings=`{ending_flow_text(chunk_labels, limit=size)}` repeated=`{' | '.join(ending_runs) if ending_runs else '无'}` top=`{top_templates}` fatigue=`{top_fatigue}`"
                    )
                summary_lines.append("")

        summary_path = stats_path_for(ordered[0], output_root).parent / "SUMMARY.md"
        write_text(summary_path, "\n".join(summary_lines) + "\n")
        written.append(summary_path)
    return written


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Build mirrored markdown stats for draft chapters.")
    parser.add_argument("paths", nargs="*", help="Draft files or directories to process")
    parser.add_argument("-i", "--input", action="append", dest="inputs", help="Input draft file or directory; may be repeated")
    parser.add_argument(
        "-o",
        "--output",
        help="Write a single-file chapter report; allowed only when one draft chapter is collected",
    )
    parser.add_argument(
        "--output-root",
        help="Root directory for generated stats; defaults to mirrored draft-stats/ tree",
    )
    parser.add_argument("--sample-limit", type=int, default=3, help="Max sample lines per rule")
    parser.add_argument(
        "--window-sizes",
        nargs="*",
        type=int,
        default=[2, 3],
        help="Rolling chapter window sizes to generate",
    )
    parser.add_argument(
        "--no-corpus-learning",
        action="store_true",
        help="Disable learned filters from existing cards, plans, and drafts.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.output and args.output_root:
        raise SystemExit("Use either --output or --output-root, not both.")

    files = collect_chapter_files(resolve_inputs(args.paths, args.inputs))
    if not files:
        raise SystemExit("No draft files found.")

    corpus_profile = None
    if not args.no_corpus_learning:
        corpus_profile = build_corpus_profile_for_files(files)
    chapter_analyses = analyze_chapters(files, args.sample_limit, corpus_profile)

    if args.output:
        if len(files) != 1:
            raise SystemExit("--output requires exactly one collected draft chapter.")
        build_single_reports(
            files,
            args.sample_limit,
            corpus_profile,
            chapter_analyses=chapter_analyses,
            single_output=Path(args.output),
        )
        return 0

    output_root = Path(args.output_root) if args.output_root else None
    build_single_reports(
        files,
        args.sample_limit,
        corpus_profile,
        output_root,
        chapter_analyses,
    )
    build_group_reports(
        files,
        args.sample_limit,
        args.window_sizes,
        corpus_profile,
        output_root,
        chapter_analyses,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
