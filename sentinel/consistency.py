#!/usr/bin/env python3
"""Build and query a lightweight consistency search index for novel files."""

from __future__ import annotations

import argparse
import collections
import json
import re
import shlex
import sqlite3
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path


CARD_GLOB = "concept/cards/**/*.md"
DOC_GLOBS = (
    "concept/cards/**/*.md",
    "arc-plan/**/*.md",
    "story-plan/**/*.md",
    "chapter-plan/**/*.md",
    "drafts/**/*.md",
)

ALIASES_SPLIT_RE = re.compile(r"[，,、；;]")
HEADER_RE = re.compile(r"^#\s+(.*)$", re.MULTILINE)
FIELD_RE = re.compile(r"^- ([^：]+)：\s*(.*)$", re.MULTILINE)
STORY_PLAN_FILE_RE = re.compile(r"^(story)-(\d+[a-z]?)", re.IGNORECASE)
INTERLUDE_PLAN_FILE_RE = re.compile(r"^(interlude)-(\d+)", re.IGNORECASE)
STORY_CHAPTER_FILE_RE = re.compile(r"^(story\d+|interlude\d+)-ch\d+", re.IGNORECASE)
CHAPTER_ONLY_FILE_RE = re.compile(r"^ch\d+\.md$", re.IGNORECASE)
FACT_SEGMENT_SPLIT_RE = re.compile(r"[。！？!?；;\n]+")
NEGATED_RELATION_PATTERNS = (
    re.compile(r"(不等于|不是|并非|非)\s*信任"),
    re.compile(r"信任\s*(?:不了|不起来|不起|不能)"),
)
FEEDBACK_DECISIONS = ("confirmed", "false_positive", "designed_keep", "watch")
FEEDBACK_FACETS = (
    "rhythm",
    "voice",
    "motif",
    "scene_callback",
    "register",
    "naming",
    "irony",
    "state_progression",
    "extractor_noise",
    "scope_drift",
)

INJURY_NEGATIVE_TERMS = (
    "受伤",
    "流血",
    "出血",
    "伤口",
    "裂开",
    "擦伤",
    "扭伤",
    "包扎",
    "咳血",
    "发白",
    "发烧",
    "疼得",
    "止血贴",
)

INJURY_STABLE_TERMS = (
    "没事",
    "稳住",
    "恢复",
    "缓过",
    "站稳",
    "止住",
    "轻伤",
    "能走",
    "还能打",
)

EQUIPMENT_DAMAGED_TERMS = (
    "裂开",
    "擦痕",
    "损坏",
    "失灵",
    "熄灭",
    "坏了",
    "断掉",
    "烧毁",
    "暴露体积",
)

EQUIPMENT_ACTIVE_TERMS = (
    "展开",
    "变形",
    "启动",
    "亮起",
    "抬起",
    "展开快",
    "护住",
    "挡在",
    "接口",
)

GOAL_ASSIGNED_TERMS = (
    "任务",
    "委托",
    "命令",
    "要求",
    "安排",
    "交给",
    "负责",
)

GOAL_CHANGED_TERMS = (
    "改成",
    "转而",
    "临时改",
    "改口",
    "换成",
    "不再是",
    "目标变成",
)

GOAL_COMPLETED_TERMS = (
    "完成",
    "办完",
    "解决",
    "结束",
    "交差",
    "收尾",
    "达成",
)

RELATIONSHIP_CLOSE_TERMS = (
    "信任",
    "护住",
    "并肩",
    "默认",
    "配合",
    "接住",
    "愿意跟",
    "攥住",
    "按回去",
    "摁回去",
    "扯下来",
    "拎起来",
    "认你",
    "替他扛",
    "替她扛",
    "挡在前面",
    "挡在身前",
    "拉住",
    "拉回来",
    "接应",
    "护在前面",
)

RELATIONSHIP_DISTANT_TERMS = (
    "提防",
    "怀疑",
    "警惕",
    "疏远",
    "冷淡",
    "不信",
    "避开",
    "甩开",
    "推开",
    "别碰",
    "闭嘴",
    "少来",
    "离远点",
)

FACT_TERM_GROUPS = {
    "injury_negative": INJURY_NEGATIVE_TERMS,
    "injury_stable": INJURY_STABLE_TERMS,
    "equipment_damaged": EQUIPMENT_DAMAGED_TERMS,
    "equipment_active": EQUIPMENT_ACTIVE_TERMS,
    "goal_assigned": GOAL_ASSIGNED_TERMS,
    "goal_changed": GOAL_CHANGED_TERMS,
    "goal_completed": GOAL_COMPLETED_TERMS,
    "relationship_close": RELATIONSHIP_CLOSE_TERMS,
    "relationship_distant": RELATIONSHIP_DISTANT_TERMS,
}


@dataclass
class Entity:
    category: str
    card_path: Path
    title: str
    card_id: str
    names: list[str]


def normalize_whitespace(text: str) -> str:
    return re.sub(r"\s+", " ", text).strip()


def split_pipe_values(value: str | None) -> list[str]:
    if not value:
        return []
    return [item.strip() for item in str(value).split("|") if item.strip()]


def default_feedback_path_from_db(db_path: Path) -> Path:
    return db_path.parent / "review-feedback.jsonl"


def find_novel_dir(path: Path) -> Path | None:
    current = path.resolve() if path.exists() else path
    for candidate in [current, *current.parents]:
        if candidate.name.startswith("novel") and (candidate / "drafts").exists():
            return candidate
    return None


def conflict_key(row: dict[str, object]) -> str:
    return "||".join(
        [
            str(row.get("category", "")),
            str(row.get("story", "")),
            str(row.get("title", "")),
            str(row.get("entity_category", "")),
            str(row.get("summary", "")),
        ]
    )


def summary_fragment(summary: str, max_chars: int = 18) -> str:
    for token in (" ; ", "；", " -> ", ",", "，"):
        if token in summary:
            summary = summary.split(token, 1)[0]
            break
    summary = normalize_whitespace(summary)
    return summary[:max_chars]


def build_pending_review_focus(row: dict[str, object]) -> str:
    evidence_kind = str(row.get("evidence_kind", ""))
    category = str(row.get("category", ""))
    if evidence_kind == "fact":
        return "先回看 fact cue 和上下文段，判断这是真跳变还是阶段推进。"
    if evidence_kind == "alias":
        return "先对照 plan / draft 两侧称呼，判断是口径漂移还是有意压拍。"
    if evidence_kind == "alignment":
        return "先看 plan_only / draft_only 实体，判断是正文漏落还是施工图写偏。"
    if "relationship" in category:
        return "先回看关系动作词和同段人物共现，判断是关系转冷还是抽取误绑。"
    if "goal" in category:
        return "先回看任务口径和章末动作，判断是目标改口还是正常推进。"
    return "先回看相关证据段，再决定是 confirmed、false_positive 还是 designed_keep。"


def build_feedback_command(db_path: Path, row: dict[str, object], decision: str = "watch") -> str:
    command_root = find_novel_dir(db_path) or db_path
    root_label = command_root.name if command_root.name.startswith("novel") else str(command_root)
    command = [
        "python3",
        "scripts/consistency_index.py",
        "feedback-add",
        root_label,
        "--category",
        str(row["category"]),
        "--story",
        str(row["story"]),
        "--title",
        str(row["title"]),
        "--decision",
        decision,
    ]
    fragment = summary_fragment(str(row.get("summary", "")))
    if fragment:
        command.extend(["--summary-contains", fragment])
    return " ".join(shlex.quote(part) for part in command)


def build_pending_review_actions(db_path: Path, rows: list[dict[str, object]], limit: int = 6) -> list[dict[str, str]]:
    actions: list[dict[str, str]] = []
    for row in rows[:limit]:
        actions.append(
            {
                "story": str(row["story"]),
                "category": str(row["category"]),
                "title": str(row["title"]),
                "confidence": str(row.get("confidence", "")),
                "focus": build_pending_review_focus(row),
                "command": build_feedback_command(db_path, row),
            }
        )
    return actions


def read_feedback_history(path: Path) -> list[dict[str, str]]:
    if not path.exists():
        return []
    history: list[dict[str, str]] = []
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line:
            continue
        data = json.loads(line)
        history.append({str(k): str(v) for k, v in data.items() if v is not None})
    return history


def load_feedback_entries(path: Path) -> dict[str, dict[str, str]]:
    entries: dict[str, dict[str, str]] = {}
    for data in read_feedback_history(path):
        key = str(data.get("conflict_key", "")).strip()
        if not key:
            continue
        entries[key] = data
    return entries


