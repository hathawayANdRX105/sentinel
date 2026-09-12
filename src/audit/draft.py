#!/usr/bin/env python3
"""Audit draft chapters for repeated wording, sentence templates, dialogue, and punctuation."""

from __future__ import annotations

import argparse
import collections
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from functools import lru_cache
from typing import Any, Iterable

from lib import rules
from lib.cli import resolve_inputs
from lib.io import write_text


RULES = rules.load_rules()
DRAFT_RULES = rules.mapping_at(RULES, "draft")
DRAFT_REGEX_RULES = rules.mapping_at(DRAFT_RULES, "regex_rules")
DRAFT_THRESHOLDS = rules.mapping_at(DRAFT_RULES, "thresholds")
DRAFT_LEARNED_WINDOW = rules.mapping_at(DRAFT_RULES, "learned_term_window")
DRAFT_SPEAKER = rules.mapping_at(DRAFT_RULES, "speaker")
DRAFT_LEXICON = rules.mapping_at(DRAFT_RULES, "lexicon")


def _rule_list(section: str) -> list[dict[str, Any]]:
    return [dict(item) for item in rules.list_at(DRAFT_REGEX_RULES, section)]


def _tuple_setting(section: str) -> tuple[str, ...]:
    return rules.tuple_list(rules.list_at(DRAFT_LEXICON, section))


def _set_setting(section: str) -> set[str]:
    return set(_tuple_setting(section))


TOKEN_RULES = _rule_list("tokens")
PATTERN_RULES = _rule_list("patterns")
PHRASE_RULES = _rule_list("phrases")
MODIFIER_RULES = _rule_list("modifiers")
PUNCTUATION_RULES = _rule_list("punctuation")
COMBO_RULES = _rule_list("punctuation_combos")


HARDCODED_TEMPLATE_RULE_NAMES = {
    str(item.get("name", ""))
    for item in PATTERN_RULES + PHRASE_RULES + TOKEN_RULES + PUNCTUATION_RULES + COMBO_RULES + MODIFIER_RULES
    if item.get("name")
} | {
    str(item.get("label", ""))
    for item in PATTERN_RULES
    if item.get("label")
}


DEFAULT_REVIEW_RULES_PATH = rules.DEFAULT_RULES_PATH

DEFAULT_CORPUS_PARTS = tuple(
    tuple(rules.tuple_list(item))
    for item in rules.list_at(DRAFT_THRESHOLDS, "default_corpus_parts")
)
SHORT_SENTENCE_MAX_CHARS = int(DRAFT_THRESHOLDS["short_sentence_max_chars"])
VERY_SHORT_SENTENCE_MAX_CHARS = int(DRAFT_THRESHOLDS["very_short_sentence_max_chars"])
SHORT_SENTENCE_RUN_MAX_CHARS = int(DRAFT_THRESHOLDS["short_sentence_run_max_chars"])
SHORT_SENTENCE_RUN_MIN = int(DRAFT_THRESHOLDS["short_sentence_run_min"])
TRACKED_TERM_WINDOW_SIZE = int(DRAFT_THRESHOLDS["tracked_term_window_size"])
TRACKED_TERM_WINDOW_MIN_TOP = int(DRAFT_THRESHOLDS["tracked_term_window_min_top"])
TRACKED_TERM_WINDOW_MIN_TOTAL = int(DRAFT_THRESHOLDS["tracked_term_window_min_total"])
TRACKED_TERM_WINDOW_MIN_CATEGORY = int(DRAFT_THRESHOLDS["tracked_term_window_min_category"])
TRACKED_TERM_WINDOW_CATEGORIES = set(rules.tuple_list(rules.list_at(DRAFT_LEARNED_WINDOW, "categories")))
LEARNED_TERM_WINDOW_NOISE_SUFFIXES = rules.tuple_list(rules.list_at(DRAFT_LEARNED_WINDOW, "noise_suffixes"))
LEARNED_TERM_WINDOW_NOISE_PREFIXES = rules.tuple_list(rules.list_at(DRAFT_LEARNED_WINDOW, "noise_prefixes"))
LEARNED_TERM_WINDOW_NOISE_CHARS = set(rules.tuple_list(rules.list_at(DRAFT_LEARNED_WINDOW, "noise_chars")))
LEARNED_FILTER_LIMIT = int(DRAFT_THRESHOLDS["learned_filter_limit"])


SPEAKER_PATTERNS = [re.compile(pattern) for pattern in rules.list_at(DRAFT_SPEAKER, "patterns")]
SPEAKER_LINE_PATTERNS = [re.compile(pattern) for pattern in rules.list_at(DRAFT_SPEAKER, "line_patterns")]
SPEAKER_SUFFIXES = rules.tuple_list(rules.list_at(DRAFT_SPEAKER, "suffixes"))
CONNECTIVE_SENTENCE_PATTERNS = tuple(
    (str(item["label"]), re.compile(str(item["pattern"])))
    for item in rules.list_at(DRAFT_RULES, "connective_sentence_patterns")
    if isinstance(item, dict) and "label" in item and "pattern" in item
)


QUOTE_LINE = re.compile(r'^\s*[“"【].*')
SENTENCE_SPLIT = re.compile(r"[。！？!?]+|\n+")
SENTENCE_CHUNK = re.compile(r"[^。！？!?\n]+(?:[。！？!?]+|$)")
COUNTABLE_CHAR = re.compile(r"[\u4e00-\u9fffA-Za-z0-9]")
LEADING_PUNCT = "“”\"'【】《》〈〉（）()[]「」『』，,：:；;、 "
WORD_STOPLIST = _set_setting("word_stoplist")
CORPUS_STOP_TERMS = _set_setting("corpus_stop_terms")
MARKDOWN_NOISE_LINE = re.compile(str(rules.mapping_at(DRAFT_RULES, "markdown_noise_line")["pattern"]))
STRUCTURE_CHARS = set(str(DRAFT_LEXICON["structure_chars"]))
SUBJECT_LEADS = _tuple_setting("subject_leads")
PARAGRAPH_LEADS = _tuple_setting("paragraph_leads")
ALLOWED_SHORT_CORPUS_TERMS = _set_setting("allowed_short_corpus_terms")
ENDING_IMAGE_TERMS = _tuple_setting("ending_image_terms")
ENDING_FLOW_TERMS = _tuple_setting("ending_flow_terms")
JUDGEMENT_ENDINGS = _tuple_setting("judgement_endings")
FATIGUE_WINDOW_JUDGEMENT_TERMS = _tuple_setting("fatigue_window_judgement_terms")
FATIGUE_WINDOW_STICKY_TERMS = _tuple_setting("fatigue_window_sticky_terms")
FATIGUE_WINDOW_BA_RE = re.compile(r"把[^，。！？!?]{1,24}")
JUDGEMENT_CONTEXT_TERMS = _tuple_setting("judgement_context_terms")
SHORT_ROLE_INFO_TERMS = _tuple_setting("short_role_info_terms")
SHORT_ROLE_EMOTION_TERMS = _tuple_setting("short_role_emotion_terms")
BA_CLUE_TERMS = _tuple_setting("ba_clue_terms")
BA_EMOTION_TERMS = _tuple_setting("ba_emotion_terms")
BA_SCENE_TERMS = _tuple_setting("ba_scene_terms")
BA_SCENE_VERBS = _tuple_setting("ba_scene_verbs")
BA_TOOL_TERMS = _tuple_setting("ba_tool_terms")
DIALOGUE_AXIS_ACTION_TERMS = _tuple_setting("dialogue_axis_action_terms")
DIALOGUE_AXIS_ENV_TERMS = _tuple_setting("dialogue_axis_env_terms")
DIALOGUE_AXIS_DEVICE_TERMS = _tuple_setting("dialogue_axis_device_terms")
DIALOGUE_AXIS_THIRD_PARTY_TERMS = _tuple_setting("dialogue_axis_third_party_terms")
CLAUSE_SPLIT = re.compile(r"[，；：]")
ADJECTIVE_HINTS = _tuple_setting("adjective_hints")
VERB_HINTS = _tuple_setting("verb_hints")
SCENE_BREAK_LEADS = _tuple_setting("scene_break_leads")
PARAGRAPH_INFO_TERMS = _tuple_setting("paragraph_info_terms")
MENTAL_STATE_TERMS = _tuple_setting("mental_state_terms")
DIALOGUE_EMOTION_RULES = rules.tuple_map(rules.mapping_at(DRAFT_LEXICON, "dialogue_emotion_rules"))
CHARACTER_NAME_STOPLIST = _set_setting("character_name_stoplist")
TONE_RULES = rules.tuple_map(rules.mapping_at(DRAFT_LEXICON, "tone_rules"))
BATTLE_ACTION_TERMS = _tuple_setting("battle_action_terms")
BATTLE_RESULT_TERMS = _tuple_setting("battle_result_terms")
BATTLE_DAMAGE_TERMS = _tuple_setting("battle_damage_terms")
BATTLE_MOVEMENT_TERMS = _tuple_setting("battle_movement_terms")


@dataclass
class Hit:
    line_no: int
    snippet: str


@dataclass
class TemplateRule:
    name: str
    pattern: str
    note: str
    max_per_10k: float
    category: str = "custom"


@dataclass
class TrackedTerm:
    category: str
    term: str
    max_per_10k: float
    note: str


@dataclass
class SentenceInfo:
    index: int
    line_no: int
    text: str
    chars: int


@dataclass
class ParagraphInfo:
    index: int
    line_start: int
    line_end: int
    text: str
    chars: int
    is_dialogue: bool


@dataclass
class LearnedPattern:
    category: str
    name: str
    count: int
    corpus_per_10k: float
    max_per_10k: float
    note: str


@dataclass
class CorpusProfile:
    source_count: int
    chars: int
    draft_chars: int
    learned_terms: list[LearnedPattern]
    learned_style_phrases: list[LearnedPattern]
    learned_sentence_leads: list[dict[str, object]]
    learned_aa_bb_shapes: list[dict[str, object]]
    sentence_length_baseline: dict[str, object]


def iter_target_files(paths: Iterable[str]) -> list[Path]:
    out: list[Path] = []
    for raw in paths:
        path = Path(raw)
        if path.is_dir():
            out.extend(
                sorted(
                    p
                    for p in path.rglob("*")
                    if p.suffix in {".md", ".txt"} and not _is_generated_or_template(p)
                )
            )
        elif path.is_file():
            out.append(path)
    return out


def _is_generated_or_template(path: Path) -> bool:
    parts = set(path.parts)
    return bool(
        "_templates" in parts
        or "draft-stats" in parts
        or "story-plan-stats" in parts
        or "chapter-plan-stats" in parts
        or "arc-plan-stats" in parts
        or "card-stats" in parts
        or path.name.upper().startswith("README")
        or path.name == "progression.md"
    )


def corpus_paths_for_targets(paths: Iterable[Path]) -> list[Path]:
    roots: dict[str, Path] = {}
    for path in paths:
        parts = path.parts
        for marker in ("drafts", "concept", "arc-plan", "story-plan", "chapter-plan"):
            if marker not in parts:
                continue
            idx = parts.index(marker)
            if idx > 0:
                novel_root = Path(*parts[:idx])
                roots[str(novel_root)] = novel_root
            break

    corpus_paths: list[Path] = []
    for novel_root in roots.values():
        for relative in DEFAULT_CORPUS_PARTS:
            candidate = novel_root.joinpath(*relative)
            if candidate.exists():
                corpus_paths.append(candidate)
    return corpus_paths


def clean_corpus_text(path: Path) -> str:
    lines: list[str] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        if MARKDOWN_NOISE_LINE.match(stripped) or re.fullmatch(r"[-:| ]+", stripped):
            continue
        stripped = re.sub(r"^[-*]\s*", "", stripped)
        stripped = re.sub(r"`([^`]+)`", r"\1", stripped)
        stripped = re.sub(r"\*\*([^*]+)\*\*", r"\1", stripped)
        if stripped:
            lines.append(stripped)
    return "\n".join(lines)


def prose_char_count(text: str) -> int:
    return len(COUNTABLE_CHAR.findall(text))


def _percentile(values: list[int], pct: float) -> int:
    if not values:
        return 0
    ordered = sorted(values)
    index = round((len(ordered) - 1) * pct)
    return ordered[index]


def density(count: int, chars: int) -> float:
    if chars <= 0:
        return 0.0
    return count * 10000.0 / chars


@lru_cache(maxsize=4096)
def _compile_pattern(pattern: str) -> re.Pattern[str]:
    return re.compile(pattern)


def find_hits(pattern: str, lines: list[str], sample_limit: int) -> tuple[int, list[Hit]]:
    regex = _compile_pattern(pattern)
    count = 0
    hits: list[Hit] = []
    for line_no, line in enumerate(lines, start=1):
        matches = list(regex.finditer(line))
        if not matches:
            continue
        count += len(matches)
        if len(hits) < sample_limit:
            hits.append(Hit(line_no, line.strip()))
    return count, hits


def quote_ratio(text: str) -> float:
    if not text:
        return 0.0
    quote_chars = sum(1 for ch in text if ch in '“”"【】')
    return quote_chars / max(len(text), 1)


def split_sentences(text: str) -> list[str]:
    out: list[str] = []
    for chunk in SENTENCE_SPLIT.split(text):
        stripped = chunk.strip()
        if stripped:
            out.append(stripped)
    return out


def split_sentence_infos(text: str) -> list[SentenceInfo]:
    infos: list[SentenceInfo] = []
    for line_no, line in enumerate(text.splitlines(), start=1):
        stripped_line = line.strip()
        if not stripped_line or MARKDOWN_NOISE_LINE.match(stripped_line):
            continue
        for match in SENTENCE_CHUNK.finditer(line):
            sentence = match.group(0).strip()
            sentence = sentence.strip("。！？!?")
            if not sentence:
                continue
            chars = prose_char_count(sentence)
            if chars <= 0:
                continue
            infos.append(
                SentenceInfo(
                    index=len(infos) + 1,
                    line_no=line_no,
                    text=sentence,
                    chars=chars,
                )
            )
    return infos


def split_paragraph_infos(text: str) -> list[ParagraphInfo]:
    infos: list[ParagraphInfo] = []
    lines = text.splitlines()
    chunk_lines: list[str] = []
    start_line = 1
    for line_no, raw_line in enumerate(lines, start=1):
        if raw_line.strip():
            if not chunk_lines:
                start_line = line_no
            chunk_lines.append(raw_line.rstrip())
            continue
        if chunk_lines:
            paragraph_text = "\n".join(chunk_lines).strip()
            if paragraph_text:
                paragraph_lines = [line.strip() for line in paragraph_text.splitlines() if line.strip()]
                is_dialogue = bool(
                    paragraph_lines
                    and (
                        all(QUOTE_LINE.match(line) for line in paragraph_lines)
                        or (len(paragraph_lines) == 1 and quote_ratio(paragraph_lines[0]) > 0.02 and "“" in paragraph_lines[0])
                    )
                )
                infos.append(
                    ParagraphInfo(
                        index=len(infos) + 1,
                        line_start=start_line,
                        line_end=line_no - 1,
                        text=paragraph_text,
                        chars=prose_char_count(paragraph_text),
                        is_dialogue=is_dialogue,
                    )
                )
            chunk_lines = []
    if chunk_lines:
        paragraph_text = "\n".join(chunk_lines).strip()
        if paragraph_text:
            paragraph_lines = [line.strip() for line in paragraph_text.splitlines() if line.strip()]
            is_dialogue = bool(
                paragraph_lines
                and (
                    all(QUOTE_LINE.match(line) for line in paragraph_lines)
                    or (len(paragraph_lines) == 1 and quote_ratio(paragraph_lines[0]) > 0.02 and "“" in paragraph_lines[0])
                )
            )
            infos.append(
                ParagraphInfo(
                    index=len(infos) + 1,
                    line_start=start_line,
                    line_end=len(lines),
                    text=paragraph_text,
                    chars=prose_char_count(paragraph_text),
                    is_dialogue=is_dialogue,
                )
            )
    return infos


def is_dialogue_like(sentence: str) -> bool:
    raw = sentence.strip()
    return bool(raw.startswith(("“", "”", "\"", "【Pi】", "「", "『")) or "：“" in raw or "】" in raw[:8])


def classify_short_sentence_role(sentence: str) -> str:
    raw = sentence.strip()
    stripped = raw.lstrip(LEADING_PUNCT)
    if is_dialogue_like(raw):
        return "对白"
    if any(term in stripped for term in JUDGEMENT_CONTEXT_TERMS) or any(
        stripped.endswith(ending) for ending in JUDGEMENT_ENDINGS
    ):
        return "判断"
    if any(term in stripped for term in SHORT_ROLE_INFO_TERMS):
        return "信息"
    if any(term in stripped for term in SHORT_ROLE_EMOTION_TERMS):
        return "情绪"
    if FATIGUE_WINDOW_BA_RE.search(stripped) or any(verb in stripped for verb in VERB_HINTS):
        return "动作"
    if stripped.count("，") + stripped.count("、") >= 2:
        return "清单"
    return "其他"


def summarize_short_roles(sentence_infos: list[SentenceInfo]) -> list[dict[str, object]]:
    counts = collections.Counter(classify_short_sentence_role(item.text) for item in sentence_infos)
    order = {"对白": 0, "动作": 1, "信息": 2, "判断": 3, "情绪": 4, "清单": 5, "其他": 6}
    return [
        {"role": role, "count": count}
        for role, count in sorted(counts.items(), key=lambda item: (order.get(item[0], 99), -item[1]))
    ]


def suggest_short_run_action(roles: list[dict[str, object]]) -> str:
    role_counts = {str(item["role"]): int(item["count"]) for item in roles}
    top_role = max(role_counts, key=role_counts.get, default="其他")
    if top_role == "对白":
        return "保留最锋利的一两句，其余用动作、环境声或第三方反应打断。"
    if top_role == "动作":
        return "保留关键动作，补动作因果、阻力或结果，避免操作日志。"
    if top_role == "信息":
        return "把信息拆成发现、误读、排除和后果，不要连续报材料。"
    if top_role == "判断":
        return "人物台词可留；旁白判断优先换成证据、动作或误读。"
    if top_role == "情绪":
        return "用身体反应、声音和场面反馈承载情绪，不要连续短评。"
    if top_role == "清单":
        return "保留一个清单节奏，其余并入动作过程或视角变化。"
    return "先判断这些短句是否都必要；只保留一个节奏点，其余展开。"


def format_short_roles(roles: list[dict[str, object]], *, separator: str = "，") -> str:
    return separator.join(f"{item['role']} x{item['count']}" for item in roles)


def tracked_term_category_label(category: str) -> str:
    labels = {
        "characters": "人物名",
        "places": "地点名",
        "devices": "设备名",
        "actions": "动作短语",
        "atmosphere": "氛围词",
        "style": "意象词",
        "learned_term": "语料高频词",
    }
    return labels.get(category, category)


def suggest_tracked_term_window_action(category: str) -> str:
    if category == "characters":
        return "用称谓、站位、动作或视角入口替换连续点名。"
    if category == "places":
        return "换成具体空间部件、声音、光线或行动路径，不要连续报地点名。"
    if category == "devices":
        return "让设备通过状态变化、故障后果或人物反应出现，不要连续点屏幕/终端。"
    if category == "actions":
        return "把重复动作拆成目的、阻力和结果，或换成身体反应。"
    if category in {"atmosphere", "style"}:
        return "保留最有用的一处意象，其余改成可见场面变化。"
    if category == "learned_term":
        return "先判断它是临时角色、物件还是概念；用称谓、位置、动作和后果分担点名。"
    return "检查同一词是否在替代镜头调度；优先换成动作、物件或视角变化。"


def format_tracked_term_counts(terms: list[dict[str, object]], *, separator: str = "，") -> str:
    return separator.join(
        f"{item['term']} x{item['count']}({tracked_term_category_label(str(item['category']))})"
        for item in terms
    )


def is_learned_term_window_candidate(term: str, known_terms: set[str]) -> bool:
    if len(term) < 2:
        return False
    if term.startswith(LEARNED_TERM_WINDOW_NOISE_PREFIXES):
        return False
    if term.endswith(LEARNED_TERM_WINDOW_NOISE_SUFFIXES):
        return False
    if any(char in term for char in LEARNED_TERM_WINDOW_NOISE_CHARS):
        return False
    for known in known_terms:
        if known and known != term and known in term:
            return False
    return True


def build_tracked_term_windows(
    tracked_terms: list[TrackedTerm],
    learned_terms: list[LearnedPattern],
    sentence_infos: list[SentenceInfo],
    *,
    sample_limit: int,
) -> list[dict[str, object]]:
    if len(sentence_infos) < 3:
        return []

    rules: list[dict[str, str]] = []
    seen_terms: set[str] = set()
    for rule in tracked_terms:
        if not rule.term or rule.term in seen_terms or rule.category not in TRACKED_TERM_WINDOW_CATEGORIES:
            continue
        rules.append(
            {
                "term": rule.term,
                "category": rule.category,
                "note": rule.note,
            }
        )
        seen_terms.add(rule.term)
    for rule in learned_terms:
        if (
            not rule.name
            or rule.name in seen_terms
            or rule.category not in TRACKED_TERM_WINDOW_CATEGORIES
            or not is_learned_term_window_candidate(rule.name, seen_terms)
        ):
            continue
        rules.append(
            {
                "term": rule.name,
                "category": rule.category,
                "note": rule.note,
            }
        )
        seen_terms.add(rule.name)
    if not rules:
        return []

    candidates: list[dict[str, object]] = []
    window_size = TRACKED_TERM_WINDOW_SIZE
    for start in range(0, max(len(sentence_infos) - window_size + 1, 1)):
        chunk = sentence_infos[start : start + window_size]
        if len(chunk) < 3:
            continue

        term_counts: collections.Counter[str] = collections.Counter()
        term_categories: dict[str, str] = {}
        term_notes: dict[str, str] = {}
        for sentence in chunk:
            for rule in rules:
                term = rule["term"]
                count = sentence.text.count(term)
                if count <= 0:
                    continue
                term_counts[term] += count
                term_categories[term] = rule["category"]
                term_notes[term] = rule["note"]
        if not term_counts:
            continue

        top_term, top_count = term_counts.most_common(1)[0]
        total_hits = sum(term_counts.values())
        category_counts: collections.Counter[str] = collections.Counter()
        for term, count in term_counts.items():
            category_counts[term_categories.get(term, "tracked")] += count
        top_category, top_category_count = category_counts.most_common(1)[0]

        reasons: list[str] = []
        if top_count >= TRACKED_TERM_WINDOW_MIN_TOP:
            reasons.append(f"同词 {top_term} x{top_count}/{len(chunk)}")
        if total_hits >= TRACKED_TERM_WINDOW_MIN_TOTAL:
            reasons.append(f"跟踪词合计 {total_hits}/{len(chunk)}")
        if top_category_count >= TRACKED_TERM_WINDOW_MIN_CATEGORY:
            reasons.append(
                f"{tracked_term_category_label(top_category)} x{top_category_count}/{len(chunk)}"
            )
        if not reasons:
            continue

        top_terms = [
            {
                "term": term,
                "count": count,
                "category": term_categories.get(term, "tracked"),
                "note": term_notes.get(term, ""),
            }
            for term, count in term_counts.most_common(5)
        ]
        candidates.append(
            {
                "start_index": chunk[0].index,
                "end_index": chunk[-1].index,
                "start_line": chunk[0].line_no,
                "end_line": chunk[-1].line_no,
                "window_size": len(chunk),
                "score": top_count * 3 + total_hits + top_category_count,
                "total_hits": total_hits,
                "top_term": top_term,
                "top_count": top_count,
                "top_category": top_category,
                "reasons": reasons,
                "terms": top_terms,
                "suggestion": suggest_tracked_term_window_action(top_category),
                "sample": [item.text for item in chunk],
            }
        )

    candidates.sort(
        key=lambda item: (
            -int(item["score"]),
            -int(item["top_count"]),
            int(item["start_index"]),
            str(item["top_term"]),
        )
    )
    total_candidates = len(candidates)
    kept: list[dict[str, object]] = []
    kept_spans_by_term: dict[str, list[tuple[int, int]]] = {}
    for candidate in candidates:
        term = str(candidate["top_term"])
        span = (int(candidate["start_index"]), int(candidate["end_index"]))
        overlaps = any(
            not (span[1] < existing[0] or span[0] > existing[1])
            for existing in kept_spans_by_term.get(term, [])
        )
        if overlaps:
            continue
        kept.append(candidate)
        kept_spans_by_term.setdefault(term, []).append(span)
        if len(kept) >= sample_limit:
            break

    for item in kept:
        item["total_candidates"] = total_candidates
    return kept


