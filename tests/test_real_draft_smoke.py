from __future__ import annotations

import collections
import json
import os
import re
import tempfile
import time
import unittest
from pathlib import Path

from audit import draft as draft_audit
from lib.paths import collect_chapter_files
from stats import draft as build_draft_stats


CHAPTER_NAME_RE = re.compile(r"^ch\d+\.md$", re.IGNORECASE)


def resolve_smoke_story_dir() -> Path | None:
    raw = os.environ.get("SENTINEL_SMOKE_DRAFT_DIR")
    path = Path(raw).expanduser() if raw else Path.home() / "projects/novel/novel1/drafts/story-3-foreign-whispers"
    return path if path.is_dir() else None


def build_stats_metrics(
    story_dir: Path,
    *,
    sample_limit: int = 3,
    window_sizes: list[int] | None = None,
    output_root: Path,
) -> dict[str, object]:
    window_sizes = window_sizes if window_sizes is not None else [2]
    files = collect_chapter_files([str(story_dir)])
    if not files:
        raise AssertionError(f"no chapter files under {story_dir}")

    started = time.perf_counter()
    chapter_analyses = build_draft_stats.analyze_chapters(
        files,
        sample_limit=sample_limit,
        corpus_profile=None,
    )
    build_draft_stats.build_single_reports(
        files,
        sample_limit=sample_limit,
        corpus_profile=None,
        output_root=output_root,
        chapter_analyses=chapter_analyses,
    )
    build_draft_stats.build_group_reports(
        files,
        sample_limit=sample_limit,
        window_sizes=window_sizes,
        corpus_profile=None,
        output_root=output_root,
        chapter_analyses=chapter_analyses,
    )
    elapsed_s = time.perf_counter() - started

    hard_flag_counter: collections.Counter[tuple[str, str]] = collections.Counter()
    ending_label_counter: collections.Counter[str] = collections.Counter()
    chapters: list[dict[str, object]] = []
    total_chars = 0
    total_warn_sections = 0

    for path, analysis in chapter_analyses:
        summary = analysis["summary"]
        chars = int(summary["chars"])
        warn_sections = int(summary["warn_sections"])
        total_chars += chars
        total_warn_sections += warn_sections
        chapters.append(
            {
                "name": path.name,
                "chars": chars,
                "warn_sections": warn_sections,
                "warned": bool(analysis["warned"]),
            }
        )
        for flag in analysis["hard_flags"]:
            hard_flag_counter[(str(flag["section"]), str(flag["name"]))] += int(flag["count"])
        ending_label_counter[build_draft_stats.infer_ending_label(analysis)] += 1

    summary_paths = sorted(output_root.rglob("SUMMARY.md"))
    if not summary_paths:
        raise AssertionError(f"SUMMARY.md missing under {output_root}")
    summary_path = summary_paths[0]

    return {
        "story_dir": str(story_dir),
        "chapter_count": len(chapters),
        "chapter_names": [item["name"] for item in chapters],
        "total_chars": total_chars,
        "total_warn_sections": total_warn_sections,
        "chapters": chapters,
        "top_hard_flags": [
            {"section": section, "name": name, "total": total}
            for (section, name), total in hard_flag_counter.most_common(5)
        ],
        "ending_labels": dict(ending_label_counter),
        "output_root": str(output_root),
        "summary_path": str(summary_path),
        "elapsed_s": round(elapsed_s, 3),
    }


class RealDraftSmokeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.story_dir = resolve_smoke_story_dir()
        if cls.story_dir is None:
            raise unittest.SkipTest(
                "real draft dir missing; set SENTINEL_SMOKE_DRAFT_DIR or place data at "
                "~/projects/novel/novel1/drafts/story-3-foreign-whispers"
            )

    def test_stats_smoke_writes_summary_and_metrics(self) -> None:
        with tempfile.TemporaryDirectory(prefix="sentinel-real-smoke-") as tmp:
            output_root = Path(tmp)
            metrics = build_stats_metrics(self.story_dir, output_root=output_root)
            print(json.dumps(metrics, ensure_ascii=False, indent=2))

            self.assertGreaterEqual(int(metrics["chapter_count"]), 3)
            self.assertGreaterEqual(int(metrics["total_chars"]), 5000)

            for chapter in metrics["chapters"]:
                name = str(chapter["name"])
                self.assertRegex(name, CHAPTER_NAME_RE)
                self.assertGreaterEqual(int(chapter["chars"]), 500)

            summary_path = Path(str(metrics["summary_path"]))
            self.assertTrue(summary_path.is_file())
            self.assertGreater(summary_path.stat().st_size, 0)
            summary_text = summary_path.read_text(encoding="utf-8")
            for heading in (
                "## Chapters",
                "## Story-Wide Hard Flags",
                "## Story-Wide Style Fatigue",
                "## Story-Wide Ending Functions",
                "## Priority",
            ):
                self.assertIn(heading, summary_text)

            pairs_dirs = list(output_root.rglob("pairs"))
            self.assertTrue(pairs_dirs, "expected pairs/ window directory")
            pair_files = list(pairs_dirs[0].glob("*.md"))
            self.assertEqual(len(pair_files), int(metrics["chapter_count"]) - 1)

    def test_audit_smoke_writes_markdown_report(self) -> None:
        chapter = self.story_dir / "ch01.md"
        if not chapter.is_file():
            raise unittest.SkipTest(f"missing chapter: {chapter}")

        analysis = draft_audit.analyze_path(chapter, sample_limit=3, corpus_profile=None)
        self.assertGreaterEqual(int(analysis["summary"]["chars"]), 500)

        with tempfile.TemporaryDirectory(prefix="sentinel-real-smoke-") as tmp:
            out_path = Path(tmp) / "ch01-audit.md"
            report = draft_audit.format_markdown_report(analysis, title=chapter.stem)
            out_path.write_text(report if report.endswith("\n") else report + "\n", encoding="utf-8")
            text = out_path.read_text(encoding="utf-8")
            self.assertGreater(out_path.stat().st_size, 0)
            self.assertIn("## 概览", text)
            self.assertIn("总体状态", text)
            print(
                json.dumps(
                    {
                        "chapter": str(chapter),
                        "chars": int(analysis["summary"]["chars"]),
                        "warn_sections": int(analysis["summary"]["warn_sections"]),
                        "output": str(out_path),
                    },
                    ensure_ascii=False,
                )
            )


if __name__ == "__main__":
    unittest.main()
