#!/usr/bin/env python3
"""Build mirrored markdown reports for arc/story/chapter plans."""

from __future__ import annotations

import argparse
import collections
from pathlib import Path

from sentinel.audit import plan as plan_audit
from sentinel.lib.cli import resolve_inputs
from sentinel.lib.io import write_text


PLAN_DIRS = ("arc-plan", "story-plan", "chapter-plan")


def summarize_runs(labels: list[str], min_run: int = 3, limit: int = 6) -> list[str]:
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
        if current not in {"unclear", "missing"} and length >= min_run:
            runs.append(f"{current} x{length} (#{start}-#{index - 1})")
        current = label
        start = index
        length = 1
    if current not in {"unclear", "missing"} and length >= min_run:
        runs.append(f"{current} x{length} (#{start}-#{start + length - 1})")
    return runs[:limit]


def stats_path_for(plan_path: Path, output_root: Path | None = None) -> Path:
    parts = list(plan_path.parts)
    for idx, part in enumerate(parts):
        if part in PLAN_DIRS:
            mirrored = list(parts)
            mirrored[idx] = f"{part}-stats"
            stats_path = Path(*mirrored)
            if output_root is None:
                return stats_path
            if idx == 0:
                return output_root / stats_path
            novel_root = Path(*parts[:idx])
            return output_root / novel_root.name / stats_path.relative_to(novel_root)
    raise ValueError(f"Path does not live under a plan directory: {plan_path}")


def collect_targets(paths: list[str]) -> list[Path]:
    targets = plan_audit.iter_targets(paths)
    return sorted(targets, key=lambda path: str(path))


def build_single_reports(
    files: list[Path],
    output_root: Path | None = None,
    single_output: Path | None = None,
) -> list[tuple[Path, str, list[plan_audit.Warning]]]:
    reports: list[tuple[Path, str, list[plan_audit.Warning]]] = []
    for path in files:
        plan_type, warnings = plan_audit.audit_file(path)
        report = plan_audit.format_report(path, plan_type, warnings)
        if single_output is not None:
            write_text(single_output, report + "\n")
        else:
            write_text(stats_path_for(path, output_root), report + "\n")
        reports.append((path, plan_type, warnings))
    return reports