def classify_ba_operation(snippet: str) -> str:
    if any(term in snippet for term in BA_EMOTION_TERMS):
        return "情绪动作"
    if any(term in snippet for term in BA_CLUE_TERMS):
        return "线索操作"
    if any(term in snippet for term in BA_SCENE_TERMS) or any(term in snippet for term in BA_SCENE_VERBS):
        return "场面调度"
    if any(term in snippet for term in BA_TOOL_TERMS):
        return "工具操作"
    return "动作操作"


def suggest_ba_operation_action(role: str) -> str:
    if role == "工具操作":
        return "必要工具动作可保留，但连续出现时要补结果、阻力或人物反应。"
    if role == "线索操作":
        return "把线索操作拆成发现、误读、排除和后果，少写整理流程。"
    if role == "情绪动作":
        return "优先改成身体反应、声音变化或他人误读，不要只把情绪推来推去。"
    if role == "场面调度":
        return "保留能改变画面的句子，其余改成环境后果或视角移动。"
    return "检查这个把字句是否只是操作日志；能换结果句、被动阻力或场面反馈就换。"


def build_ba_operation_contexts(
    sentence_infos: list[SentenceInfo],
    *,
    sample_limit: int,
) -> list[dict[str, object]]:
    buckets: dict[str, dict[str, object]] = {}
    total = 0
    for sentence in sentence_infos:
        for match in FATIGUE_WINDOW_BA_RE.finditer(sentence.text):
            snippet = match.group(0)
            role = classify_ba_operation(snippet)
            bucket = buckets.setdefault(
                role,
                {
                    "role": role,
                    "count": 0,
                    "samples": [],
                    "suggestion": suggest_ba_operation_action(role),
                },
            )
            bucket["count"] = int(bucket["count"]) + 1
            total += 1
            samples = bucket["samples"]
            if isinstance(samples, list) and len(samples) < sample_limit:
                samples.append(
                    {
                        "index": sentence.index,
                        "line_no": sentence.line_no,
                        "snippet": snippet,
                        "sentence": sentence.text,
                    }
                )

    order = {"线索操作": 0, "情绪动作": 1, "动作操作": 2, "场面调度": 3, "工具操作": 4}
    contexts: list[dict[str, object]] = []
    for role, bucket in sorted(
        buckets.items(),
        key=lambda item: (order.get(item[0], 99), -int(item[1]["count"])),
    ):
        count = int(bucket["count"])
        warn = count >= 2 if role in {"线索操作", "情绪动作", "动作操作"} else count >= 4
        bucket["warn"] = warn
        bucket["total"] = total
        contexts.append(bucket)
    return contexts


def build_sentence_length_profile(sentence_infos: list[SentenceInfo]) -> dict[str, object]:
    lengths = [item.chars for item in sentence_infos]
    short_items = [
        item for item in sentence_infos if item.chars <= SHORT_SENTENCE_MAX_CHARS
    ]
    very_short_items = [
        item for item in sentence_infos if item.chars <= VERY_SHORT_SENTENCE_MAX_CHARS
    ]

    runs: list[dict[str, object]] = []
    current: list[SentenceInfo] = []

    def flush() -> None:
        nonlocal current
        if len(current) >= SHORT_SENTENCE_RUN_MIN:
            roles = summarize_short_roles(current)
            runs.append(
                {
                    "start_index": current[0].index,
                    "end_index": current[-1].index,
                    "start_line": current[0].line_no,
                    "end_line": current[-1].line_no,
                    "avg_chars": round(sum(item.chars for item in current) / len(current), 2),
                    "roles": roles,
                    "suggestion": suggest_short_run_action(roles),
                    "sample": [item.text for item in current[:5]],
                }
            )
        current = []

    previous_index = 0
    for item in sentence_infos:
        if item.chars <= SHORT_SENTENCE_RUN_MAX_CHARS and (
            not current or item.index == previous_index + 1
        ):
            current.append(item)
        else:
            flush()
            if item.chars <= SHORT_SENTENCE_RUN_MAX_CHARS:
                current.append(item)
        previous_index = item.index
    flush()

    short_ratio = len(short_items) / max(len(sentence_infos), 1)
    warn = bool(
        len(very_short_items) >= 3
        or len(runs) >= 1
        or short_ratio >= 0.18
    )
    return {
        "count": len(sentence_infos),
        "min_chars": min(lengths) if lengths else 0,
        "p10_chars": _percentile(lengths, 0.10),
        "p25_chars": _percentile(lengths, 0.25),
        "median_chars": _percentile(lengths, 0.50),
        "avg_chars": round(sum(lengths) / max(len(lengths), 1), 2),
        "max_chars": max(lengths) if lengths else 0,
        "short_count": len(short_items),
        "very_short_count": len(very_short_items),
        "short_ratio": round(short_ratio, 4),
        "warn": warn,
        "short_sentences": [
            {
                "index": item.index,
                "line_no": item.line_no,
                "chars": item.chars,
                "text": item.text,
            }
            for item in short_items[:20]
        ],
        "very_short_sentences": [
            {
                "index": item.index,
                "line_no": item.line_no,
                "chars": item.chars,
                "text": item.text,
            }
            for item in very_short_items[:20]
        ],
        "short_runs": runs[:10],
        "sentences": [
            {
                "index": item.index,
                "line_no": item.line_no,
                "chars": item.chars,
                "text": item.text,
            }
            for item in sentence_infos
        ],
    }


def build_fatigue_windows(sentence_infos: list[SentenceInfo], sample_limit: int) -> list[dict[str, object]]:
    """Find local sentence clusters where several fatigue families stack together."""
    if len(sentence_infos) < 4:
        return []

    window_size = 5
    candidates: list[dict[str, object]] = []
    for start in range(0, max(len(sentence_infos) - window_size + 1, 1)):
        chunk = sentence_infos[start : start + window_size]
        if len(chunk) < 4:
            continue

        short_count = sum(1 for item in chunk if item.chars <= SHORT_SENTENCE_MAX_CHARS)
        very_short_count = sum(1 for item in chunk if item.chars <= VERY_SHORT_SENTENCE_MAX_CHARS)
        judgement_count = 0
        ba_count = 0
        sticky_count = 0
        role_lead_count = 0
        dialogue_count = 0
        listish_count = 0

        for item in chunk:
            stripped = item.text.lstrip(LEADING_PUNCT)
            judgement_count += int(any(term in stripped for term in FATIGUE_WINDOW_JUDGEMENT_TERMS))
            ba_count += int(bool(FATIGUE_WINDOW_BA_RE.search(stripped)))
            sticky_count += int(any(term in stripped for term in FATIGUE_WINDOW_STICKY_TERMS))
            role_lead_count += int(any(stripped.startswith(candidate) for candidate in SUBJECT_LEADS))
            dialogue_count += int(stripped.startswith(("“", "【Pi】")) or "：“" in stripped)
            listish_count += int(stripped.count("，") + stripped.count("、") >= 3)

        reasons: list[str] = []
        if short_count >= 3:
            reasons.append(f"短句 {short_count}/5")
        if very_short_count >= 2:
            reasons.append(f"极短句 {very_short_count}/5")
        if judgement_count >= 2:
            reasons.append(f"判断解释 {judgement_count}/5")
        if ba_count >= 2:
            reasons.append(f"把字操作 {ba_count}/5")
        if sticky_count >= 2:
            reasons.append(f"黏糊词 {sticky_count}/5")
        if role_lead_count >= 3:
            reasons.append(f"角色起手 {role_lead_count}/5")
        if dialogue_count >= 4:
            reasons.append(f"对白挤压 {dialogue_count}/5")
        if listish_count >= 2:
            reasons.append(f"清单分句 {listish_count}/5")
        if not reasons:
            continue

        score = (
            short_count * 2
            + very_short_count
            + judgement_count * 2
            + ba_count * 2
            + sticky_count
            + role_lead_count
            + dialogue_count
            + listish_count
        )
        roles = summarize_short_roles(chunk)
        candidates.append(
            {
                "start_index": chunk[0].index,
                "end_index": chunk[-1].index,
                "start_line": chunk[0].line_no,
                "end_line": chunk[-1].line_no,
                "score": score,
                "reasons": reasons,
                "roles": roles,
                "suggestion": suggest_short_run_action(roles),
                "sample": [item.text for item in chunk],
            }
        )

    selected: list[dict[str, object]] = []
    occupied: set[int] = set()
    for candidate in sorted(candidates, key=lambda item: (-int(item["score"]), int(item["start_index"]))):
        indexes = set(range(int(candidate["start_index"]), int(candidate["end_index"]) + 1))
        if occupied.intersection(indexes):
            continue
        selected.append(candidate)
        occupied.update(indexes)
        if len(selected) >= sample_limit:
            break
    for item in selected:
        item["total_candidates"] = len(candidates)
    return selected


def sentence_context_label(sentence: str) -> str:
    raw = sentence.strip()
    if is_dialogue_like(raw):
        return "dialogue"
    stripped = raw.lstrip(LEADING_PUNCT)
    if stripped.startswith(("【Pi】", "Pi")):
        return "dialogue"
    return "narration"


def classify_dialogue_axis(sentence: str) -> str:
    stripped = sentence.strip().lstrip(LEADING_PUNCT)
    if any(term in stripped for term in DIALOGUE_AXIS_DEVICE_TERMS):
        return "设备声"
    if any(term in stripped for term in DIALOGUE_AXIS_THIRD_PARTY_TERMS):
        return "第三方"
    if any(term in stripped for term in DIALOGUE_AXIS_ENV_TERMS):
        return "环境"
    if any(term in stripped for term in DIALOGUE_AXIS_ACTION_TERMS):
        return "动作"
    return ""


def suggest_dialogue_axis_action(axes: list[str]) -> str:
    if not axes:
        return "插入动作、环境变化、第三方打断或设备声，让对白改变场面。"
    if "动作" not in axes:
        return "补一个能改变站位或物件状态的动作，不要只让角色继续接话。"
    if "环境" not in axes and "设备声" not in axes:
        return "补环境声、设备反馈或空间变化，把话题从互答里拨出来。"
    return "保留已有转轴，再压掉重复问答或合并台词。"


def build_dialogue_axis_gaps(
    sentence_infos: list[SentenceInfo],
    *,
    sample_limit: int,
) -> list[dict[str, object]]:
    if len(sentence_infos) < 4:
        return []

    candidates: list[dict[str, object]] = []
    window_size = 4
    for start in range(0, max(len(sentence_infos) - window_size + 1, 1)):
        chunk = sentence_infos[start : start + window_size]
        if len(chunk) < window_size:
            continue
        dialogue_items = [item for item in chunk if is_dialogue_like(item.text)]
        if len(dialogue_items) < window_size:
            continue
        axes = [axis for item in chunk if (axis := classify_dialogue_axis(item.text))]
        if axes:
            continue
        question_count = sum(1 for item in chunk if "？" in item.text or "?" in item.text)
        short_count = sum(1 for item in chunk if item.chars <= SHORT_SENTENCE_RUN_MAX_CHARS)
        candidates.append(
            {
                "start_index": chunk[0].index,
                "end_index": chunk[-1].index,
                "start_line": chunk[0].line_no,
                "end_line": chunk[-1].line_no,
                "score": window_size * 2 + short_count + question_count,
                "reasons": [
                    f"纯对白 {len(dialogue_items)}/{window_size}",
                    f"短句 {short_count}/{window_size}",
                ]
                + ([f"问句 {question_count}/{window_size}"] if question_count else []),
                "axes": axes,
                "suggestion": suggest_dialogue_axis_action(axes),
                "sample": [item.text for item in chunk],
            }
        )

    selected: list[dict[str, object]] = []
    occupied: set[int] = set()
    for candidate in sorted(candidates, key=lambda item: (-int(item["score"]), int(item["start_index"]))):
        indexes = set(range(int(candidate["start_index"]), int(candidate["end_index"]) + 1))
        if occupied.intersection(indexes):
            continue
        selected.append(candidate)
        occupied.update(indexes)
        if len(selected) >= sample_limit:
            break
    for item in selected:
        item["total_candidates"] = len(candidates)
    return selected


def collect_judgement_contexts(sentence_infos: list[SentenceInfo], sample_limit: int) -> list[dict[str, object]]:
    buckets: dict[str, dict[str, object]] = {
        "narration": {
            "label": "旁白判断",
            "count": 0,
            "terms": collections.Counter(),
            "samples": [],
        },
        "dialogue": {
            "label": "对白判断",
            "count": 0,
            "terms": collections.Counter(),
            "samples": [],
        },
    }
    for item in sentence_infos:
        raw = item.text
        stripped = raw.lstrip(LEADING_PUNCT)
        matched = [term for term in JUDGEMENT_CONTEXT_TERMS if term in stripped]
        if not matched:
            continue
        context = sentence_context_label(raw)
        bucket = buckets[context]
        bucket["count"] = int(bucket["count"]) + 1
        for term in set(matched):
            bucket["terms"][term] += 1
        samples = bucket["samples"]
        if isinstance(samples, list) and len(samples) < sample_limit:
            samples.append(
                {
                    "index": item.index,
                    "line_no": item.line_no,
                    "terms": matched,
                    "text": item.text,
                }
            )

    out: list[dict[str, object]] = []
    for context in ("narration", "dialogue"):
        bucket = buckets[context]
        count = int(bucket["count"])
        if count <= 0:
            continue
        terms = bucket["terms"]
        assert isinstance(terms, collections.Counter)
        out.append(
            {
                "context": context,
                "label": bucket["label"],
                "count": count,
                "warn": context == "narration" and count >= 3,
                "watch": (context == "narration" and count >= 2) or (context == "dialogue" and count >= 8),
                "top_terms": [
                    {"term": term, "count": term_count}
                    for term, term_count in terms.most_common(6)
                ],
                "samples": bucket["samples"],
            }
        )
    return out


def leading_phrase(sentence: str, max_len: int = 8) -> str:
    stripped = sentence.lstrip(LEADING_PUNCT)
    return stripped[:max_len]


def collect_sentence_starts(sentences: list[str]) -> list[tuple[str, int]]:
    counts: collections.Counter[str] = collections.Counter()
    for sentence in sentences:
        lead = leading_phrase(sentence)
        if len(lead) < 2:
            continue
        counts[lead] += 1
    return counts.most_common()


def collect_subject_leads(sentences: list[str]) -> list[tuple[str, int]]:
    counts: collections.Counter[str] = collections.Counter()
    for sentence in sentences:
        lead = sentence.lstrip(LEADING_PUNCT)
        for candidate in SUBJECT_LEADS:
            if lead.startswith(candidate):
                counts[candidate] += 1
                break
    return [(phrase, count) for phrase, count in counts.most_common() if count >= 3]


def collect_paragraph_leads(paragraphs: list[str]) -> list[tuple[str, int]]:
    counts: collections.Counter[str] = collections.Counter()
    for paragraph in paragraphs:
        lead = paragraph.lstrip(LEADING_PUNCT)
        for candidate in PARAGRAPH_LEADS:
            if lead.startswith(candidate):
                counts[candidate] += 1
                break
    return [(phrase, count) for phrase, count in counts.most_common() if count >= 3]


def collect_ngram_terms(
    text: str,
    *,
    min_count_by_size: dict[int, int],
    require_structure: bool,
) -> list[tuple[str, int]]:
    cleaned = re.sub(r"[^\u4e00-\u9fffA-Za-z]", "", text)
    counts: collections.Counter[str] = collections.Counter()
    sizes = tuple(sorted(min_count_by_size))
    one_terms = {size: "一" * size for size in sizes}
    stoplist = WORD_STOPLIST
    structure_chars = STRUCTURE_CHARS

    for size in sizes:
        if len(cleaned) < size:
            continue
        for idx in range(len(cleaned) - size + 1):
            phrase = cleaned[idx : idx + size]
            if phrase in stoplist:
                continue
            if phrase.isascii() and phrase.isalpha():
                continue
            if phrase == one_terms[size]:
                continue
            if require_structure and structure_chars.isdisjoint(phrase):
                continue
            counts[phrase] += 1
    filtered = [
        (phrase, count)
        for phrase, count in counts.items()
        if count >= min_count_by_size.get(len(phrase), 99)
    ]
    filtered.sort(key=lambda item: (-item[1], -len(item[0]), item[0]))

    deduped: list[tuple[str, int]] = []
    covered_phrases: set[str] = set()
    for phrase, count in filtered:
        if phrase in covered_phrases:
            continue
        deduped.append((phrase, count))
        phrase_len = len(phrase)
        for size in sizes:
            if size > phrase_len:
                continue
            for idx in range(phrase_len - size + 1):
                sub = phrase[idx : idx + size]
                if counts.get(sub, 0) <= count:
                    covered_phrases.add(sub)
    return deduped


def collect_connective_sentence_patterns(sentences: list[str]) -> list[tuple[str, int]]:
    counts: collections.Counter[str] = collections.Counter()
    for sentence in sentences:
        lead = sentence.lstrip(LEADING_PUNCT)
        for label, regex in CONNECTIVE_SENTENCE_PATTERNS:
            if regex.search(lead):
                counts[label] += 1
                break
    return counts.most_common()


def collect_clause_prefixes(sentences: list[str]) -> list[tuple[str, int]]:
    counts: collections.Counter[str] = collections.Counter()
    for sentence in sentences:
        for clause in CLAUSE_SPLIT.split(sentence):
            lead = clause.lstrip(LEADING_PUNCT)
            if len(lead) < 2:
                continue
            counts[lead[:4]] += 1
    return [(phrase, count) for phrase, count in counts.most_common() if count >= 4]


def load_template_bank(path: Path | None) -> list[TemplateRule]:
    payload = rules.load_rules(path)
    bank: list[TemplateRule] = []
    for raw in rules.list_at(payload, "draft", "template_rules"):
        if not isinstance(raw, dict) or not all(key in raw for key in ("name", "pattern", "note", "max_per_10k")):
            continue
        rule_name = str(raw["name"])
        pattern = str(raw["pattern"])
        if not pattern or raw.get("enabled") is False or rule_name in HARDCODED_TEMPLATE_RULE_NAMES:
            continue
        bank.append(
            TemplateRule(
                name=rule_name,
                pattern=pattern,
                note=str(raw["note"]),
                max_per_10k=float(raw["max_per_10k"]),
                category=str(raw.get("category", "custom")),
            )
        )
    return bank


def load_term_bank(path: Path | None) -> list[TrackedTerm]:
    payload = rules.load_rules(path)
    bank: list[TrackedTerm] = []
    for raw in rules.list_at(payload, "draft", "tracked_terms"):
        if not isinstance(raw, dict):
            continue
        bank.append(
            TrackedTerm(
                category=str(raw["category"]),
                term=str(raw["term"]),
                max_per_10k=float(raw["max_per_10k"]),
                note=str(raw["note"]),
            )
        )
    return bank


def detect_dialogue_runs(text: str) -> list[tuple[int, int, list[str]]]:
    paragraphs = text.split("\n\n")
    runs: list[tuple[int, int, list[str]]] = []
    current: list[str] = []
    start_idx = 0

    def flush(end_idx: int) -> None:
        nonlocal current, start_idx
        if len(current) >= 4:
            runs.append((start_idx + 1, end_idx, current[:4]))
        current = []

    for idx, para in enumerate(paragraphs):
        stripped = para.strip()
        if not stripped:
            flush(idx)
            continue
        lines = [line.strip() for line in stripped.splitlines() if line.strip()]
        is_dialogue = all(QUOTE_LINE.match(line) for line in lines) or (
            len(lines) == 1 and quote_ratio(lines[0]) > 0.02 and "“" in lines[0]
        )
        if is_dialogue:
            if not current:
                start_idx = idx
            current.append(lines[0])
        else:
            flush(idx)
    flush(len(paragraphs))
    return runs


def detect_short_dialogue_runs(text: str) -> list[tuple[int, int, float, list[str]]]:
    paragraphs = text.split("\n\n")
    flagged: list[tuple[int, int, float, list[str]]] = []
    current: list[str] = []
    start_idx = 0

    def flush(end_idx: int) -> None:
        nonlocal current, start_idx
        if len(current) >= 4:
            avg_len = sum(len(item.strip("“”\"")) for item in current) / len(current)
            if avg_len <= 15:
                flagged.append((start_idx + 1, end_idx, round(avg_len, 2), current[:4]))
        current = []

    for idx, para in enumerate(paragraphs):
        stripped = para.strip()
        if not stripped:
            flush(idx)
            continue
        lines = [line.strip() for line in stripped.splitlines() if line.strip()]
        is_dialogue = all(QUOTE_LINE.match(line) for line in lines) or (
            len(lines) == 1 and quote_ratio(lines[0]) > 0.02 and "“" in lines[0]
        )
        if is_dialogue:
            if not current:
                start_idx = idx
            current.append(lines[0])
        else:
            flush(idx)
    flush(len(paragraphs))
    return flagged


def detect_question_ping_pong(text: str) -> list[tuple[int, int, list[str]]]:
    paragraphs = text.split("\n\n")
    flagged: list[tuple[int, int, list[str]]] = []
    current: list[str] = []
    start_idx = 0

    def flush(end_idx: int) -> None:
        nonlocal current, start_idx
        if len(current) >= 3:
            flagged.append((start_idx + 1, end_idx, current[:4]))
        current = []

    for idx, para in enumerate(paragraphs):
        stripped = para.strip()
        if not stripped:
            flush(idx)
            continue
        lines = [line.strip() for line in stripped.splitlines() if line.strip()]
        if len(lines) != 1 or "“" not in lines[0]:
            flush(idx)
            continue
        line = lines[0]
        is_question = "？" in line
        short_line = len(line.strip("“”\"")) <= 18
        if is_question and short_line:
            if not current:
                start_idx = idx
            current.append(line)
        else:
            flush(idx)
    flush(len(paragraphs))
    return flagged


