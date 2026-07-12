from __future__ import annotations

from functools import lru_cache
from pathlib import Path
from typing import Any, Iterable, Mapping

import yaml


DEFAULT_RULES_PATH = Path(__file__).resolve().parents[2] / "configs" / "rules" / "review.yaml"


@lru_cache(maxsize=None)
def _load_rules_cached(resolved_path: str) -> dict[str, Any]:
    try:
        payload = yaml.safe_load(Path(resolved_path).read_text(encoding="utf-8"))
    except yaml.YAMLError as exc:
        raise ValueError(f"Invalid review rules YAML: {exc}") from exc
    if not isinstance(payload, dict):
        raise ValueError("Invalid review rules YAML: root must be a mapping")
    return payload


def load_rules(path: Path | None = None) -> dict[str, Any]:
    rules_path = (path or DEFAULT_RULES_PATH).resolve()
    return _load_rules_cached(str(rules_path))


def _section_path(keys: tuple[str, ...]) -> str:
    return ".".join(keys)


def mapping_at(payload: dict[str, Any], *keys: str) -> dict[str, Any]:
    current: Any = payload
    walked: list[str] = []
    for key in keys:
        walked.append(key)
        if not isinstance(current, dict) or key not in current:
            raise ValueError(f"Missing review rules section: {_section_path(tuple(walked))}")
        current = current[key]
    if not isinstance(current, dict):
        raise ValueError(f"Missing review rules section: {_section_path(keys)}")
    return current


def list_at(payload: dict[str, Any], *keys: str) -> list[Any]:
    current: Any = payload
    walked: list[str] = []
    for key in keys:
        walked.append(key)
        if not isinstance(current, dict) or key not in current:
            raise ValueError(f"Missing review rules section: {_section_path(tuple(walked))}")
        current = current[key]
    if not isinstance(current, list):
        raise ValueError(f"Missing review rules section: {_section_path(keys)}")
    return current


def tuple_list(raw: Iterable[Any]) -> tuple[str, ...]:
    return tuple(str(item) for item in raw)


def tuple_map(raw: Mapping[str, Any]) -> dict[str, tuple[str, ...]]:
    return {str(label): tuple_list(terms) for label, terms in raw.items()}


def heading_groups(raw: Mapping[str, Any]) -> dict[str, list[tuple[str, ...]]]:
    return {str(label): [tuple_list(group) for group in groups] for label, groups in raw.items()}