def build_directory_summaries(
    reports: list[tuple[Path, str, list[plan_audit.Warning]]],
    output_root: Path | None = None,
) -> list[Path]:
    written: list[Path] = []
    grouped: dict[Path, list[tuple[Path, str, list[plan_audit.Warning]]]] = {}
    for item in reports:
        grouped.setdefault(item[0].parent, []).append(item)

    for source_dir, group in grouped.items():
        ordered = sorted(group, key=lambda item: item[0].name)
        warning_counter: collections.Counter[str] = collections.Counter()
        chapter_function_counter: collections.Counter[str] = collections.Counter()
        ending_function_counter: collections.Counter[str] = collections.Counter()
        scene_function_warning_counter: collections.Counter[str] = collections.Counter()
        role_warning_count = 0
        chapter_flow: list[str] = []
        ending_flow: list[str] = []
        for _, _, warnings in ordered:
            warning_counter.update(w.kind for w in warnings)
            if any(w.kind == "thin_role_functions" for w in warnings):
                role_warning_count += 1

        for path, plan_type, _warnings in ordered:
            text = path.read_text(encoding="utf-8")
            lines = text.splitlines()
            headings = plan_audit.parse_headings(lines)
            sections = plan_audit.collect_section_lines(lines, headings)
            if plan_type == "chapter-plan":
                _name, chapter_function_section = plan_audit.find_section(sections, ("本章功能",))
                chapter_function_text = " ".join(text for _line_no, text in plan_audit.bullet_lines(chapter_function_section))
                if not chapter_function_text:
                    chapter_function_text = plan_audit.section_text(chapter_function_section)
                chapter_function = plan_audit.detect_function_label(
                    chapter_function_text,
                    plan_audit.CHAPTER_FUNCTION_RULES,
                )
                chapter_function_counter[chapter_function] += 1
                chapter_flow.append(chapter_function)

                _ending_name, ending_section = plan_audit.find_section(sections, plan_audit.CHAPTER_ENDING_GROUP)
                ending_text = " ".join(text for _line_no, text in plan_audit.bullet_lines(ending_section))
                if not ending_text:
                    ending_text = plan_audit.section_text(ending_section)
                ending_function = plan_audit.detect_function_label(
                    ending_text,
                    plan_audit.ENDING_FUNCTION_RULES,
                )
                ending_function_counter[ending_function] += 1
                ending_flow.append(ending_function)
            if plan_type == "chapter-plan":
                for warning in _warnings:
                    if warning.kind == "scene_function_monotony":
                        scene_function_warning_counter[warning.snippet or warning.message] += 1

        summary_lines = ["# SUMMARY", "", "## Files"]
        for path, plan_type, warnings in ordered:
            status = "WARN" if warnings else "OK"
            summary_lines.append(
                f"- `{path.name}` type=`{plan_type}` status=`{status}` warnings=`{len(warnings)}`"
            )
        summary_lines.append("")

        summary_lines.append("## Priority")
        for path, plan_type, warnings in sorted(
            ordered,
            key=lambda item: (-len(item[2]), item[0].name),
        )[:5]:
            top_kinds = ", ".join(
                f"{kind} x{count}" for kind, count in collections.Counter(w.kind for w in warnings).most_common(3)
            ) or "无"
            summary_lines.append(
                f"- `{path.name}` type=`{plan_type}` warnings=`{len(warnings)}` top=`{top_kinds}`"
            )
        summary_lines.append("")

        summary_lines.append("## Warning Kinds")
        if warning_counter:
            for kind, count in warning_counter.most_common():
                summary_lines.append(f"- `{kind}` x{count}")
        else:
            summary_lines.append("- 无")
        summary_lines.append("")

        if chapter_function_counter:
            summary_lines.append("## Chapter Function Distribution")
            for name, count in chapter_function_counter.most_common():
                summary_lines.append(f"- `{name}` x{count}")
            summary_lines.append("")

        if ending_function_counter:
            summary_lines.append("## Ending Function Distribution")
            for name, count in ending_function_counter.most_common():
                summary_lines.append(f"- `{name}` x{count}")
            summary_lines.append("")

        chapter_runs = summarize_runs(chapter_flow)
        ending_runs = summarize_runs(ending_flow)
        if chapter_runs or ending_runs:
            summary_lines.append("## Function Trend Signals")
            if chapter_runs:
                for item in chapter_runs:
                    summary_lines.append(f"- `chapter_run` {item}")
            if ending_runs:
                for item in ending_runs:
                    summary_lines.append(f"- `ending_run` {item}")
            summary_lines.append("")

        if scene_function_warning_counter:
            summary_lines.append("## Repeated Scene Function Signals")
            for name, count in scene_function_warning_counter.most_common(8):
                summary_lines.append(f"- `{name}` x{count}")
            summary_lines.append("")

        if role_warning_count:
            summary_lines.append("## Role Function Signals")
            summary_lines.append(
                f"- `thin_role_functions` x{role_warning_count}：这些 Story 的“主要角色与功能”更像点名名单，缺少谁在推动/阻拦/见证/施压。"
            )

        summary_path = stats_path_for(source_dir / "SUMMARY.md", output_root)
        write_text(summary_path, "\n".join(summary_lines) + "\n")
        written.append(summary_path)

    return written


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Build mirrored markdown stats for plans.")
    parser.add_argument("paths", nargs="*", help="Plan files or directories to process")
    parser.add_argument("-i", "--input", action="append", dest="inputs", help="Input plan file or directory; may be repeated")
    parser.add_argument("-o", "--output", help="Write a single-file report; allowed only when one plan file is collected")
    parser.add_argument(
        "--output-root",
        help="Root directory for mirrored plan stats; defaults to novel-local *-stats trees",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.output and args.output_root:
        raise SystemExit("Use either --output or --output-root, not both.")

    files = collect_targets(resolve_inputs(args.paths, args.inputs))
    if not files:
        raise SystemExit("No plan files found.")

    if args.output:
        if len(files) != 1:
            raise SystemExit("--output requires exactly one collected plan file.")
        build_single_reports(files, single_output=Path(args.output))
        return 0

    output_root = Path(args.output_root) if args.output_root else None
    reports = build_single_reports(files, output_root=output_root)
    build_directory_summaries(reports, output_root=output_root)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