def detect_quote_ping_pong(text: str) -> list[tuple[int, int, float, list[str]]]:
    paragraphs = text.split("\n\n")
    flagged: list[tuple[int, int, float, list[str]]] = []
    current: list[str] = []
    start_idx = 0

    def flush(end_idx: int) -> None:
        nonlocal current, start_idx
        if len(current) >= 4:
            avg_len = sum(len(item.strip("“”\"")) for item in current) / len(current)
            if avg_len <= 22:
                flagged.append((start_idx + 1, end_idx, round(avg_len, 2), current[:6]))
        current = []

    for idx, para in enumerate(paragraphs):
        stripped = para.strip()
        if not stripped:
            flush(idx)
            continue
        lines = [line.strip() for line in stripped.splitlines() if line.strip()]
        if len(lines) != 1 or "“" not in lines[0]:
            flush(idx)
            continue
        if not current:
            start_idx = idx
        current.append(lines[0])
    flush(len(paragraphs))
    return flagged


def collect_parallel_clauses(sentences: list[str]) -> list[tuple[str, int]]:
    counts: collections.Counter[str] = collections.Counter()
    for sentence in sentences:
        if "，" not in sentence:
            continue
        clauses = [item.strip() for item in CLAUSE_SPLIT.split(sentence) if item.strip()]
        for left, right in zip(clauses, clauses[1:]):
            left_lead = left[:2]
            right_lead = right[:2]
            if len(left_lead) < 2 or len(right_lead) < 2:
                continue
            counts[f"{left_lead}/{right_lead}"] += 1
    return [(phrase, count) for phrase, count in counts.most_common() if count >= 4]


def collect_modifier_pressure(sentences: list[str]) -> list[dict[str, object]]:
    findings: list[dict[str, object]] = []
    for label, hints in (("形容词提示", ADJECTIVE_HINTS), ("动词提示", VERB_HINTS)):
        total = 0
        hit_sentences = 0
        for sentence in sentences:
            count = sum(sentence.count(hint) for hint in hints)
            total += count
            if count >= 3:
                hit_sentences += 1
        if total:
            findings.append(
                {
                    "label": label,
                    "total": total,
                    "dense_sentences": hit_sentences,
                    "warn": hit_sentences >= 4,
                }
            )
    return findings


def collect_judgement_endings(sentences: list[str]) -> list[tuple[str, int]]:
    counts: collections.Counter[str] = collections.Counter()
    for sentence in sentences:
        stripped = sentence.strip()
        for ending in JUDGEMENT_ENDINGS:
            if stripped.endswith(ending):
                counts[ending] += 1
                break
    return [(phrase, count) for phrase, count in counts.most_common() if count >= 2]


def collect_aa_bb_patterns(sentences: list[str], *, sample_limit: int) -> list[dict[str, object]]:
    balanced_samples: dict[str, list[str]] = collections.defaultdict(list)
    redup_counts: collections.Counter[str] = collections.Counter()
    redup_samples: dict[str, list[str]] = collections.defaultdict(list)
    redup_regex = re.compile(
        r"(?:一([\u4e00-\u9fff])\1|([\u4e00-\u9fff])\2([\u4e00-\u9fff])\3|([\u4e00-\u9fff]{2})\4)"
    )

    for sentence in sentences:
        stripped = sentence.strip()
        if not stripped or len(stripped) > 120:
            continue
        for match in redup_regex.finditer(stripped):
            token = match.group(0)
            redup_counts[token] += 1
            if len(redup_samples[token]) < sample_limit:
                redup_samples[token].append(stripped)

        if "，" not in stripped:
            continue
        clauses = [
            clause.strip(LEADING_PUNCT)
            for clause in CLAUSE_SPLIT.split(stripped)
            if clause.strip(LEADING_PUNCT)
        ]
        if len(clauses) < 3:
            continue
        clause_lengths = [prose_char_count(clause) for clause in clauses]
        for start in range(0, len(clauses) - 2):
            for end in range(start + 3, min(len(clauses), start + 5) + 1):
                window = clause_lengths[start:end]
                if min(window) < 2 or max(window) > 10:
                    continue
                if max(window) - min(window) > 2:
                    continue
                shape = "/".join(str(item) for item in window)
                label = f"短分句排比 {shape}"
                if len(balanced_samples[label]) < sample_limit:
                    balanced_samples[label].append(stripped)
                break

    findings: list[dict[str, object]] = []
    for label, samples in sorted(
        balanced_samples.items(),
        key=lambda item: (-len(item[1]), item[0]),
    ):
        findings.append(
            {
                "type": "balanced_clauses",
                "name": label,
                "count": len(samples),
                "note": "AA/BB式短分句排比，密集时会把画面写成清单",
                "warn": len(samples) >= 2,
                "samples": samples,
            }
        )
    for token, count in redup_counts.most_common(12):
        findings.append(
            {
                "type": "reduplicative_word",
                "name": token,
                "count": count,
                "note": "重叠词节奏，重复后会暴露手癖",
                "warn": count >= 4,
                "samples": redup_samples[token],
            }
        )
    return findings


def _is_useful_corpus_term(term: str, category: str) -> bool:
    if term in CORPUS_STOP_TERMS:
        return False
    if len(term) < 2:
        return False
    if re.fullmatch(r"[A-Za-z0-9]+", term):
        return False
    if term.count(term[0]) == len(term):
        return False
    if category == "learned_term" and len(term) == 2 and term not in ALLOWED_SHORT_CORPUS_TERMS:
        return False
    return True


def _learned_patterns_from_terms(
    category: str,
    raw_terms: list[tuple[str, int]],
    corpus_chars: int,
    *,
    min_per_10k_floor: float,
    multiplier: float,
    note: str,
) -> list[LearnedPattern]:
    patterns: list[LearnedPattern] = []
    seen: set[str] = set()
    for term, count in raw_terms:
        if term in seen or not _is_useful_corpus_term(term, category):
            continue
        corpus_per_10k = density(count, corpus_chars)
        patterns.append(
            LearnedPattern(
                category=category,
                name=term,
                count=count,
                corpus_per_10k=round(corpus_per_10k, 2),
                max_per_10k=round(max(corpus_per_10k * multiplier, min_per_10k_floor), 2),
                note=note,
            )
        )
        seen.add(term)
        if len(patterns) >= LEARNED_FILTER_LIMIT:
            break
    return patterns


def build_corpus_profile(paths: Iterable[str | Path]) -> CorpusProfile | None:
    files = [
        path
        for path in iter_target_files(str(raw) for raw in paths)
        if path.suffix in {".md", ".txt"} and not _is_generated_or_template(path)
    ]
    if not files:
        return None

    all_text_parts: list[str] = []
    draft_text_parts: list[str] = []
    for path in files:
        cleaned = clean_corpus_text(path)
        if not cleaned:
            continue
        all_text_parts.append(cleaned)
        if "drafts" in path.parts:
            draft_text_parts.append(cleaned)

    all_text = "\n\n".join(all_text_parts)
    draft_text = "\n\n".join(draft_text_parts) or all_text
    corpus_chars = len(all_text.replace("\n", ""))
    draft_chars = len(draft_text.replace("\n", ""))
    if corpus_chars <= 0:
        return None

    raw_terms = collect_ngram_terms(
        all_text,
        min_count_by_size={2: 30, 3: 18, 4: 12},
        require_structure=False,
    )
    style_raw_terms = collect_ngram_terms(
        draft_text,
        min_count_by_size={2: 24, 3: 14, 4: 10},
        require_structure=True,
    )
    style_chars = set("得像把还说看没不只更在就")
    style_raw_terms = [
        (term, count)
        for term, count in style_raw_terms
        if any(ch in style_chars for ch in term)
    ]

    learned_terms = _learned_patterns_from_terms(
        "learned_term",
        raw_terms,
        corpus_chars,
        min_per_10k_floor=10.0,
        multiplier=1.25,
        note="从卡片/大纲/草稿语料学到的高频实体或动作词，当前章过线时要查是否点名过密",
    )
    learned_style_phrases = _learned_patterns_from_terms(
        "learned_style_phrase",
        style_raw_terms,
        max(draft_chars, 1),
        min_per_10k_floor=5.0,
        multiplier=1.15,
        note="从现有草稿学到的高频句法手势，当前章过线时优先改写",
    )

    draft_sentences = split_sentences(draft_text)
    sentence_leads = [
        {
            "phrase": phrase,
            "count": count,
            "corpus_per_10k": round(density(count, max(draft_chars, 1)), 2),
        }
        for phrase, count in collect_sentence_starts(draft_sentences)
        if count >= 6 and not MARKDOWN_NOISE_LINE.match(phrase)
    ][:LEARNED_FILTER_LIMIT]
    aa_bb_shapes = [
        {
            "name": item["name"],
            "count": item["count"],
            "note": item["note"],
        }
        for item in collect_aa_bb_patterns(draft_sentences, sample_limit=1)
        if item["count"] >= 2
    ][:LEARNED_FILTER_LIMIT]
    sentence_length_baseline = build_sentence_length_profile(split_sentence_infos(draft_text))

    return CorpusProfile(
        source_count=len(files),
        chars=corpus_chars,
        draft_chars=draft_chars,
        learned_terms=learned_terms,
        learned_style_phrases=learned_style_phrases,
        learned_sentence_leads=sentence_leads,
        learned_aa_bb_shapes=aa_bb_shapes,
        sentence_length_baseline={
            "sentence_count": sentence_length_baseline["count"],
            "p10_chars": sentence_length_baseline["p10_chars"],
            "p25_chars": sentence_length_baseline["p25_chars"],
            "median_chars": sentence_length_baseline["median_chars"],
            "avg_chars": sentence_length_baseline["avg_chars"],
            "short_ratio": sentence_length_baseline["short_ratio"],
        },
    )


def build_learned_filter_metrics(
    corpus_profile: CorpusProfile | None,
    lines: list[str],
    chars: int,
    *,
    sample_limit: int,
) -> list[dict[str, object]]:
    if corpus_profile is None:
        return []
    metrics: list[dict[str, object]] = []
    for rule in corpus_profile.learned_terms + corpus_profile.learned_style_phrases:
        count, hits = find_hits(re.escape(rule.name), lines, sample_limit)
        if count < 2:
            continue
        per_10k = density(count, chars)
        flag = count >= 2 and per_10k > rule.max_per_10k
        metrics.append(
            {
                "category": rule.category,
                "name": rule.name,
                "count": count,
                "per_10k": round(per_10k, 2),
                "corpus_per_10k": rule.corpus_per_10k,
                "max_per_10k": rule.max_per_10k,
                "note": rule.note,
                "warn": flag,
                "samples": [{"line_no": hit.line_no, "snippet": hit.snippet} for hit in hits],
            }
        )
    metrics.sort(
        key=lambda item: (
            not bool(item["warn"]),
            -int(item["count"]),
            item["category"],
            item["name"],
        )
    )
    return metrics


def detect_a_b_turns(text: str) -> list[tuple[int, str]]:
    paragraphs = text.split("\n\n")
    flagged: list[tuple[int, str]] = []
    speakers: list[tuple[int, str]] = []
    for idx, para in enumerate(paragraphs, start=1):
        stripped = para.strip()
        if not stripped:
            continue
        speaker = None
        for pattern in SPEAKER_PATTERNS:
            match = pattern.search(stripped)
            if match:
                speaker = match.group(1)
                break
        if speaker:
            speakers.append((idx, speaker))
    for idx in range(len(speakers) - 3):
        seq = speakers[idx : idx + 4]
        names = [speaker for _, speaker in seq]
        if names[0] == names[2] and names[1] == names[3] and names[0] != names[1]:
            flagged.append((seq[0][0], " -> ".join(names)))
    return flagged


def _count_term_hits(text: str, terms: tuple[str, ...]) -> int:
    return sum(text.count(term) for term in terms)


def classify_paragraph_role(paragraph: ParagraphInfo) -> str:
    stripped = paragraph.text.strip().lstrip(LEADING_PUNCT)
    if paragraph.is_dialogue:
        return "dialogue"
    battle_hits = _count_term_hits(stripped, BATTLE_ACTION_TERMS + BATTLE_DAMAGE_TERMS + BATTLE_RESULT_TERMS)
    action_hits = _count_term_hits(stripped, VERB_HINTS) + int(bool(FATIGUE_WINDOW_BA_RE.search(stripped)))
    info_hits = _count_term_hits(stripped, PARAGRAPH_INFO_TERMS) + stripped.count("：")
    emotion_hits = _count_term_hits(stripped, SHORT_ROLE_EMOTION_TERMS + MENTAL_STATE_TERMS)
    tone_hits = sum(_count_term_hits(stripped, terms) for terms in TONE_RULES.values())
    if battle_hits >= 2:
        return "battle"
    if info_hits >= 3 and info_hits >= action_hits:
        return "info"
    if action_hits >= 3 and action_hits >= emotion_hits:
        return "action"
    if emotion_hits >= 2:
        return "emotion"
    if tone_hits >= 2:
        return "environment"
    return "mixed"


def build_scene_map(paragraph_infos: list[ParagraphInfo], *, sample_limit: int) -> dict[str, object]:
    if not paragraph_infos:
        return {
            "blocks": [],
            "role_counts": {},
            "dominant_role": "mixed",
            "dominance_ratio": 0.0,
            "warn": False,
            "block_count": 0,
            "switch_count": 0,
        }

    blocks: list[dict[str, object]] = []
    current_role = ""
    current_items: list[ParagraphInfo] = []

    def flush() -> None:
        nonlocal current_items, current_role
        if not current_items:
            return
        blocks.append(
            {
                "role": current_role or "mixed",
                "start_paragraph": current_items[0].index,
                "end_paragraph": current_items[-1].index,
                "start_line": current_items[0].line_start,
                "end_line": current_items[-1].line_end,
                "paragraphs": len(current_items),
                "chars": sum(item.chars for item in current_items),
                "sample": [item.text.replace("\n", " ")[:80] for item in current_items[:2]],
            }
        )
        current_items = []
        current_role = ""

    for info in paragraph_infos:
        role = classify_paragraph_role(info)
        stripped = info.text.strip().lstrip(LEADING_PUNCT)
        force_break = bool(current_items) and (
            any(stripped.startswith(term) for term in SCENE_BREAK_LEADS)
            or (current_role == "dialogue" and role != "dialogue")
            or (current_role != role and len(current_items) >= 2)
        )
        if force_break:
            flush()
        if not current_items:
            current_role = role
        current_items.append(info)
    flush()

    role_counter: collections.Counter[str] = collections.Counter(str(block["role"]) for block in blocks)
    dominant_role, dominant_count = role_counter.most_common(1)[0] if role_counter else ("mixed", 0)
    dominance_ratio = round(dominant_count / max(len(blocks), 1), 4)
    warn = (
        len(blocks) >= 4
        and dominant_role in {"dialogue", "info"}
        and dominance_ratio >= 0.6
    )
    return {
        "blocks": blocks[: sample_limit * 2],
        "role_counts": dict(role_counter),
        "dominant_role": dominant_role,
        "dominance_ratio": dominance_ratio,
        "warn": warn,
        "block_count": len(blocks),
        "switch_count": max(len(blocks) - 1, 0),
    }


def build_dialogue_emotion_profile(sentence_infos: list[SentenceInfo], *, sample_limit: int) -> dict[str, object]:
    dialogue_items = [item for item in sentence_infos if is_dialogue_like(item.text)]
    emotion_counter: collections.Counter[str] = collections.Counter()
    annotated: list[dict[str, object]] = []
    last_label = ""
    shift_count = 0
    for item in dialogue_items:
        text = item.text.strip()
        labels = [label for label, terms in DIALOGUE_EMOTION_RULES.items() if any(term in text for term in terms)]
        if not labels:
            if "？" in text or "?" in text:
                labels = ["pressure"]
            elif "！" in text:
                labels = ["hostility"]
        primary = labels[0] if labels else "neutral"
        emotion_counter[primary] += 1
        if annotated and primary != "neutral" and last_label and primary != last_label:
            shift_count += 1
        if primary != "neutral":
            last_label = primary
        if len(annotated) < sample_limit:
            annotated.append(
                {
                    "line_no": item.line_no,
                    "label": primary,
                    "labels": labels or ["neutral"],
                    "text": text,
                }
            )
    dominant_label, dominant_count = emotion_counter.most_common(1)[0] if emotion_counter else ("neutral", 0)
    non_neutral = sum(count for label, count in emotion_counter.items() if label != "neutral")
    flatness_warn = len(dialogue_items) >= 6 and dominant_label != "neutral" and dominant_count / max(len(dialogue_items), 1) >= 0.7
    volatility_warn = shift_count >= 4 and non_neutral >= 5
    return {
        "dialogue_sentences": len(dialogue_items),
        "emotion_counts": dict(emotion_counter),
        "dominant_emotion": dominant_label,
        "dominant_ratio": round(dominant_count / max(len(dialogue_items), 1), 4) if dialogue_items else 0.0,
        "shift_count": shift_count,
        "flatness_warn": flatness_warn,
        "volatility_warn": volatility_warn,
        "samples": annotated,
    }


def extract_speaker_name(text: str) -> str:
    cleaned = text.strip()
    for pattern in SPEAKER_LINE_PATTERNS:
        match = pattern.search(cleaned)
        if not match:
            continue
        name = match.group(1).strip(LEADING_PUNCT)
        for suffix in SPEAKER_SUFFIXES:
            if name.endswith(suffix) and len(name) > len(suffix):
                name = name[: -len(suffix)].strip()
                break
        if len(name) >= 2 and name[0] in {"他", "她", "我", "你"} and name[1:] in SPEAKER_SUFFIXES:
            name = name[0]
        if name and name not in CHARACTER_NAME_STOPLIST and len(name) <= 12:
            return name
    return ""


def classify_dialogue_emotion(text: str) -> str:
    labels = [label for label, terms in DIALOGUE_EMOTION_RULES.items() if any(term in text for term in terms)]
    if labels:
        return labels[0]
    if "？" in text or "?" in text:
        return "pressure"
    if "！" in text:
        return "hostility"
    return "neutral"


def build_character_voice_profile(sentence_infos: list[SentenceInfo], *, sample_limit: int) -> dict[str, object]:
    speaker_counter: collections.Counter[str] = collections.Counter()
    speaker_stats: dict[str, dict[str, object]] = {}
    unknown_count = 0

    for item in sentence_infos:
        if not is_dialogue_like(item.text):
            continue
        text = item.text.strip()
        speaker = extract_speaker_name(text)
        if not speaker:
            unknown_count += 1
            continue
        speaker_counter[speaker] += 1
        stats = speaker_stats.setdefault(
            speaker,
            {
                "lines": 0,
                "chars": 0,
                "questions": 0,
                "exclaims": 0,
                "judgements": 0,
                "short_lines": 0,
                "emotion_counter": collections.Counter(),
                "samples": [],
            },
        )
        stats["lines"] = int(stats["lines"]) + 1
        stats["chars"] = int(stats["chars"]) + prose_char_count(text)
        stats["questions"] = int(stats["questions"]) + int(("？" in text) or ("?" in text))
        stats["exclaims"] = int(stats["exclaims"]) + int("！" in text)
        stats["judgements"] = int(stats["judgements"]) + int(any(term in text for term in JUDGEMENT_CONTEXT_TERMS))
        stats["short_lines"] = int(stats["short_lines"]) + int(prose_char_count(text) <= SHORT_SENTENCE_RUN_MAX_CHARS)
        emotion = classify_dialogue_emotion(text)
        emotion_counter = stats["emotion_counter"]
        assert isinstance(emotion_counter, collections.Counter)
        emotion_counter[emotion] += 1
        samples = stats["samples"]
        assert isinstance(samples, list)
        if len(samples) < sample_limit:
            samples.append({"line_no": item.line_no, "text": text, "emotion": emotion})

    speakers: list[dict[str, object]] = []
    for speaker, count in speaker_counter.most_common(sample_limit * 2):
        stats = speaker_stats[speaker]
        lines = max(int(stats["lines"]), 1)
        emotion_counter = stats["emotion_counter"]
        assert isinstance(emotion_counter, collections.Counter)
        dominant_emotion, dominant_count = emotion_counter.most_common(1)[0] if emotion_counter else ("neutral", 0)
        speakers.append(
            {
                "speaker": speaker,
                "lines": int(stats["lines"]),
                "avg_chars": round(int(stats["chars"]) / lines, 2),
                "question_ratio": round(int(stats["questions"]) / lines, 4),
                "exclaim_ratio": round(int(stats["exclaims"]) / lines, 4),
                "judgement_ratio": round(int(stats["judgements"]) / lines, 4),
                "short_ratio": round(int(stats["short_lines"]) / lines, 4),
                "dominant_emotion": dominant_emotion,
                "dominant_ratio": round(dominant_count / lines, 4) if lines else 0.0,
                "samples": list(stats["samples"])[:sample_limit],
            }
        )

    identifiable_lines = sum(int(item["lines"]) for item in speakers)
    coverage_ratio = round(identifiable_lines / max(identifiable_lines + unknown_count, 1), 4)
    homogenized_pairs: list[str] = []
    comparable = [item for item in speakers if int(item["lines"]) >= 3]
    for idx, left in enumerate(comparable):
        for right in comparable[idx + 1 :]:
            if (
                left["dominant_emotion"] == right["dominant_emotion"]
                and abs(float(left["avg_chars"]) - float(right["avg_chars"])) <= 3.0
                and abs(float(left["question_ratio"]) - float(right["question_ratio"])) <= 0.2
                and abs(float(left["short_ratio"]) - float(right["short_ratio"])) <= 0.2
            ):
                homogenized_pairs.append(
                    f"{left['speaker']}~{right['speaker']} emotion={left['dominant_emotion']} avg={left['avg_chars']}/{right['avg_chars']}"
                )
    warn = len(homogenized_pairs) >= 1 and len(comparable) >= 2
    dominant_speaker = speakers[0]["speaker"] if speakers else ""
    dominant_ratio = round(int(speakers[0]["lines"]) / max(identifiable_lines, 1), 4) if speakers else 0.0
    return {
        "speaker_count": len(speakers),
        "identified_lines": identifiable_lines,
        "unknown_lines": unknown_count,
        "coverage_ratio": coverage_ratio,
        "dominant_speaker": dominant_speaker,
        "dominant_ratio": dominant_ratio,
        "warn": warn,
        "homogenized_pairs": homogenized_pairs[:sample_limit],
        "speakers": speakers[:sample_limit],
    }


