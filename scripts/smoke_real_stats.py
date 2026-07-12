#!/usr/bin/env python3
"""Real-draft stats smoke. Writes only under /tmp and prints metric JSON."""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path


DEFAULT_STORY = Path.home() / "projects/novel/novel1/drafts/story-3-foreign-whispers"
OUT_ROOT = Path("/tmp/sentinel-real-smoke-stats")


def main(argv: list[str]) -> int:
    story = Path(argv[1]).expanduser() if len(argv) > 1 else DEFAULT_STORY
    if not story.is_dir():
        print(f"SKIP: draft dir missing: {story}")
        return 0

    if OUT_ROOT.exists():
        shutil.rmtree(OUT_ROOT)
    OUT_ROOT.mkdir(parents=True, exist_ok=True)

    env = os.environ.copy()
    env["PYTHONPATH"] = "src"
    cmd = [
        sys.executable,
        "-m",
        "stats.draft",
        "--input",
        str(story),
        "--output-root",
        str(OUT_ROOT),
        "--no-corpus-learning",
        "--window-sizes",
        "2",
    ]
    started = time.perf_counter()
    subprocess.run(cmd, check=True, env=env)
    print(f"elapsed_s={time.perf_counter() - started:.3f}")

    summary = next(OUT_ROOT.rglob("SUMMARY.md"), None)
    if summary is None or not summary.is_file() or summary.stat().st_size <= 0:
        raise SystemExit(f"SUMMARY.md missing under {OUT_ROOT}")

    text = summary.read_text(encoding="utf-8")
    chapters = re.findall(
        r"^- `(ch\d+\.md)` status=`([^`]+)` warn_sections=`(\d+)` chars=`(\d+)`",
        text,
        re.M,
    )
    if not chapters:
        raise SystemExit("no chapter lines in SUMMARY")

    hard_flag_lines = [
        ln[2:]
        for ln in text.splitlines()
        if ln.startswith("- `") and "total=`" in ln
    ][:8]
    metrics = {
        "summary_path": str(summary),
        "chapter_count": len(chapters),
        "chapters": [
            {"name": name, "status": status, "warn_sections": int(warns), "chars": int(chars)}
            for name, status, warns, chars in chapters
        ],
        "total_chars": sum(int(chars) for *_rest, chars in chapters),
        "total_warn_sections": sum(int(warns) for *_rest, warns, _chars in chapters),
        "hard_flag_lines": hard_flag_lines,
        "has_priority": "## Priority" in text,
    }
    print(json.dumps(metrics, ensure_ascii=False, indent=2))
    if metrics["chapter_count"] < 3:
        raise SystemExit("chapter_count < 3")
    if metrics["total_chars"] < 5000:
        raise SystemExit("total_chars < 5000")
    if not metrics["has_priority"]:
        raise SystemExit("SUMMARY missing ## Priority")
    print("smoke-real-stats OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
