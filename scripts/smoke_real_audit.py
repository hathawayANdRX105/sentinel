#!/usr/bin/env python3
"""Real single-chapter audit smoke. Writes only /tmp/sentinel-real-smoke-audit.md."""

from __future__ import annotations

import os
import re
import subprocess
import sys
import time
from pathlib import Path


DEFAULT_CHAPTER = Path.home() / "projects/novel/novel1/drafts/story-3-foreign-whispers/ch01.md"
OUT_PATH = Path("/tmp/sentinel-real-smoke-audit.md")


def main(argv: list[str]) -> int:
    chapter = Path(argv[1]).expanduser() if len(argv) > 1 else DEFAULT_CHAPTER
    if not chapter.is_file():
        print(f"SKIP: {chapter}")
        return 0

    env = os.environ.copy()
    env["PYTHONPATH"] = "src"
    cmd = [
        sys.executable,
        "-m",
        "audit.draft",
        "--input",
        str(chapter),
        "--format",
        "markdown",
        "--output",
        str(OUT_PATH),
        "--no-corpus-learning",
    ]
    started = time.perf_counter()
    subprocess.run(cmd, check=True, env=env)
    print(f"elapsed_s={time.perf_counter() - started:.3f}")

    if not OUT_PATH.is_file() or OUT_PATH.stat().st_size <= 0:
        raise SystemExit(f"missing output: {OUT_PATH}")
    text = OUT_PATH.read_text(encoding="utf-8")
    if "## 概览" not in text or "字数：`" not in text or "警告分区数：`" not in text:
        raise SystemExit("audit report missing expected sections")
    chars = int(re.search(r"字数：`(\d+)`", text).group(1))
    warns = int(re.search(r"警告分区数：`(\d+)`", text).group(1))
    print({"chars": chars, "warn_sections": warns, "output": str(OUT_PATH)})
    if chars < 500:
        raise SystemExit("chars < 500")
    print("smoke-real-audit OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