def append_feedback_entry(path: Path, entry: dict[str, str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(entry, ensure_ascii=False) + "\n")


def build_feedback_backlog(feedback_history: list[dict[str, str]]) -> list[dict[str, str]]:
    category_decision_counter: collections.Counter[str] = collections.Counter()
    story_decision_counter: collections.Counter[str] = collections.Counter()
    facet_counter: collections.Counter[str] = collections.Counter()
    for entry in feedback_history:
        category = entry.get("category", "")
        story = entry.get("story", "")
        decision = entry.get("decision", "")
        facet = entry.get("facet", "")
        if category and decision:
            category_decision_counter[f"{category}::{decision}"] += 1
        if story and decision:
            story_decision_counter[f"{story}::{decision}"] += 1
        if decision == "designed_keep" and facet:
            facet_counter[facet] += 1

    backlog: list[dict[str, str]] = []

    false_positive_hits = [(name, count) for name, count in category_decision_counter.items() if name.endswith("::false_positive")]
    if false_positive_hits:
        top_name, top_count = sorted(false_positive_hits, key=lambda item: (-item[1], item[0]))[0]
        category = top_name.split("::", 1)[0]
        backlog.append(
            {
                "target": "scripts/consistency_index.py",
                "reason": f"`{category}` 已累计 {top_count} 条误报反馈，优先压抽取噪声，不要继续把人工复核当默认补丁。",
            }
        )

    designed_keep_hits = [(name, count) for name, count in category_decision_counter.items() if name.endswith("::designed_keep")]
    if designed_keep_hits:
        top_name, top_count = sorted(designed_keep_hits, key=lambda item: (-item[1], item[0]))[0]
        category = top_name.split("::", 1)[0]
        target = "scripts/rules.yaml#draft.template_rules"
        reason_tail = "说明这类变化应开始沉淀为可保留模式样本。"
        if facet_counter:
            top_facet, facet_count = facet_counter.most_common(1)[0]
            if top_facet in {"register", "naming"}:
                target = "novel1/rules/draft.md"
                reason_tail = f"其中 `{top_facet}` 已出现 {facet_count} 次，更适合先写成命名/称谓边界规则。"
            elif top_facet in {"voice", "rhythm", "motif", "scene_callback", "irony"}:
                target = "scripts/rules.yaml#draft.template_rules"
                reason_tail = f"其中 `{top_facet}` 已出现 {facet_count} 次，应开始积累这类可保留风格样本。"
        backlog.append(
            {
                "target": target,
                "reason": f"`{category}` 已累计 {top_count} 条设计性保留反馈，{reason_tail}",
            }
        )

    confirmed_hits = [(name, count) for name, count in story_decision_counter.items() if name.endswith("::confirmed")]
    if confirmed_hits:
        top_name, top_count = sorted(confirmed_hits, key=lambda item: (-item[1], item[0]))[0]
        story = top_name.split("::", 1)[0]
        backlog.append(
            {
                "target": "novel1/rules/draft.md",
                "reason": f"`{story}` 已累计 {top_count} 条确认成立的一致性问题，说明这不是偶发手误，值得沉淀为返工规则。",
            }
        )

    watch_hits = [(name, count) for name, count in story_decision_counter.items() if name.endswith("::watch")]
    if watch_hits:
        top_name, top_count = sorted(watch_hits, key=lambda item: (-item[1], item[0]))[0]
        story = top_name.split("::", 1)[0]
        backlog.append(
            {
                "target": "novel1/research/consistency/review-feedback.jsonl",
                "reason": f"`{story}` 仍有 {top_count} 条长期待观察反馈，说明这条 Story 的一致性口径还没真正收敛。",
            }
        )

    return backlog[:6]


def filter_feedback_history_by_story(history: list[dict[str, str]], story: str | None) -> list[dict[str, str]]:
    if story is None:
        return history
    return [entry for entry in history if entry.get("story", "") == story]


def build_story_conflict_snapshot_from_path(draft_path: Path, limit: int = 200) -> dict[str, object]:
    resolved_path = draft_path.resolve() if draft_path.exists() else draft_path
    novel_dir = find_novel_dir(resolved_path)
    if novel_dir is None:
        return {"available": False, "reason": "novel_dir_not_found"}

    db_path = novel_dir / "research" / "consistency" / "consistency.sqlite3"
    feedback_path = default_feedback_path_from_db(db_path)
    if not db_path.exists():
        return {"available": False, "reason": "db_not_found", "db_path": db_path, "feedback_path": feedback_path}

    doc_type, _arc, story, _chapter = classify_document(resolved_path, novel_dir)
    if doc_type != "drafts" or story is None:
        return {"available": False, "reason": "story_not_found", "db_path": db_path, "feedback_path": feedback_path}

    conn = open_db(db_path)
    try:
        rows = [row for row in collect_conflict_rows(conn, limit, feedback_path) if str(row["story"]) == story]
    finally:
        conn.close()

    feedback_entries = load_feedback_entries(feedback_path)
    feedback_history = read_feedback_history(feedback_path)
    decision_counter: collections.Counter[str] = collections.Counter()
    facet_counter: collections.Counter[str] = collections.Counter()
    pending_rows: list[dict[str, object]] = []
    category_counter: collections.Counter[str] = collections.Counter()
    for row in rows:
        decision = str(row.get("feedback_decision", ""))
        if decision:
            decision_counter[decision] += 1
            category_counter[f"{row['category']}::{decision}"] += 1
            facet = str(row.get("feedback_facet", ""))
            if facet:
                facet_counter[facet] += 1
        else:
            pending_rows.append(row)

    return {
        "available": True,
        "novel_dir": novel_dir,
        "db_path": db_path,
        "feedback_path": feedback_path,
        "story": story,
        "review_queue_command": f"python3 scripts/consistency_index.py review-queue {novel_dir.name} --story {story}",
        "feedback_summary_command": f"python3 scripts/consistency_index.py feedback-summary {novel_dir.name} --story {story}",
        "rows": rows,
        "decision_counter": decision_counter,
        "category_counter": category_counter,
        "facet_counter": facet_counter,
        "pending_rows": pending_rows,
        "pending_actions": build_pending_review_actions(db_path, pending_rows, 4),
        "global_feedback_backlog": build_feedback_backlog(feedback_history),
    }


def split_fact_segments(text: str) -> list[str]:
    return [segment.strip() for segment in FACT_SEGMENT_SPLIT_RE.split(text) if segment.strip()]


def is_negated_relationship_segment(segment: str, term: str) -> bool:
    if term not in {"信任", "默认", "配合", "护住", "并肩", "愿意跟", "接住"}:
        return False
    return any(pattern.search(segment) for pattern in NEGATED_RELATION_PATTERNS)


def collect_local_fact_cues(passage_text: str, entity_name: str, fact_type: str, terms: tuple[str, ...]) -> list[str]:
    cues: list[str] = []
    for segment in split_fact_segments(passage_text):
        if entity_name not in segment:
            continue
        entity_index = segment.find(entity_name)
        for term in terms:
            term_index = segment.find(term)
            if term_index < 0:
                continue
            if abs(term_index - entity_index) > 60:
                continue
            if fact_type == "relationship_close" and is_negated_relationship_segment(segment, term):
                continue
            if term not in cues:
                cues.append(term)
    return cues


def parse_field_map(text: str) -> dict[str, str]:
    fields: dict[str, str] = {}
    for key, value in FIELD_RE.findall(text):
        fields[normalize_whitespace(key)] = normalize_whitespace(value)
    return fields


def infer_title_variants(title: str) -> list[str]:
    variants: list[str] = []
    if "·" in title:
        for part in title.split("·"):
            cleaned = normalize_whitespace(part)
            if cleaned and cleaned not in variants:
                variants.append(cleaned)
    return variants


def extract_names(title: str, fields: dict[str, str]) -> list[str]:
    names: list[str] = [title.strip()]
    for variant in infer_title_variants(title):
        if variant not in names:
            names.append(variant)
    alias_value = fields.get("别名 / 英文名", "")
    if alias_value and alias_value not in {"无", "-", "待定"}:
        for part in ALIASES_SPLIT_RE.split(alias_value):
            cleaned = normalize_whitespace(part)
            if cleaned and cleaned not in names:
                names.append(cleaned)
    return names


def load_entities(novel_dir: Path) -> list[Entity]:
    entities: list[Entity] = []
    for card_path in sorted(novel_dir.glob(CARD_GLOB)):
        if "_templates" in card_path.parts:
            continue
        text = card_path.read_text(encoding="utf-8")
        match = HEADER_RE.search(text)
        if not match:
            continue
        title = normalize_whitespace(match.group(1))
        fields = parse_field_map(text)
        category = card_path.parent.name
        card_id = fields.get("卡片 ID", "")
        entities.append(
            Entity(
                category=category,
                card_path=card_path,
                title=title,
                card_id=card_id,
                names=extract_names(title, fields),
            )
        )
    return entities


def iter_documents(novel_dir: Path) -> list[Path]:
    docs: set[Path] = set()
    for pattern in DOC_GLOBS:
        docs.update(path for path in novel_dir.glob(pattern) if path.is_file())
    return sorted(docs)


def split_passages(text: str) -> list[tuple[int, int, str]]:
    passages: list[tuple[int, int, str]] = []
    paragraph_lines: list[str] = []
    start_line = 1
    current_line = 1

    for raw_line in text.splitlines():
        line = raw_line.rstrip()
        if line.strip():
            if not paragraph_lines:
                start_line = current_line
            paragraph_lines.append(line)
        elif paragraph_lines:
            passages.append((start_line, current_line - 1, "\n".join(paragraph_lines)))
            paragraph_lines = []
        current_line += 1
    if paragraph_lines:
        passages.append((start_line, current_line - 1, "\n".join(paragraph_lines)))
    return passages


def build_schema(conn: sqlite3.Connection) -> None:
    conn.executescript(
        """
        DROP TABLE IF EXISTS fact_candidates;
        DROP TABLE IF EXISTS mentions;
        DROP TABLE IF EXISTS passages;
        DROP TABLE IF EXISTS entity_names;
        DROP TABLE IF EXISTS entities;
        DROP TABLE IF EXISTS documents;
        DROP TABLE IF EXISTS passage_fts;

        CREATE TABLE documents (
            id INTEGER PRIMARY KEY,
            path TEXT NOT NULL UNIQUE,
            doc_type TEXT NOT NULL,
            arc TEXT,
            story TEXT,
            chapter TEXT
        );

        CREATE TABLE passages (
            id INTEGER PRIMARY KEY,
            document_id INTEGER NOT NULL,
            line_start INTEGER NOT NULL,
            line_end INTEGER NOT NULL,
            text TEXT NOT NULL,
            FOREIGN KEY(document_id) REFERENCES documents(id)
        );

        CREATE VIRTUAL TABLE passage_fts USING fts5(
            path UNINDEXED,
            text,
            content=''
        );

        CREATE TABLE entities (
            id INTEGER PRIMARY KEY,
            category TEXT NOT NULL,
            card_path TEXT NOT NULL,
            card_id TEXT,
            title TEXT NOT NULL
        );

        CREATE TABLE entity_names (
            id INTEGER PRIMARY KEY,
            entity_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            FOREIGN KEY(entity_id) REFERENCES entities(id)
        );

        CREATE TABLE mentions (
            id INTEGER PRIMARY KEY,
            entity_id INTEGER NOT NULL,
            entity_name_id INTEGER NOT NULL,
            document_id INTEGER NOT NULL,
            passage_id INTEGER NOT NULL,
            count INTEGER NOT NULL,
            FOREIGN KEY(entity_id) REFERENCES entities(id),
            FOREIGN KEY(entity_name_id) REFERENCES entity_names(id),
            FOREIGN KEY(document_id) REFERENCES documents(id),
            FOREIGN KEY(passage_id) REFERENCES passages(id)
        );

        CREATE TABLE fact_candidates (
            id INTEGER PRIMARY KEY,
            entity_id INTEGER NOT NULL,
            entity_name_id INTEGER NOT NULL,
            document_id INTEGER NOT NULL,
            passage_id INTEGER NOT NULL,
            fact_type TEXT NOT NULL,
            cue TEXT NOT NULL,
            FOREIGN KEY(entity_id) REFERENCES entities(id),
            FOREIGN KEY(entity_name_id) REFERENCES entity_names(id),
            FOREIGN KEY(document_id) REFERENCES documents(id),
            FOREIGN KEY(passage_id) REFERENCES passages(id)
        );

        CREATE INDEX idx_passages_document ON passages(document_id);
        CREATE INDEX idx_mentions_entity ON mentions(entity_id);
        CREATE INDEX idx_mentions_document ON mentions(document_id);
        CREATE INDEX idx_entity_names_name ON entity_names(name);
        CREATE INDEX idx_fact_candidates_entity ON fact_candidates(entity_id);
        CREATE INDEX idx_fact_candidates_type ON fact_candidates(fact_type);
        """
    )


def classify_document(path: Path, novel_dir: Path) -> tuple[str, str | None, str | None, str | None]:
    relative = path.relative_to(novel_dir)
    parts = relative.parts
    doc_type = parts[0]
    arc = None
    story = None
    chapter = None
    for part in parts:
        if re.fullmatch(r"arc\d+", part, re.IGNORECASE):
            arc = part.lower()
            break
    for part in parts:
        if re.fullmatch(r"story\d+", part, re.IGNORECASE) or re.fullmatch(r"interlude\d+", part, re.IGNORECASE):
            story = part.lower()
        if re.fullmatch(r"ch\d+\.md", part, re.IGNORECASE):
            chapter = part[:-3]
    stem = path.stem.lower()
    if story is None:
        match = STORY_CHAPTER_FILE_RE.match(stem)
        if match:
            story = match.group(1).lower()
        else:
            match = STORY_PLAN_FILE_RE.match(stem)
            if match:
                story = f"story{match.group(2).lower()}"
            else:
                match = INTERLUDE_PLAN_FILE_RE.match(stem)
                if match:
                    story = f"interlude{match.group(2).lower()}"
                elif doc_type == "chapter-plan" and CHAPTER_ONLY_FILE_RE.match(path.name):
                    story = "story1"
    return doc_type, arc, story, chapter


def build_index(novel_dir: Path, db_path: Path) -> None:
    entities = load_entities(novel_dir)
    documents = iter_documents(novel_dir)
    db_path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(db_path)
    try:
        build_schema(conn)
        entity_name_rows: list[tuple[int, str]] = []
        entity_rows: dict[int, Entity] = {}

        for entity in entities:
            cursor = conn.execute(
                "INSERT INTO entities(category, card_path, card_id, title) VALUES (?, ?, ?, ?)",
                (entity.category, str(entity.card_path), entity.card_id, entity.title),
            )
            entity_id = int(cursor.lastrowid)
            entity_rows[entity_id] = entity
            for name in entity.names:
                conn.execute(
                    "INSERT INTO entity_names(entity_id, name) VALUES (?, ?)",
                    (entity_id, name),
                )
        conn.commit()

        name_rows = conn.execute("SELECT id, entity_id, name FROM entity_names ORDER BY LENGTH(name) DESC").fetchall()
        name_records = [(int(row[0]), int(row[1]), str(row[2])) for row in name_rows]

        for path in documents:
            text = path.read_text(encoding="utf-8")
            doc_type, arc, story, chapter = classify_document(path, novel_dir)
            cursor = conn.execute(
                "INSERT INTO documents(path, doc_type, arc, story, chapter) VALUES (?, ?, ?, ?, ?)",
                (str(path), doc_type, arc, story, chapter),
            )
            document_id = int(cursor.lastrowid)
            passages = split_passages(text)

            for line_start, line_end, passage_text in passages:
                passage_cursor = conn.execute(
                    "INSERT INTO passages(document_id, line_start, line_end, text) VALUES (?, ?, ?, ?)",
                    (document_id, line_start, line_end, passage_text),
                )
                passage_id = int(passage_cursor.lastrowid)
                conn.execute(
                    "INSERT INTO passage_fts(rowid, path, text) VALUES (?, ?, ?)",
                    (passage_id, str(path), passage_text),
                )
                for entity_name_id, entity_id, name in name_records:
                    count = passage_text.count(name)
                    if count <= 0:
                        continue
                    conn.execute(
                        """
                        INSERT INTO mentions(entity_id, entity_name_id, document_id, passage_id, count)
                        VALUES (?, ?, ?, ?, ?)
                        """,
                        (entity_id, entity_name_id, document_id, passage_id, count),
                    )
                    for fact_type, terms in FACT_TERM_GROUPS.items():
                        matched_terms = collect_local_fact_cues(passage_text, name, fact_type, terms)
                        for term in matched_terms:
                            conn.execute(
                                """
                                INSERT INTO fact_candidates(entity_id, entity_name_id, document_id, passage_id, fact_type, cue)
                                VALUES (?, ?, ?, ?, ?, ?)
                                """,
                                (entity_id, entity_name_id, document_id, passage_id, fact_type, term),
                            )
        conn.commit()
    finally:
        conn.close()


def open_db(path: Path) -> sqlite3.Connection:
    conn = sqlite3.connect(path)
    conn.row_factory = sqlite3.Row
    return conn


def resolve_db_path(raw_path: Path) -> Path:
    if raw_path.is_dir():
        if raw_path.name == "consistency":
            return raw_path / "consistency.sqlite3"
        novel_dir = find_novel_dir(raw_path)
        if novel_dir is not None:
            return novel_dir / "research" / "consistency" / "consistency.sqlite3"
    if raw_path.name.startswith("novel") and raw_path.suffix == "":
        return raw_path / "research" / "consistency" / "consistency.sqlite3"
    return raw_path


def query_story_tension_rows(conn: sqlite3.Connection, limit: int) -> list[sqlite3.Row]:
    return conn.execute(
        """
        SELECT
            d.story,
            e.title,
            e.category,
            GROUP_CONCAT(DISTINCT CASE WHEN f.fact_type = 'injury_negative' THEN f.cue END) AS injury_negative,
            GROUP_CONCAT(DISTINCT CASE WHEN f.fact_type = 'injury_stable' THEN f.cue END) AS injury_stable,
            GROUP_CONCAT(DISTINCT CASE WHEN f.fact_type = 'equipment_damaged' THEN f.cue END) AS equipment_damaged,
            GROUP_CONCAT(DISTINCT CASE WHEN f.fact_type = 'equipment_active' THEN f.cue END) AS equipment_active
        FROM fact_candidates f
        JOIN entities e ON e.id = f.entity_id
        JOIN documents d ON d.id = f.document_id
        WHERE d.story IS NOT NULL
          AND e.category IN ('characters', 'units', 'items', 'technology')
        GROUP BY d.story, e.id
        HAVING
            (injury_negative IS NOT NULL AND injury_stable IS NOT NULL)
            OR
            (equipment_damaged IS NOT NULL AND equipment_active IS NOT NULL)
        ORDER BY d.story, e.title
        LIMIT ?
        """,
        (limit,),
    ).fetchall()


def query_story_goal_tension_rows(conn: sqlite3.Connection, limit: int) -> list[sqlite3.Row]:
    return conn.execute(
        """
        SELECT
            d.story,
            e.title,
            e.category,
            GROUP_CONCAT(DISTINCT CASE WHEN f.fact_type = 'goal_assigned' THEN f.cue END) AS goal_assigned,
            GROUP_CONCAT(DISTINCT CASE WHEN f.fact_type = 'goal_changed' THEN f.cue END) AS goal_changed,
            GROUP_CONCAT(DISTINCT CASE WHEN f.fact_type = 'goal_completed' THEN f.cue END) AS goal_completed
        FROM fact_candidates f
        JOIN entities e ON e.id = f.entity_id
        JOIN documents d ON d.id = f.document_id
        WHERE d.story IS NOT NULL
          AND e.category IN ('characters', 'units', 'organizations')
        GROUP BY d.story, e.id
        HAVING
            (goal_assigned IS NOT NULL AND goal_changed IS NOT NULL)
            OR
            (goal_assigned IS NOT NULL AND goal_completed IS NOT NULL)
        ORDER BY d.story, e.title
        LIMIT ?
        """,
        (limit,),
    ).fetchall()


def query_story_relationship_tension_rows(conn: sqlite3.Connection, limit: int) -> list[sqlite3.Row]:
    return conn.execute(
        """
        SELECT
            d.story,
            e.title,
            e.category,
            GROUP_CONCAT(DISTINCT CASE WHEN f.fact_type = 'relationship_close' THEN f.cue END) AS relationship_close,
            GROUP_CONCAT(DISTINCT CASE WHEN f.fact_type = 'relationship_distant' THEN f.cue END) AS relationship_distant
        FROM fact_candidates f
        JOIN entities e ON e.id = f.entity_id
        JOIN documents d ON d.id = f.document_id
        WHERE d.story IS NOT NULL
          AND e.category IN ('characters', 'units', 'organizations')
        GROUP BY d.story, e.id
        HAVING relationship_close IS NOT NULL AND relationship_distant IS NOT NULL
        ORDER BY d.story, e.title
        LIMIT ?
        """,
        (limit,),
    ).fetchall()


def collect_relationship_cues(text: str) -> tuple[list[str], list[str]]:
    close = [term for term in RELATIONSHIP_CLOSE_TERMS if term in text]
    distant = [term for term in RELATIONSHIP_DISTANT_TERMS if term in text]
    return close, distant


def has_relationship_pronoun_bridge(text: str) -> bool:
    return any(token in text for token in ("他", "她", "你", "你们", "两人", "两个人", "对方"))


def query_story_relationship_pair_rows(conn: sqlite3.Connection, limit: int) -> list[dict[str, object]]:
    rows = conn.execute(
        """
        SELECT
            d.story,
            d.path,
            d.chapter,
            p.line_start,
            p.line_end,
            p.text,
            e.title,
            n.name AS matched_name
        FROM mentions m
        JOIN entities e ON e.id = m.entity_id
        JOIN entity_names n ON n.id = m.entity_name_id
        JOIN documents d ON d.id = m.document_id
        JOIN passages p ON p.id = m.passage_id
        WHERE d.story IS NOT NULL
          AND d.doc_type IN ('story-plan', 'chapter-plan', 'drafts')
          AND e.category = 'characters'
        ORDER BY d.story, d.path, p.line_start, e.title, matched_name
        """
    ).fetchall()

    by_passage: dict[tuple[str, str, str | None, int, int, str], dict[str, set[str]]] = collections.defaultdict(dict)
    for row in rows:
        key = (
            str(row["story"]),
            str(row["path"]),
            str(row["chapter"]) if row["chapter"] is not None else None,
            int(row["line_start"]),
            int(row["line_end"]),
            str(row["text"]),
        )
        title = str(row["title"])
        matched_name = str(row["matched_name"])
        by_passage.setdefault(key, {}).setdefault(title, set()).add(matched_name)

    pair_rows: list[dict[str, object]] = []
    for (story, path, chapter, line_start, line_end, passage_text), title_map in by_passage.items():
        if len(title_map) < 2:
            continue
        all_titles = sorted(title_map)
        segments = split_fact_segments(passage_text) or [passage_text]
        seen_pairs: set[tuple[str, str, str, str]] = set()
        for segment in segments:
            close_cues, distant_cues = collect_relationship_cues(segment)
            if not close_cues and not distant_cues:
                continue
            present_titles: list[str] = []
            for title, names in title_map.items():
                if any(name in segment for name in names):
                    present_titles.append(title)
            if len(present_titles) < 2:
                if len(all_titles) == 2 and has_relationship_pronoun_bridge(segment):
                    present_titles = list(all_titles)
                else:
                    continue
            if len(present_titles) < 2:
                continue
            ordered_titles = sorted(set(present_titles))
            for idx, left_title in enumerate(ordered_titles):
                for right_title in ordered_titles[idx + 1 :]:
                    pair_key = (story, left_title, right_title, segment)
                    if pair_key in seen_pairs:
                        continue
                    seen_pairs.add(pair_key)
                    pair_rows.append(
                        {
                            "story": story,
                            "path": path,
                            "chapter": chapter,
                            "line_start": line_start,
                            "line_end": line_end,
                            "text": segment,
                            "left_title": left_title,
                            "right_title": right_title,
                            "close_cues": ",".join(close_cues),
                            "distant_cues": ",".join(distant_cues),
                        }
                    )
                    if len(pair_rows) >= limit:
                        return pair_rows
    return pair_rows


def query_story_alignment_rows(conn: sqlite3.Connection, limit: int) -> list[sqlite3.Row]:
    return conn.execute(
        """
        WITH per_story AS (
            SELECT
                e.title AS entity_title,
                d.story AS story,
                MAX(CASE WHEN d.doc_type IN ('story-plan', 'chapter-plan') THEN 1 ELSE 0 END) AS in_plan,
                MAX(CASE WHEN d.doc_type = 'drafts' THEN 1 ELSE 0 END) AS in_draft
            FROM mentions m
            JOIN entities e ON e.id = m.entity_id
            JOIN documents d ON d.id = m.document_id
            WHERE d.story IS NOT NULL
            GROUP BY e.id, d.story
        )
        SELECT
            story,
            SUM(CASE WHEN in_plan = 1 THEN 1 ELSE 0 END) AS plan_entities,
            SUM(CASE WHEN in_draft = 1 THEN 1 ELSE 0 END) AS draft_entities,
            GROUP_CONCAT(CASE WHEN in_plan = 1 AND in_draft = 0 THEN entity_title END, ' | ') AS plan_only_entities,
            GROUP_CONCAT(CASE WHEN in_plan = 0 AND in_draft = 1 THEN entity_title END, ' | ') AS draft_only_entities
        FROM per_story
        GROUP BY story
        ORDER BY story
        LIMIT ?
        """,
        (limit,),
    ).fetchall()


def query_story_alignment_gap_rows(conn: sqlite3.Connection, limit: int) -> list[sqlite3.Row]:
    return [
        row for row in query_story_alignment_rows(conn, limit)
        if row["plan_only_entities"] is not None or row["draft_only_entities"] is not None
    ]


def query_story_alignment_evidence(conn: sqlite3.Connection, story: str, entity_title: str, side: str, limit: int) -> list[sqlite3.Row]:
    if side == "plan":
        doc_types = ("story-plan", "chapter-plan")
    elif side == "draft":
        doc_types = ("drafts",)
    else:
        raise ValueError(f"Unknown side: {side}")
    placeholders = ", ".join("?" for _ in doc_types)
    sql = f"""
        SELECT d.path, p.line_start, p.line_end, p.text
        FROM mentions m
        JOIN entities e ON e.id = m.entity_id
        JOIN documents d ON d.id = m.document_id
        JOIN passages p ON p.id = m.passage_id
        WHERE d.story = ? AND e.title = ? AND d.doc_type IN ({placeholders})
        ORDER BY d.path, p.line_start
        LIMIT ?
    """
    params = [story, entity_title, *doc_types, limit]
    return conn.execute(sql, params).fetchall()


def query_story_tension_evidence(
    conn: sqlite3.Connection,
    story: str,
    entity_title: str,
    limit: int,
    fact_types: tuple[str, ...] | None = None,
) -> list[sqlite3.Row]:
    sql = """
        SELECT
            d.path,
            p.line_start,
            p.line_end,
            p.text,
            f.fact_type,
            f.cue
        FROM fact_candidates f
        JOIN entities e ON e.id = f.entity_id
        JOIN documents d ON d.id = f.document_id
        JOIN passages p ON p.id = f.passage_id
        WHERE d.story = ? AND e.title = ?
    """
    params: list[object] = [story, entity_title]
    if fact_types:
        placeholders = ", ".join("?" for _ in fact_types)
        sql += f" AND f.fact_type IN ({placeholders})"
        params.extend(fact_types)
    sql += """
        ORDER BY d.path, p.line_start
        LIMIT ?
    """
    params.append(limit)
    return conn.execute(sql, params).fetchall()


def query_story_alias_drift_rows(conn: sqlite3.Connection, limit: int) -> list[sqlite3.Row]:
    return conn.execute(
        """
        SELECT
            d.story,
            e.title,
            e.category,
            GROUP_CONCAT(
                DISTINCT CASE WHEN d.doc_type = 'drafts' THEN n.name END
            ) AS draft_aliases,
            GROUP_CONCAT(
                DISTINCT CASE WHEN d.doc_type IN ('story-plan', 'chapter-plan') THEN n.name END
            ) AS plan_aliases,
            COUNT(DISTINCT CASE WHEN d.doc_type = 'drafts' THEN n.name END) AS draft_alias_count,
            COUNT(DISTINCT CASE WHEN d.doc_type IN ('story-plan', 'chapter-plan') THEN n.name END) AS plan_alias_count
        FROM mentions m
        JOIN entities e ON e.id = m.entity_id
        JOIN entity_names n ON n.id = m.entity_name_id
        JOIN documents d ON d.id = m.document_id
        WHERE d.story IS NOT NULL
        GROUP BY d.story, e.id
        HAVING draft_alias_count >= 1 AND plan_alias_count >= 1 AND draft_aliases != plan_aliases
        ORDER BY d.story, e.title
        LIMIT ?
        """,
        (limit,),
    ).fetchall()


def query_story_alias_evidence(conn: sqlite3.Connection, story: str, entity_title: str, side: str, limit: int) -> list[sqlite3.Row]:
    if side == "plan":
        doc_types = ("story-plan", "chapter-plan")
    elif side == "draft":
        doc_types = ("drafts",)
    else:
        raise ValueError(f"Unknown side: {side}")
    placeholders = ", ".join("?" for _ in doc_types)
    sql = f"""
        SELECT
            d.path,
            p.line_start,
            p.line_end,
            p.text,
            n.name AS matched_name
        FROM mentions m
        JOIN entities e ON e.id = m.entity_id
        JOIN entity_names n ON n.id = m.entity_name_id
        JOIN documents d ON d.id = m.document_id
        JOIN passages p ON p.id = m.passage_id
        WHERE d.story = ? AND e.title = ? AND d.doc_type IN ({placeholders})
        ORDER BY d.path, p.line_start
        LIMIT ?
    """
    params = [story, entity_title, *doc_types, limit]
    return conn.execute(sql, params).fetchall()


def query_fact_support_summary(
    conn: sqlite3.Connection,
    story: str,
    entity_title: str,
    fact_types: tuple[str, ...],
) -> dict[str, int]:
    placeholders = ", ".join("?" for _ in fact_types)
    row = conn.execute(
        f"""
        SELECT
            COUNT(DISTINCT CASE WHEN d.doc_type = 'drafts' THEN d.id END) AS draft_docs,
            COUNT(DISTINCT CASE WHEN d.doc_type = 'chapter-plan' THEN d.id END) AS chapter_plan_docs,
            COUNT(DISTINCT CASE WHEN d.doc_type = 'story-plan' THEN d.id END) AS story_plan_docs
        FROM fact_candidates f
        JOIN entities e ON e.id = f.entity_id
        JOIN documents d ON d.id = f.document_id
        WHERE d.story = ? AND e.title = ? AND f.fact_type IN ({placeholders})
        """,
        (story, entity_title, *fact_types),
    ).fetchone()
    return {
        "draft_docs": int(row["draft_docs"] or 0),
        "chapter_plan_docs": int(row["chapter_plan_docs"] or 0),
        "story_plan_docs": int(row["story_plan_docs"] or 0),
    }


def score_fact_confidence(support: dict[str, int]) -> tuple[str, str]:
    draft_docs = support["draft_docs"]
    chapter_plan_docs = support["chapter_plan_docs"]
    story_plan_docs = support["story_plan_docs"]
    if draft_docs >= 2:
        return "high", f"draft_docs={draft_docs}"
    if draft_docs >= 1 and (chapter_plan_docs + story_plan_docs) >= 1:
        return "high", f"draft_docs={draft_docs} upstream_docs={chapter_plan_docs + story_plan_docs}"
    if draft_docs >= 1:
        return "medium", f"draft_docs={draft_docs}"
    if chapter_plan_docs >= 1 and story_plan_docs >= 1:
        return "medium", f"chapter_plan_docs={chapter_plan_docs} story_plan_docs={story_plan_docs}"
    return "low", f"chapter_plan_docs={chapter_plan_docs} story_plan_docs={story_plan_docs}"


def score_alignment_confidence(summary: str) -> tuple[str, str]:
    plan_only = split_pipe_values(re.search(r"plan_only=([^;]+)", summary).group(1)) if "plan_only=" in summary else []
    draft_only = split_pipe_values(re.search(r"draft_only=([^;]+)", summary).group(1)) if "draft_only=" in summary else []
    entity_count = len(plan_only) + len(draft_only)
    if entity_count >= 3:
        return "high", f"drift_entities={entity_count}"
    if entity_count >= 2:
        return "medium", f"drift_entities={entity_count}"
    return "low", f"drift_entities={entity_count}"


def score_alias_confidence(plan_aliases: str, draft_aliases: str) -> tuple[str, str]:
    plan_count = len(split_pipe_values(plan_aliases.replace(",", "|")))
    draft_count = len(split_pipe_values(draft_aliases.replace(",", "|")))
    if plan_count >= 2 and draft_count >= 2:
        return "high", f"plan_aliases={plan_count} draft_aliases={draft_count}"
    if plan_count >= 1 and draft_count >= 1:
        return "medium", f"plan_aliases={plan_count} draft_aliases={draft_count}"
    return "low", f"plan_aliases={plan_count} draft_aliases={draft_count}"


def apply_feedback(rows: list[dict[str, object]], feedback_entries: dict[str, dict[str, str]]) -> list[dict[str, object]]:
    for row in rows:
        key = conflict_key(row)
        feedback = feedback_entries.get(key)
        if not feedback:
            continue
        row["feedback_decision"] = feedback.get("decision", "")
        row["feedback_facet"] = feedback.get("facet", "")
        row["feedback_note"] = feedback.get("note", "")
        row["feedback_updated_at"] = feedback.get("updated_at", "")
    return rows


def query_conflict_rows(conn: sqlite3.Connection, limit: int) -> list[dict[str, object]]:
    rows: list[dict[str, object]] = []

    for row in query_story_tension_rows(conn, limit):
        if row["injury_negative"] and row["injury_stable"]:
            fact_types = ("injury_negative", "injury_stable")
            support = query_fact_support_summary(conn, str(row["story"]), str(row["title"]), fact_types)
            confidence, support_note = score_fact_confidence(support)
            if confidence == "low":
                continue
            rows.append(
                {
                    "category": "injury_state_jump",
                    "story": row["story"],
                    "title": row["title"],
                    "entity_category": row["category"],
                    "summary": f"injury={row['injury_negative']} -> {row['injury_stable']}",
                    "evidence_kind": "fact",
                    "fact_types": fact_types,
                    "confidence": confidence,
                    "support_note": support_note,
                }
            )
        if row["equipment_damaged"] and row["equipment_active"]:
            fact_types = ("equipment_damaged", "equipment_active")
            support = query_fact_support_summary(conn, str(row["story"]), str(row["title"]), fact_types)
            confidence, support_note = score_fact_confidence(support)
            if confidence == "low":
                continue
            rows.append(
                {
                    "category": "equipment_state_jump",
                    "story": row["story"],
                    "title": row["title"],
                    "entity_category": row["category"],
                    "summary": f"equipment={row['equipment_damaged']} -> {row['equipment_active']}",
                    "evidence_kind": "fact",
                    "fact_types": fact_types,
                    "confidence": confidence,
                    "support_note": support_note,
                }
            )

    for row in query_story_goal_tension_rows(conn, limit):
        fact_types = ("goal_assigned", "goal_changed", "goal_completed")
        support = query_fact_support_summary(conn, str(row["story"]), str(row["title"]), fact_types)
        confidence, support_note = score_fact_confidence(support)
        if confidence == "low":
            continue
        parts: list[str] = []
        if row["goal_assigned"]:
            parts.append(f"assigned={row['goal_assigned']}")
        if row["goal_changed"]:
            parts.append(f"changed={row['goal_changed']}")
        if row["goal_completed"]:
            parts.append(f"completed={row['goal_completed']}")
        rows.append(
            {
                "category": "goal_state_drift",
                "story": row["story"],
                "title": row["title"],
                "entity_category": row["category"],
                "summary": " ; ".join(parts),
                "evidence_kind": "fact",
                "fact_types": fact_types,
                "confidence": confidence,
                "support_note": support_note,
            }
        )

    for row in query_story_relationship_tension_rows(conn, limit):
        fact_types = ("relationship_close", "relationship_distant")
        support = query_fact_support_summary(conn, str(row["story"]), str(row["title"]), fact_types)
        confidence, support_note = score_fact_confidence(support)
        if confidence == "low":
            continue
        rows.append(
            {
                "category": "relationship_tone_shift",
                "story": row["story"],
                "title": row["title"],
                "entity_category": row["category"],
                "summary": f"close={row['relationship_close']} ; distant={row['relationship_distant']}",
                "evidence_kind": "fact",
                "fact_types": fact_types,
                "confidence": confidence,
                "support_note": support_note,
            }
        )

    for row in query_story_alias_drift_rows(conn, limit):
        confidence, support_note = score_alias_confidence(str(row["plan_aliases"] or ""), str(row["draft_aliases"] or ""))
        if confidence == "low":
            continue
        rows.append(
            {
                "category": "alias_register_drift",
                "story": row["story"],
                "title": row["title"],
                "entity_category": row["category"],
                "summary": f"plan={row['plan_aliases'] or ''} ; draft={row['draft_aliases'] or ''}",
                "evidence_kind": "alias",
                "confidence": confidence,
                "support_note": support_note,
            }
        )

    for row in query_story_alignment_gap_rows(conn, limit):
        parts: list[str] = []
        if row["plan_only_entities"]:
            parts.append(f"plan_only={row['plan_only_entities']}")
        if row["draft_only_entities"]:
            parts.append(f"draft_only={row['draft_only_entities']}")
        summary = " ; ".join(parts)
        confidence, support_note = score_alignment_confidence(summary)
        if confidence == "low":
            continue
        rows.append(
            {
                "category": "plan_draft_entity_drift",
                "story": row["story"],
                "title": "-",
                "entity_category": "story",
                "summary": summary,
                "evidence_kind": "alignment",
                "confidence": confidence,
                "support_note": support_note,
            }
        )

    confidence_rank = {"high": 0, "medium": 1, "low": 2}
    rows.sort(
        key=lambda item: (
            confidence_rank.get(str(item.get("confidence", "low")), 9),
            str(item["story"]),
            str(item["category"]),
            str(item["title"]),
        )
    )
    return rows[:limit]


def collect_conflict_rows(
    conn: sqlite3.Connection,
    limit: int,
    feedback_path: Path | None = None,
) -> list[dict[str, object]]:
    rows = query_conflict_rows(conn, limit)
    if feedback_path is None:
        return rows
    return apply_feedback(rows, load_feedback_entries(feedback_path))


def print_search_results(conn: sqlite3.Connection, term: str, limit: int) -> None:
    rows = conn.execute(
        """
        SELECT d.path, ps.line_start, ps.line_end, ps.text
        FROM passage_fts f
        JOIN passages ps ON ps.id = f.rowid
        JOIN documents d ON d.id = ps.document_id
        WHERE passage_fts MATCH ?
        LIMIT ?
        """,
        (term, limit),
    ).fetchall()
    if not rows:
        print("No matches.")
        return
    for row in rows:
        print(f"{row['path']}:{row['line_start']}-{row['line_end']}")
        print(normalize_whitespace(row["text"]))
        print()


def print_entity_results(conn: sqlite3.Connection, name: str, limit: int) -> None:
    rows = conn.execute(
        """
        SELECT
            e.title,
            e.category,
            n.name AS matched_name,
            d.path,
            p.line_start,
            p.line_end,
            p.text,
            m.count
        FROM mentions m
        JOIN entities e ON e.id = m.entity_id
        JOIN entity_names n ON n.id = m.entity_name_id
        JOIN documents d ON d.id = m.document_id
        JOIN passages p ON p.id = m.passage_id
        WHERE e.title = ? OR n.name = ?
        ORDER BY d.path, p.line_start
        LIMIT ?
        """,
        (name, name, limit),
    ).fetchall()
    if not rows:
        print("No entity matches.")
        return
    first = rows[0]
    print(f"Entity: {first['title']} ({first['category']})")
    print()
    for row in rows:
        print(f"{row['path']}:{row['line_start']}-{row['line_end']} matched=`{row['matched_name']}` count={row['count']}")
        print(normalize_whitespace(row["text"]))
        print()


def print_entity_catalog(conn: sqlite3.Connection, limit: int) -> None:
    rows = conn.execute(
        """
        SELECT
            e.title,
            e.category,
            COUNT(m.id) AS mentions,
            (
                SELECT GROUP_CONCAT(name, ' | ')
                FROM (
                    SELECT DISTINCT name
                    FROM entity_names
                    WHERE entity_id = e.id
                    ORDER BY name
                )
            ) AS names
        FROM entities e
        LEFT JOIN mentions m ON m.entity_id = e.id
        GROUP BY e.id
        ORDER BY mentions DESC, e.title
        LIMIT ?
        """,
        (limit,),
    ).fetchall()
    for row in rows:
        print(f"{row['title']} [{row['category']}] mentions={row['mentions']} names={row['names']}")


def print_entity_facts(conn: sqlite3.Connection, name: str, limit: int) -> None:
    rows = conn.execute(
        """
        SELECT
            e.title,
            e.category,
            n.name AS matched_name,
            d.path,
            d.story,
            p.line_start,
            p.line_end,
            p.text,
            f.fact_type,
            f.cue
        FROM fact_candidates f
        JOIN entities e ON e.id = f.entity_id
        JOIN entity_names n ON n.id = f.entity_name_id
        JOIN documents d ON d.id = f.document_id
        JOIN passages p ON p.id = f.passage_id
        WHERE e.title = ? OR n.name = ?
        ORDER BY d.path, p.line_start
        LIMIT ?
        """,
        (name, name, limit),
    ).fetchall()
    if not rows:
        print("No fact matches.")
        return
    first = rows[0]
    print(f"Entity Facts: {first['title']} ({first['category']})")
    print()
    for row in rows:
        story = row["story"] or "-"
        print(
            f"{row['path']}:{row['line_start']}-{row['line_end']} story=`{story}` matched=`{row['matched_name']}` fact=`{row['fact_type']}` cue=`{row['cue']}`"
        )
        print(normalize_whitespace(row["text"]))
        print()


def print_story_facts(conn: sqlite3.Connection, story: str, limit: int) -> None:
    rows = conn.execute(
        """
        SELECT
            e.title,
            e.category,
            GROUP_CONCAT(DISTINCT CASE WHEN f.fact_type = 'injury_negative' THEN f.cue END) AS injury_negative,
            GROUP_CONCAT(DISTINCT CASE WHEN f.fact_type = 'injury_stable' THEN f.cue END) AS injury_stable,
            GROUP_CONCAT(DISTINCT CASE WHEN f.fact_type = 'equipment_damaged' THEN f.cue END) AS equipment_damaged,
            GROUP_CONCAT(DISTINCT CASE WHEN f.fact_type = 'equipment_active' THEN f.cue END) AS equipment_active,
            GROUP_CONCAT(DISTINCT CASE WHEN f.fact_type = 'goal_assigned' THEN f.cue END) AS goal_assigned,
            GROUP_CONCAT(DISTINCT CASE WHEN f.fact_type = 'goal_changed' THEN f.cue END) AS goal_changed,
            GROUP_CONCAT(DISTINCT CASE WHEN f.fact_type = 'goal_completed' THEN f.cue END) AS goal_completed,
            GROUP_CONCAT(DISTINCT CASE WHEN f.fact_type = 'relationship_close' THEN f.cue END) AS relationship_close,
            GROUP_CONCAT(DISTINCT CASE WHEN f.fact_type = 'relationship_distant' THEN f.cue END) AS relationship_distant
        FROM fact_candidates f
        JOIN entities e ON e.id = f.entity_id
        JOIN documents d ON d.id = f.document_id
        WHERE d.story = ?
        GROUP BY e.id
        HAVING
            injury_negative IS NOT NULL
            OR injury_stable IS NOT NULL
            OR equipment_damaged IS NOT NULL
            OR equipment_active IS NOT NULL
            OR goal_assigned IS NOT NULL
            OR goal_changed IS NOT NULL
            OR goal_completed IS NOT NULL
            OR relationship_close IS NOT NULL
            OR relationship_distant IS NOT NULL
        ORDER BY e.title
        LIMIT ?
        """,
        (story, limit),
    ).fetchall()
    if not rows:
        print("No story fact rows.")
        return
    for row in rows:
        details: list[str] = []
        if row["injury_negative"]:
            details.append(f"injury_negative={row['injury_negative']}")
        if row["injury_stable"]:
            details.append(f"injury_stable={row['injury_stable']}")
        if row["equipment_damaged"]:
            details.append(f"equipment_damaged={row['equipment_damaged']}")
        if row["equipment_active"]:
            details.append(f"equipment_active={row['equipment_active']}")
        if row["goal_assigned"]:
            details.append(f"goal_assigned={row['goal_assigned']}")
        if row["goal_changed"]:
            details.append(f"goal_changed={row['goal_changed']}")
        if row["goal_completed"]:
            details.append(f"goal_completed={row['goal_completed']}")
        if row["relationship_close"]:
            details.append(f"relationship_close={row['relationship_close']}")
        if row["relationship_distant"]:
            details.append(f"relationship_distant={row['relationship_distant']}")
        print(f"{row['title']} [{row['category']}] :: {' ; '.join(details)}")


def print_story_tension(conn: sqlite3.Connection, limit: int) -> None:
    rows = query_story_tension_rows(conn, limit)
    if not rows:
        print("No story tension rows.")
        return
    for row in rows:
        details: list[str] = []
        if row["injury_negative"] and row["injury_stable"]:
            details.append(f"injury={row['injury_negative']} -> {row['injury_stable']}")
        if row["equipment_damaged"] and row["equipment_active"]:
            details.append(f"equipment={row['equipment_damaged']} -> {row['equipment_active']}")
        print(f"{row['story']} :: {row['title']} [{row['category']}] :: {' ; '.join(details)}")


def print_conflict_evidence(conn: sqlite3.Connection, row: dict[str, object], indent: str = "  - ") -> None:
    if row["evidence_kind"] == "fact" and row["title"] != "-":
        fact_types = tuple(row.get("fact_types", ())) or None
        evidence_rows = query_story_tension_evidence(
            conn,
            str(row["story"]),
            str(row["title"]),
            4,
            fact_types=fact_types,
        )
        for evidence in evidence_rows:
            print(
                f"{indent}{evidence['path']}:{evidence['line_start']}-{evidence['line_end']} fact=`{evidence['fact_type']}` cue=`{evidence['cue']}`"
            )
            print(f"{indent}  {normalize_whitespace(evidence['text'])}")
    elif row["evidence_kind"] == "alias":
        for side in ("plan", "draft"):
            print(f"{indent}{side}:")
            for evidence in query_story_alias_evidence(conn, str(row["story"]), str(row["title"]), side, 2):
                print(
                    f"{indent}  {evidence['path']}:{evidence['line_start']}-{evidence['line_end']} matched=`{evidence['matched_name']}`"
                )
                print(f"{indent}    {normalize_whitespace(evidence['text'])}")
    elif row["evidence_kind"] == "alignment":
        summary = str(row["summary"])
        plan_match = re.search(r"plan_only=([^;]+)", summary)
        draft_match = re.search(r"draft_only=([^;]+)", summary)
        if plan_match:
            for entity_title in split_pipe_values(plan_match.group(1))[:3]:
                print(f"{indent}plan_only `{entity_title}`")
                for evidence in query_story_alignment_evidence(conn, str(row["story"]), entity_title, "plan", 2):
                    print(f"{indent}  {evidence['path']}:{evidence['line_start']}-{evidence['line_end']}")
                    print(f"{indent}    {normalize_whitespace(evidence['text'])}")
        if draft_match:
            for entity_title in split_pipe_values(draft_match.group(1))[:3]:
                print(f"{indent}draft_only `{entity_title}`")
                for evidence in query_story_alignment_evidence(conn, str(row["story"]), entity_title, "draft", 2):
                    print(f"{indent}  {evidence['path']}:{evidence['line_start']}-{evidence['line_end']}")
                    print(f"{indent}    {normalize_whitespace(evidence['text'])}")


def print_conflicts(conn: sqlite3.Connection, limit: int, feedback_path: Path | None = None) -> None:
    conflict_rows = collect_conflict_rows(conn, limit, feedback_path)
    if not conflict_rows:
        print("No conflict candidates.")
        return

    grouped: dict[str, list[dict[str, object]]] = collections.defaultdict(list)
    for row in conflict_rows:
        grouped[str(row["category"])].append(row)

    for category in sorted(grouped):
        print(f"## {category}")
        for row in grouped[category]:
            confidence = row.get("confidence", "low")
            support_note = row.get("support_note", "")
            feedback_decision = row.get("feedback_decision", "")
            feedback_facet = row.get("feedback_facet", "")
            feedback_note = row.get("feedback_note", "")
            print(
                f"{row['story']} :: {row['title']} [{row['entity_category']}] :: confidence={confidence} support={support_note} :: {row['summary']}"
            )
            if feedback_decision:
                extra = f" feedback={feedback_decision}"
                if feedback_facet:
                    extra += f" facet={feedback_facet}"
                if feedback_note:
                    extra += f" note={feedback_note}"
                print(f"  -{extra}")
            print_conflict_evidence(conn, row)
        print()


def print_review_queue(
    conn: sqlite3.Connection,
    feedback_path: Path,
    *,
    story: str | None,
    limit: int,
) -> None:
    summary = summarize_feedback(conn, feedback_path, limit * 4)
    unresolved = [
        row for row in summary["unresolved"]
        if story is None or str(row["story"]) == story
    ]
    if not unresolved:
        print("No pending review rows.")
        return

    print("## Review Queue")
    if story is not None:
        print(f"- story: `{story}`")
    else:
        print("- story: `all`")
    print(f"- feedback_log: `{feedback_path}`")
    print(f"- pending_total: `{len(unresolved)}`")
    print()

    for row in unresolved[:limit]:
        command = build_feedback_command(resolve_db_path(feedback_path.parent), row)
        print(
            f"### {row['story']} :: {row['category']} :: {row['title']} :: confidence={row.get('confidence', '')}"
        )
        print(f"- focus: {build_pending_review_focus(row)}")
        print(f"- summary: {row['summary']}")
        print(f"- command: `{command}`")
        print("- evidence:")
        print_conflict_evidence(conn, row, indent="  - ")
        print()


def summarize_feedback(
    conn: sqlite3.Connection,
    feedback_path: Path,
    limit: int,
    story: str | None = None,
) -> dict[str, object]:
    feedback_entries = load_feedback_entries(feedback_path)
    feedback_history = read_feedback_history(feedback_path)
    all_conflict_rows = collect_conflict_rows(conn, limit, feedback_path)
    conflict_rows = [
        row for row in all_conflict_rows
        if story is None or str(row["story"]) == story
    ]
    scoped_history = filter_feedback_history_by_story(feedback_history, story)
    decision_counter: collections.Counter[str] = collections.Counter()
    category_counter: collections.Counter[str] = collections.Counter()
    story_counter: collections.Counter[str] = collections.Counter()
    facet_counter: collections.Counter[str] = collections.Counter()
    unresolved: list[dict[str, object]] = []

    for row in conflict_rows:
        decision = str(row.get("feedback_decision", ""))
        if decision:
            decision_counter[decision] += 1
            category_counter[f"{row['category']}::{decision}"] += 1
            story_counter[f"{row['story']}::{decision}"] += 1
            facet = str(row.get("feedback_facet", ""))
            if facet:
                facet_counter[f"{facet}::{decision}"] += 1
        else:
            unresolved.append(row)

    return {
        "entries": feedback_entries,
        "history": feedback_history,
        "conflict_rows": conflict_rows,
        "decision_counter": decision_counter,
        "category_counter": category_counter,
        "story_counter": story_counter,
        "facet_counter": facet_counter,
        "unresolved": unresolved,
        "pending_actions": build_pending_review_actions(resolve_db_path(feedback_path.parent), unresolved, 8),
        "backlog": build_feedback_backlog(scoped_history),
        "unresolved_by_story": collections.Counter(str(row["story"]) for row in unresolved),
        "story_filter": story,
    }


def print_feedback_summary(conn: sqlite3.Connection, feedback_path: Path, limit: int, story: str | None = None) -> None:
    summary = summarize_feedback(conn, feedback_path, limit, story)
    feedback_entries = summary["entries"]
    decision_counter = summary["decision_counter"]
    category_counter = summary["category_counter"]
    story_counter = summary["story_counter"]
    facet_counter = summary["facet_counter"]
    unresolved = summary["unresolved"]

    if not feedback_entries:
        print("No feedback entries yet.")
        print()

    if story is not None:
        print(f"## Story Filter")
        print(f"- story: `{story}`")
        print()

    print("## Feedback Decisions")
    for decision in FEEDBACK_DECISIONS:
        print(f"- {decision}: {decision_counter.get(decision, 0)}")
    print()

    print("## By Category")
    if category_counter:
        for name, count in category_counter.most_common(12):
            print(f"- {name} x{count}")
    else:
        print("- 无")
    print()

    print("## By Facet")
    if facet_counter:
        for name, count in facet_counter.most_common(12):
            print(f"- {name} x{count}")
    else:
        print("- 无")
    print()

    print("## Deposition Suggestions")
    backlog = summary["backlog"]
    if backlog:
        for item in backlog:
            print(f"- `{item['target']}` {item['reason']}")
    else:
        print("- 无")
    print()

    print("## By Story")
    if story_counter:
        for name, count in story_counter.most_common(12):
            print(f"- {name} x{count}")
    else:
        print("- 无")
    print()

    print("## Pending Review")
    if unresolved:
        for row in unresolved[:12]:
            print(f"- {row['story']} :: {row['category']} :: {row['title']} :: confidence={row.get('confidence', '')}")
    else:
        print("- 无")
    print()

    print("## Pending Review Actions")
    if summary["pending_actions"]:
        for item in summary["pending_actions"]:
            print(
                f"- {item['story']} :: {item['category']} :: {item['title']} :: confidence={item['confidence']} :: {item['focus']}"
            )
            print(f"  {item['command']}")
    else:
        print("- 无")
    print()


def write_feedback(
    conn: sqlite3.Connection,
    feedback_path: Path,
    *,
    category: str,
    story: str,
    title: str,
    decision: str,
    facet: str,
    note: str,
    summary_contains: str | None,
) -> None:
    if decision not in FEEDBACK_DECISIONS:
        raise SystemExit(f"Unknown decision: {decision}")
    if facet and facet not in FEEDBACK_FACETS:
        raise SystemExit(f"Unknown facet: {facet}")
    rows = query_conflict_rows(conn, 1000)
    matched = [
        row for row in rows
        if str(row["category"]) == category
        and str(row["story"]) == story
        and str(row["title"]) == title
        and (summary_contains is None or summary_contains in str(row["summary"]))
    ]
    if not matched:
        raise SystemExit("No matching conflict row found.")
    if len(matched) > 1:
        raise SystemExit("Multiple conflict rows matched. Add --summary-contains to disambiguate.")

    row = matched[0]
    entry = {
        "conflict_key": conflict_key(row),
        "category": category,
        "story": story,
        "title": title,
        "entity_category": str(row["entity_category"]),
        "summary": str(row["summary"]),
        "decision": decision,
        "facet": facet,
        "note": note,
        "updated_at": datetime.now(timezone.utc).isoformat(),
    }
    append_feedback_entry(feedback_path, entry)
    print(feedback_path)
    print(f"{category} :: {story} :: {title} :: decision={decision}")


def print_suspects(conn: sqlite3.Connection, limit: int) -> None:
    alias_rows = conn.execute(
        """
        SELECT
            e.title,
            e.category,
            d.path,
            COUNT(DISTINCT n.name) AS alias_count,
            GROUP_CONCAT(DISTINCT n.name) AS aliases
        FROM mentions m
        JOIN entities e ON e.id = m.entity_id
        JOIN entity_names n ON n.id = m.entity_name_id
        JOIN documents d ON d.id = m.document_id
        WHERE d.doc_type != 'concept'
        GROUP BY e.id, d.id
        HAVING alias_count >= 2
        ORDER BY alias_count DESC, d.path, e.title
        LIMIT ?
        """,
        (limit,),
    ).fetchall()

    draft_rows = conn.execute(
        """
        SELECT
            e.title,
            e.category,
            GROUP_CONCAT(DISTINCT CASE WHEN d.doc_type = 'drafts' THEN n.name END) AS draft_aliases,
            GROUP_CONCAT(DISTINCT CASE WHEN d.doc_type IN ('arc-plan', 'story-plan', 'chapter-plan') THEN n.name END) AS upstream_aliases,
            COUNT(DISTINCT CASE WHEN d.doc_type = 'drafts' THEN n.name END) AS draft_alias_count,
            COUNT(DISTINCT CASE WHEN d.doc_type IN ('arc-plan', 'story-plan', 'chapter-plan') THEN n.name END) AS upstream_alias_count
        FROM mentions m
        JOIN entities e ON e.id = m.entity_id
        JOIN entity_names n ON n.id = m.entity_name_id
        JOIN documents d ON d.id = m.document_id
        GROUP BY e.id
        HAVING draft_alias_count >= 1 AND upstream_alias_count >= 1 AND draft_aliases != upstream_aliases
        ORDER BY e.title
        LIMIT ?
        """,
        (limit,),
    ).fetchall()

    plan_only_rows = conn.execute(
        """
        SELECT
            e.title,
            e.category,
            COUNT(DISTINCT CASE WHEN d.doc_type IN ('arc-plan', 'story-plan', 'chapter-plan') THEN d.id END) AS plan_docs,
            COUNT(DISTINCT CASE WHEN d.doc_type = 'drafts' THEN d.id END) AS draft_docs,
            GROUP_CONCAT(DISTINCT CASE WHEN d.doc_type IN ('arc-plan', 'story-plan', 'chapter-plan') THEN d.path END) AS plan_paths
        FROM mentions m
        JOIN entities e ON e.id = m.entity_id
        JOIN documents d ON d.id = m.document_id
        GROUP BY e.id
        HAVING plan_docs >= 2 AND draft_docs = 0
        ORDER BY plan_docs DESC, e.title
        LIMIT ?
        """,
        (limit,),
    ).fetchall()

    draft_only_rows = conn.execute(
        """
        SELECT
            e.title,
            e.category,
            COUNT(DISTINCT CASE WHEN d.doc_type = 'drafts' THEN d.id END) AS draft_docs,
            COUNT(DISTINCT CASE WHEN d.doc_type IN ('arc-plan', 'story-plan', 'chapter-plan') THEN d.id END) AS plan_docs,
            GROUP_CONCAT(DISTINCT CASE WHEN d.doc_type = 'drafts' THEN d.path END) AS draft_paths
        FROM mentions m
        JOIN entities e ON e.id = m.entity_id
        JOIN documents d ON d.id = m.document_id
        GROUP BY e.id
        HAVING draft_docs >= 2 AND plan_docs = 0
        ORDER BY draft_docs DESC, e.title
        LIMIT ?
        """,
        (limit,),
    ).fetchall()

    fact_rows = query_story_tension_rows(conn, limit)
    story_alignment_rows = query_story_alignment_gap_rows(conn, limit)

    if (
        not alias_rows
        and not draft_rows
        and not plan_only_rows
        and not draft_only_rows
        and not fact_rows
        and not story_alignment_rows
    ):
        print("No suspects.")
        return

    if alias_rows:
        print("## Same Document Alias Mixing")
        for row in alias_rows:
            print(
                f"{row['path']} :: {row['title']} [{row['category']}] aliases={row['aliases']}"
            )
        print()

    if draft_rows:
        print("## Draft vs Upstream Alias Drift")
        for row in draft_rows:
            draft_aliases = row["draft_aliases"] or ""
            upstream_aliases = row["upstream_aliases"] or ""
            print(
                f"{row['title']} [{row['category']}] draft={draft_aliases} upstream={upstream_aliases}"
            )
        print()

    if plan_only_rows:
        print("## Plan Mentioned But Draft Missing")
        for row in plan_only_rows:
            paths = row["plan_paths"] or ""
            preview = " | ".join(paths.split(",")[:3])
            print(
                f"{row['title']} [{row['category']}] plan_docs={row['plan_docs']} draft_docs={row['draft_docs']} paths={preview}"
            )
        print()

    if draft_only_rows:
        print("## Draft Mentioned But Plan Missing")
        for row in draft_only_rows:
            paths = row["draft_paths"] or ""
            preview = " | ".join(paths.split(",")[:3])
            print(
                f"{row['title']} [{row['category']}] draft_docs={row['draft_docs']} plan_docs={row['plan_docs']} paths={preview}"
            )
        print()

    if fact_rows:
        print("## Draft State Tension Candidates")
        for row in fact_rows:
            parts: list[str] = []
            if row["injury_negative"] and row["injury_stable"]:
                parts.append(f"injury={row['injury_negative']} -> {row['injury_stable']}")
            if row["equipment_damaged"] and row["equipment_active"]:
                parts.append(f"equipment={row['equipment_damaged']} -> {row['equipment_active']}")
            print(f"{row['story']} :: {row['title']} [{row['category']}] {' ; '.join(parts)}")
        print()

    if story_alignment_rows:
        print("## Story Plan / Draft Entity Drift")
        for row in story_alignment_rows:
            details: list[str] = []
            if row["plan_only_entities"]:
                details.append(f"plan_only={row['plan_only_entities']}")
            if row["draft_only_entities"]:
                details.append(f"draft_only={row['draft_only_entities']}")
            print(f"{row['story']} :: {' ; '.join(details)}")
        print()


def print_story_alignment(conn: sqlite3.Connection, limit: int) -> None:
    rows = query_story_alignment_rows(conn, limit)
    if not rows:
        print("No story alignment rows.")
        return
    for row in rows:
        details = [f"plan_entities={row['plan_entities']}", f"draft_entities={row['draft_entities']}"]
        if row["plan_only_entities"]:
            details.append(f"plan_only={row['plan_only_entities']}")
        if row["draft_only_entities"]:
            details.append(f"draft_only={row['draft_only_entities']}")
        print(f"{row['story']} :: {' ; '.join(details)}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Build and query a novel consistency index.")
    subparsers = parser.add_subparsers(dest="command", required=True)

    build_parser = subparsers.add_parser("build", help="Build the index for a novel directory")
    build_parser.add_argument("novel_dir", help="Novel directory, for example novel1")
    build_parser.add_argument(
        "--db-path",
        help="Output SQLite path; defaults to <novel_dir>/research/consistency/consistency.sqlite3",
    )

    search_parser = subparsers.add_parser("search", help="Full-text search indexed passages")
    search_parser.add_argument("db_path", help="SQLite database path, consistency dir, or novel dir")
    search_parser.add_argument("term", help="FTS term or phrase")
    search_parser.add_argument("--limit", type=int, default=20, help="Max passages to print")

    entity_parser = subparsers.add_parser("entity", help="Show passages mentioning a known entity")
    entity_parser.add_argument("db_path", help="SQLite database path, consistency dir, or novel dir")
    entity_parser.add_argument("name", help="Entity title or alias")
    entity_parser.add_argument("--limit", type=int, default=30, help="Max passages to print")

    facts_parser = subparsers.add_parser("facts", help="Show extracted fact candidates for a known entity")
    facts_parser.add_argument("db_path", help="SQLite database path, consistency dir, or novel dir")
    facts_parser.add_argument("name", help="Entity title or alias")
    facts_parser.add_argument("--limit", type=int, default=30, help="Max fact passages to print")

    story_facts_parser = subparsers.add_parser("story-facts", help="Show aggregated fact candidates for one story")
    story_facts_parser.add_argument("db_path", help="SQLite database path, consistency dir, or novel dir")
    story_facts_parser.add_argument("story", help="Story id, for example story1 or interlude1")
    story_facts_parser.add_argument("--limit", type=int, default=50, help="Max entities to print")

    tension_parser = subparsers.add_parser("tension", help="Show story-level state tension candidates")
    tension_parser.add_argument("db_path", help="SQLite database path, consistency dir, or novel dir")
    tension_parser.add_argument("--limit", type=int, default=100, help="Max rows to print")

    conflicts_parser = subparsers.add_parser("conflicts", help="Show conflict candidates with evidence")
    conflicts_parser.add_argument("db_path", help="SQLite database path, consistency dir, or novel dir")
    conflicts_parser.add_argument("--limit", type=int, default=50, help="Max grouped rows to print")
    conflicts_parser.add_argument(
        "--feedback-path",
        help="Optional feedback JSONL path; defaults to <db dir>/review-feedback.jsonl",
    )

    feedback_add_parser = subparsers.add_parser("feedback-add", help="Record a manual review decision for one conflict")
    feedback_add_parser.add_argument("db_path", help="SQLite database path, consistency dir, or novel dir")
    feedback_add_parser.add_argument("--category", required=True, help="Conflict category")
    feedback_add_parser.add_argument("--story", required=True, help="Story id")
    feedback_add_parser.add_argument("--title", required=True, help="Entity title, or - for story-level drift")
    feedback_add_parser.add_argument(
        "--decision",
        required=True,
        choices=FEEDBACK_DECISIONS,
        help="Manual decision: confirmed / false_positive / designed_keep / watch",
    )
    feedback_add_parser.add_argument(
        "--facet",
        choices=FEEDBACK_FACETS,
        default="",
        help="Optional facet such as rhythm / voice / motif / scene_callback / register / naming / irony / state_progression / extractor_noise / scope_drift",
    )
    feedback_add_parser.add_argument("--note", default="", help="Short manual note")
    feedback_add_parser.add_argument("--summary-contains", help="Optional summary fragment to disambiguate rows")
    feedback_add_parser.add_argument(
        "--feedback-path",
        help="Feedback JSONL path; defaults to <db dir>/review-feedback.jsonl",
    )

    feedback_summary_parser = subparsers.add_parser("feedback-summary", help="Summarize recorded consistency feedback")
    feedback_summary_parser.add_argument("db_path", help="SQLite database path, consistency dir, or novel dir")
    feedback_summary_parser.add_argument("--limit", type=int, default=200, help="Max conflict rows to scan")
    feedback_summary_parser.add_argument("--story", help="Optional story id, for example story3")
    feedback_summary_parser.add_argument(
        "--feedback-path",
        help="Feedback JSONL path; defaults to <db dir>/review-feedback.jsonl",
    )

    review_queue_parser = subparsers.add_parser("review-queue", help="Print pending consistency review queue with evidence")
    review_queue_parser.add_argument("db_path", help="SQLite database path, consistency dir, or novel dir")
    review_queue_parser.add_argument("--story", help="Optional story id, for example story3")
    review_queue_parser.add_argument("--limit", type=int, default=8, help="Max pending rows to print")
    review_queue_parser.add_argument(
        "--feedback-path",
        help="Feedback JSONL path; defaults to <db dir>/review-feedback.jsonl",
    )

    list_parser = subparsers.add_parser("catalog", help="List indexed entities with mention counts")
    list_parser.add_argument("db_path", help="SQLite database path, consistency dir, or novel dir")
    list_parser.add_argument("--limit", type=int, default=50, help="Max entities to print")

    suspect_parser = subparsers.add_parser("suspects", help="Print likely alias-mixing or naming-drift suspects")
    suspect_parser.add_argument("db_path", help="SQLite database path, consistency dir, or novel dir")
    suspect_parser.add_argument("--limit", type=int, default=50, help="Max suspect rows to print")

    alignment_parser = subparsers.add_parser("alignment", help="Print story-level plan/draft entity coverage")
    alignment_parser.add_argument("db_path", help="SQLite database path, consistency dir, or novel dir")
    alignment_parser.add_argument("--limit", type=int, default=100, help="Max stories to print")

    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.command == "build":
        novel_dir = Path(args.novel_dir)
        db_path = Path(args.db_path) if args.db_path else novel_dir / "research" / "consistency" / "consistency.sqlite3"
        build_index(novel_dir, db_path)
        print(db_path)
        return 0
    db_path = resolve_db_path(Path(args.db_path))
    if args.command == "search":
        conn = open_db(db_path)
        try:
            print_search_results(conn, args.term, args.limit)
        finally:
            conn.close()
        return 0
    if args.command == "entity":
        conn = open_db(db_path)
        try:
            print_entity_results(conn, args.name, args.limit)
        finally:
            conn.close()
        return 0
    if args.command == "facts":
        conn = open_db(db_path)
        try:
            print_entity_facts(conn, args.name, args.limit)
        finally:
            conn.close()
        return 0
    if args.command == "story-facts":
        conn = open_db(db_path)
        try:
            print_story_facts(conn, args.story, args.limit)
        finally:
            conn.close()
        return 0
    if args.command == "catalog":
        conn = open_db(db_path)
        try:
            print_entity_catalog(conn, args.limit)
        finally:
            conn.close()
        return 0
    if args.command == "suspects":
        conn = open_db(db_path)
        try:
            print_suspects(conn, args.limit)
        finally:
            conn.close()
        return 0
    if args.command == "alignment":
        conn = open_db(db_path)
        try:
            print_story_alignment(conn, args.limit)
        finally:
            conn.close()
        return 0
    if args.command == "tension":
        conn = open_db(db_path)
        try:
            print_story_tension(conn, args.limit)
        finally:
            conn.close()
        return 0
    if args.command == "conflicts":
        conn = open_db(db_path)
        try:
            feedback_path = Path(args.feedback_path) if args.feedback_path else default_feedback_path_from_db(db_path)
            print_conflicts(conn, args.limit, feedback_path)
        finally:
            conn.close()
        return 0
    if args.command == "feedback-add":
        conn = open_db(db_path)
        try:
            feedback_path = Path(args.feedback_path) if args.feedback_path else default_feedback_path_from_db(db_path)
            write_feedback(
                conn,
                feedback_path,
                category=args.category,
                story=args.story,
                title=args.title,
                decision=args.decision,
                facet=args.facet,
                note=args.note,
                summary_contains=args.summary_contains,
            )
        finally:
            conn.close()
        return 0
    if args.command == "feedback-summary":
        conn = open_db(db_path)
        try:
            feedback_path = Path(args.feedback_path) if args.feedback_path else default_feedback_path_from_db(db_path)
            print_feedback_summary(conn, feedback_path, args.limit, args.story)
        finally:
            conn.close()
        return 0
    if args.command == "review-queue":
        conn = open_db(db_path)
        try:
            feedback_path = Path(args.feedback_path) if args.feedback_path else default_feedback_path_from_db(db_path)
            print_review_queue(conn, feedback_path, story=args.story, limit=args.limit)
        finally:
            conn.close()
        return 0
    raise SystemExit(f"Unknown command: {args.command}")


if __name__ == "__main__":
    raise SystemExit(main())