def build_tone_profile(paragraph_infos: list[ParagraphInfo], *, sample_limit: int) -> dict[str, object]:
    tone_counter: collections.Counter[str] = collections.Counter()
    paragraph_tones: list[str] = []
    samples: list[dict[str, object]] = []
    switch_count = 0
    last_tone = ""
    for info in paragraph_infos:
        stripped = info.text.strip()
        hits = {
            label: _count_term_hits(stripped, terms)
            for label, terms in TONE_RULES.items()
        }
        active = [(label, count) for label, count in hits.items() if count > 0]
        if active:
            label, count = sorted(active, key=lambda item: (-item[1], item[0]))[0]
            tone_counter[label] += count
            paragraph_tones.append(label)
            if last_tone and label != last_tone:
                switch_count += 1
            last_tone = label
            if len(samples) < sample_limit:
                samples.append(
                    {
                        "paragraph": info.index,
                        "line_no": info.line_start,
                        "tone": label,
                        "text": stripped[:80],
                    }
                )
    dominant_tone, dominant_count = tone_counter.most_common(1)[0] if tone_counter else ("none", 0)
    stable_ratio = round(dominant_count / max(sum(tone_counter.values()), 1), 4) if tone_counter else 0.0
    warn = bool(
        paragraph_tones
        and (
            (len(set(paragraph_tones)) >= 4 and switch_count >= max(len(paragraph_tones) // 2, 2))
            or dominant_tone == "none"
        )
    )
    return {
        "tone_counts": dict(tone_counter),
        "dominant_tone": dominant_tone,
        "stable_ratio": stable_ratio,
        "switch_count": switch_count,
        "samples": samples,
        "warn": warn,
    }


def build_battle_profile(sentence_infos: list[SentenceInfo], *, sample_limit: int) -> dict[str, object]:
    sequences: list[dict[str, object]] = []
    current: list[SentenceInfo] = []

    def is_battle_sentence(text: str) -> bool:
        return _count_term_hits(text, BATTLE_ACTION_TERMS + BATTLE_DAMAGE_TERMS + BATTLE_RESULT_TERMS) >= 1

    def flush() -> None:
        nonlocal current
        if len(current) < 2:
            current = []
            return
        text = " ".join(item.text for item in current)
        action_hits = _count_term_hits(text, BATTLE_ACTION_TERMS)
        result_hits = _count_term_hits(text, BATTLE_RESULT_TERMS)
        damage_hits = _count_term_hits(text, BATTLE_DAMAGE_TERMS)
        movement_hits = _count_term_hits(text, BATTLE_MOVEMENT_TERMS)
        sequences.append(
            {
                "start_index": current[0].index,
                "end_index": current[-1].index,
                "start_line": current[0].line_no,
                "end_line": current[-1].line_no,
                "sentences": len(current),
                "action_hits": action_hits,
                "result_hits": result_hits,
                "damage_hits": damage_hits,
                "movement_hits": movement_hits,
                "sample": [item.text for item in current[:4]],
                "warn": action_hits >= 3 and result_hits == 0 and damage_hits == 0,
            }
        )
        current = []

    previous_index = 0
    for item in sentence_infos:
        if is_battle_sentence(item.text):
            if current and item.index != previous_index + 1:
                flush()
            current.append(item)
            previous_index = item.index
        else:
            flush()
            previous_index = item.index
    flush()

    total_action = sum(int(item["action_hits"]) for item in sequences)
    total_result = sum(int(item["result_hits"]) for item in sequences)
    total_damage = sum(int(item["damage_hits"]) for item in sequences)
    total_movement = sum(int(item["movement_hits"]) for item in sequences)
    warn_sequences = sum(1 for item in sequences if bool(item["warn"]))
    return {
        "sequence_count": len(sequences),
        "max_sequence_sentences": max((int(item["sentences"]) for item in sequences), default=0),
        "action_hits": total_action,
        "result_hits": total_result,
        "damage_hits": total_damage,
        "movement_hits": total_movement,
        "result_ratio": round((total_result + total_damage) / max(total_action, 1), 4) if total_action else 0.0,
        "warn_sequences": warn_sequences,
        "warn": bool(
            warn_sequences >= 2
            or (
                len(sequences) >= 3
                and warn_sequences / max(len(sequences), 1) >= 0.5
                and total_action >= 6
            )
        ),
        "samples": sequences[:sample_limit],
    }


def build_viewpoint_profile(paragraph_infos: list[ParagraphInfo], *, sample_limit: int) -> dict[str, object]:
    anchors = SUBJECT_LEADS
    overlaps: list[dict[str, object]] = []
    switches = 0
    last_anchor = ""
    anchor_counter: collections.Counter[str] = collections.Counter()
    for info in paragraph_infos:
        text = info.text.strip()
        paragraph_anchors = [
            anchor
            for anchor in anchors
            if anchor in text and any(term in text for term in MENTAL_STATE_TERMS)
        ]
        unique_anchors = sorted(set(paragraph_anchors))
        if len(unique_anchors) >= 2 and len(overlaps) < sample_limit:
            overlaps.append(
                {
                    "paragraph": info.index,
                    "line_no": info.line_start,
                    "anchors": unique_anchors,
                    "text": text[:100],
                }
            )
        if unique_anchors:
            primary = unique_anchors[0]
            anchor_counter[primary] += 1
            if last_anchor and primary != last_anchor:
                switches += 1
            last_anchor = primary
    return {
        "anchor_counts": dict(anchor_counter),
        "dominant_anchor": anchor_counter.most_common(1)[0][0] if anchor_counter else "",
        "switch_count": switches,
        "overlap_count": len(overlaps),
        "overlaps": overlaps,
        "warn": len(overlaps) >= 1 or switches >= 3,
    }


def build_rule_metrics(
    rules: list[dict[str, object]],
    lines: list[str],
    chars: int,
    *,
    label_field: str,
    sample_limit: int,
) -> tuple[list[dict[str, object]], bool]:
    metrics: list[dict[str, object]] = []
    warned = False
    for rule in rules:
        count, hits = find_hits(str(rule["pattern"]), lines, sample_limit)
        per_10k = density(count, chars)
        flag = per_10k > float(rule["max_per_10k"])
        warned = warned or flag
        metrics.append(
            {
                "name": str(rule[label_field]),
                "count": count,
                "per_10k": round(per_10k, 2),
                "max_per_10k": float(rule["max_per_10k"]),
                "note": str(rule["note"]),
                "warn": flag,
                "samples": [{"line_no": hit.line_no, "snippet": hit.snippet} for hit in hits],
            }
        )
    return metrics, warned


def build_tracked_term_metrics(
    tracked_terms: list[TrackedTerm],
    lines: list[str],
    chars: int,
    *,
    sample_limit: int,
) -> tuple[list[dict[str, object]], list[dict[str, object]], bool]:
    metrics: list[dict[str, object]] = []
    category_buckets: dict[str, dict[str, object]] = {}
    warned = False
    for rule in tracked_terms:
        count, hits = find_hits(re.escape(rule.term), lines, sample_limit)
        per_10k = density(count, chars)
        flag = per_10k > rule.max_per_10k
        warned = warned or flag
        metrics.append(
            {
                "category": rule.category,
                "name": rule.term,
                "count": count,
                "per_10k": round(per_10k, 2),
                "max_per_10k": rule.max_per_10k,
                "note": rule.note,
                "warn": flag,
                "samples": [{"line_no": hit.line_no, "snippet": hit.snippet} for hit in hits],
            }
        )
        bucket = category_buckets.setdefault(
            rule.category,
            {
                "category": rule.category,
                "count": 0,
                "warn_terms": 0,
                "terms": [],
            },
        )
        bucket["count"] += count
        if flag:
            bucket["warn_terms"] += 1
        if count > 0:
            bucket["terms"].append(
                {
                    "term": rule.term,
                    "count": count,
                    "per_10k": round(per_10k, 2),
                    "warn": flag,
                }
            )

    categories: list[dict[str, object]] = []
    for category, bucket in sorted(category_buckets.items()):
        top_terms = sorted(
            bucket["terms"],
            key=lambda item: (-int(item["count"]), item["term"]),
        )
        categories.append(
            {
                "category": category,
                "count": bucket["count"],
                "warn_terms": bucket["warn_terms"],
                "active_terms": len(top_terms),
                "warn": bucket["warn_terms"] > 0,
                "top_terms": top_terms[:8],
            }
        )
    return metrics, categories, warned


def _metric_evidence(metric: dict[str, object]) -> str:
    samples = metric.get("samples") or []
    if samples:
        sample = samples[0]
        return f"L{sample['line_no']} {sample['snippet']}"
    count = metric.get("count", 0)
    per_10k = metric.get("per_10k")
    if per_10k is not None:
        return f"count={count}, per_10k={per_10k}"
    return f"count={count}"


def _metrics_named(
    analysis: dict[str, object],
    section: str,
    names: set[str],
) -> list[dict[str, object]]:
    return [
        metric
        for metric in analysis.get(section, [])
        if metric.get("name") in names and int(metric.get("count", 0)) > 0
    ]


def _fatigue_status(*, warn: bool, count: int, watch_at: int = 1) -> str:
    if warn:
        return "WARN"
    if count >= watch_at:
        return "WATCH"
    return "OK"


def _metric_count(metrics: list[dict[str, object]]) -> int:
    return sum(int(metric.get("count", 0)) for metric in metrics)


def _unique_evidence(items: list[str], limit: int) -> list[str]:
    out: list[str] = []
    seen: set[str] = set()
    for item in items:
        if not item or item in seen:
            continue
        out.append(item)
        seen.add(item)
        if len(out) >= limit:
            break
    return out


def build_style_fatigue(analysis: dict[str, object]) -> list[dict[str, object]]:
    """Summarize sentence-family fatigue for single-chapter review."""
    rows: list[dict[str, object]] = []

    def add(
        family: str,
        status: str,
        count: int,
        risk: str,
        reduce: str,
        evidence: list[str],
    ) -> None:
        rows.append(
            {
                "family": family,
                "status": status,
                "count": count,
                "risk": risk,
                "reduce": reduce,
                "evidence": _unique_evidence(evidence, 3),
            }
        )

    pi_metrics = _metrics_named(analysis, "patterns", {"Pi竖线状态栏", "Pi是否菜单"})
    add(
        "Pi UI/菜单句",
        _fatigue_status(
            warn=any(bool(metric.get("warn")) for metric in pi_metrics),
            count=_metric_count(pi_metrics),
        ),
        _metric_count(pi_metrics),
        "Pi 像系统面板，会削弱搭档感和人物反应。",
        "只留最有角色感的一处，其余改成卡顿、延迟、误读或人物自行判断。",
        [_metric_evidence(metric) for metric in pi_metrics],
    )

    clue_metrics = _metrics_named(analysis, "patterns", {"线索面板词"})
    add(
        "线索面板句",
        _fatigue_status(
            warn=any(bool(metric.get("warn")) for metric in clue_metrics),
            count=_metric_count(clue_metrics),
            watch_at=3,
        ),
        _metric_count(clue_metrics),
        "线索被归档、首屏、标签、坐标、重合等词收拢，读感像任务列表。",
        "把完整结论拆成发现、排除、误判、半确认，章末用行动阻力收束。",
        [_metric_evidence(metric) for metric in clue_metrics],
    )

    conclusion_metrics = _metrics_named(analysis, "patterns", {"这不是X是Y"})
    negation_metrics = _metrics_named(
        analysis,
        "patterns",
        {"不是A而是B", "不是A只是B/更像B", "肯定后否定", "否定后肯定", "问题在于/这就是"},
    )
    negation_tokens = _metrics_named(analysis, "tokens", {"不是", "只是", "而是"})
    negation_total = _metric_count(conclusion_metrics + negation_metrics + negation_tokens)
    add(
        "否定/肯定判断句",
        _fatigue_status(
            warn=bool(conclusion_metrics) or any(bool(metric.get("warn")) for metric in negation_metrics),
            count=negation_total,
            watch_at=4,
        ),
        negation_total,
        "不是/只是/而是/这就是一类句子会让旁白替读者解释。",
        "人物台词可保留；旁白判断改成动作、证据、误读或后果。",
        [_metric_evidence(metric) for metric in (conclusion_metrics + negation_metrics + negation_tokens)],
    )

    narration_context = next(
        (item for item in analysis.get("judgement_contexts", []) if item["context"] == "narration"),
        None,
    )
    if narration_context:
        add(
            "旁白判断句",
            _fatigue_status(
                warn=bool(narration_context.get("warn")),
                count=int(narration_context["count"]),
                watch_at=2,
            ),
            int(narration_context["count"]),
            "判断词集中在旁白里时，作者会替读者完成理解。",
            "人物台词可保留；旁白判断优先换成动作、证据、误读或后果。",
            [
                f"L{sample['line_no']} {','.join(str(term) for term in sample['terms'])}：{sample['text']}"
                for sample in narration_context["samples"][:3]
            ],
        )

    assertive_metrics = _metrics_named(analysis, "patterns", {"肯定判断/解释腔"})
    cliche_metrics = _metrics_named(
        analysis,
        "phrases",
        {"不是因为", "问题不在", "看起来", "更像", "像是", "至少"},
    )
    cliche_total = _metric_count(assertive_metrics + cliche_metrics)
    add(
        "陈词/解释腔",
        _fatigue_status(
            warn=any(bool(metric.get("warn")) for metric in assertive_metrics + cliche_metrics),
            count=cliche_total,
            watch_at=3,
        ),
        cliche_total,
        "真正、其实、显然、更像、至少等解释词过密时，旁白会变成评语。",
        "删掉只负责解释的句子，改成角色误读、物件变化或场面后果。",
        [_metric_evidence(metric) for metric in (assertive_metrics + cliche_metrics)],
    )

    sentence_lengths = analysis["sentence_lengths"]
    add(
        "短句/极短句",
        "WARN" if sentence_lengths["warn"] else "OK",
        int(sentence_lengths["short_count"]),
        "短句连发会把动作、情绪和信息压成碎拍。",
        "每个短句连发块只保留一个节拍点，其余改成动作因果或场面阻力。",
        (
            [
                f"short={sentence_lengths['short_count']}, very_short={sentence_lengths['very_short_count']}, ratio={sentence_lengths['short_ratio']}, runs={len(sentence_lengths['short_runs'])}"
            ]
            + [
                f"短句连发类型：{format_short_roles(sentence_lengths['short_runs'][0].get('roles', []))}；建议：{sentence_lengths['short_runs'][0].get('suggestion', '')}"
            ]
            if sentence_lengths["short_runs"]
            else [
                f"short={sentence_lengths['short_count']}, very_short={sentence_lengths['very_short_count']}, ratio={sentence_lengths['short_ratio']}, runs={len(sentence_lengths['short_runs'])}"
            ]
        ),
    )

    fatigue_windows = analysis.get("fatigue_windows", [])
    add(
        "局部疲劳窗口",
        _fatigue_status(warn=bool(fatigue_windows), count=len(fatigue_windows), watch_at=1),
        len(fatigue_windows),
        "短句、解释、把字操作和角色起手在同一小段叠加时，读感会突然变累。",
        "先改分数最高的窗口：保留一个节奏点，其余改成动作因果、环境反应或视角切换。",
        [
            f"S{item['start_index']}-{item['end_index']} L{item['start_line']}-{item['end_line']} {'、'.join(str(reason) for reason in item['reasons'])}；类型：{format_short_roles(item.get('roles', []))}；建议：{item.get('suggestion', '')}"
            for item in fatigue_windows[:3]
        ],
    )

    ba_metrics = _metrics_named(analysis, "patterns", {"把字操作句"})
    ba_contexts = analysis.get("ba_operation_contexts", [])
    ba_evidence = [_metric_evidence(metric) for metric in ba_metrics]
    for item in ba_contexts[:3]:
        samples = item.get("samples", [])
        sample = ""
        if samples:
            first_sample = samples[0]
            sample = f"L{first_sample['line_no']} {first_sample['snippet']}"
        ba_evidence.append(
            f"{item['role']} x{item['count']}；建议：{item.get('suggestion', '')}"
            + (f"；{sample}" if sample else "")
        )
    add(
        "把字操作句",
        _fatigue_status(
            warn=any(bool(metric.get("warn")) for metric in ba_metrics) or any(
                bool(item.get("warn")) for item in ba_contexts
            ),
            count=max(_metric_count(ba_metrics), sum(int(item["count"]) for item in ba_contexts)),
            watch_at=10,
        ),
        max(_metric_count(ba_metrics), sum(int(item["count"]) for item in ba_contexts)),
        "把 X 拖上/放进/压住/推过去过密，会像操作日志。",
        "工具操作保留；情绪和线索操作改成视觉结果、环境反应或被动阻力。",
        ba_evidence,
    )

    simile_metrics = _metrics_named(analysis, "patterns", {"像/活像模板"})
    add(
        "像/活像比喻",
        _fatigue_status(
            warn=any(bool(metric.get("warn")) for metric in simile_metrics),
            count=_metric_count(simile_metrics),
            watch_at=3,
        ),
        _metric_count(simile_metrics),
        "比喻模板过密会替代真实动作，让旁白解释气氛。",
        "每章只保留少数最有新意的比喻，其余改成具体动作、声音或物件变化。",
        [_metric_evidence(metric) for metric in simile_metrics],
    )

    sticky_metrics = _metrics_named(
        analysis,
        "modifiers",
        {"微微", "轻轻", "慢慢", "有点", "一点点", "显得", "过于", "几乎", "几乎没有"},
    )
    add(
        "黏糊词/弱判断",
        _fatigue_status(
            warn=any(bool(metric.get("warn")) for metric in sticky_metrics),
            count=_metric_count(sticky_metrics),
            watch_at=4,
        ),
        _metric_count(sticky_metrics),
        "轻轻、微微、有点、显得等词过密时，动作力度会被磨软。",
        "优先删弱判断词；用动作幅度、声音、阻力来表示轻重。",
        [_metric_evidence(metric) for metric in sticky_metrics],
    )

    aa_bb_warns = [item for item in analysis["aa_bb_patterns"] if item["warn"]]
    aa_bb_count = sum(int(item["count"]) for item in analysis["aa_bb_patterns"])
    add(
        "AA/BB 短排比",
        _fatigue_status(warn=bool(aa_bb_warns), count=aa_bb_count, watch_at=2),
        aa_bb_count,
        "短分句排比会把画面写成清单。",
        "保留一个节奏点，其余并入动作过程或拆给人物反应。",
        [item["samples"][0] for item in aa_bb_warns if item["samples"]],
    )

    paragraph_leads = analysis.get("paragraph_leads", [])
    subject_leads = analysis.get("subject_leads", [])
    lead_count = max([int(item["count"]) for item in paragraph_leads + subject_leads] or [0])
    lead_evidence = []
    if paragraph_leads:
        lead_evidence.append("段首 " + "，".join(f"{item['phrase']} x{item['count']}" for item in paragraph_leads[:3]))
    if subject_leads:
        lead_evidence.append("主语 " + "，".join(f"{item['phrase']} x{item['count']}" for item in subject_leads[:3]))
    add(
        "角色名/他她起手",
        _fatigue_status(warn=lead_count >= 8, count=lead_count, watch_at=4),
        lead_count,
        "段落总从角色名或他她起步，会让镜头调度单一。",
        "每三到四个角色起手里，换一个空间、物件、声音或证据变化起手。",
        lead_evidence,
    )

    tracked_warns = [metric for metric in analysis["tracked_terms"] if metric["warn"]]
    tracked_windows = analysis.get("tracked_term_windows", [])
    tracked_total = sum(int(metric["count"]) for metric in tracked_warns) + len(tracked_windows)
    tracked_evidence = [_metric_evidence(metric) for metric in tracked_warns[:4]]
    for item in tracked_windows[:3]:
        tracked_evidence.append(
            f"S{item['start_index']}-{item['end_index']} L{item['start_line']}-{item['end_line']} "
            f"{'、'.join(str(reason) for reason in item['reasons'])}；"
            f"{format_tracked_term_counts(item.get('terms', [])[:3])}；建议：{item.get('suggestion', '')}"
        )
    add(
        "高频词/点名册",
        _fatigue_status(warn=bool(tracked_warns or tracked_windows), count=tracked_total, watch_at=8),
        tracked_total,
        "人物名、地名、设备名过密时，叙述会像点名册或设定表。",
        "用动作、称谓、空间位置和具体物件轮换，不要只靠同一个名词推进。",
        tracked_evidence,
    )

    dialogue = analysis["dialogue"]
    dialogue_count = (
        len(dialogue["short_quote_runs"])
        + len(dialogue["question_ping_pong"])
        + len(dialogue["quote_ping_pong"])
        + len(dialogue.get("dialogue_axis_gaps", []))
    )
    dialogue_evidence = []
    for key in ("short_quote_runs", "question_ping_pong", "quote_ping_pong"):
        if dialogue[key]:
            dialogue_evidence.append(" | ".join(dialogue[key][0]["sample"]))
    for item in dialogue.get("dialogue_axis_gaps", [])[:2]:
        dialogue_evidence.append(
            f"S{item['start_index']}-{item['end_index']} L{item['start_line']}-{item['end_line']} "
            f"{'、'.join(str(reason) for reason in item['reasons'])}；建议：{item.get('suggestion', '')}"
        )
    add(
        "对白乒乓",
        _fatigue_status(warn=dialogue_count >= 1, count=dialogue_count),
        dialogue_count,
        "短对白连续互顶时，动作和场面会消失。",
        "每四句对白至少插入一个动作、环境变化或人物误读作为转轴。",
        dialogue_evidence,
    )

    scene_map = analysis.get("scene_map", {})
    block_count = int(scene_map.get("block_count", 0))
    role_counts = scene_map.get("role_counts", {})
    role_summary = "，".join(f"{name} x{count}" for name, count in role_counts.items()) if role_counts else "无"
    add(
        "场面功能分布",
        _fatigue_status(warn=bool(scene_map.get("warn")), count=block_count, watch_at=4),
        block_count,
        "如果整章长时间停在对白块或说明块，场面会失去功能切换。",
        "让信息、动作、环境和关系推进互相接力，不要让单一功能吃满整章。",
        [f"dominant={scene_map.get('dominant_role', 'mixed')} ratio={scene_map.get('dominance_ratio', 0)}；{role_summary}"],
    )

    dialogue_emotions = analysis.get("dialogue_emotions", {})
    emotion_counts = dialogue_emotions.get("emotion_counts", {})
    emotion_summary = "，".join(f"{name} x{count}" for name, count in emotion_counts.items()) if emotion_counts else "无"
    add(
        "对白情绪曲线",
        _fatigue_status(
            warn=bool(dialogue_emotions.get("flatness_warn") or dialogue_emotions.get("volatility_warn")),
            count=int(dialogue_emotions.get("shift_count", 0)),
            watch_at=2,
        ),
        int(dialogue_emotions.get("dialogue_sentences", 0)),
        "对白如果长期只剩一种情绪，或情绪标签频繁横跳，关系推进会发假。",
        "检查台词是在逼问、回避、防御还是安抚，并补动作或停顿让情绪转折落地。",
        [
            f"dominant={dialogue_emotions.get('dominant_emotion', 'neutral')} ratio={dialogue_emotions.get('dominant_ratio', 0)} shift={dialogue_emotions.get('shift_count', 0)}；{emotion_summary}"
        ],
    )

    character_voice = analysis.get("character_voice", {})
    voice_speakers = character_voice.get("speakers", [])
    voice_evidence = []
    for item in voice_speakers[:3]:
        voice_evidence.append(
            f"{item['speaker']} line={item['lines']} avg={item['avg_chars']} q={item['question_ratio']} short={item['short_ratio']} emotion={item['dominant_emotion']}"
        )
    for item in character_voice.get("homogenized_pairs", [])[:2]:
        voice_evidence.append(str(item))
    add(
        "角色声音",
        _fatigue_status(warn=bool(character_voice.get("warn")), count=int(character_voice.get("speaker_count", 0)), watch_at=2),
        int(character_voice.get("identified_lines", 0)),
        "如果多名角色的对白节拍、问句率和情绪主导长期接近，人物会越来越像同一个人在说话。",
        "让不同角色在句长、追问强度、判断习惯和情绪入口上拉开距离。",
        voice_evidence,
    )

    battle_profile = analysis.get("battle_profile", {})
    add(
        "动作结果链",
        _fatigue_status(warn=bool(battle_profile.get("warn")), count=int(battle_profile.get("sequence_count", 0)), watch_at=1),
        int(battle_profile.get("action_hits", 0)),
        "冲突段如果只有动作没有结果、伤害或位移反馈，会像挥空的动作脚本。",
        "每段冲突至少补一个结果句：谁退了、谁失衡了、什么东西坏了、谁被迫改动作。",
        [
            f"sequences={battle_profile.get('sequence_count', 0)} action={battle_profile.get('action_hits', 0)} result={battle_profile.get('result_hits', 0)} damage={battle_profile.get('damage_hits', 0)} ratio={battle_profile.get('result_ratio', 0)}"
        ],
    )

    viewpoint_profile = analysis.get("viewpoint_profile", {})
    anchor_counts = viewpoint_profile.get("anchor_counts", {})
    anchor_summary = "，".join(f"{name} x{count}" for name, count in anchor_counts.items()) if anchor_counts else "无"
    add(
        "视角锚点",
        _fatigue_status(warn=bool(viewpoint_profile.get("warn")), count=int(viewpoint_profile.get("overlap_count", 0)), watch_at=1),
        int(viewpoint_profile.get("switch_count", 0)),
        "同段多人物心理暴露或近距离切锚偏多时，镜头中心会发飘。",
        "近距离视角段先固定一个感知中心；别在同段同时替两个人解释内心。",
        [f"anchors={anchor_summary} switch={viewpoint_profile.get('switch_count', 0)} overlap={viewpoint_profile.get('overlap_count', 0)}"],
    )

    ending = analysis["ending"]
    ending_count = len(ending["image_terms"]) + len(ending["flow_terms"])
    ending_evidence = []
    if ending["flow_terms"]:
        ending_evidence.append("流程词 " + "，".join(f"{item['term']} x{item['count']}" for item in ending["flow_terms"]))
    if ending["image_terms"]:
        ending_evidence.append("意象词 " + "，".join(f"{item['term']} x{item['count']}" for item in ending["image_terms"]))
    if ending["tail_excerpt"]:
        ending_evidence.append(str(ending["tail_excerpt"])[:120])
    add(
        "章末模板",
        _fatigue_status(warn=bool(ending["warn"]), count=ending_count, watch_at=2),
        ending_count,
        "章末反复用流程词或同类意象收束，会让钩子同质。",
        "在动作余波、关系变化、外部阻力三类里换一种收束手势。",
        ending_evidence,
    )

    modifier_warns = [item for item in analysis["modifier_pressure"] if item["warn"]]
    modifier_total = sum(int(item["total"]) for item in analysis["modifier_pressure"])
    add(
        "修饰/动作压力",
        _fatigue_status(warn=bool(modifier_warns), count=modifier_total, watch_at=80),
        modifier_total,
        "同类修饰词和动作词过密时，画面会发僵。",
        "优先改 dense_sentences，不要只替换同义词。",
        [
            f"{item['label']} total={item['total']} dense={item['dense_sentences']}"
            for item in analysis["modifier_pressure"]
            if item["total"] > 0
        ],
    )

    status_order = {"WARN": 0, "WATCH": 1, "OK": 2}
    rows.sort(key=lambda item: (status_order.get(str(item["status"]), 9), item["family"]))
    return rows


def build_review_reminders(analysis: dict[str, object]) -> list[dict[str, object]]:
    """Translate raw warnings into concrete questions a reviewer should answer."""
    reminders: list[dict[str, object]] = []

    def add(
        priority: str,
        category: str,
        title: str,
        reason: str,
        check: str,
        action: str,
        evidence: list[str],
    ) -> None:
        reminders.append(
            {
                "priority": priority,
                "category": category,
                "title": title,
                "reason": reason,
                "check": check,
                "action": action,
                "evidence": _unique_evidence(evidence, 4),
            }
        )

    pattern_metrics = analysis.get("patterns", [])
    pi_metrics = _metrics_named(
        analysis,
        "patterns",
        {"Pi竖线状态栏", "Pi是否菜单"},
    )
    if pi_metrics:
        add(
            "P1",
            "Pi",
            "Pi 输出正在滑向 UI/菜单",
            "Pi 负责给结论或按钮提示时，会从搭档变成系统面板。",
            "检查 Pi 输出后是否还有人物误读、停顿、拒绝配合或行动后果。",
            "保留最有角色感的一处 Pi 文本，其余改成蓝字卡顿、反应延迟或人物自己判断。",
            [_metric_evidence(metric) for metric in pi_metrics],
        )

    clue_metrics = _metrics_named(analysis, "patterns", {"线索面板词"})
    if clue_metrics:
        add(
            "P1",
            "信息",
            "线索被面板词收拢",
            "归档、首屏、标签、坐标、重合等词密集时，章节会像任务列表。",
            "检查本章结论是否由场面冲突推出，而不是由屏幕/图表替读者盖章。",
            "把一次完整结论拆成发现、排除、误判、半确认；章末改用行动阻力收束。",
            [_metric_evidence(metric) for metric in clue_metrics],
        )

    conclusion_metrics = _metrics_named(analysis, "patterns", {"这不是X是Y"})
    negation_metrics = _metrics_named(
        analysis,
        "patterns",
        {"不是A而是B", "不是A只是B/更像B", "肯定后否定", "否定后肯定", "问题在于/这就是"},
    )
    negation_tokens = _metrics_named(analysis, "tokens", {"不是", "只是", "而是"})
    negation_count = sum(int(metric.get("count", 0)) for metric in negation_metrics + negation_tokens)
    if conclusion_metrics or negation_count >= 5:
        add(
            "P1" if conclusion_metrics else "P2",
            "句式",
            "否定/肯定判断句过密",
            "不是/只是/而是/这就是一类句子会让旁白替读者解释。",
            "区分人物台词和作者旁白；人物声音可保留，旁白判断优先改。",
            "把结论改成证据、动作或误读；保留一处关键否定，其余让读者自己推出来。",
            [_metric_evidence(metric) for metric in (conclusion_metrics + negation_metrics + negation_tokens)[:4]],
        )

    narration_context = next(
        (item for item in analysis.get("judgement_contexts", []) if item["context"] == "narration"),
        None,
    )
    if narration_context and narration_context["warn"]:
        add(
            "P1" if int(narration_context["count"]) >= 5 else "P2",
            "句式",
            "旁白判断句偏密",
            "判断词集中在旁白里时，作者会替读者完成理解。",
            "先把人物台词和旁白判断分开；只处理旁白里负责下结论的句子。",
            "把旁白判断改成动作、证据、误读、后果或第三方反应。",
            [
                f"L{sample['line_no']} {','.join(str(term) for term in sample['terms'])}：{sample['text']}"
                for sample in narration_context["samples"][:4]
            ],
        )

    assertive_metrics = _metrics_named(analysis, "patterns", {"肯定判断/解释腔"})
    cliche_metrics = _metrics_named(
        analysis,
        "phrases",
        {"不是因为", "问题不在", "看起来", "更像", "像是", "至少"},
    )
    cliche_count = sum(int(metric.get("count", 0)) for metric in assertive_metrics + cliche_metrics)
    cliche_warn = any(bool(metric.get("warn")) for metric in assertive_metrics + cliche_metrics)
    if cliche_warn or cliche_count >= 3:
        add(
            "P2",
            "文风",
            "陈词/解释腔偏密",
            "真正、其实、显然、更像、至少等词会让旁白像评语。",
            "检查这些句子是不是只在解释观感，而没有制造动作或阻力。",
            "删掉只负责解释的句子，或改成角色误读、物件变化、场面后果。",
            [_metric_evidence(metric) for metric in (assertive_metrics + cliche_metrics)[:4]],
        )

    sentence_lengths = analysis["sentence_lengths"]
    short_runs = sentence_lengths["short_runs"]
    if sentence_lengths["warn"]:
        priority = "P1" if float(sentence_lengths["short_ratio"]) >= 0.25 or len(short_runs) >= 5 else "P2"
        evidence = [
            f"short={sentence_lengths['short_count']}, very_short={sentence_lengths['very_short_count']}, ratio={sentence_lengths['short_ratio']}, runs={len(short_runs)}"
        ]
        if short_runs:
            evidence.append(" | ".join(short_runs[0]["sample"]))
            roles = "，".join(f"{item['role']} x{item['count']}" for item in short_runs[0].get("roles", []))
            if roles:
                evidence.append(f"类型：{roles}；建议：{short_runs[0].get('suggestion', '')}")
        add(
            priority,
            "节奏",
            "短句正在变成默认节拍",
            "连续短句会把动作、情绪和信息压成碎拍。",
            "检查短句是在制造节奏，还是在把应展开的过程写成提纲。",
            "每个短句连发块只保留一个节拍点，其余改成动作因果或场面阻力。",
            evidence,
        )

    fatigue_windows = analysis.get("fatigue_windows", [])
    if fatigue_windows:
        first_window = fatigue_windows[0]
        add(
            "P1" if int(first_window["score"]) >= 10 else "P2",
            "定位",
            "局部句式疲劳窗口",
            "同一小段里短句、判断解释、把字操作或角色起手叠加，会比单项总数更影响观感。",
            "优先看分数最高的窗口，不要平均用力改全章。",
            "保留一个最有用的节奏点；其余改成动作因果、环境反应、人物误读或视角入口。",
            [
                f"S{item['start_index']}-{item['end_index']} L{item['start_line']}-{item['end_line']} score={item['score']} {'、'.join(str(reason) for reason in item['reasons'])}：{' | '.join(str(sample) for sample in item['sample'][:5])}"
                for item in fatigue_windows[:3]
            ],
        )

    ba_metrics = _metrics_named(analysis, "patterns", {"把字操作句"})
    ba_contexts = analysis.get("ba_operation_contexts", [])
    if ba_metrics or ba_contexts:
        evidence = [_metric_evidence(metric) for metric in ba_metrics]
        for item in ba_contexts[:3]:
            samples = item.get("samples", [])
            sample = ""
            if samples:
                first_sample = samples[0]
                sample = f"L{first_sample['line_no']} {first_sample['snippet']}"
            evidence.append(
                f"{item['role']} x{item['count']}；建议：{item.get('suggestion', '')}"
                + (f"；{sample}" if sample else "")
            )
        add(
            "P2",
            "动作",
            "把字句过密",
            "把 X 拖上/放进/压住/推过去连续出现时，场面像操作日志。",
            "检查这些把字句属于工具操作、线索操作、情绪动作还是场面调度。",
            "工具操作可保留必要句；线索操作拆发现-误读-后果，情绪动作改身体反应或他人误读。",
            evidence,
        )

    simile_metrics = _metrics_named(analysis, "patterns", {"像/活像模板"})
    if simile_metrics:
        add(
            "P2",
            "文风",
            "比喻模板过密",
            "像/活像类句式能快速给气氛，但过密时会替代真实动作。",
            "检查比喻是否提供新信息；只解释气氛的比喻优先删。",
            "每章保留少数最有新意的比喻，其余改成具体动作、声音、物件变化。",
            [_metric_evidence(metric) for metric in simile_metrics],
        )

    sticky_metrics = _metrics_named(
        analysis,
        "modifiers",
        {"微微", "轻轻", "慢慢", "有点", "一点点", "显得", "过于", "几乎", "几乎没有"},
    )
    if sum(int(metric.get("count", 0)) for metric in sticky_metrics) >= 4 or any(
        bool(metric.get("warn")) for metric in sticky_metrics
    ):
        add(
            "P2",
            "文风",
            "黏糊词/弱判断偏密",
            "轻轻、微微、有点、显得等词会削弱动作力度。",
            "检查这些词是否在替代动作幅度、声音、阻力或人物状态。",
            "优先删弱判断词；用可见动作和场面反应表达轻重。",
            [_metric_evidence(metric) for metric in sticky_metrics[:4]],
        )

    tracked_warns = [metric for metric in analysis["tracked_terms"] if metric["warn"]]
    tracked_windows = analysis.get("tracked_term_windows", [])
    if tracked_warns or tracked_windows:
        evidence = [_metric_evidence(metric) for metric in tracked_warns[:4]]
        for item in tracked_windows[:3]:
            evidence.append(
                f"S{item['start_index']}-{item['end_index']} L{item['start_line']}-{item['end_line']} "
                f"{'、'.join(str(reason) for reason in item['reasons'])}；"
                f"{format_tracked_term_counts(item.get('terms', [])[:3])}；建议：{item.get('suggestion', '')}"
            )
        add(
            "P2",
            "词汇",
            "高频词/点名过密",
            "人物名、地名、设备名过密时，叙述会像点名册或设定表。",
            "检查同一名词是否在局部窗口里连续点名，或是否可以用动作、称谓、空间位置、具体物件轮换。",
            "先改最密的 1-2 个窗口，不要只做同义词替换。",
            evidence,
        )

    paragraph_leads = analysis.get("paragraph_leads", [])
    subject_leads = analysis.get("subject_leads", [])
    lead_evidence: list[str] = []
    if paragraph_leads:
        lead_evidence.append(
            "段首 " + "，".join(f"{item['phrase']} x{item['count']}" for item in paragraph_leads[:3])
        )
    if subject_leads:
        lead_evidence.append(
            "主语 " + "，".join(f"{item['phrase']} x{item['count']}" for item in subject_leads[:3])
        )
    if lead_evidence and max([int(item["count"]) for item in paragraph_leads + subject_leads] or [0]) >= 8:
        add(
            "P2",
            "镜头",
            "角色名/他她起手过密",
            "段落总从角色名或他她起步，会让镜头调度单一。",
            "检查连续段落是否都是角色先出现、再动作、再判断。",
            "每三到四个角色起手里，至少换一个空间、物件、声音或证据变化起手。",
            lead_evidence,
        )

    dialogue = analysis["dialogue"]
    dialogue_evidence: list[str] = []
    if dialogue["short_quote_runs"]:
        item = dialogue["short_quote_runs"][0]
        dialogue_evidence.append(" | ".join(item["sample"]))
    if dialogue["quote_ping_pong"]:
        item = dialogue["quote_ping_pong"][0]
        dialogue_evidence.append(" | ".join(item["sample"]))
    for item in dialogue.get("dialogue_axis_gaps", [])[:2]:
        dialogue_evidence.append(
            f"S{item['start_index']}-{item['end_index']} L{item['start_line']}-{item['end_line']} "
            f"{'、'.join(str(reason) for reason in item['reasons'])}；建议：{item.get('suggestion', '')}"
        )
    if dialogue_evidence:
        add(
            "P2",
            "对话",
            "对白像互答录音",
            "短对白连续互顶时，场面动作会消失。",
            "检查每四句对白里是否有动作、环境变化、第三方打断或设备声作为转轴。",
            "保留最有锋芒的两句，其余用动作、环境声、第三方反应或设备反馈打断。",
            dialogue_evidence,
        )

    scene_map = analysis.get("scene_map", {})
    if scene_map.get("warn"):
        role_counts = scene_map.get("role_counts", {})
        role_summary = "，".join(f"{name} x{count}" for name, count in role_counts.items()) if role_counts else "无"
        add(
            "P2",
            "场面",
            "章节长时间停在同一种功能块",
            "如果整章大部分粗分块都在对白或说明，场面会失去切换和推进。",
            "检查这章有没有让动作、环境、关系和信息交替接力，而不是一直停在解释或接话。",
            "补一个改变站位、空间、外部阻力或关系温度的块，不要只扩写原功能。",
            [
                f"dominant={scene_map.get('dominant_role', 'mixed')} ratio={scene_map.get('dominance_ratio', 0)}；{role_summary}"
            ],
        )

    dialogue_emotions = analysis.get("dialogue_emotions", {})
    if dialogue_emotions.get("flatness_warn") or dialogue_emotions.get("volatility_warn"):
        emotion_counts = dialogue_emotions.get("emotion_counts", {})
        emotion_summary = "，".join(f"{name} x{count}" for name, count in emotion_counts.items()) if emotion_counts else "无"
        add(
            "P2",
            "对话",
            "对白情绪曲线失衡",
            "对白如果长期只剩逼问/防御一种温度，或情绪标签频繁横跳，人物关系会发假。",
            "检查这段对话是在升级关系、回避真问题，还是只在重复情绪姿态。",
            "给情绪转折补动作、停顿、误读或第三方干扰，让变化落到场面上。",
            [
                f"dominant={dialogue_emotions.get('dominant_emotion', 'neutral')} ratio={dialogue_emotions.get('dominant_ratio', 0)} shift={dialogue_emotions.get('shift_count', 0)}；{emotion_summary}"
            ],
        )

    character_voice = analysis.get("character_voice", {})
    if character_voice.get("warn"):
        evidence = [
            f"coverage={character_voice.get('coverage_ratio', 0)} dominant={character_voice.get('dominant_speaker', '')} ratio={character_voice.get('dominant_ratio', 0)}"
        ]
        for item in character_voice.get("speakers", [])[:3]:
            evidence.append(
                f"{item['speaker']} line={item['lines']} avg={item['avg_chars']} q={item['question_ratio']} short={item['short_ratio']} emotion={item['dominant_emotion']}"
            )
        evidence.extend(str(item) for item in character_voice.get("homogenized_pairs", [])[:2])
        add(
            "P2",
            "角色",
            "角色对白开始同腔",
            "多名角色的问句率、短句率、判断姿态和主导情绪过近时，人物声音会并轨。",
            "检查这些角色是不是都在用同一种追问、回避或判断手势说话。",
            "至少给核心角色拉开一项稳定差异：句长、问句密度、脏话/判断句习惯、安抚还是施压入口。",
            evidence,
        )

    battle_profile = analysis.get("battle_profile", {})
    if battle_profile.get("warn"):
        add(
            "P2",
            "动作",
            "冲突段动作多但结果少",
            "动作和碰撞已经出现，但结果句、受伤反馈或位移后果不足时，冲突会像挥空。",
            "检查每段动作后，是否有人被逼退、卡住、受伤、失手或改变目标。",
            "每段冲突至少补一个结果句，不要只累计动作动词。",
            [
                f"sequences={battle_profile.get('sequence_count', 0)} action={battle_profile.get('action_hits', 0)} result={battle_profile.get('result_hits', 0)} damage={battle_profile.get('damage_hits', 0)} ratio={battle_profile.get('result_ratio', 0)}"
            ],
        )

    viewpoint_profile = analysis.get("viewpoint_profile", {})
    if viewpoint_profile.get("warn"):
        anchor_counts = viewpoint_profile.get("anchor_counts", {})
        anchor_summary = "，".join(f"{name} x{count}" for name, count in anchor_counts.items()) if anchor_counts else "无"
        evidence = [f"anchors={anchor_summary} switch={viewpoint_profile.get('switch_count', 0)} overlap={viewpoint_profile.get('overlap_count', 0)}"]
        for item in viewpoint_profile.get("overlaps", [])[:2]:
            evidence.append(f"L{item['line_no']} {','.join(item['anchors'])}：{item['text']}")
        add(
            "P2",
            "视角",
            "近距离视角锚点漂移",
            "同段多人物心理暴露或近距离切锚偏多时，读者会丢当前镜头中心。",
            "检查这些段落是不是同时替两个人解释内心，或刚贴近一个人就跳去另一个人。",
            "近距离段先固定一个感知中心，其余人物只通过动作、台词和误读出现。",
            evidence,
        )

    if analysis["ending"]["warn"]:
        evidence = []
        if analysis["ending"]["flow_terms"]:
            evidence.append(
                "流程词 " + "，".join(
                    f"{item['term']} x{item['count']}" for item in analysis["ending"]["flow_terms"]
                )
            )
        if analysis["ending"]["image_terms"]:
            evidence.append(
                "意象词 " + "，".join(
                    f"{item['term']} x{item['count']}" for item in analysis["ending"]["image_terms"]
                )
            )
        evidence.append(str(analysis["ending"]["tail_excerpt"])[:120])
        add(
            "P2",
            "章末",
            "章末收束可能模板化",
            "章末反复用冷光、夜、首屏、继续、下一步等词，会让钩子同质。",
            "检查结尾是在打开新行动，还是只把本章线索摆整齐。",
            "在动作余波、关系变化、外部阻力三类里换一种收束手势。",
            evidence,
        )

    priority_order = {"P1": 0, "P2": 1, "P3": 2}
    reminders.sort(key=lambda item: (priority_order.get(str(item["priority"]), 9), item["category"], item["title"]))
    return reminders


def analyze_text(
    text: str,
    *,
    sample_limit: int,
    source: str,
    template_bank: list[TemplateRule] | None = None,
    term_bank: list[TrackedTerm] | None = None,
    corpus_profile: CorpusProfile | None = None,
) -> dict[str, object]:
    lines = text.splitlines()
    sentences = split_sentences(text)
    sentence_infos = split_sentence_infos(text)
    paragraph_infos = split_paragraph_infos(text)
    paragraphs = [para for para in text.split("\n\n") if para.strip()]
    chars = len(text.replace("\n", ""))
    quote_runs = detect_dialogue_runs(text)
    short_quote_runs = detect_short_dialogue_runs(text)
    question_ping_pong = detect_question_ping_pong(text)
    quote_ping_pong = detect_quote_ping_pong(text)
    dialogue_axis_gaps = build_dialogue_axis_gaps(sentence_infos, sample_limit=sample_limit * 2)
    ab_turns = detect_a_b_turns(text)

    token_metrics, token_warn = build_rule_metrics(
        TOKEN_RULES,
        lines,
        chars,
        label_field="name",
        sample_limit=sample_limit,
    )
    pattern_metrics, pattern_warn = build_rule_metrics(
        PATTERN_RULES,
        lines,
        chars,
        label_field="label",
        sample_limit=sample_limit,
    )
    phrase_metrics, phrase_warn = build_rule_metrics(
        PHRASE_RULES,
        lines,
        chars,
        label_field="name",
        sample_limit=sample_limit,
    )
    modifier_metrics, modifier_warn = build_rule_metrics(
        MODIFIER_RULES,
        lines,
        chars,
        label_field="name",
        sample_limit=sample_limit,
    )
    punctuation_metrics, punctuation_warn = build_rule_metrics(
        PUNCTUATION_RULES,
        lines,
        chars,
        label_field="name",
        sample_limit=sample_limit,
    )
    combo_metrics, combo_warn = build_rule_metrics(
        COMBO_RULES,
        lines,
        chars,
        label_field="name",
        sample_limit=sample_limit,
    )
    custom_template_metrics = []
    custom_warn = False
    for rule in template_bank or []:
        count, hits = find_hits(rule.pattern, lines, sample_limit)
        per_10k = density(count, chars)
        flag = per_10k > rule.max_per_10k
        custom_warn = custom_warn or flag
        custom_template_metrics.append(
            {
                "name": rule.name,
                "count": count,
                "per_10k": round(per_10k, 2),
                "max_per_10k": rule.max_per_10k,
                "note": rule.note,
                "warn": flag,
                "samples": [{"line_no": hit.line_no, "snippet": hit.snippet} for hit in hits],
                "category": rule.category,
            }
        )
    tracked_term_metrics, tracked_term_categories, tracked_term_warn = build_tracked_term_metrics(
        term_bank or [],
        lines,
        chars,
        sample_limit=sample_limit,
    )
    learned_filter_metrics = build_learned_filter_metrics(
        corpus_profile,
        lines,
        chars,
        sample_limit=sample_limit,
    )
    learned_filter_warn = any(item["warn"] for item in learned_filter_metrics)

    sentence_starts = collect_sentence_starts(sentences)
    subject_leads = [
        {"phrase": phrase, "count": count}
        for phrase, count in collect_subject_leads(sentences)
    ]
    paragraph_leads = [
        {"phrase": phrase, "count": count}
        for phrase, count in collect_paragraph_leads(paragraphs)
    ]
    repeated_starts = [
        {"phrase": phrase, "count": count}
        for phrase, count in sentence_starts
        if count >= 3
    ]
    sentence_patterns = [
        {"phrase": phrase, "count": count}
        for phrase, count in collect_connective_sentence_patterns(sentences)
        if count >= 3
    ]
    judgement_endings = [
        {"phrase": phrase, "count": count}
        for phrase, count in collect_judgement_endings(sentences)
    ]
    clause_prefixes = [
        {"phrase": phrase, "count": count}
        for phrase, count in collect_clause_prefixes(sentences)
    ]
    parallel_clauses = [
        {"phrase": phrase, "count": count}
        for phrase, count in collect_parallel_clauses(sentences)
    ]
    aa_bb_patterns = collect_aa_bb_patterns(sentences, sample_limit=sample_limit)
    aa_bb_warn = any(item["warn"] for item in aa_bb_patterns)
    ba_operation_contexts = build_ba_operation_contexts(sentence_infos, sample_limit=sample_limit)
    ba_operation_context_warn = any(bool(item["warn"]) for item in ba_operation_contexts)
    modifier_pressure = collect_modifier_pressure(sentences)
    sentence_lengths = build_sentence_length_profile(sentence_infos)
    fatigue_windows = build_fatigue_windows(sentence_infos, sample_limit=sample_limit * 2)
    fatigue_window_count = int(fatigue_windows[0]["total_candidates"]) if fatigue_windows else 0
    judgement_contexts = collect_judgement_contexts(sentence_infos, sample_limit=sample_limit)
    scene_map = build_scene_map(paragraph_infos, sample_limit=sample_limit)
    dialogue_emotions = build_dialogue_emotion_profile(sentence_infos, sample_limit=sample_limit)
    character_voice = build_character_voice_profile(sentence_infos, sample_limit=sample_limit)
    tone_profile = build_tone_profile(paragraph_infos, sample_limit=sample_limit)
    battle_profile = build_battle_profile(sentence_infos, sample_limit=sample_limit)
    viewpoint_profile = build_viewpoint_profile(paragraph_infos, sample_limit=sample_limit)
    judgement_context_warn = any(bool(item["warn"]) for item in judgement_contexts)
    tracked_term_windows = build_tracked_term_windows(
        term_bank or [],
        corpus_profile.learned_terms if corpus_profile else [],
        sentence_infos,
        sample_limit=sample_limit * 2,
    )
    tracked_term_window_count = int(tracked_term_windows[0]["total_candidates"]) if tracked_term_windows else 0
    terms = [
        {"term": phrase, "count": count}
        for phrase, count in collect_ngram_terms(
            text,
            min_count_by_size={2: 8, 3: 5, 4: 4},
            require_structure=False,
        )
    ]
    short_phrases = [
        {"term": phrase, "count": count}
        for phrase, count in collect_ngram_terms(
            text,
            min_count_by_size={2: 6, 3: 5, 4: 4},
            require_structure=True,
        )
    ]
    dominant_punctuation = sorted(
        (
            {
                "mark": metric["name"],
                "count": metric["count"],
                "per_10k": metric["per_10k"],
            }
            for metric in punctuation_metrics
            if metric["count"] > 0
        ),
        key=lambda item: (-item["count"], item["mark"]),
    )

    dialogue = {
        "consecutive_quote_paragraph_runs": [
            {"start_paragraph": start, "end_paragraph": end, "sample": sample}
            for start, end, sample in quote_runs
        ],
        "short_quote_runs": [
            {
                "start_paragraph": start,
                "end_paragraph": end,
                "avg_len": avg_len,
                "sample": sample,
            }
            for start, end, avg_len, sample in short_quote_runs
        ],
        "question_ping_pong": [
            {"start_paragraph": start, "end_paragraph": end, "sample": sample}
            for start, end, sample in question_ping_pong
        ],
        "quote_ping_pong": [
            {
                "start_paragraph": start,
                "end_paragraph": end,
                "avg_len": avg_len,
                "sample": sample,
            }
            for start, end, avg_len, sample in quote_ping_pong
        ],
        "dialogue_axis_gaps": dialogue_axis_gaps,
        "alternating_speaker_runs": [
            {"paragraph": idx, "pattern": pattern}
            for idx, pattern in ab_turns
        ],
        "quote_paragraph_ratio": round(len(quote_runs) / max(len(paragraphs), 1), 4),
        "dense_quote_run_max": max((end - start + 1 for start, end, _ in quote_runs), default=0),
        "dense_quote_run_count": len([1 for start, end, _ in quote_runs if end - start + 1 >= 5]),
    }
    dialogue_warn = bool(quote_runs or ab_turns or dialogue_axis_gaps)

    summary = {
        "chars": chars,
        "sentences": len(sentence_infos),
        "paragraphs": len(paragraphs),
        "avg_sentence_chars": round(chars / max(len(sentence_infos), 1), 2),
        "short_sentences": sentence_lengths["short_count"],
        "very_short_sentences": sentence_lengths["very_short_count"],
        "short_sentence_ratio": sentence_lengths["short_ratio"],
        "quote_ratio": round(quote_ratio(text), 4),
        "warn_sections": 0,
    }
    tail_text = text[-180:]
    ending_images = [{"term": term, "count": tail_text.count(term)} for term in ENDING_IMAGE_TERMS if term in tail_text]
    ending_flows = [{"term": term, "count": tail_text.count(term)} for term in ENDING_FLOW_TERMS if term in tail_text]
    ending_warn = len(ending_images) >= 3 or len(ending_flows) >= 2

    template_candidates: list[dict[str, object]] = []
    for section_name, metrics in (
        ("tracked_term", tracked_term_metrics),
        ("tokens", token_metrics),
        ("patterns", pattern_metrics),
        ("phrases", phrase_metrics),
        ("modifiers", modifier_metrics),
        ("punctuation", punctuation_metrics),
        ("punctuation_combo", combo_metrics),
        ("custom_template", custom_template_metrics),
        ("learned_filter", learned_filter_metrics),
    ):
        for metric in metrics:
            if metric["warn"]:
                template_candidates.append(
                    {
                        "type": section_name,
                        "name": metric["name"],
                        "count": metric["count"],
                        "note": metric["note"],
                        "sample": metric["samples"][0]["snippet"] if metric["samples"] else "",
                    }
                )
    for item in sentence_patterns:
        template_candidates.append(
            {
                "type": "sentence_pattern",
                "name": item["phrase"],
                "count": item["count"],
                "note": "句首骨架重复",
                "sample": "",
            }
        )
    for item in aa_bb_patterns:
        if item["warn"]:
            template_candidates.append(
                {
                    "type": "aa_bb_pattern",
                    "name": item["name"],
                    "count": item["count"],
                    "note": item["note"],
                    "sample": item["samples"][0] if item["samples"] else "",
                }
            )
    for item in ba_operation_contexts:
        if not item["warn"]:
            continue
        samples = item.get("samples", [])
        sample = samples[0]["sentence"] if samples else ""
        template_candidates.append(
            {
                "type": "ba_operation_context",
                "name": item["role"],
                "count": item["count"],
                "note": f"把字句类型偏密；{item.get('suggestion', '')}",
                "sample": sample,
            }
        )
    if sentence_lengths["warn"]:
        sample = ""
        if sentence_lengths["short_sentences"]:
            first_short = sentence_lengths["short_sentences"][0]
            sample = f"L{first_short['line_no']} {first_short['chars']}字：{first_short['text']}"
        template_candidates.append(
            {
                "type": "sentence_length",
                "name": "短句密度",
                "count": sentence_lengths["short_count"],
                "note": "短句过多或连发，会让草稿像节拍器或对白录音",
                "sample": sample,
            }
        )
    for item in short_phrases[:5]:
        template_candidates.append(
            {
                "type": "short_phrase",
                "name": item["term"],
                "count": item["count"],
                "note": "短语手感重复",
                "sample": "",
            }
        )
    if quote_runs:
        template_candidates.append(
            {
                "type": "dialogue",
                "name": "连续短对白",
                "count": len(quote_runs),
                "note": "A/B 乒乓过长",
                "sample": " | ".join(quote_runs[0][2]),
            }
        )
    if short_quote_runs:
        template_candidates.append(
            {
                "type": "dialogue",
                "name": "短句对白块",
                "count": len(short_quote_runs),
                "note": "对白短句过密，容易写成互答录音",
                "sample": " | ".join(short_quote_runs[0][3]),
            }
        )
    if quote_ping_pong:
        template_candidates.append(
            {
                "type": "dialogue",
                "name": "对白乒乓",
                "count": len(quote_ping_pong),
                "note": "纯对白互顶过长，缺少动作或场面转轴",
                "sample": " | ".join(quote_ping_pong[0][3]),
            }
        )
    if dialogue_axis_gaps:
        first_gap = dialogue_axis_gaps[0]
        template_candidates.append(
            {
                "type": "dialogue_axis_gap",
                "name": "对白转轴缺口",
                "count": len(dialogue_axis_gaps),
                "note": f"连续对白缺少动作、环境、第三方或设备转轴；{first_gap.get('suggestion', '')}",
                "sample": " | ".join(str(item) for item in first_gap["sample"][:4]),
            }
        )
    if scene_map["warn"]:
        first_block = scene_map["blocks"][0] if scene_map["blocks"] else {}
        template_candidates.append(
            {
                "type": "scene_map",
                "name": "场面功能失衡",
                "count": int(scene_map["block_count"]),
                "note": f"粗分块里 `{scene_map['dominant_role']}` 占比偏高，检查这章是否长时间停在同一种叙事功能里。",
                "sample": " | ".join(str(item) for item in first_block.get("sample", [])[:2]),
            }
        )
    if dialogue_emotions["flatness_warn"] or dialogue_emotions["volatility_warn"]:
        first_sample = dialogue_emotions["samples"][0] if dialogue_emotions["samples"] else {}
        template_candidates.append(
            {
                "type": "dialogue_emotion",
                "name": "对白情绪单一/横跳",
                "count": int(dialogue_emotions["shift_count"] or dialogue_emotions["dialogue_sentences"]),
                "note": f"dominant={dialogue_emotions['dominant_emotion']} shift={dialogue_emotions['shift_count']}，检查对白是否只在重复顶回去。",
                "sample": str(first_sample.get("text", "")),
            }
        )
    if character_voice["warn"]:
        first_speaker = character_voice["speakers"][0] if character_voice["speakers"] else {}
        template_candidates.append(
            {
                "type": "character_voice",
                "name": "角色对白同质化",
                "count": int(character_voice["speaker_count"]),
                "note": "多名角色的对白节拍、问句率和情绪主导过近，检查是否越来越像同一个人在说话。",
                "sample": str(first_speaker.get("speaker", "")),
            }
        )
    if battle_profile["warn"]:
        first_sample = battle_profile["samples"][0] if battle_profile["samples"] else {}
        template_candidates.append(
            {
                "type": "battle_profile",
                "name": "动作链缺结果",
                "count": int(battle_profile["sequence_count"]),
                "note": "动作/冲突段有推进，但结果句、受伤反馈或位移后果不足，容易只剩挥打。",
                "sample": " | ".join(str(item) for item in first_sample.get("sample", [])[:4]),
            }
        )
    if viewpoint_profile["warn"]:
        first_overlap = viewpoint_profile["overlaps"][0] if viewpoint_profile["overlaps"] else {}
        template_candidates.append(
            {
                "type": "viewpoint_profile",
                "name": "视角锚点漂移",
                "count": int(viewpoint_profile["overlap_count"] or viewpoint_profile["switch_count"]),
                "note": "同段多人物心理暴露或近距离视角反复换锚，读者容易丢当前镜头中心。",
                "sample": str(first_overlap.get("text", "")),
            }
        )
    if ending_warn:
        template_candidates.append(
            {
                "type": "ending",
                "name": "章末模板",
                "count": len(ending_images) + len(ending_flows),
                "note": "章末意象或流程词偏密，检查是否又在模板化收尾",
                "sample": tail_text.strip().replace("\n", " ")[:80],
            }
        )
    if fatigue_windows:
        first_window = fatigue_windows[0]
        template_candidates.append(
            {
                "type": "fatigue_window",
                "name": "局部疲劳窗口",
                "count": len(fatigue_windows),
                "note": "短句、判断、把字句、角色起手等问题在局部连续叠加",
                "sample": " | ".join(str(item) for item in first_window["sample"][:5]),
            }
        )
    if tracked_term_windows:
        first_window = tracked_term_windows[0]
        template_candidates.append(
            {
                "type": "tracked_term_window",
                "name": "点名局部密度",
                "count": len(tracked_term_windows),
                "note": f"同一名词或同类跟踪词在局部窗口内密集出现；{format_tracked_term_counts(first_window.get('terms', [])[:3])}",
                "sample": " | ".join(str(item) for item in first_window["sample"][:5]),
            }
        )
    for item in judgement_contexts:
        if not item["warn"]:
            continue
        sample = ""
        if item["samples"]:
            first_sample = item["samples"][0]
            sample = f"L{first_sample['line_no']} {first_sample['text']}"
        template_candidates.append(
            {
                "type": "judgement_context",
                "name": item["label"],
                "count": item["count"],
                "note": "旁白判断句偏密，容易替场面下结论",
                "sample": sample,
            }
        )

    hard_flags: list[dict[str, object]] = []

    def extend_hard_flags(section: str, metrics: list[dict[str, object]]) -> None:
        for metric in metrics:
            if not metric["warn"]:
                continue
            hard_flags.append(
                {
                    "section": section,
                    "name": metric["name"],
                    "count": int(metric["count"]),
                    "per_10k": metric.get("per_10k"),
                    "note": metric["note"],
                    "sample": metric["samples"][0]["snippet"] if metric["samples"] else "",
                }
            )

    extend_hard_flags("tracked_terms", tracked_term_metrics)
    extend_hard_flags("tokens", token_metrics)
    extend_hard_flags("patterns", pattern_metrics)
    extend_hard_flags("phrases", phrase_metrics)
    extend_hard_flags("modifiers", modifier_metrics)
    extend_hard_flags("punctuation", punctuation_metrics)
    extend_hard_flags("punctuation_combos", combo_metrics)
    extend_hard_flags("custom_templates", custom_template_metrics)
    extend_hard_flags("learned_filters", learned_filter_metrics)

    for item in sentence_patterns:
        hard_flags.append(
            {
                "section": "sentence_patterns",
                "name": item["phrase"],
                "count": int(item["count"]),
                "per_10k": None,
                "note": "句首骨架重复",
                "sample": "",
            }
        )
    for item in subject_leads:
        hard_flags.append(
            {
                "section": "subject_leads",
                "name": item["phrase"],
                "count": int(item["count"]),
                "per_10k": None,
                "note": "主语起手重复",
                "sample": "",
            }
        )
    for item in paragraph_leads:
        hard_flags.append(
            {
                "section": "paragraph_leads",
                "name": item["phrase"],
                "count": int(item["count"]),
                "per_10k": None,
                "note": "段首起手重复",
                "sample": "",
            }
        )
    for item in clause_prefixes:
        hard_flags.append(
            {
                "section": "clause_prefixes",
                "name": item["phrase"],
                "count": int(item["count"]),
                "per_10k": None,
                "note": "分句骨架重复",
                "sample": "",
            }
        )
    for item in short_phrases[:10]:
        hard_flags.append(
            {
                "section": "short_phrases",
                "name": item["term"],
                "count": int(item["count"]),
                "per_10k": None,
                "note": "结构短语重复",
                "sample": "",
            }
        )
    for item in aa_bb_patterns:
        if not item["warn"]:
            continue
        hard_flags.append(
            {
                "section": "aa_bb_patterns",
                "name": item["name"],
                "count": int(item["count"]),
                "per_10k": None,
                "note": item["note"],
                "sample": item["samples"][0] if item["samples"] else "",
            }
        )
    for item in ba_operation_contexts:
        if not item["warn"]:
            continue
        samples = item.get("samples", [])
        sample = samples[0]["sentence"] if samples else ""
        hard_flags.append(
            {
                "section": "ba_operation_contexts",
                "name": item["role"],
                "count": int(item["count"]),
                "per_10k": None,
                "note": f"把字句类型偏密；{item.get('suggestion', '')}",
                "sample": sample,
            }
        )
    if sentence_lengths["warn"]:
        if sentence_lengths["short_runs"]:
            first_run = sentence_lengths["short_runs"][0]
            hard_flags.append(
                {
                    "section": "sentence_lengths",
                    "name": "短句连发",
                    "count": len(sentence_lengths["short_runs"]),
                    "per_10k": None,
                    "note": f"连续短句会把叙述切成机械节拍；类型：{format_short_roles(first_run.get('roles', []))}；建议：{first_run.get('suggestion', '')}",
                    "sample": " | ".join(first_run["sample"]),
                }
            )
        if sentence_lengths["short_count"]:
            first_short = sentence_lengths["short_sentences"][0]
            hard_flags.append(
                {
                    "section": "sentence_lengths",
                    "name": "短句密度",
                    "count": int(sentence_lengths["short_count"]),
                    "per_10k": None,
                    "note": "短句过多时需要判断是节奏控制还是内容没写开",
                    "sample": f"L{first_short['line_no']} {first_short['chars']}字：{first_short['text']}",
                }
            )
    if quote_runs:
        hard_flags.append(
            {
                "section": "dialogue",
                "name": "连续短对白块",
                "count": len(quote_runs),
                "per_10k": None,
                "note": "对话过长且缺少动作转轴",
                "sample": " | ".join(quote_runs[0][2]),
            }
        )
    if short_quote_runs:
        hard_flags.append(
            {
                "section": "dialogue",
                "name": "短句对白块",
                "count": len(short_quote_runs),
                "per_10k": None,
                "note": "对白像互答录音",
                "sample": " | ".join(short_quote_runs[0][3]),
            }
        )
    if question_ping_pong:
        hard_flags.append(
            {
                "section": "dialogue",
                "name": "问答互顶",
                "count": len(question_ping_pong),
                "per_10k": None,
                "note": "问一句顶一句，像脚本对白",
                "sample": " | ".join(question_ping_pong[0][2]),
            }
        )
    if quote_ping_pong:
        hard_flags.append(
            {
                "section": "dialogue",
                "name": "对白乒乓",
                "count": len(quote_ping_pong),
                "per_10k": None,
                "note": "纯对白来回互顶",
                "sample": " | ".join(quote_ping_pong[0][3]),
            }
        )
    if dialogue_axis_gaps:
        first_gap = dialogue_axis_gaps[0]
        hard_flags.append(
            {
                "section": "dialogue_axis_gaps",
                "name": "对白转轴缺口",
                "count": len(dialogue_axis_gaps),
                "per_10k": None,
                "note": f"连续对白缺少动作、环境、第三方或设备转轴；{first_gap.get('suggestion', '')}",
                "sample": " | ".join(str(item) for item in first_gap["sample"][:4]),
            }
        )
    if scene_map["warn"]:
        first_block = scene_map["blocks"][0] if scene_map["blocks"] else {}
        hard_flags.append(
            {
                "section": "scene_map",
                "name": "场面功能失衡",
                "count": int(scene_map["block_count"]),
                "per_10k": None,
                "note": f"粗分块里 `{scene_map['dominant_role']}` 占比 `{scene_map['dominance_ratio']}`，场面功能切换偏少。",
                "sample": " | ".join(str(item) for item in first_block.get("sample", [])[:2]),
            }
        )
    if dialogue_emotions["flatness_warn"] or dialogue_emotions["volatility_warn"]:
        first_sample = dialogue_emotions["samples"][0] if dialogue_emotions["samples"] else {}
        hard_flags.append(
            {
                "section": "dialogue_emotions",
                "name": "对白情绪单一/横跳",
                "count": int(dialogue_emotions["shift_count"] or dialogue_emotions["dialogue_sentences"]),
                "per_10k": None,
                "note": f"dominant={dialogue_emotions['dominant_emotion']} ratio={dialogue_emotions['dominant_ratio']} shift={dialogue_emotions['shift_count']}",
                "sample": str(first_sample.get("text", "")),
            }
        )
    if character_voice["warn"]:
        hard_flags.append(
            {
                "section": "character_voice",
                "name": "角色对白同质化",
                "count": int(character_voice["speaker_count"]),
                "per_10k": None,
                "note": f"dominant={character_voice['dominant_speaker']} coverage={character_voice['coverage_ratio']}，多名角色对白画像过近。",
                "sample": " | ".join(str(item) for item in character_voice["homogenized_pairs"][:2]),
            }
        )
    if battle_profile["warn"]:
        first_sample = battle_profile["samples"][0] if battle_profile["samples"] else {}
        hard_flags.append(
            {
                "section": "battle_profile",
                "name": "动作链缺结果",
                "count": int(battle_profile["sequence_count"]),
                "per_10k": None,
                "note": f"result_ratio={battle_profile['result_ratio']}，动作句已有堆积，但结果/伤害反馈不足。",
                "sample": " | ".join(str(item) for item in first_sample.get("sample", [])[:4]),
            }
        )
    if viewpoint_profile["warn"]:
        first_overlap = viewpoint_profile["overlaps"][0] if viewpoint_profile["overlaps"] else {}
        hard_flags.append(
            {
                "section": "viewpoint_profile",
                "name": "视角锚点漂移",
                "count": int(viewpoint_profile["overlap_count"] or viewpoint_profile["switch_count"]),
                "per_10k": None,
                "note": "同段多人物心理暴露或近距离切锚偏多。",
                "sample": str(first_overlap.get("text", "")),
            }
        )
    if ending_warn:
        hard_flags.append(
            {
                "section": "ending",
                "name": "章末模板",
                "count": len(ending_images) + len(ending_flows),
                "per_10k": None,
                "note": "章末意象或流程词偏密",
                "sample": tail_text.strip().replace("\n", " ")[:80],
            }
        )
    if fatigue_windows:
        first_window = fatigue_windows[0]
        hard_flags.append(
            {
                "section": "fatigue_windows",
                "name": "局部疲劳窗口",
                "count": len(fatigue_windows),
                "per_10k": None,
                "note": f"短句、判断、把字句、角色起手等问题在局部连续叠加；类型：{format_short_roles(first_window.get('roles', []))}；建议：{first_window.get('suggestion', '')}",
                "sample": " | ".join(str(item) for item in first_window["sample"][:5]),
            }
        )
    if tracked_term_windows:
        first_window = tracked_term_windows[0]
        hard_flags.append(
            {
                "section": "tracked_term_windows",
                "name": "点名局部密度",
                "count": len(tracked_term_windows),
                "per_10k": None,
                "note": f"同一名词或同类跟踪词在局部窗口内密集出现；{format_tracked_term_counts(first_window.get('terms', [])[:3])}；建议：{first_window.get('suggestion', '')}",
                "sample": " | ".join(str(item) for item in first_window["sample"][:5]),
            }
        )
    for item in judgement_contexts:
        if not item["warn"]:
            continue
        sample = ""
        if item["samples"]:
            first_sample = item["samples"][0]
            sample = f"L{first_sample['line_no']} {first_sample['text']}"
        hard_flags.append(
            {
                "section": "judgement_contexts",
                "name": item["label"],
                "count": int(item["count"]),
                "per_10k": None,
                "note": "旁白判断句偏密，容易替场面下结论",
                "sample": sample,
            }
        )
    hard_flags.sort(
        key=lambda item: (
            -int(item["count"]),
            item["section"],
            item["name"],
        )
    )

    warned = any(
        [
            token_warn,
            pattern_warn,
            phrase_warn,
            modifier_warn,
            punctuation_warn,
            combo_warn,
            custom_warn,
            tracked_term_warn,
            learned_filter_warn,
            bool(repeated_starts),
            bool(subject_leads),
            bool(paragraph_leads),
            bool(sentence_patterns),
            bool(judgement_endings),
            bool(clause_prefixes),
            bool(parallel_clauses),
            aa_bb_warn,
            ba_operation_context_warn,
            bool(sentence_lengths["warn"]),
            any(item["warn"] for item in modifier_pressure),
            bool(short_phrases),
            dialogue_warn,
            scene_map["warn"],
            dialogue_emotions["flatness_warn"] or dialogue_emotions["volatility_warn"],
            battle_profile["warn"],
            viewpoint_profile["warn"],
            bool(short_quote_runs),
            bool(question_ping_pong),
            bool(quote_ping_pong),
            ending_warn,
            bool(fatigue_windows),
            judgement_context_warn,
            bool(tracked_term_windows),
        ]
    )
    warn_sections = sum(
        [
            int(token_warn),
            int(pattern_warn),
            int(phrase_warn),
            int(modifier_warn),
            int(punctuation_warn),
            int(combo_warn),
            int(custom_warn),
            int(tracked_term_warn),
            int(learned_filter_warn),
            int(bool(repeated_starts)),
            int(bool(subject_leads)),
            int(bool(paragraph_leads)),
            int(bool(sentence_patterns)),
            int(bool(judgement_endings)),
            int(bool(clause_prefixes)),
            int(bool(parallel_clauses)),
            int(aa_bb_warn),
            int(ba_operation_context_warn),
            int(bool(sentence_lengths["warn"])),
            int(any(item["warn"] for item in modifier_pressure)),
            int(bool(short_phrases)),
            int(dialogue_warn),
            int(scene_map["warn"]),
            int(dialogue_emotions["flatness_warn"] or dialogue_emotions["volatility_warn"]),
            int(battle_profile["warn"]),
            int(viewpoint_profile["warn"]),
            int(bool(short_quote_runs)),
            int(bool(question_ping_pong)),
            int(bool(quote_ping_pong)),
            int(ending_warn),
            int(bool(fatigue_windows)),
            int(judgement_context_warn),
            int(bool(tracked_term_windows)),
        ]
    )
    summary["warn_sections"] = warn_sections

    analysis = {
        "source": source,
        "summary": summary,
        "warned": warned,
        "tokens": token_metrics,
        "tracked_terms": tracked_term_metrics,
        "tracked_term_categories": tracked_term_categories,
        "tracked_term_windows": tracked_term_windows,
        "tracked_term_window_count": tracked_term_window_count,
        "ba_operation_contexts": ba_operation_contexts,
        "patterns": pattern_metrics,
        "phrases": phrase_metrics,
        "modifiers": modifier_metrics,
        "punctuation": punctuation_metrics,
        "punctuation_combos": combo_metrics,
        "custom_templates": custom_template_metrics,
        "learned_filters": learned_filter_metrics,
        "corpus_profile": {
            "enabled": corpus_profile is not None,
            "source_count": corpus_profile.source_count if corpus_profile else 0,
            "chars": corpus_profile.chars if corpus_profile else 0,
            "draft_chars": corpus_profile.draft_chars if corpus_profile else 0,
            "learned_terms": [
                {
                    "name": item.name,
                    "category": item.category,
                    "count": item.count,
                    "corpus_per_10k": item.corpus_per_10k,
                    "max_per_10k": item.max_per_10k,
                }
                for item in (corpus_profile.learned_terms if corpus_profile else [])
            ],
            "learned_style_phrases": [
                {
                    "name": item.name,
                    "category": item.category,
                    "count": item.count,
                    "corpus_per_10k": item.corpus_per_10k,
                    "max_per_10k": item.max_per_10k,
                }
                for item in (corpus_profile.learned_style_phrases if corpus_profile else [])
            ],
            "learned_sentence_leads": corpus_profile.learned_sentence_leads if corpus_profile else [],
            "learned_aa_bb_shapes": corpus_profile.learned_aa_bb_shapes if corpus_profile else [],
            "sentence_length_baseline": corpus_profile.sentence_length_baseline if corpus_profile else {},
        },
        "dominant_punctuation": dominant_punctuation,
        "sentence_starts": repeated_starts,
        "subject_leads": subject_leads,
        "paragraph_leads": paragraph_leads,
        "sentence_patterns": sentence_patterns,
        "judgement_endings": judgement_endings,
        "clause_prefixes": clause_prefixes,
        "parallel_clauses": parallel_clauses,
        "aa_bb_patterns": aa_bb_patterns,
        "sentence_lengths": sentence_lengths,
        "fatigue_windows": fatigue_windows,
        "fatigue_window_count": fatigue_window_count,
        "judgement_contexts": judgement_contexts,
        "modifier_pressure": modifier_pressure,
        "terms": terms,
        "short_phrases": short_phrases,
        "dialogue": dialogue,
        "scene_map": scene_map,
        "dialogue_emotions": dialogue_emotions,
        "character_voice": character_voice,
        "tone_profile": tone_profile,
        "battle_profile": battle_profile,
        "viewpoint_profile": viewpoint_profile,
        "ending": {
            "tail_excerpt": tail_text.strip(),
            "image_terms": ending_images,
            "flow_terms": ending_flows,
            "warn": ending_warn,
        },
        "template_candidates": template_candidates,
        "hard_flags": hard_flags,
    }
    analysis["style_fatigue"] = build_style_fatigue(analysis)
    analysis["review_reminders"] = build_review_reminders(analysis)
    return analysis


def analyze_path(
    path: Path,
    *,
    sample_limit: int,
    corpus_profile: CorpusProfile | None = None,
    template_bank: list[TemplateRule] | None = None,
    term_bank: list[TrackedTerm] | None = None,
) -> dict[str, object]:
    text = path.read_text(encoding="utf-8")
    return analyze_text(
        text,
        sample_limit=sample_limit,
        source=str(path),
        template_bank=template_bank if template_bank is not None else load_template_bank(DEFAULT_REVIEW_RULES_PATH),
        term_bank=term_bank if term_bank is not None else load_term_bank(DEFAULT_REVIEW_RULES_PATH),
        corpus_profile=corpus_profile,
    )


def _format_metric_block(title: str, metrics: list[dict[str, object]], *, sample_limit: int) -> list[str]:
    out = [f"{title}:"]
    for metric in metrics:
        status = "WARN" if metric["warn"] else "OK"
        out.append(
            f"  [{status}] {metric['name']}: count={metric['count']}, per_10k={metric['per_10k']:.2f}, max={metric['max_per_10k']:.2f}  # {metric['note']}"
        )
        for sample in metric["samples"][:sample_limit]:
            out.append(f"    L{sample['line_no']}: {sample['snippet']}")
    return out


def _markdown_table_cell(value: object) -> str:
    return str(value).replace("\n", " ").replace("|", "\\|")


def format_text_report(analysis: dict[str, object], *, sample_limit: int) -> str:
    summary = analysis["summary"]
    output = [
        f"FILE {analysis['source']}",
        f"chars={summary['chars']}",
        f"sentences={summary['sentences']}",
        f"paragraphs={summary['paragraphs']}",
        f"avg_sentence_chars={summary['avg_sentence_chars']}",
        f"short_sentences={summary['short_sentences']}",
        f"very_short_sentences={summary['very_short_sentences']}",
        f"short_sentence_ratio={summary['short_sentence_ratio']}",
        f"quote_ratio={summary['quote_ratio']}",
        f"warn_sections={summary['warn_sections']}",
    ]
    output.append("hard_flags:")
    output.append(f"  count={len(analysis['hard_flags'])}")
    for item in analysis["hard_flags"][: sample_limit * 8]:
        per_10k = (
            f", per_10k={item['per_10k']}"
            if item["per_10k"] is not None
            else ""
        )
        line = f"    [{item['section']}] {item['name']}: count={item['count']}{per_10k}  # {item['note']}"
        if item["sample"]:
            line += f" | {item['sample']}"
        output.append(line)
    output.append("review_reminders:")
    output.append(f"  count={len(analysis['review_reminders'])}")
    for item in analysis["review_reminders"][: sample_limit * 4]:
        output.append(
            f"    [{item['priority']}] {item['category']} {item['title']}  # {item['reason']}"
        )
        output.append(f"      check: {item['check']}")
        output.append(f"      action: {item['action']}")
        for evidence in item["evidence"][:sample_limit]:
            output.append(f"      evidence: {evidence}")
    output.append("style_fatigue:")
    output.append(f"  count={len(analysis['style_fatigue'])}")
    for item in analysis["style_fatigue"]:
        output.append(
            f"    [{item['status']}] {item['family']}: count={item['count']}  # {item['risk']}"
        )
        output.append(f"      reduce: {item['reduce']}")
        for evidence in item["evidence"][:sample_limit]:
            output.append(f"      evidence: {evidence}")
    output.append("fatigue_windows:")
    output.append(
        f"  count={analysis['fatigue_window_count']}, shown={len(analysis['fatigue_windows'])}"
    )
    for item in analysis["fatigue_windows"][:sample_limit]:
        roles = format_short_roles(item.get("roles", []), separator=",")
        output.append(
            f"    S{item['start_index']}-{item['end_index']} L{item['start_line']}-{item['end_line']} score={item['score']} reasons={','.join(str(reason) for reason in item['reasons'])} roles={roles}"
        )
        output.append(f"      suggestion: {item.get('suggestion', '')}")
        output.append(f"      sample: {' | '.join(str(sample) for sample in item['sample'][:5])}")
    output.append("ba_operation_contexts:")
    output.append(
        f"  [{'WARN' if any(item['warn'] for item in analysis['ba_operation_contexts']) else 'OK'}] types={len(analysis['ba_operation_contexts'])}"
    )
    for item in analysis["ba_operation_contexts"][:sample_limit * 3]:
        output.append(
            f"    {'WARN' if item['warn'] else 'WATCH'} {item['role']}: count={item['count']} suggestion={item.get('suggestion', '')}"
        )
        for sample in item.get("samples", [])[:sample_limit]:
            output.append(f"      - S{sample['index']} L{sample['line_no']} {sample['snippet']}: {sample['sentence']}")
    output.extend(_format_metric_block("tokens", analysis["tokens"], sample_limit=sample_limit))
    output.extend(_format_metric_block("tracked_terms", analysis["tracked_terms"], sample_limit=sample_limit))
    output.extend(_format_metric_block("patterns", analysis["patterns"], sample_limit=sample_limit))
    output.extend(_format_metric_block("phrases", analysis["phrases"], sample_limit=sample_limit))
    output.extend(_format_metric_block("modifiers", analysis["modifiers"], sample_limit=sample_limit))
    output.extend(_format_metric_block("punctuation", analysis["punctuation"], sample_limit=sample_limit))
    output.extend(_format_metric_block("punctuation_combos", analysis["punctuation_combos"], sample_limit=sample_limit))
    output.extend(_format_metric_block("custom_templates", analysis["custom_templates"], sample_limit=sample_limit))
    output.extend(_format_metric_block("learned_filters", analysis["learned_filters"], sample_limit=sample_limit))

    profile = analysis["corpus_profile"]
    output.append("corpus_profile:")
    output.append(
        f"  [{'OK' if profile['enabled'] else 'OFF'}] sources={profile['source_count']} chars={profile['chars']} draft_chars={profile['draft_chars']}"
    )
    baseline = profile["sentence_length_baseline"]
    if baseline:
        output.append(
            f"  baseline_sentence_chars: p10={baseline['p10_chars']}, p25={baseline['p25_chars']}, median={baseline['median_chars']}, avg={baseline['avg_chars']}, short_ratio={baseline['short_ratio']}"
        )
    for item in profile["learned_sentence_leads"][:sample_limit]:
        output.append(
            f"    learned_lead {item['phrase']}: count={item['count']}, corpus_per_10k={item['corpus_per_10k']}"
        )
    for item in profile["learned_aa_bb_shapes"][:sample_limit]:
        output.append(f"    learned_aa_bb {item['name']}: count={item['count']}")

    output.append("tracked_term_categories:")
    output.append(
        f"  [{'WARN' if any(item['warn'] for item in analysis['tracked_term_categories']) else 'OK'}] active_categories={len(analysis['tracked_term_categories'])}"
    )
    for item in analysis["tracked_term_categories"][: sample_limit * 4]:
        output.append(
            f"    {item['category']}: count={item['count']}, active_terms={item['active_terms']}, warn_terms={item['warn_terms']}"
        )
        for term in item["top_terms"][:3]:
            output.append(
                f"      - {term['term']}: count={term['count']}, per_10k={term['per_10k']}, warn={'Y' if term['warn'] else 'N'}"
            )
    output.append("tracked_term_windows:")
    output.append(
        f"  count={analysis['tracked_term_window_count']}, shown={len(analysis['tracked_term_windows'])}"
    )
    for item in analysis["tracked_term_windows"][:sample_limit]:
        output.append(
            f"    S{item['start_index']}-{item['end_index']} L{item['start_line']}-{item['end_line']} score={item['score']} reasons={','.join(str(reason) for reason in item['reasons'])} terms={format_tracked_term_counts(item.get('terms', []), separator=',')}"
        )
        output.append(f"      suggestion: {item.get('suggestion', '')}")
        output.append(f"      sample: {' | '.join(str(sample) for sample in item['sample'][:5])}")

    output.append("sentence_starts:")
    output.append(
        f"  [{'WARN' if analysis['sentence_starts'] else 'OK'}] repeated_sentence_leads={len(analysis['sentence_starts'])}"
    )
    for item in analysis["sentence_starts"][: sample_limit * 3]:
        output.append(f"    {item['phrase']}: {item['count']}")

    output.append("subject_leads:")
    output.append(
        f"  [{'WARN' if analysis['subject_leads'] else 'OK'}] repeated_subject_leads={len(analysis['subject_leads'])}"
    )
    for item in analysis["subject_leads"][: sample_limit * 4]:
        output.append(f"    {item['phrase']}: {item['count']}")

    output.append("paragraph_leads:")
    output.append(
        f"  [{'WARN' if analysis['paragraph_leads'] else 'OK'}] repeated_paragraph_leads={len(analysis['paragraph_leads'])}"
    )
    for item in analysis["paragraph_leads"][: sample_limit * 4]:
        output.append(f"    {item['phrase']}: {item['count']}")

    output.append("sentence_patterns:")
    output.append(
        f"  [{'WARN' if analysis['sentence_patterns'] else 'OK'}] repeated_sentence_skeletons={len(analysis['sentence_patterns'])}"
    )
    for item in analysis["sentence_patterns"][: sample_limit * 4]:
        output.append(f"    {item['phrase']}: {item['count']}")

    output.append("judgement_endings:")
    output.append(
        f"  [{'WARN' if analysis['judgement_endings'] else 'OK'}] repeated_judgement_endings={len(analysis['judgement_endings'])}"
    )
    for item in analysis["judgement_endings"][: sample_limit * 4]:
        output.append(f"    {item['phrase']}: {item['count']}")

    output.append("judgement_contexts:")
    output.append(
        f"  [{'WARN' if any(item['warn'] for item in analysis['judgement_contexts']) else 'OK'}] contexts={len(analysis['judgement_contexts'])}"
    )
    for item in analysis["judgement_contexts"]:
        terms = ", ".join(f"{term['term']}:{term['count']}" for term in item["top_terms"])
        output.append(
            f"    {'WARN' if item['warn'] else 'WATCH' if item['watch'] else 'OK'} {item['label']}: count={item['count']} terms={terms or '无'}"
        )
        for sample in item["samples"][:sample_limit]:
            output.append(
                f"      S{sample['index']} L{sample['line_no']} {','.join(str(term) for term in sample['terms'])}: {sample['text']}"
            )

    output.append("clause_prefixes:")
    output.append(
        f"  [{'WARN' if analysis['clause_prefixes'] else 'OK'}] repeated_clause_prefixes={len(analysis['clause_prefixes'])}"
    )
    for item in analysis["clause_prefixes"][: sample_limit * 4]:
        output.append(f"    {item['phrase']}: {item['count']}")

    output.append("parallel_clauses:")
    output.append(
        f"  [{'WARN' if analysis['parallel_clauses'] else 'OK'}] repeated_parallel_clauses={len(analysis['parallel_clauses'])}"
    )
    for item in analysis["parallel_clauses"][: sample_limit * 4]:
        output.append(f"    {item['phrase']}: {item['count']}")

    output.append("aa_bb_patterns:")
    output.append(
        f"  [{'WARN' if any(item['warn'] for item in analysis['aa_bb_patterns']) else 'OK'}] aa_bb_patterns={len(analysis['aa_bb_patterns'])}"
    )
    for item in analysis["aa_bb_patterns"][: sample_limit * 4]:
        output.append(
            f"    {'WARN' if item['warn'] else 'OK'} {item['type']} {item['name']}: count={item['count']}  # {item['note']}"
        )
        for sample in item["samples"][:sample_limit]:
            output.append(f"      - {sample}")

    sentence_lengths = analysis["sentence_lengths"]
    output.append("sentence_lengths:")
    output.append(
        f"  [{'WARN' if sentence_lengths['warn'] else 'OK'}] count={sentence_lengths['count']}, min={sentence_lengths['min_chars']}, p10={sentence_lengths['p10_chars']}, p25={sentence_lengths['p25_chars']}, median={sentence_lengths['median_chars']}, avg={sentence_lengths['avg_chars']}, max={sentence_lengths['max_chars']}"
    )
    output.append(
        f"    short_count={sentence_lengths['short_count']}, very_short_count={sentence_lengths['very_short_count']}, short_ratio={sentence_lengths['short_ratio']}, short_runs={len(sentence_lengths['short_runs'])}"
    )
    for item in sentence_lengths["short_sentences"][: sample_limit * 4]:
        output.append(
            f"    S{item['index']} L{item['line_no']} chars={item['chars']}: {item['text']}"
        )
    for item in sentence_lengths["short_runs"][:sample_limit]:
        roles = format_short_roles(item.get("roles", []), separator=",")
        output.append(
            f"    run S{item['start_index']}-{item['end_index']} L{item['start_line']}-{item['end_line']} avg={item['avg_chars']}: {' | '.join(item['sample'])}"
        )
        if roles:
            output.append(f"      roles: {roles}")
        if item.get("suggestion"):
            output.append(f"      suggestion: {item['suggestion']}")

    output.append("terms:")
    output.append(f"  [{'WARN' if analysis['terms'] else 'OK'}] repeated_terms={len(analysis['terms'])}")
    for item in analysis["terms"][: sample_limit * 5]:
        output.append(f"    {item['term']}: {item['count']}")

    output.append("short_phrases:")
    output.append(
        f"  [{'WARN' if analysis['short_phrases'] else 'OK'}] repeated_short_phrases={len(analysis['short_phrases'])}"
    )
    for item in analysis["short_phrases"][: sample_limit * 5]:
        output.append(f"    {item['term']}: {item['count']}")

    output.append("dialogue:")
    runs = analysis["dialogue"]["consecutive_quote_paragraph_runs"]
    short_runs = analysis["dialogue"]["short_quote_runs"]
    question_runs = analysis["dialogue"]["question_ping_pong"]
    ping_pong_runs = analysis["dialogue"]["quote_ping_pong"]
    axis_gaps = analysis["dialogue"]["dialogue_axis_gaps"]
    turns = analysis["dialogue"]["alternating_speaker_runs"]
    output.append(f"  [{'WARN' if runs else 'OK'}] consecutive_quote_paragraph_runs={len(runs)}")
    for item in runs[:sample_limit]:
        output.append(
            f"    paragraph {item['start_paragraph']}-{item['end_paragraph']}: {' | '.join(item['sample'])}"
        )
    output.append(f"  [{'WARN' if short_runs else 'OK'}] short_quote_runs={len(short_runs)}")
    for item in short_runs[:sample_limit]:
        output.append(
            f"    paragraph {item['start_paragraph']}-{item['end_paragraph']}: avg_len={item['avg_len']} | {' | '.join(item['sample'])}"
        )
    output.append(f"  [{'WARN' if question_runs else 'OK'}] question_ping_pong={len(question_runs)}")
    for item in question_runs[:sample_limit]:
        output.append(
            f"    paragraph {item['start_paragraph']}-{item['end_paragraph']}: {' | '.join(item['sample'])}"
        )
    output.append(f"  [{'WARN' if ping_pong_runs else 'OK'}] quote_ping_pong={len(ping_pong_runs)}")
    for item in ping_pong_runs[:sample_limit]:
        output.append(
            f"    paragraph {item['start_paragraph']}-{item['end_paragraph']}: avg_len={item['avg_len']} | {' | '.join(item['sample'])}"
        )
    output.append(f"  [{'WARN' if axis_gaps else 'OK'}] dialogue_axis_gaps={len(axis_gaps)}")
    for item in axis_gaps[:sample_limit]:
        output.append(
            f"    S{item['start_index']}-{item['end_index']} L{item['start_line']}-{item['end_line']} score={item['score']} reasons={','.join(str(reason) for reason in item['reasons'])}"
        )
        output.append(f"      suggestion: {item.get('suggestion', '')}")
        output.append(f"      sample: {' | '.join(str(sample) for sample in item['sample'][:4])}")
    output.append(f"  [{'WARN' if turns else 'OK'}] alternating_speaker_runs={len(turns)}")
    for item in turns[:sample_limit]:
        output.append(f"    paragraph {item['paragraph']}: {item['pattern']}")
    output.append(f"  [INFO] quote_paragraph_ratio={analysis['dialogue']['quote_paragraph_ratio']}")
    output.append(f"  [INFO] dense_quote_run_max={analysis['dialogue']['dense_quote_run_max']}")
    output.append(f"  [INFO] dense_quote_run_count={analysis['dialogue']['dense_quote_run_count']}")

    output.append("dominant_punctuation:")
    output.append(
        f"  [{'WARN' if analysis['dominant_punctuation'] else 'OK'}] active_marks={len(analysis['dominant_punctuation'])}"
    )
    for item in analysis["dominant_punctuation"][: sample_limit * 4]:
        output.append(f"    {item['mark']}: count={item['count']}, per_10k={item['per_10k']}")

    output.append("modifier_pressure:")
    active_modifier_pressure = [item for item in analysis["modifier_pressure"] if item["total"] > 0]
    output.append(
        f"  [{'WARN' if any(item['warn'] for item in active_modifier_pressure) else 'OK'}] active_groups={len(active_modifier_pressure)}"
    )
    for item in active_modifier_pressure[:sample_limit]:
        output.append(
            f"    {item['label']}: total={item['total']}, dense_sentences={item['dense_sentences']}, warn={'Y' if item['warn'] else 'N'}"
        )

    output.append("ending:")
    output.append(f"  [{'WARN' if analysis['ending']['warn'] else 'OK'}] tail_template_check")
    image_summary = ", ".join(
        f"{item['term']}:{item['count']}" for item in analysis["ending"]["image_terms"]
    ) or "无"
    flow_summary = ", ".join(
        f"{item['term']}:{item['count']}" for item in analysis["ending"]["flow_terms"]
    ) or "无"
    ending_image_md = ", ".join(
        f"{item['term']} x{item['count']}" for item in analysis["ending"]["image_terms"]
    )
    ending_flow_md = ", ".join(
        f"{item['term']} x{item['count']}" for item in analysis["ending"]["flow_terms"]
    )
    output.append(
        f"    image_terms={image_summary}"
    )
    output.append(
        f"    flow_terms={flow_summary}"
    )

    output.append("template_candidates:")
    output.append(f"  count={len(analysis['template_candidates'])}")
    for item in analysis["template_candidates"][: sample_limit * 6]:
        line = f"    [{item['type']}] {item['name']} x{item['count']}  # {item['note']}"
        if item["sample"]:
            line += f" | {item['sample']}"
        output.append(line)

    return "\n".join(output)


def format_markdown_report(analysis: dict[str, object], *, title: str | None = None) -> str:
    summary = analysis["summary"]
    title = title or analysis["source"]
    ending_image_md = ", ".join(
        f"{item['term']} x{item['count']}" for item in analysis["ending"]["image_terms"]
    )
    ending_flow_md = ", ".join(
        f"{item['term']} x{item['count']}" for item in analysis["ending"]["flow_terms"]
    )
    lines = [f"# {title}", ""]
    lines.append("## 概览")
    lines.append(f"- 来源：`{analysis['source']}`")
    lines.append(f"- 字数：`{summary['chars']}`")
    lines.append(f"- 句子数：`{summary['sentences']}`")
    lines.append(f"- 段落数：`{summary['paragraphs']}`")
    lines.append(f"- 句均字数：`{summary['avg_sentence_chars']}`")
    lines.append(f"- 短句数：`{summary['short_sentences']}`")
    lines.append(f"- 极短句数：`{summary['very_short_sentences']}`")
    lines.append(f"- 短句占比：`{summary['short_sentence_ratio']}`")
    lines.append(f"- 引号占比：`{summary['quote_ratio']}`")
    lines.append(f"- 警告分区数：`{summary['warn_sections']}`")
    lines.append(f"- 总体状态：`{'WARN' if analysis['warned'] else 'OK'}`")
    lines.append("")

    lines.append("## 审查提醒")
    if analysis["review_reminders"]:
        for item in analysis["review_reminders"][:8]:
            line = (
                f"- `{item['priority']}` `{item['category']}` {item['title']}："
                f"{item['reason']} 检查：{item['check']} 动作：{item['action']}"
            )
            if item["evidence"]:
                line += f" 证据：{'；'.join(_markdown_table_cell(value) for value in item['evidence'])}"
            lines.append(line)
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## 句式疲劳雷达")
    if analysis["style_fatigue"]:
        lines.append("| 状态 | 句式家族 | 数量 | 风险 | 减少方式 | 证据 |")
        lines.append("|---|---|---:|---|---|---|")
        for item in analysis["style_fatigue"]:
            evidence = "；".join(_markdown_table_cell(value) for value in item["evidence"]) or "无"
            lines.append(
                f"| `{item['status']}` | {_markdown_table_cell(item['family'])} | `{item['count']}` | {_markdown_table_cell(item['risk'])} | {_markdown_table_cell(item['reduce'])} | {evidence} |"
            )
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## 局部疲劳窗口")
    if analysis["fatigue_windows"]:
        lines.append(
            f"- 命中总数：`{analysis['fatigue_window_count']}`；展示：`{len(analysis['fatigue_windows'][:8])}`"
        )
        for item in analysis["fatigue_windows"][:8]:
            roles = format_short_roles(item.get("roles", []))
            lines.append(
                f"- `S{item['start_index']}-{item['end_index']}` `L{item['start_line']}-{item['end_line']}` score=`{item['score']}`：{_markdown_table_cell('、'.join(str(reason) for reason in item['reasons']))}"
            )
            if roles:
                lines.append(f"  类型：{_markdown_table_cell(roles)}")
            if item.get("suggestion"):
                lines.append(f"  建议：{_markdown_table_cell(item['suggestion'])}")
            lines.append(f"  样例：{_markdown_table_cell(' | '.join(str(sample) for sample in item['sample'][:5]))}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## 把字操作分类")
    if analysis["ba_operation_contexts"]:
        for item in analysis["ba_operation_contexts"]:
            lines.append(
                f"- `{'WARN' if item['warn'] else 'WATCH'}` `{item['role']}` count=`{item['count']}`：{_markdown_table_cell(item.get('suggestion', ''))}"
            )
            for sample in item.get("samples", [])[:5]:
                lines.append(
                    f"  - `S{sample['index']}` `L{sample['line_no']}` `{sample['snippet']}`：{_markdown_table_cell(sample['sentence'])}"
                )
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## 优先修项")
    if analysis["hard_flags"]:
        for item in analysis["hard_flags"][:15]:
            line = f"- `{item['section']}` `{item['name']}` x{item['count']}：{item['note']}"
            if item["per_10k"] is not None:
                line += f"；per_10k=`{item['per_10k']}`"
            if item["sample"]:
                line += f"；样例：{item['sample']}"
            lines.append(line)
    else:
        lines.append("- 无")
    lines.append("")

    def add_metric_section(header: str, metrics: list[dict[str, object]]) -> None:
        lines.append(f"## {header}")
        if not metrics:
            lines.append("- 无")
            lines.append("")
            return
        for metric in metrics:
            status = "WARN" if metric["warn"] else "OK"
            lines.append(
                f"- `{status}` `{metric['name']}` count=`{metric['count']}` per_10k=`{metric['per_10k']}` max=`{metric['max_per_10k']}`"
            )
            if metric["samples"]:
                sample = metric["samples"][0]
                lines.append(f"  样例：`L{sample['line_no']}` {sample['snippet']}")
        lines.append("")

    add_metric_section("高频词", analysis["tokens"])
    add_metric_section("跟踪词", analysis["tracked_terms"])
    add_metric_section("模板句", analysis["patterns"])
    add_metric_section("短触发词", analysis["phrases"])
    add_metric_section("黏糊词与判断副词", analysis["modifiers"])
    add_metric_section("标点", analysis["punctuation"])
    add_metric_section("组合标点", analysis["punctuation_combos"])
    add_metric_section("模板库命中", analysis["custom_templates"])
    add_metric_section("语料学习筛选", analysis["learned_filters"])

    lines.append("## 语料学习基线")
    profile = analysis["corpus_profile"]
    if profile["enabled"]:
        lines.append(
            f"- 学习来源：`{profile['source_count']}` 个文件，语料字数=`{profile['chars']}`，草稿字数=`{profile['draft_chars']}`"
        )
        baseline = profile["sentence_length_baseline"]
        if baseline:
            lines.append(
                f"- 草稿句长基线：p10=`{baseline['p10_chars']}` p25=`{baseline['p25_chars']}` median=`{baseline['median_chars']}` avg=`{baseline['avg_chars']}` short_ratio=`{baseline['short_ratio']}`"
            )
        if profile["learned_sentence_leads"]:
            leads = "，".join(
                f"{item['phrase']} x{item['count']}"
                for item in profile["learned_sentence_leads"][:8]
            )
            lines.append(f"- 学到的句首高频：{leads}")
        if profile["learned_aa_bb_shapes"]:
            shapes = "，".join(
                f"{item['name']} x{item['count']}"
                for item in profile["learned_aa_bb_shapes"][:8]
            )
            lines.append(f"- 学到的 AA/BB 风险：{shapes}")
    else:
        lines.append("- 未启用")
    lines.append("")

    lines.append("## 跟踪词分类")
    if analysis["tracked_term_categories"]:
        for item in analysis["tracked_term_categories"]:
            lines.append(
                f"- `{'WARN' if item['warn'] else 'OK'}` `{item['category']}` count=`{item['count']}` active_terms=`{item['active_terms']}` warn_terms=`{item['warn_terms']}`"
            )
            for term in item["top_terms"][:5]:
                lines.append(
                    f"  - `{term['term']}` x{term['count']} per_10k=`{term['per_10k']}` warn=`{'Y' if term['warn'] else 'N'}`"
                )
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## 点名局部密度")
    if analysis["tracked_term_windows"]:
        lines.append(
            f"- 命中总数：`{analysis['tracked_term_window_count']}`；展示：`{len(analysis['tracked_term_windows'][:8])}`"
        )
        for item in analysis["tracked_term_windows"][:8]:
            lines.append(
                f"- `S{item['start_index']}-{item['end_index']}` `L{item['start_line']}-{item['end_line']}` score=`{item['score']}`：{_markdown_table_cell('、'.join(str(reason) for reason in item['reasons']))}"
            )
            terms = format_tracked_term_counts(item.get("terms", []))
            if terms:
                lines.append(f"  词项：{_markdown_table_cell(terms)}")
            if item.get("suggestion"):
                lines.append(f"  建议：{_markdown_table_cell(item['suggestion'])}")
            lines.append(f"  样例：{_markdown_table_cell(' | '.join(str(sample) for sample in item['sample'][:5]))}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## 高频词片段")
    if analysis["terms"]:
        for item in analysis["terms"][:15]:
            lines.append(f"- `{item['term']}` x{item['count']}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## 结构短语")
    if analysis["short_phrases"]:
        for item in analysis["short_phrases"][:15]:
            lines.append(f"- `{item['term']}` x{item['count']}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## 句式骨架")
    if analysis["sentence_patterns"]:
        for item in analysis["sentence_patterns"]:
            lines.append(f"- `{item['phrase']}` x{item['count']}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## 判断句尾")
    if analysis["judgement_endings"]:
        for item in analysis["judgement_endings"]:
            lines.append(f"- `{item['phrase']}` x{item['count']}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## 判断句上下文")
    if analysis["judgement_contexts"]:
        for item in analysis["judgement_contexts"]:
            status = "WARN" if item["warn"] else "WATCH" if item["watch"] else "OK"
            terms = "，".join(f"{term['term']} x{term['count']}" for term in item["top_terms"]) or "无"
            lines.append(f"- `{status}` `{item['label']}` count=`{item['count']}` terms={terms}")
            for sample in item["samples"][:5]:
                lines.append(
                    f"  - `S{sample['index']}` `L{sample['line_no']}` `{','.join(str(term) for term in sample['terms'])}`：{sample['text']}"
                )
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## 句首重复")
    if analysis["sentence_starts"]:
        for item in analysis["sentence_starts"]:
            lines.append(f"- `{item['phrase']}` x{item['count']}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## 主语起手")
    if analysis["subject_leads"]:
        for item in analysis["subject_leads"]:
            lines.append(f"- `{item['phrase']}` x{item['count']}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## 段首起手")
    if analysis["paragraph_leads"]:
        for item in analysis["paragraph_leads"]:
            lines.append(f"- `{item['phrase']}` x{item['count']}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## 分句骨架")
    if analysis["clause_prefixes"]:
        for item in analysis["clause_prefixes"][:15]:
            lines.append(f"- `{item['phrase']}` x{item['count']}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## 并列分句")
    if analysis["parallel_clauses"]:
        for item in analysis["parallel_clauses"][:15]:
            lines.append(f"- `{item['phrase']}` x{item['count']}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## AA/BB 式短节奏")
    if analysis["aa_bb_patterns"]:
        for item in analysis["aa_bb_patterns"][:15]:
            lines.append(
                f"- `{'WARN' if item['warn'] else 'OK'}` `{item['type']}` `{item['name']}` x{item['count']}：{item['note']}"
            )
            if item["samples"]:
                lines.append(f"  样例：{item['samples'][0]}")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## 逐句字数")
    sentence_lengths = analysis["sentence_lengths"]
    lines.append(
        f"- 状态：`{'WARN' if sentence_lengths['warn'] else 'OK'}` count=`{sentence_lengths['count']}` min=`{sentence_lengths['min_chars']}` p10=`{sentence_lengths['p10_chars']}` p25=`{sentence_lengths['p25_chars']}` median=`{sentence_lengths['median_chars']}` avg=`{sentence_lengths['avg_chars']}` max=`{sentence_lengths['max_chars']}`"
    )
    lines.append(
        f"- 短句：`{sentence_lengths['short_count']}`；极短句：`{sentence_lengths['very_short_count']}`；短句占比：`{sentence_lengths['short_ratio']}`；短句连发：`{len(sentence_lengths['short_runs'])}`"
    )
    if sentence_lengths["short_sentences"]:
        for item in sentence_lengths["short_sentences"][:12]:
            lines.append(
                f"- `S{item['index']}` `L{item['line_no']}` `{item['chars']}字`：{item['text']}"
            )
    if sentence_lengths["short_runs"]:
        lines.append("- 短句连发样例：")
        for item in sentence_lengths["short_runs"][:5]:
            roles = format_short_roles(item.get("roles", []))
            lines.append(
                f"- `S{item['start_index']}-{item['end_index']}` `L{item['start_line']}-{item['end_line']}` avg=`{item['avg_chars']}`：{' | '.join(item['sample'])}"
            )
            if roles:
                lines.append(f"  类型：{roles}")
            if item.get("suggestion"):
                lines.append(f"  建议：{item['suggestion']}")
    if " | " not in str(analysis["source"]):
        lines.append("")
        lines.append("### 每句字数明细")
        for item in sentence_lengths["sentences"]:
            lines.append(
                f"- `S{item['index']}` `L{item['line_no']}` `{item['chars']}字`：{item['text']}"
            )
    lines.append("")

    lines.append("## 对话")
    runs = analysis["dialogue"]["consecutive_quote_paragraph_runs"]
    short_runs = analysis["dialogue"]["short_quote_runs"]
    question_runs = analysis["dialogue"]["question_ping_pong"]
    ping_pong_runs = analysis["dialogue"]["quote_ping_pong"]
    axis_gaps = analysis["dialogue"]["dialogue_axis_gaps"]
    turns = analysis["dialogue"]["alternating_speaker_runs"]
    lines.append(f"- 连续短对白块：`{len(runs)}`")
    for item in runs[:5]:
        lines.append(f"- 段落 `{item['start_paragraph']}-{item['end_paragraph']}`: {' | '.join(item['sample'])}")
    lines.append(f"- 短句对白块：`{len(short_runs)}`")
    for item in short_runs[:5]:
        lines.append(
            f"- 段落 `{item['start_paragraph']}-{item['end_paragraph']}` 平均句长=`{item['avg_len']}`: {' | '.join(item['sample'])}"
        )
    lines.append(f"- 问答互顶块：`{len(question_runs)}`")
    for item in question_runs[:5]:
        lines.append(f"- 段落 `{item['start_paragraph']}-{item['end_paragraph']}`: {' | '.join(item['sample'])}")
    lines.append(f"- 白话乒乓块：`{len(ping_pong_runs)}`")
    for item in ping_pong_runs[:5]:
        lines.append(
            f"- 段落 `{item['start_paragraph']}-{item['end_paragraph']}` 平均句长=`{item['avg_len']}`: {' | '.join(item['sample'])}"
        )
    lines.append(f"- 对白转轴缺口：`{len(axis_gaps)}`")
    for item in axis_gaps[:5]:
        lines.append(
            f"- `S{item['start_index']}-{item['end_index']}` `L{item['start_line']}-{item['end_line']}` score=`{item['score']}`：{_markdown_table_cell('、'.join(str(reason) for reason in item['reasons']))}"
        )
        lines.append(f"  建议：{_markdown_table_cell(item.get('suggestion', ''))}")
        lines.append(f"  样例：{_markdown_table_cell(' | '.join(str(sample) for sample in item['sample'][:4]))}")
    lines.append(f"- A/B 乒乓：`{len(turns)}`")
    for item in turns[:5]:
        lines.append(f"- 段落 `{item['paragraph']}`: `{item['pattern']}`")
    lines.append(f"- 对话段占比：`{analysis['dialogue']['quote_paragraph_ratio']}`")
    lines.append(f"- 最长连续对白块：`{analysis['dialogue']['dense_quote_run_max']}`")
    lines.append(f"- 超长对白块数：`{analysis['dialogue']['dense_quote_run_count']}`")
    lines.append("")

    lines.append("## 活跃标点")
    if analysis["dominant_punctuation"]:
        for item in analysis["dominant_punctuation"][:10]:
            lines.append(f"- `{item['mark']}` x{item['count']} per_10k=`{item['per_10k']}`")
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## 形容词 / 动词压力")
    active_modifier_pressure = [item for item in analysis["modifier_pressure"] if item["total"] > 0]
    if active_modifier_pressure:
        for item in active_modifier_pressure:
            lines.append(
                f"- `{'WARN' if item['warn'] else 'OK'}` `{item['label']}` total=`{item['total']}` dense_sentences=`{item['dense_sentences']}`"
            )
    else:
        lines.append("- 无")
    lines.append("")

    lines.append("## 章末检查")
    lines.append(f"- 状态：`{'WARN' if analysis['ending']['warn'] else 'OK'}`")
    if analysis["ending"]["image_terms"]:
        lines.append(f"- 章末意象词：`{ending_image_md}`")
    if analysis["ending"]["flow_terms"]:
        lines.append(f"- 章末流程词：`{ending_flow_md}`")
    lines.append(f"- 章末摘录：{analysis['ending']['tail_excerpt'][:100] or '无'}")
    lines.append("")

    lines.append("## 模版候选")
    if analysis["template_candidates"]:
        for item in analysis["template_candidates"][:20]:
            line = f"- `{item['type']}` `{item['name']}` x{item['count']}：{item['note']}"
            if item["sample"]:
                line += f"；样例：{item['sample']}"
            lines.append(line)
    else:
        lines.append("- 无")
    lines.append("")
    return "\n".join(lines)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Audit drafts for repeated words and template sentence patterns.")
    parser.add_argument("paths", nargs="*", help="Files or directories to scan")
    parser.add_argument("-i", "--input", action="append", dest="inputs", help="Input file or directory; may be repeated")
    parser.add_argument("--sample-limit", type=int, default=3, help="Max sample lines per rule")
    parser.add_argument("--fail-on-warn", action="store_true", help="Exit 1 when any warning appears")
    parser.add_argument("--format", choices=["text", "json", "markdown"], default="text", help="Report output format")
    parser.add_argument("-o", "--output", help="Output file or directory")
    parser.add_argument(
        "--learn-from",
        nargs="*",
        help="Optional corpus paths for learned filters. Defaults to concept/cards, plans, and drafts under the same novel.",
    )
    parser.add_argument(
        "--no-corpus-learning",
        action="store_true",
        help="Disable learned filters from existing cards, plans, and drafts.",
    )
    return parser.parse_args()


def _render_report(analysis: dict[str, object], output_format: str, sample_limit: int) -> str:
    if output_format == "markdown":
        return format_markdown_report(analysis, title=Path(str(analysis["source"])).stem)
    return format_text_report(analysis, sample_limit=sample_limit)


def _write_reports(reports: list[dict[str, object]], output_format: str, output: str | None, sample_limit: int) -> None:
    if output is None:
        if output_format == "json":
            print(json.dumps(reports, ensure_ascii=False, indent=2))
            return
        for report in reports:
            print(_render_report(report, output_format, sample_limit))
            print()
        return

    out_path = Path(output)
    if output_format == "json":
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(json.dumps(reports, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        return

    suffix = ".md" if output_format == "markdown" else ".txt"
    if len(reports) == 1:
        write_text(out_path, _render_report(reports[0], output_format, sample_limit) + "\n")
        return

    out_path.mkdir(parents=True, exist_ok=True)
    for report in reports:
        source = Path(str(report["source"]))
        write_text(out_path / f"{source.stem}{suffix}", _render_report(report, output_format, sample_limit) + "\n")


def main() -> int:
    args = parse_args()
    files = iter_target_files(resolve_inputs(args.paths, args.inputs))
    if not files:
        print("No target files found.", file=sys.stderr)
        return 2

    any_warn = False
    reports: list[dict[str, object]] = []
    corpus_profile = None
    if not args.no_corpus_learning:
        corpus_paths = args.learn_from
        if corpus_paths is None:
            corpus_paths = [str(path) for path in corpus_paths_for_targets(files)]
        corpus_profile = build_corpus_profile(corpus_paths)
    template_bank = load_template_bank(DEFAULT_REVIEW_RULES_PATH)
    term_bank = load_term_bank(DEFAULT_REVIEW_RULES_PATH)
    for path in files:
        analysis = analyze_path(
            path,
            sample_limit=args.sample_limit,
            corpus_profile=corpus_profile,
            template_bank=template_bank,
            term_bank=term_bank,
        )
        any_warn = any_warn or bool(analysis["warned"])
        reports.append(analysis)

    _write_reports(reports, args.format, args.output, args.sample_limit)

    if args.fail_on_warn and any_warn:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
