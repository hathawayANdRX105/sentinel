#!/usr/bin/env python3
"""Build mirrored markdown reports for concept cards."""

from __future__ import annotations

import argparse
import collections
from pathlib import Path

from audit import concept as concept_audit


def stats_path_for(card_path: Path) -> Path:
    parts = list(card_path.parts)
    for idx, part in enumerate(parts):
        if part == "cards":
            parts[idx] = "card-stats"
            return Path(*parts)
    raise ValueError(f"Path does not live under concept/cards: {card_path}")


def write_text(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def collect_targets(paths: list[str], include_templates: bool) -> list[Path]:
    return sorted(concept_audit.iter_targets(paths, include_templates), key=lambda path: str(path))


def build_single_reports(files: list[Path]) -> list[tuple[Path, list[concept_audit.Warning]]]:
    reports: list[tuple[Path, list[concept_audit.Warning]]] = []
    for path in files:
        warnings = concept_audit.audit_card(path)
        out_path = stats_path_for(path)
        report = concept_audit.format_report(path, warnings)
        write_text(out_path, report)
        reports.append((path, warnings))
    return reports


def build_directory_summaries(reports: list[tuple[Path, list[concept_audit.Warning]]]) -> list[Path]:
    written: list[Path] = []
    grouped: dict[Path, list[tuple[Path, list[concept_audit.Warning]]]] = {}
    for item in reports:
        grouped.setdefault(item[0].parent, []).append(item)

    for source_dir, group in grouped.items():
        ordered = sorted(group, key=lambda item: item[0].name)
        warning_counter: collections.Counter[str] = collections.Counter()
        for _, warnings in ordered:
            warning_counter.update(w.kind for w in warnings)

        summary_lines = ["# SUMMARY", "", "## Cards"]
        for path, warnings in ordered:
            status = "WARN" if warnings else "OK"
            summary_lines.append(f"- `{path.name}` status=`{status}` warnings=`{len(warnings)}`")
        summary_lines.append("")

        summary_lines.append("## Priority")
        for path, warnings in sorted(ordered, key=lambda item: (-len(item[1]), item[0].name))[:5]:
            top_kinds = ", ".join(
                f"{kind} x{count}" for kind, count in collections.Counter(w.kind for w in warnings).most_common(3)
            ) or "无"
            summary_lines.append(f"- `{path.name}` warnings=`{len(warnings)}` top=`{top_kinds}`")
        summary_lines.append("")

        summary_lines.append("## Warning Kinds")
        if warning_counter:
            for kind, count in warning_counter.most_common():
                summary_lines.append(f"- `{kind}` x{count}")
        else:
            summary_lines.append("- 无")

        summary_path = stats_path_for(source_dir / "SUMMARY.md")
        write_text(summary_path, "\n".join(summary_lines))
        written.append(summary_path)

    return written


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Build mirrored markdown stats for concept cards.")
    parser.add_argument("paths", nargs="+", help="Card files or directories to process")
    parser.add_argument("--include-templates", action="store_true", help="Include template files in the audit")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    files = collect_targets(args.paths, args.include_templates)
    if not files:
        raise SystemExit("No concept cards found.")

    reports = build_single_reports(files)
    build_directory_summaries(reports)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
