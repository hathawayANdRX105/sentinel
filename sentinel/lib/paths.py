from __future__ import annotations

import re
from pathlib import Path

from sentinel.audit import draft as draft_audit


CHAPTER_RE = re.compile(r"ch(\d+)", re.IGNORECASE)


def chapter_sort_key(path: Path) -> tuple[int, str]:
    match = CHAPTER_RE.search(path.stem)
    if match:
        return int(match.group(1)), path.name
    return 9999, path.name


def stats_path_for(draft_path: Path) -> Path:
    parts = list(draft_path.parts)
    try:
        idx = parts.index("drafts")
    except ValueError as exc:
        raise ValueError(f"Path does not live under drafts/: {draft_path}") from exc
    parts[idx] = "draft-stats"
    return Path(*parts)


def collect_chapter_files(paths: list[str]) -> list[Path]:
    files = draft_audit.iter_target_files(paths)
    chapter_files = [path for path in files if CHAPTER_RE.search(path.stem)]
    return sorted(chapter_files, key=lambda path: (str(path.parent), chapter_sort_key(path)))


def novel_dir_for_draft(draft_path: Path) -> Path | None:
    for parent in draft_path.parents:
        if parent.name == "drafts":
            return parent.parent
    return None

