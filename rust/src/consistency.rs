//! 一致性（consistency）模块：`src/consistency.py` 的 Rust 移植。
//!
//! 为小说文件构建并查询轻量一致性搜索索引：SQLite（rusqlite，bundled
//! FTS5）+ 反馈 JSONL 回路。子命令：
//! build / search / entity / facts / story-facts / tension / conflicts /
//! feedback-add / feedback-summary / review-queue / catalog / suspects /
//! alignment（13 支）。
//!
//! 输出文案逐字对齐 Python（含中文措辞）；SQL 原文照抄并参数化；
//! Python 隐式提交（每命令退出前 commit）在 rusqlite 下语句级 autocommit，
//! 最终状态一致。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::Result;
use chrono::{SecondsFormat, Utc};
use regex::Regex;
use rusqlite::{params, Connection, ToSql};
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::stats::Ctr;

// ---------------------------------------------------------------------------
// 常量（逐字照抄 Python 字面量）
// ---------------------------------------------------------------------------

/// 概念卡 glob。
pub const CARD_GLOB: &str = "concept/cards/**/*.md";
/// 索引文档 glob。
pub const DOC_GLOBS: &[&str] = &[
    "concept/cards/**/*.md",
    "arc-plan/**/*.md",
    "story-plan/**/*.md",
    "chapter-plan/**/*.md",
    "drafts/**/*.md",
];

pub const CONSISTENCY_MODULE_TARGET: &str = "src/consistency.py";
pub const RULES_TEMPLATE_TARGET: &str = "configs/rules/review.yaml#draft.template_rules";
pub const BOOK_DRAFT_RULES_TARGET: &str = "novel1/rules/draft.md";
pub const CONSISTENCY_CLI: &[&str] = &["python3", "-m", "consistency"];

pub const FEEDBACK_DECISIONS: &[&str] = &["confirmed", "false_positive", "designed_keep", "watch"];
pub const FEEDBACK_FACETS: &[&str] = &[
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
];

pub const INJURY_NEGATIVE_TERMS: &[&str] = &[
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
];

pub const INJURY_STABLE_TERMS: &[&str] = &[
    "没事",
    "稳住",
    "恢复",
    "缓过",
    "站稳",
    "止住",
    "轻伤",
    "能走",
    "还能打",
];

pub const EQUIPMENT_DAMAGED_TERMS: &[&str] = &[
    "裂开",
    "擦痕",
    "损坏",
    "失灵",
    "熄灭",
    "坏了",
    "断掉",
    "烧毁",
    "暴露体积",
];

pub const EQUIPMENT_ACTIVE_TERMS: &[&str] = &[
    "展开",
    "变形",
    "启动",
    "亮起",
    "抬起",
    "展开快",
    "护住",
    "挡在",
    "接口",
];

pub const GOAL_ASSIGNED_TERMS: &[&str] = &["任务", "委托", "命令", "要求", "安排", "交给", "负责"];

pub const GOAL_CHANGED_TERMS: &[&str] = &[
    "改成",
    "转而",
    "临时改",
    "改口",
    "换成",
    "不再是",
    "目标变成",
];

pub const GOAL_COMPLETED_TERMS: &[&str] = &["完成", "办完", "解决", "结束", "交差", "收尾", "达成"];

pub const RELATIONSHIP_CLOSE_TERMS: &[&str] = &[
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
];

pub const RELATIONSHIP_DISTANT_TERMS: &[&str] = &[
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
];

/// `FACT_TERM_GROUPS`（Python dict 插入序 = 字面量顺序）。
const FACT_TERM_GROUPS: &[(&str, &[&str])] = &[
    ("injury_negative", INJURY_NEGATIVE_TERMS),
    ("injury_stable", INJURY_STABLE_TERMS),
    ("equipment_damaged", EQUIPMENT_DAMAGED_TERMS),
    ("equipment_active", EQUIPMENT_ACTIVE_TERMS),
    ("goal_assigned", GOAL_ASSIGNED_TERMS),
    ("goal_changed", GOAL_CHANGED_TERMS),
    ("goal_completed", GOAL_COMPLETED_TERMS),
    ("relationship_close", RELATIONSHIP_CLOSE_TERMS),
    ("relationship_distant", RELATIONSHIP_DISTANT_TERMS),
];

// ---------------------------------------------------------------------------
// 正则（re → regex；Python `\d` 为 Unicode，本仓文件名均为 ASCII 数字，等价）
// ---------------------------------------------------------------------------

static WS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("空白正则"));
static ALIASES_SPLIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[，,、；;]+").expect("别名分隔正则"));
static HEADER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^#\s+(.*)$").expect("卡片标题正则"));
static FIELD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^- ([^：]+)：\s*(.*)$").expect("卡片字段正则"));
static STORY_PLAN_FILE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(story)-(\d+[a-z]?)").expect("story plan 文件名正则"));
static INTERLUDE_PLAN_FILE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(interlude)-(\d+)").expect("interlude plan 文件名正则"));
static STORY_CHAPTER_FILE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(story\d+|interlude\d+)-ch\d+").expect("story/chapter 文件名正则")
});
static CHAPTER_ONLY_FILE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^ch\d+\.md$").expect("ch 文件名正则"));
static FACT_SEGMENT_SPLIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[。！？!?；;\n]+").expect("fact 分段正则"));
static NEGATED_RELATION_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(不等于|不是|并非|非)\s*信任",
        r"信任\s*(不了|不起来|不起|不能)",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("否定关系正则"))
    .collect()
});
static ARC_PART_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)arc\d+").expect("arc 目录正则"));
static STORY_PART_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)story\d+").expect("story 目录正则"));
static INTERLUDE_PART_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)interlude\d+").expect("interlude 目录正则"));
static CHAPTER_PART_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)ch\d+\.md").expect("ch 目录正则"));
static ALIGN_PLAN_ONLY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"plan_only=([^;]+)").expect("plan_only 摘要正则"));
static ALIGN_DRAFT_ONLY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"draft_only=([^;]+)").expect("draft_only 摘要正则"));

// ---------------------------------------------------------------------------
// 基础类型
// ---------------------------------------------------------------------------

/// 实体（对应 Python `Entity` dataclass）。
#[derive(Debug, Clone)]
pub struct Entity {
    pub category: String,
    pub card_path: PathBuf,
    pub title: String,
    pub card_id: String,
    pub names: Vec<String>,
}

/// 证据类型（`evidence_kind` 字段取值）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EvidenceKind {
    /// 事实跳变证据。
    #[default]
    Fact,
    /// 称呼口径漂移证据。
    Alias,
    /// plan/draft 实体对齐缺口证据。
    Alignment,
}

/// 冲突候选行（Python `dict[str, object]` 行模型）。
#[derive(Debug, Clone, Default)]
pub struct ConflictRow {
    pub category: String,
    pub story: String,
    pub title: String,
    pub entity_category: String,
    pub summary: String,
    /// `fact` / `alias` / `alignment`。
    pub evidence_kind: EvidenceKind,
    /// fact 行携带；alias/alignment 行为空。
    pub fact_types: Vec<String>,
    /// high / medium / low。
    pub confidence: String,
    pub support_note: String,
    pub feedback_decision: String,
    pub feedback_facet: String,
    pub feedback_note: String,
    pub feedback_updated_at: String,
}

/// 关系动作词共现对（`query_story_relationship_pair_rows` 输出）。
#[derive(Debug, Clone)]
pub struct PairRow {
    pub story: String,
    pub path: String,
    pub chapter: Option<String>,
    pub line_start: i64,
    pub line_end: i64,
    pub text: String,
    pub left_title: String,
    pub right_title: String,
    pub close_cues: String,
    pub distant_cues: String,
}

/// story 级 plan/draft 实体覆盖行。
#[derive(Debug, Clone)]
pub struct AlignmentRow {
    pub story: String,
    pub plan_entities: i64,
    pub draft_entities: i64,
    pub plan_only_entities: Option<String>,
    pub draft_only_entities: Option<String>,
}

/// 称呼漂移行。
#[derive(Debug, Clone)]
pub struct AliasDriftRow {
    pub story: String,
    pub title: String,
    pub category: String,
    pub draft_aliases: Option<String>,
    pub plan_aliases: Option<String>,
}

/// 状态张力行（受伤/装备）。
#[derive(Debug, Clone)]
pub struct TensionRow {
    pub story: String,
    pub title: String,
    pub category: String,
    pub injury_negative: Option<String>,
    pub injury_stable: Option<String>,
    pub equipment_damaged: Option<String>,
    pub equipment_active: Option<String>,
}

/// 目标张力行。
#[derive(Debug, Clone)]
pub struct GoalTensionRow {
    pub story: String,
    pub title: String,
    pub category: String,
    pub goal_assigned: Option<String>,
    pub goal_changed: Option<String>,
    pub goal_completed: Option<String>,
}

/// 关系张力行。
#[derive(Debug, Clone)]
pub struct RelationshipTensionRow {
    pub story: String,
    pub title: String,
    pub category: String,
    pub relationship_close: Option<String>,
    pub relationship_distant: Option<String>,
}

/// 通用证据行。
#[derive(Debug, Clone)]
pub struct EvidenceRow {
    pub path: String,
    pub line_start: i64,
    pub line_end: i64,
    pub text: String,
}

/// fact 证据行。
#[derive(Debug, Clone)]
pub struct FactEvidenceRow {
    pub path: String,
    pub line_start: i64,
    pub line_end: i64,
    pub text: String,
    pub fact_type: String,
    pub cue: String,
}

/// 称呼证据行。
#[derive(Debug, Clone)]
pub struct AliasEvidenceRow {
    pub path: String,
    pub line_start: i64,
    pub line_end: i64,
    pub text: String,
    pub matched_name: String,
}

/// 人工复核行动（`build_pending_review_actions` 条目）。
#[derive(Debug, Clone)]
pub struct ReviewAction {
    pub story: String,
    pub category: String,
    pub title: String,
    pub confidence: String,
    pub focus: String,
    pub command: String,
}

/// 反馈沉淀建议（`build_feedback_backlog` 条目）。
#[derive(Debug, Clone)]
pub struct BacklogItem {
    pub target: String,
    pub reason: String,
}

/// 反馈 JSONL 记录（键保序 map，值一律字符串化）。
pub type FeedbackRecord = JsonMap<String, JsonValue>;

/// `summarize_feedback` 输出。
#[derive(Debug, Default)]
pub struct FeedbackSummary {
    pub entries: BTreeMap<String, FeedbackRecord>,
    pub history: Vec<FeedbackRecord>,
    pub conflict_rows: Vec<ConflictRow>,
    pub decision_counter: Ctr,
    pub category_counter: Ctr,
    pub story_counter: Ctr,
    pub facet_counter: Ctr,
    pub unresolved: Vec<ConflictRow>,
    pub pending_actions: Vec<ReviewAction>,
    pub backlog: Vec<BacklogItem>,
    pub unresolved_by_story: Ctr,
    pub story_filter: Option<String>,
}

/// `build_story_conflict_snapshot_from_path` 输出（Python 快照 dict 的全字段）。
#[derive(Debug)]
pub struct StoryConflictSnapshot {
    pub available: bool,
    /// novel_dir_not_found / db_not_found / story_not_found。
    pub reason: Option<String>,
    pub novel_dir: Option<PathBuf>,
    pub db_path: Option<PathBuf>,
    pub feedback_path: Option<PathBuf>,
    pub story: Option<String>,
    pub review_queue_command: Option<String>,
    pub feedback_summary_command: Option<String>,
    pub rows: Vec<ConflictRow>,
    pub decision_counter: Ctr,
    pub category_counter: Ctr,
    pub facet_counter: Ctr,
    pub pending_rows: Vec<ConflictRow>,
    /// `pending_rows.len()`（`len(pending_rows)`）。
    pub pending_count: usize,
    pub pending_actions: Vec<ReviewAction>,
    pub global_feedback_backlog: Vec<BacklogItem>,
}

// ---------------------------------------------------------------------------
// 基础工具（逐段对齐 Python）
// ---------------------------------------------------------------------------

/// `normalize_whitespace`：连续空白压成单空格并去首尾空白。
pub fn normalize_whitespace(text: &str) -> String {
    WS_RE.replace_all(text, " ").trim().to_string()
}

/// `prefix_chars`：前 n 个码点。
fn prefix_chars(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// `split_pipe_values`。
pub fn split_pipe_values(value: Option<&str>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    if value.is_empty() {
        return Vec::new();
    }
    value
        .split('|')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

/// `split_pipe_values_str`：非 Option 版本（对 `""` 返回空列表）。
pub fn split_pipe_values_str(value: &str) -> Vec<String> {
    split_pipe_values(Some(value))
}

/// `default_feedback_path_from_db`。
pub fn default_feedback_path_from_db(db_path: &Path) -> PathBuf {
    parent_or_dot(db_path).join("review-feedback.jsonl")
}

fn parent_or_dot(path: &Path) -> PathBuf {
    path.parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `find_novel_dir`：沿父目录链找 `novel*` 且含 `drafts/` 的目录。
pub fn find_novel_dir(path: &Path) -> Option<PathBuf> {
    let current = if path.exists() {
        std::fs::canonicalize(path).ok()?
    } else {
        path.to_path_buf()
    };
    let mut cursor = current;
    loop {
        if is_novel_root(&cursor) {
            return Some(cursor);
        }
        match cursor.parent() {
            Some(parent) => {
                if parent == cursor.as_path() {
                    return None;
                }
                cursor = parent.to_path_buf();
            }
            None => return None,
        }
    }
}

fn is_novel_root(candidate: &Path) -> bool {
    let name = candidate.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name.starts_with("novel") && candidate.join("drafts").is_dir()
}

/// `conflict_key`。
pub fn conflict_key(row: &ConflictRow) -> String {
    [
        row.category.as_str(),
        row.story.as_str(),
        row.title.as_str(),
        row.entity_category.as_str(),
        row.summary.as_str(),
    ]
    .join("||")
}

/// `summary_fragment`。
pub fn summary_fragment(summary: &str, max_chars: usize) -> String {
    let mut s = summary.to_string();
    for token in [" ; ", "；", " -> ", ",", "，"] {
        if let Some(pos) = s.find(token) {
            s.truncate(pos);
            break;
        }
    }
    prefix_chars(&normalize_whitespace(&s), max_chars)
}

/// `build_pending_review_focus`。
pub fn build_pending_review_focus(row: &ConflictRow) -> String {
    let evidence_kind = match row.evidence_kind {
        EvidenceKind::Fact => "fact",
        EvidenceKind::Alias => "alias",
        EvidenceKind::Alignment => "alignment",
    };
    if evidence_kind == "fact" {
        return "先回看 fact cue 和上下文段，判断这是真跳变还是阶段推进。".to_string();
    }
    if evidence_kind == "alias" {
        return "先对照 plan / draft 两侧称呼，判断是口径漂移还是有意压拍。".to_string();
    }
    if evidence_kind == "alignment" {
        return "先看 plan_only / draft_only 实体，判断是正文漏落还是施工图写偏。".to_string();
    }
    if row.category.contains("relationship") {
        return "先回看关系动作词和同段人物共现，判断是关系转冷还是抽取误绑。".to_string();
    }
    if row.category.contains("goal") {
        return "先回看任务口径和章末动作，判断是目标改口还是正常推进。".to_string();
    }
    "先回看相关证据段，再决定是 confirmed、false_positive 还是 designed_keep。".to_string()
}

/// `shlex.quote`（POSIX，对齐 CPython `shlex._quote`）。
fn shlex_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    let safe = value.bytes().all(|b| {
        matches!(
            b,
            b'a'..=b'z'
                | b'A'..=b'Z'
                | b'0'..=b'9'
                | b'_'
                | b'@'
                | b'%'
                | b'+'
                | b'='
                | b':'
                | b','
                | b'.'
                | b'/'
                | b'-'
        )
    });
    if safe {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// `build_feedback_command`（`decision` 缺省 `watch`）。
pub fn build_feedback_command(db_path: &Path, row: &ConflictRow, decision: Option<&str>) -> String {
    let decision = decision.unwrap_or("watch");
    let command_root = find_novel_dir(db_path).unwrap_or_else(|| db_path.to_path_buf());
    let name = command_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let root_label = if name.starts_with("novel") {
        name.to_string()
    } else {
        command_root.display().to_string()
    };
    let mut parts: Vec<String> = CONSISTENCY_CLI.iter().map(|s| s.to_string()).collect();
    parts.extend([
        "feedback-add".to_string(),
        root_label,
        "--category".to_string(),
        row.category.clone(),
        "--story".to_string(),
        row.story.clone(),
        "--title".to_string(),
        row.title.clone(),
        "--decision".to_string(),
        decision.to_string(),
    ]);
    let fragment = summary_fragment(&row.summary, 18);
    if !fragment.is_empty() {
        parts.push("--summary-contains".to_string());
        parts.push(fragment);
    }
    parts
        .iter()
        .map(|p| shlex_quote(p))
        .collect::<Vec<String>>()
        .join(" ")
}

/// `build_pending_review_actions`（`limit` 缺省 6）。
pub fn build_pending_review_actions(
    db_path: &Path,
    rows: &[ConflictRow],
    limit: usize,
) -> Vec<ReviewAction> {
    rows.iter()
        .take(limit)
        .map(|row| ReviewAction {
            story: row.story.clone(),
            category: row.category.clone(),
            title: row.title.clone(),
            confidence: row.confidence.clone(),
            focus: build_pending_review_focus(row),
            command: build_feedback_command(db_path, row, None),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 反馈 JSONL
// ---------------------------------------------------------------------------

/// `read_feedback_history`：逐行 JSON，值字符串化（None 丢弃）。
pub fn read_feedback_history(path: &Path) -> Result<Vec<FeedbackRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path)?;
    let mut history = Vec::new();
    for raw_line in raw.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let data: FeedbackRecord = serde_json::from_str(line)?;
        let mut record: FeedbackRecord = JsonMap::new();
        for (key, value) in data {
            let stringified = match value {
                JsonValue::Null => continue,
                JsonValue::String(s) => JsonValue::String(s),
                JsonValue::Bool(b) => JsonValue::String(b.to_string()),
                other => JsonValue::String(other.to_string()),
            };
            record.insert(key, stringified);
        }
        history.push(record);
    }
    Ok(history)
}

fn record_str(record: &FeedbackRecord, key: &str) -> String {
    record
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// `load_feedback_entries`：`conflict_key` 重复时后写覆盖。
pub fn load_feedback_entries(path: &Path) -> Result<BTreeMap<String, FeedbackRecord>> {
    let mut entries: BTreeMap<String, FeedbackRecord> = BTreeMap::new();
    for data in read_feedback_history(path)? {
        let key = record_str(&data, "conflict_key").trim().to_string();
        if key.is_empty() {
            continue;
        }
        entries.insert(key, data);
    }
    Ok(entries)
}

/// `append_feedback_entry`（Python `json.dumps` 默认分隔符 `": "` / `", "`；
/// 键序 = Python entry dict 插入序，与 map 后端无关）。
pub fn append_feedback_entry(path: &Path, entry: &FeedbackRecord) -> Result<()> {
    const KEY_ORDER: &[&str] = &[
        "conflict_key",
        "category",
        "story",
        "title",
        "entity_category",
        "summary",
        "decision",
        "facet",
        "note",
        "updated_at",
    ];
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent)?;
    let mut line = String::from("{");
    let mut first = true;
    for key in KEY_ORDER {
        let Some(value) = entry.get(*key) else {
            continue;
        };
        if !first {
            line.push_str(", ");
        }
        first = false;
        line.push_str(&serde_json::to_string(key)?);
        line.push_str(": ");
        line.push_str(&serde_json::to_string(value)?);
    }
    // 兼容未知键（read-back 再写场景）：按字节序排尾。
    let known: std::collections::BTreeSet<&str> = KEY_ORDER.iter().copied().collect();
    let extra: Vec<(String, &JsonValue)> = entry
        .iter()
        .filter(|(k, _)| !known.contains(k.as_str()))
        .map(|(k, v)| (k.clone(), v))
        .collect();
    for (key, value) in extra {
        if !first {
            line.push_str(", ");
        }
        first = false;
        line.push_str(&serde_json::to_string(&key)?);
        line.push_str(": ");
        line.push_str(&serde_json::to_string(value)?);
    }
    line.push('}');
    line.push('\n');
    use std::io::Write;
    let mut handle = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    handle.write_all(line.as_bytes())?;
    Ok(())
}

/// `build_feedback_backlog`。
pub fn build_feedback_backlog(history: &[FeedbackRecord]) -> Vec<BacklogItem> {
    let mut category_decision = Ctr::default();
    let mut story_decision = Ctr::default();
    let mut facet = Ctr::default();
    for entry in history {
        let category = record_str(entry, "category");
        let story = record_str(entry, "story");
        let decision = record_str(entry, "decision");
        let facet_value = record_str(entry, "facet");
        if !category.is_empty() && !decision.is_empty() {
            category_decision.add(&format!("{category}::{decision}"), 1);
        }
        if !story.is_empty() && !decision.is_empty() {
            story_decision.add(&format!("{story}::{decision}"), 1);
        }
        if decision == "designed_keep" && !facet_value.is_empty() {
            facet.add(&facet_value, 1);
        }
    }

    let top_by_suffix = |counter: &Ctr, suffix: &str| -> Option<(String, usize)> {
        counter
            .most_common_all()
            .into_iter()
            .filter(|(name, _)| name.ends_with(suffix))
            .min_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)))
    };

    let mut backlog: Vec<BacklogItem> = Vec::new();

    if let Some((top_name, top_count)) = top_by_suffix(&category_decision, "::false_positive") {
        let category = top_name.split("::").next().unwrap_or(&top_name);
        backlog.push(BacklogItem {
            target: CONSISTENCY_MODULE_TARGET.to_string(),
            reason: format!(
                "`{category}` 已累计 {top_count} 条误报反馈，优先压抽取噪声，不要继续把人工复核当默认补丁。"
            ),
        });
    }

    if let Some((top_name, top_count)) = top_by_suffix(&category_decision, "::designed_keep") {
        let category = top_name.split("::").next().unwrap_or(&top_name);
        let mut target = RULES_TEMPLATE_TARGET.to_string();
        let mut reason_tail = "说明这类变化应开始沉淀为可保留模式样本。".to_string();
        if let Some((top_facet, facet_count)) = facet.most_common(1).into_iter().next() {
            if top_facet == "register" || top_facet == "naming" {
                target = BOOK_DRAFT_RULES_TARGET.to_string();
                reason_tail = format!(
                    "其中 `{top_facet}` 已出现 {facet_count} 次，更适合先写成命名/称谓边界规则。"
                );
            } else if matches!(
                top_facet.as_str(),
                "voice" | "rhythm" | "motif" | "scene_callback" | "irony"
            ) {
                target = RULES_TEMPLATE_TARGET.to_string();
                reason_tail = format!(
                    "其中 `{top_facet}` 已出现 {facet_count} 次，应开始积累这类可保留风格样本。"
                );
            }
        }
        backlog.push(BacklogItem {
            target,
            reason: format!("`{category}` 已累计 {top_count} 条设计性保留反馈，{reason_tail}"),
        });
    }

    if let Some((top_name, top_count)) = top_by_suffix(&story_decision, "::confirmed") {
        let story = top_name.split("::").next().unwrap_or(&top_name);
        backlog.push(BacklogItem {
            target: BOOK_DRAFT_RULES_TARGET.to_string(),
            reason: format!(
                "`{story}` 已累计 {top_count} 条确认成立的一致性问题，说明这不是偶发手误，值得沉淀为返工规则。"
            ),
        });
    }

    if let Some((top_name, top_count)) = top_by_suffix(&story_decision, "::watch") {
        let story = top_name.split("::").next().unwrap_or(&top_name);
        backlog.push(BacklogItem {
            target: "novel1/research/consistency/review-feedback.jsonl".to_string(),
            reason: format!("`{story}` 仍有 {top_count} 条长期待观察反馈，说明这条 Story 的一致性口径还没真正收敛。"),
        });
    }

    backlog.truncate(6);
    backlog
}

/// `filter_feedback_history_by_story`。
pub fn filter_feedback_history_by_story(
    history: &[FeedbackRecord],
    story: Option<&str>,
) -> Vec<FeedbackRecord> {
    match story {
        None => history.to_vec(),
        Some(story) => history
            .iter()
            .filter(|entry| record_str(entry, "story") == story)
            .cloned()
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// 卡片 / 文档扫描
// ---------------------------------------------------------------------------

/// `split_fact_segments`。
pub fn split_fact_segments(text: &str) -> Vec<String> {
    FACT_SEGMENT_SPLIT_RE
        .split(text)
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect()
}

/// `is_negated_relationship_segment`。
pub fn is_negated_relationship_segment(segment: &str, term: &str) -> bool {
    if !["信任", "默认", "配合", "护住", "并肩", "愿意跟", "接住"].contains(&term) {
        return false;
    }
    NEGATED_RELATION_PATTERNS
        .iter()
        .any(|pattern| pattern.is_match(segment))
}

/// `collect_local_fact_cues`。
pub fn collect_local_fact_cues(
    passage_text: &str,
    entity_name: &str,
    fact_type: &str,
    terms: &[&str],
) -> Vec<String> {
    let mut cues: Vec<String> = Vec::new();
    for segment in split_fact_segments(passage_text) {
        if !segment.contains(entity_name) {
            continue;
        }
        let entity_index = segment.find(entity_name).unwrap_or(0);
        for &term in terms {
            let Some(term_index) = segment.find(term) else {
                continue;
            };
            if (term_index as i64 - entity_index as i64).abs() > 60 {
                continue;
            }
            if fact_type == "relationship_close" && is_negated_relationship_segment(&segment, term)
            {
                continue;
            }
            if !cues.iter().any(|c| c == term) {
                cues.push(term.to_string());
            }
        }
    }
    cues
}

/// `parse_field_map`。
pub fn parse_field_map(text: &str) -> BTreeMap<String, String> {
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    for caps in FIELD_RE.captures_iter(text) {
        let key = normalize_whitespace(caps.get(1).unwrap().as_str());
        let value = normalize_whitespace(caps.get(2).unwrap().as_str());
        fields.insert(key, value);
    }
    fields
}

/// `infer_title_variants`。
pub fn infer_title_variants(title: &str) -> Vec<String> {
    let mut variants: Vec<String> = Vec::new();
    if title.contains('·') {
        for part in title.split('·') {
            let cleaned = normalize_whitespace(part);
            if !cleaned.is_empty() && !variants.iter().any(|v| v == &cleaned) {
                variants.push(cleaned);
            }
        }
    }
    variants
}

/// `extract_names`。
pub fn extract_names(title: &str, fields: &BTreeMap<String, String>) -> Vec<String> {
    let mut names: Vec<String> = vec![title.trim().to_string()];
    for variant in infer_title_variants(title) {
        if !names.iter().any(|n| n == &variant) {
            names.push(variant);
        }
    }
    let alias_value = fields.get("别名 / 英文名").cloned().unwrap_or_default();
    if !alias_value.is_empty() && !["无", "-", "待定"].contains(&alias_value.as_str()) {
        for part in ALIASES_SPLIT_RE.split(&alias_value) {
            let cleaned = normalize_whitespace(part);
            if !cleaned.is_empty() && !names.iter().any(|n| n == &cleaned) {
                names.push(cleaned);
            }
        }
    }
    names
}

fn iter_glob(novel_dir: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    let full = novel_dir.join(pattern).to_string_lossy().into_owned();
    let mut paths = std::collections::BTreeSet::new();
    for entry in glob::glob(&full)?.filter_map(Result::ok) {
        paths.insert(entry);
    }
    let mut out: Vec<PathBuf> = paths.into_iter().filter(|p| p.is_file()).collect();
    out.sort();
    Ok(out)
}

/// `load_entities`。
pub fn load_entities(novel_dir: &Path) -> Result<Vec<Entity>> {
    let mut entities: Vec<Entity> = Vec::new();
    for card_path in iter_glob(novel_dir, CARD_GLOB)? {
        if card_path
            .components()
            .any(|c| c.as_os_str() == std::ffi::OsStr::new("_templates"))
        {
            continue;
        }
        let text = std::fs::read_to_string(&card_path)?;
        let Some(matched) = HEADER_RE.captures(&text) else {
            continue;
        };
        let title = normalize_whitespace(matched.get(1).unwrap().as_str());
        let fields = parse_field_map(&text);
        let category = card_path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let card_id = fields.get("卡片 ID").cloned().unwrap_or_default();
        let names = extract_names(&title, &fields);
        entities.push(Entity {
            category,
            card_path,
            title,
            card_id,
            names,
        });
    }
    Ok(entities)
}

/// `iter_documents`。
pub fn iter_documents(novel_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut docs = std::collections::BTreeSet::new();
    for pattern in DOC_GLOBS {
        for path in iter_glob(novel_dir, pattern)? {
            docs.insert(path);
        }
    }
    let mut out: Vec<PathBuf> = docs.into_iter().collect();
    out.sort();
    Ok(out)
}

/// Python `str.splitlines()` 等价迭代（\r\n 计一次换行）。
fn splitlines_iter(text: &str) -> impl Iterator<Item = &str> {
    struct Splitlines<'a>(&'a str, usize, usize);
    impl<'a> Iterator for Splitlines<'a> {
        type Item = &'a str;
        fn next(&mut self) -> Option<&'a str> {
            let (text, pos, len) = (self.0, self.1, self.2);
            if pos >= len {
                return None;
            }
            let mut i = pos;
            while i < len {
                let (ch, width) = text[i..].chars().next().map(|c| (c, c.len_utf8())).unwrap();
                let line = &text[pos..i];
                match ch {
                    '\r' | '\n' => {
                        i += width;
                        if ch == '\r' && i < len && text[i..].starts_with('\n') {
                            i += 1;
                        }
                        self.1 = i;
                        return Some(line);
                    }
                    '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}'
                    | '\u{2028}' | '\u{2029}' => {
                        i += width;
                        self.1 = i;
                        return Some(line);
                    }
                    _ => i += width,
                }
            }
            let line = &text[pos..len];
            self.1 = len;
            if line.is_empty() {
                None
            } else {
                Some(line)
            }
        }
    }
    Splitlines(text, 0, text.len())
}

/// `split_passages`：段落 → `(line_start, line_end, text)`。
pub fn split_passages(text: &str) -> Vec<(usize, usize, String)> {
    let mut passages: Vec<(usize, usize, String)> = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut start_line = 1usize;
    let mut current_line = 1usize;
    for raw_line in splitlines_iter(text) {
        let line = raw_line.trim_end();
        if !line.trim().is_empty() {
            if paragraph.is_empty() {
                start_line = current_line;
            }
            paragraph.push(line.to_string());
        } else if !paragraph.is_empty() {
            let end = current_line - 1;
            passages.push((start_line, end, paragraph.join("\n")));
            paragraph.clear();
        }
        current_line += 1;
    }
    if !paragraph.is_empty() {
        passages.push((start_line, current_line - 1, paragraph.join("\n")));
    }
    passages
}

/// schema DDL：与 Python `build_schema` 的 `executescript` 字面量逐字一致
/// （SQLite 按执行文本原样存 `sqlite_master.sql`，dump 字节级一致要求 8 空格缩进）。
const SCHEMA_DDL: &str = r#"
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
        "#;

fn build_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA_DDL)?;
    Ok(())
}

/// Python `re.fullmatch(pat, part)` 语义（regex 1.x 无 `is_exact_match`，用 `find` + 全跨度判断）。
fn part_full_match(re: &Regex, part: &str) -> bool {
    re.find(part)
        .is_some_and(|m| m.start() == 0 && m.end() == part.len())
}

/// `classify_document`：`(doc_type, arc, story, chapter)`。
pub fn classify_document(
    path: &Path,
    novel_dir: &Path,
) -> (String, Option<String>, Option<String>, Option<String>) {
    let relative = path.strip_prefix(novel_dir).unwrap_or(path);
    let parts: Vec<String> = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let doc_type = parts.first().cloned().unwrap_or_default();
    let mut arc: Option<String> = None;
    for part in &parts {
        if part_full_match(&ARC_PART_RE, part) {
            arc = Some(part.to_lowercase());
            break;
        }
    }
    let mut story: Option<String> = None;
    let mut chapter: Option<String> = None;
    for part in &parts {
        if part_full_match(&STORY_PART_RE, part) || part_full_match(&INTERLUDE_PART_RE, part) {
            story = Some(part.to_lowercase());
        }
        if part_full_match(&CHAPTER_PART_RE, part) {
            chapter = Some(part[..part.len() - 3].to_string());
        }
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned().to_lowercase())
        .unwrap_or_default();
    if story.is_none() {
        if let Some(caps) = STORY_CHAPTER_FILE_RE.captures(&stem) {
            story = Some(caps.get(1).unwrap().as_str().to_lowercase());
        } else if let Some(caps) = STORY_PLAN_FILE_RE.captures(&stem) {
            story = Some(format!(
                "story{}",
                caps.get(2).unwrap().as_str().to_lowercase()
            ));
        } else if let Some(caps) = INTERLUDE_PLAN_FILE_RE.captures(&stem) {
            story = Some(format!(
                "interlude{}",
                caps.get(2).unwrap().as_str().to_lowercase()
            ));
        } else if doc_type == "chapter-plan"
            && path
                .file_name()
                .map(|n| CHAPTER_ONLY_FILE_RE.is_match(&n.to_string_lossy()))
                .unwrap_or(false)
        {
            story = Some("story1".to_string());
        }
    }
    (doc_type, arc, story, chapter)
}

/// `build_index`：重建索引（Python 在实体插入后与函数尾部各 `conn.commit()` 一次；
/// rusqlite autocommit 逐语句提交，终态一致）。
pub fn build_index(novel_dir: &Path, db_path: &Path) -> Result<()> {
    let entities = load_entities(novel_dir)?;
    let documents = iter_documents(novel_dir)?;
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(db_path)?;
    build_schema(&conn)?;

    for entity in &entities {
        conn.execute(
            "INSERT INTO entities(category, card_path, card_id, title) VALUES (?1, ?2, ?3, ?4)",
            params![
                entity.category,
                entity.card_path.display().to_string(),
                entity.card_id,
                entity.title
            ],
        )?;
        let entity_id = conn.last_insert_rowid();
        for name in &entity.names {
            conn.execute(
                "INSERT INTO entity_names(entity_id, name) VALUES (?1, ?2)",
                params![entity_id, name],
            )?;
        }
    }

    let name_records: Vec<(i64, i64, String)> = {
        let mut stmt = conn
            .prepare("SELECT id, entity_id, name FROM entity_names ORDER BY LENGTH(name) DESC")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    for path in &documents {
        let text = std::fs::read_to_string(path)?;
        let (doc_type, arc, story, chapter) = classify_document(path, novel_dir);
        conn.execute(
            "INSERT INTO documents(path, doc_type, arc, story, chapter) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                path.display().to_string(),
                doc_type,
                arc,
                story,
                chapter
            ],
        )?;
        let document_id = conn.last_insert_rowid();
        for (line_start, line_end, passage_text) in split_passages(&text) {
            conn.execute(
                "INSERT INTO passages(document_id, line_start, line_end, text) VALUES (?1, ?2, ?3, ?4)",
                params![document_id, line_start as i64, line_end as i64, passage_text],
            )?;
            let passage_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO passage_fts(rowid, path, text) VALUES (?1, ?2, ?3)",
                params![passage_id, path.display().to_string(), passage_text],
            )?;
            for record in &name_records {
                let (entity_name_id, entity_id, name) = record;
                let count = passage_text.matches(name.as_str()).count();
                if count == 0 {
                    continue;
                }
                conn.execute(
                    "INSERT INTO mentions(entity_id, entity_name_id, document_id, passage_id, count)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![entity_id, entity_name_id, document_id, passage_id, count],
                )?;
                for (fact_type, terms) in FACT_TERM_GROUPS {
                    let cues = collect_local_fact_cues(&passage_text, name, fact_type, terms);
                    for cue in &cues {
                        conn.execute(
                            "INSERT INTO fact_candidates(entity_id, entity_name_id, document_id, passage_id, fact_type, cue)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            params![entity_id, entity_name_id, document_id, passage_id, fact_type, cue],
                        )?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// `open_db`：不存在则创建（与 Python `sqlite3.connect` 一致）。
pub fn open_db(path: &Path) -> Result<Connection> {
    Ok(Connection::open(path)?)
}

/// `resolve_db_path`：db 文件 / consistency 目录 / novel 目录三形态。
pub fn resolve_db_path(raw_path: &Path) -> PathBuf {
    if raw_path.is_dir() {
        if raw_path.file_name().and_then(|n| n.to_str()) == Some("consistency") {
            return raw_path.join("consistency.sqlite3");
        }
        if let Some(novel_dir) = find_novel_dir(raw_path) {
            return novel_dir
                .join("research")
                .join("consistency")
                .join("consistency.sqlite3");
        }
    }
    if let Some(name) = raw_path.file_name().and_then(|n| n.to_str()) {
        if name.starts_with("novel") && raw_path.extension().is_none() {
            return raw_path
                .join("research")
                .join("consistency")
                .join("consistency.sqlite3");
        }
    }
    raw_path.to_path_buf()
}

// ---------------------------------------------------------------------------
// 查询（SQL 原文照抄，参数化）
// ---------------------------------------------------------------------------

/// `query_story_tension_rows`。
pub fn query_story_tension_rows(conn: &Connection, limit: i64) -> Result<Vec<TensionRow>> {
    let sql = "
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
        LIMIT ?";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![limit], |row| {
        Ok(TensionRow {
            story: row.get::<_, String>(0)?,
            title: row.get(1)?,
            category: row.get(2)?,
            injury_negative: row.get(3)?,
            injury_stable: row.get(4)?,
            equipment_damaged: row.get(5)?,
            equipment_active: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// `query_story_goal_tension_rows`。
pub fn query_story_goal_tension_rows(conn: &Connection, limit: i64) -> Result<Vec<GoalTensionRow>> {
    let sql = "
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
        LIMIT ?";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![limit], |row| {
        Ok(GoalTensionRow {
            story: row.get::<_, String>(0)?,
            title: row.get(1)?,
            category: row.get(2)?,
            goal_assigned: row.get(3)?,
            goal_changed: row.get(4)?,
            goal_completed: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// `query_story_relationship_tension_rows`。
pub fn query_story_relationship_tension_rows(
    conn: &Connection,
    limit: i64,
) -> Result<Vec<RelationshipTensionRow>> {
    let sql = "
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
        LIMIT ?";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![limit], |row| {
        Ok(RelationshipTensionRow {
            story: row.get::<_, String>(0)?,
            title: row.get(1)?,
            category: row.get(2)?,
            relationship_close: row.get(3)?,
            relationship_distant: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// `collect_relationship_cues`。
pub fn collect_relationship_cues(text: &str) -> (Vec<&'static str>, Vec<&'static str>) {
    let close = RELATIONSHIP_CLOSE_TERMS
        .iter()
        .copied()
        .filter(|t| text.contains(t))
        .collect();
    let distant = RELATIONSHIP_DISTANT_TERMS
        .iter()
        .copied()
        .filter(|t| text.contains(t))
        .collect();
    (close, distant)
}

/// `has_relationship_pronoun_bridge`。
pub fn has_relationship_pronoun_bridge(text: &str) -> bool {
    ["他", "她", "你", "你们", "两人", "两个人", "对方"]
        .iter()
        .any(|token| text.contains(token))
}

/// `query_story_relationship_pair_rows`。
pub fn query_story_relationship_pair_rows(conn: &Connection, limit: i64) -> Result<Vec<PairRow>> {
    let sql = "
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
        ORDER BY d.story, d.path, p.line_start, e.title, matched_name";
    let mut stmt = conn.prepare(sql)?;
    type RawPairRow = (
        String,
        String,
        Option<String>,
        i64,
        i64,
        String,
        String,
        String,
    );
    let raw_rows: Vec<RawPairRow> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    type PassageKey = (String, String, Option<String>, i64, i64, String);
    // (story, path, chapter, line_start, line_end, text) → {title: [matched names]}（首现序）
    let mut by_passage: BTreeMap<PassageKey, Vec<(String, Vec<String>)>> = BTreeMap::new();
    for (story, path, chapter, line_start, line_end, text, title, matched_name) in raw_rows {
        let key = (story, path, chapter, line_start, line_end, text);
        let entry = by_passage.entry(key).or_default();
        if let Some(names) = entry.iter_mut().find(|item| item.0 == title) {
            if !names.1.contains(&matched_name) {
                names.1.push(matched_name);
            }
        } else {
            entry.push((title, vec![matched_name]));
        }
    }

    let mut pair_rows: Vec<PairRow> = Vec::new();
    for ((story, path, chapter, line_start, line_end, passage_text), title_map) in
        by_passage.into_iter()
    {
        if title_map.len() < 2 {
            continue;
        }
        let mut all_titles: Vec<String> = title_map.iter().map(|(t, _)| t.clone()).collect();
        all_titles.sort();
        all_titles.dedup();
        let segments = {
            let split = split_fact_segments(&passage_text);
            if split.is_empty() {
                vec![passage_text.clone()]
            } else {
                split
            }
        };
        let mut seen_pairs: std::collections::BTreeSet<(String, String, String, String)> =
            std::collections::BTreeSet::new();
        for segment in &segments {
            let (close_cues, distant_cues) = collect_relationship_cues(segment);
            if close_cues.is_empty() && distant_cues.is_empty() {
                continue;
            }
            let mut present_titles: Vec<String> = Vec::new();
            for (title, names) in &title_map {
                if names.iter().any(|name| segment.contains(name)) {
                    present_titles.push(title.clone());
                }
            }
            if present_titles.len() < 2 {
                if all_titles.len() == 2 && has_relationship_pronoun_bridge(segment) {
                    present_titles = all_titles.clone();
                } else {
                    continue;
                }
            }
            if present_titles.len() < 2 {
                continue;
            }
            let mut ordered_titles = present_titles;
            ordered_titles.sort();
            ordered_titles.dedup();
            for (idx, left_title) in ordered_titles.iter().enumerate() {
                for right_title in ordered_titles.iter().skip(idx + 1) {
                    let pair_key = (
                        story.clone(),
                        left_title.clone(),
                        right_title.clone(),
                        segment.clone(),
                    );
                    if !seen_pairs.insert(pair_key.clone()) {
                        continue;
                    }
                    pair_rows.push(PairRow {
                        story: story.clone(),
                        path: path.clone(),
                        chapter: chapter.clone(),
                        line_start,
                        line_end,
                        text: segment.clone(),
                        left_title: left_title.clone(),
                        right_title: right_title.clone(),
                        close_cues: close_cues.join(","),
                        distant_cues: distant_cues.join(","),
                    });
                    if pair_rows.len() as i64 >= limit {
                        return Ok(pair_rows);
                    }
                }
            }
        }
    }
    Ok(pair_rows)
}

/// `query_story_alignment_rows`。
pub fn query_story_alignment_rows(conn: &Connection, limit: i64) -> Result<Vec<AlignmentRow>> {
    let sql = "
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
        LIMIT ?";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![limit], |row| {
        Ok(AlignmentRow {
            story: row.get::<_, String>(0)?,
            plan_entities: row.get(1)?,
            draft_entities: row.get(2)?,
            plan_only_entities: row.get(3)?,
            draft_only_entities: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// `query_story_alignment_gap_rows`。
pub fn query_story_alignment_gap_rows(conn: &Connection, limit: i64) -> Result<Vec<AlignmentRow>> {
    let rows = query_story_alignment_rows(conn, limit)?;
    Ok(rows
        .into_iter()
        .filter(|row| row.plan_only_entities.is_some() || row.draft_only_entities.is_some())
        .collect())
}

/// `query_story_alignment_evidence`。
pub fn query_story_alignment_evidence(
    conn: &Connection,
    story: &str,
    entity_title: &str,
    side: &str,
    limit: i64,
) -> Result<Vec<EvidenceRow>> {
    let doc_types: &[&str] = match side {
        "plan" => &["story-plan", "chapter-plan"],
        "draft" => &["drafts"],
        other => anyhow::bail!("Unknown side: {other}"),
    };
    let placeholders = vec!["?"; doc_types.len()].join(", ");
    let sql = format!(
        r#"
        SELECT d.path, p.line_start, p.line_end, p.text
        FROM mentions m
        JOIN entities e ON e.id = m.entity_id
        JOIN documents d ON d.id = m.document_id
        JOIN passages p ON p.id = m.passage_id
        WHERE d.story = ? AND e.title = ? AND d.doc_type IN ({placeholders})
        ORDER BY d.path, p.line_start
        LIMIT ?
    "#
    );
    let mut params: Vec<Box<dyn ToSql>> = vec![
        Box::new(story.to_string()) as Box<dyn ToSql>,
        Box::new(entity_title.to_string()) as Box<dyn ToSql>,
    ];
    for dt in doc_types {
        params.push(Box::new(dt.to_string()) as Box<dyn ToSql>);
    }
    params.push(Box::new(limit) as Box<dyn ToSql>);
    let param_refs: Vec<&dyn ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        Ok(EvidenceRow {
            path: row.get::<_, String>(0)?,
            line_start: row.get(1)?,
            line_end: row.get(2)?,
            text: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// `query_story_tension_evidence`。
pub fn query_story_tension_evidence(
    conn: &Connection,
    story: &str,
    entity_title: &str,
    limit: i64,
    fact_types: Option<&[String]>,
) -> Result<Vec<FactEvidenceRow>> {
    let mut sql = String::from(
        "
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
        WHERE d.story = ? AND e.title = ?",
    );
    let mut params: Vec<Box<dyn ToSql>> = vec![
        Box::new(story.to_string()) as Box<dyn ToSql>,
        Box::new(entity_title.to_string()) as Box<dyn ToSql>,
    ];
    if let Some(types) = fact_types.filter(|t| !t.is_empty()) {
        let placeholders = vec!["?"; types.len()].join(", ");
        sql.push_str(&format!(" AND f.fact_type IN ({placeholders})"));
        for t in types {
            params.push(Box::new(t.clone()) as Box<dyn ToSql>);
        }
    }
    sql.push_str(
        "
        ORDER BY d.path, p.line_start
        LIMIT ?
    ",
    );
    params.push(Box::new(limit) as Box<dyn ToSql>);
    let param_refs: Vec<&dyn ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(sql.as_str())?;
    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        Ok(FactEvidenceRow {
            path: row.get::<_, String>(0)?,
            line_start: row.get(1)?,
            line_end: row.get(2)?,
            text: row.get(3)?,
            fact_type: row.get(4)?,
            cue: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// `query_story_alias_drift_rows`。
pub fn query_story_alias_drift_rows(conn: &Connection, limit: i64) -> Result<Vec<AliasDriftRow>> {
    let sql = "
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
        LIMIT ?";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![limit], |row| {
        Ok(AliasDriftRow {
            story: row.get::<_, String>(0)?,
            title: row.get(1)?,
            category: row.get(2)?,
            draft_aliases: row.get(3)?,
            plan_aliases: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// `query_story_alias_evidence`。
pub fn query_story_alias_evidence(
    conn: &Connection,
    story: &str,
    entity_title: &str,
    side: &str,
    limit: i64,
) -> Result<Vec<AliasEvidenceRow>> {
    let doc_types: &[&str] = match side {
        "plan" => &["story-plan", "chapter-plan"],
        "draft" => &["drafts"],
        other => anyhow::bail!("Unknown side: {other}"),
    };
    let placeholders = vec!["?"; doc_types.len()].join(", ");
    let sql = format!(
        r#"
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
    "#
    );
    let mut params: Vec<Box<dyn ToSql>> = vec![
        Box::new(story.to_string()) as Box<dyn ToSql>,
        Box::new(entity_title.to_string()) as Box<dyn ToSql>,
    ];
    for dt in doc_types {
        params.push(Box::new(dt.to_string()) as Box<dyn ToSql>);
    }
    params.push(Box::new(limit) as Box<dyn ToSql>);
    let param_refs: Vec<&dyn ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(sql.as_str())?;
    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        Ok(AliasEvidenceRow {
            path: row.get::<_, String>(0)?,
            line_start: row.get(1)?,
            line_end: row.get(2)?,
            text: row.get(3)?,
            matched_name: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// `query_fact_support_summary`。
pub fn query_fact_support_summary(
    conn: &Connection,
    story: &str,
    entity_title: &str,
    fact_types: &[String],
) -> Result<(i64, i64, i64)> {
    let placeholders = vec!["?"; fact_types.len()].join(", ");
    let sql = format!(
        r#"
        SELECT
            COUNT(DISTINCT CASE WHEN d.doc_type = 'drafts' THEN d.id END) AS draft_docs,
            COUNT(DISTINCT CASE WHEN d.doc_type = 'chapter-plan' THEN d.id END) AS chapter_plan_docs,
            COUNT(DISTINCT CASE WHEN d.doc_type = 'story-plan' THEN d.id END) AS story_plan_docs
        FROM fact_candidates f
        JOIN entities e ON e.id = f.entity_id
        JOIN documents d ON d.id = f.document_id
        WHERE d.story = ? AND e.title = ? AND f.fact_type IN ({placeholders})
    "#
    );
    let mut params: Vec<Box<dyn ToSql>> = vec![
        Box::new(story.to_string()) as Box<dyn ToSql>,
        Box::new(entity_title.to_string()) as Box<dyn ToSql>,
    ];
    for t in fact_types {
        params.push(Box::new(t.clone()) as Box<dyn ToSql>);
    }
    let param_refs: Vec<&dyn ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(sql.as_str())?;
    let row = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .next()
        .ok_or_else(|| anyhow::anyhow!("support 查询无结果"))??;
    // Python `int(row["x"] or 0)`：COUNT 非 NULL，直接返回。
    Ok(row)
}

/// `score_fact_confidence`。
pub fn score_fact_confidence(support: &(i64, i64, i64)) -> (String, String) {
    let (draft_docs, chapter_plan_docs, story_plan_docs) = *support;
    if draft_docs >= 2 {
        return ("high".into(), format!("draft_docs={draft_docs}"));
    }
    if draft_docs >= 1 && (chapter_plan_docs + story_plan_docs) >= 1 {
        return (
            "high".into(),
            format!(
                "draft_docs={draft_docs} upstream_docs={}",
                chapter_plan_docs + story_plan_docs
            ),
        );
    }
    if draft_docs >= 1 {
        return ("medium".into(), format!("draft_docs={draft_docs}"));
    }
    if chapter_plan_docs >= 1 && story_plan_docs >= 1 {
        return (
            "medium".into(),
            format!("chapter_plan_docs={chapter_plan_docs} story_plan_docs={story_plan_docs}"),
        );
    }
    (
        "low".into(),
        format!("chapter_plan_docs={chapter_plan_docs} story_plan_docs={story_plan_docs}"),
    )
}

/// `score_alignment_confidence`。
pub fn score_alignment_confidence(summary: &str) -> (String, String) {
    let plan_only = if summary.contains("plan_only=") {
        ALIGN_PLAN_ONLY_RE
            .captures(summary)
            .map(|caps| split_pipe_values(Some(caps.get(1).unwrap().as_str())))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let draft_only = if summary.contains("draft_only=") {
        ALIGN_DRAFT_ONLY_RE
            .captures(summary)
            .map(|caps| split_pipe_values(Some(caps.get(1).unwrap().as_str())))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let entity_count = plan_only.len() + draft_only.len();
    let label = if entity_count >= 3 {
        "high"
    } else if entity_count >= 2 {
        "medium"
    } else {
        "low"
    };
    (label.into(), format!("drift_entities={entity_count}"))
}

/// `score_alias_confidence`。
pub fn score_alias_confidence(plan_aliases: &str, draft_aliases: &str) -> (String, String) {
    let plan_count = split_pipe_values_str(&plan_aliases.replace(',', "|")).len();
    let draft_count = split_pipe_values_str(&draft_aliases.replace(',', "|")).len();
    let label = if plan_count >= 2 && draft_count >= 2 {
        "high"
    } else if plan_count >= 1 && draft_count >= 1 {
        "medium"
    } else {
        "low"
    };
    (
        label.into(),
        format!("plan_aliases={plan_count} draft_aliases={draft_count}"),
    )
}

/// `apply_feedback`。
pub fn apply_feedback(
    mut rows: Vec<ConflictRow>,
    feedback_entries: &BTreeMap<String, FeedbackRecord>,
) -> Vec<ConflictRow> {
    for row in &mut rows {
        let key = conflict_key(row);
        let Some(feedback) = feedback_entries.get(&key) else {
            continue;
        };
        row.feedback_decision = record_str(feedback, "decision");
        row.feedback_facet = record_str(feedback, "facet");
        row.feedback_note = record_str(feedback, "note");
        row.feedback_updated_at = record_str(feedback, "updated_at");
    }
    rows
}

/// `query_conflict_rows`。
pub fn query_conflict_rows(conn: &Connection, limit: i64) -> Result<Vec<ConflictRow>> {
    let mut rows: Vec<ConflictRow> = Vec::new();

    for row in query_story_tension_rows(conn, limit)? {
        if !row.injury_negative.as_deref().unwrap_or("").is_empty()
            && !row.injury_stable.as_deref().unwrap_or("").is_empty()
        {
            let fact_types = vec!["injury_negative".to_string(), "injury_stable".to_string()];
            let support = query_fact_support_summary(conn, &row.story, &row.title, &fact_types)?;
            let (confidence, support_note) = score_fact_confidence(&support);
            if confidence == "low" {
                continue;
            }
            rows.push(ConflictRow {
                category: "injury_state_jump".into(),
                story: row.story.clone(),
                title: row.title.clone(),
                entity_category: row.category.clone(),
                summary: format!(
                    "injury={} -> {}",
                    row.injury_negative.as_deref().unwrap_or(""),
                    row.injury_stable.as_deref().unwrap_or("")
                ),
                evidence_kind: EvidenceKind::Fact,
                fact_types,
                confidence,
                support_note,
                ..Default::default()
            });
        }
        if !row.equipment_damaged.as_deref().unwrap_or("").is_empty()
            && !row.equipment_active.as_deref().unwrap_or("").is_empty()
        {
            let fact_types = vec![
                "equipment_damaged".to_string(),
                "equipment_active".to_string(),
            ];
            let support = query_fact_support_summary(conn, &row.story, &row.title, &fact_types)?;
            let (confidence, support_note) = score_fact_confidence(&support);
            if confidence == "low" {
                continue;
            }
            rows.push(ConflictRow {
                category: "equipment_state_jump".into(),
                story: row.story.clone(),
                title: row.title.clone(),
                entity_category: row.category.clone(),
                summary: format!(
                    "equipment={} -> {}",
                    row.equipment_damaged.as_deref().unwrap_or(""),
                    row.equipment_active.as_deref().unwrap_or("")
                ),
                evidence_kind: EvidenceKind::Fact,
                fact_types,
                confidence,
                support_note,
                ..Default::default()
            });
        }
    }

    for row in query_story_goal_tension_rows(conn, limit)? {
        let fact_types = vec![
            "goal_assigned".to_string(),
            "goal_changed".to_string(),
            "goal_completed".to_string(),
        ];
        let support = query_fact_support_summary(conn, &row.story, &row.title, &fact_types)?;
        let (confidence, support_note) = score_fact_confidence(&support);
        if confidence == "low" {
            continue;
        }
        let mut parts: Vec<String> = Vec::new();
        if !row.goal_assigned.as_deref().unwrap_or("").is_empty() {
            parts.push(format!(
                "assigned={}",
                row.goal_assigned.as_deref().unwrap_or("")
            ));
        }
        if !row.goal_changed.as_deref().unwrap_or("").is_empty() {
            parts.push(format!(
                "changed={}",
                row.goal_changed.as_deref().unwrap_or("")
            ));
        }
        if !row.goal_completed.as_deref().unwrap_or("").is_empty() {
            parts.push(format!(
                "completed={}",
                row.goal_completed.as_deref().unwrap_or("")
            ));
        }
        rows.push(ConflictRow {
            category: "goal_state_drift".into(),
            story: row.story,
            title: row.title,
            entity_category: row.category,
            summary: parts.join(" ; "),
            evidence_kind: EvidenceKind::Fact,
            fact_types,
            confidence,
            support_note,
            ..Default::default()
        });
    }

    for row in query_story_relationship_tension_rows(conn, limit)? {
        let fact_types = vec![
            "relationship_close".to_string(),
            "relationship_distant".to_string(),
        ];
        let support = query_fact_support_summary(conn, &row.story, &row.title, &fact_types)?;
        let (confidence, support_note) = score_fact_confidence(&support);
        if confidence == "low" {
            continue;
        }
        rows.push(ConflictRow {
            category: "relationship_tone_shift".into(),
            story: row.story,
            title: row.title,
            entity_category: row.category,
            summary: format!(
                "close={} ; distant={}",
                row.relationship_close.as_deref().unwrap_or(""),
                row.relationship_distant.as_deref().unwrap_or("")
            ),
            evidence_kind: EvidenceKind::Fact,
            fact_types,
            confidence,
            support_note,
            ..Default::default()
        });
    }

    for row in query_story_alias_drift_rows(conn, limit)? {
        let plan_aliases = row.plan_aliases.clone().unwrap_or_default();
        let draft_aliases = row.draft_aliases.clone().unwrap_or_default();
        let (confidence, support_note) = score_alias_confidence(&plan_aliases, &draft_aliases);
        if confidence == "low" {
            continue;
        }
        rows.push(ConflictRow {
            category: "alias_register_drift".into(),
            story: row.story,
            title: row.title,
            entity_category: row.category,
            summary: format!("plan={} ; draft={}", plan_aliases, draft_aliases),
            evidence_kind: EvidenceKind::Alias,
            fact_types: Vec::new(),
            confidence,
            support_note,
            ..Default::default()
        });
    }

    for row in query_story_alignment_gap_rows(conn, limit)? {
        let mut parts: Vec<String> = Vec::new();
        if let Some(plan_only) = &row.plan_only_entities {
            parts.push(format!("plan_only={plan_only}"));
        }
        if let Some(draft_only) = &row.draft_only_entities {
            parts.push(format!("draft_only={draft_only}"));
        }
        let summary = parts.join(" ; ");
        let (confidence, support_note) = score_alignment_confidence(&summary);
        if confidence == "low" {
            continue;
        }
        rows.push(ConflictRow {
            category: "plan_draft_entity_drift".into(),
            story: row.story,
            title: "-".into(),
            entity_category: "story".into(),
            summary,
            evidence_kind: EvidenceKind::Alignment,
            fact_types: Vec::new(),
            confidence,
            support_note,
            ..Default::default()
        });
    }

    let confidence_rank = |value: &str| -> i64 {
        match value {
            "high" => 0,
            "medium" => 1,
            "low" => 2,
            _ => 9,
        }
    };
    rows.sort_by(|a, b| {
        confidence_rank(&a.confidence)
            .cmp(&confidence_rank(&b.confidence))
            .then_with(|| a.story.cmp(&b.story))
            .then_with(|| a.category.cmp(&b.category))
            .then_with(|| a.title.cmp(&b.title))
    });
    rows.truncate(limit as usize);
    Ok(rows)
}

/// `collect_conflict_rows`。
pub fn collect_conflict_rows(
    conn: &Connection,
    limit: i64,
    feedback_path: Option<&Path>,
) -> Result<Vec<ConflictRow>> {
    let rows = query_conflict_rows(conn, limit)?;
    let Some(feedback_path) = feedback_path else {
        return Ok(rows);
    };
    let entries = load_feedback_entries(feedback_path)?;
    Ok(apply_feedback(rows, &entries))
}

/// `write_feedback` 请求参数。
pub struct FeedbackRequest {
    pub category: String,
    pub story: String,
    pub title: String,
    pub decision: String,
    pub facet: String,
    pub note: String,
    pub summary_contains: Option<String>,
}

/// `write_feedback`：成功返回 `Ok(None)`；校验/匹配失败返回 `Ok(Some(msg))`
/// （Python `raise SystemExit(msg)` → stderr + 退出码 1）。
pub fn write_feedback(
    conn: &Connection,
    feedback_path: &Path,
    req: &FeedbackRequest,
) -> Result<Option<String>> {
    if !FEEDBACK_DECISIONS.contains(&req.decision.as_str()) {
        return Ok(Some(format!("Unknown decision: {}", req.decision)));
    }
    if !req.facet.is_empty() && !FEEDBACK_FACETS.contains(&req.facet.as_str()) {
        return Ok(Some(format!("Unknown facet: {}", req.facet)));
    }
    let rows = query_conflict_rows(conn, 1000)?;
    let matched: Vec<&ConflictRow> = rows
        .iter()
        .filter(|row| {
            row.category == req.category
                && row.story == req.story
                && row.title == req.title
                && req
                    .summary_contains
                    .as_deref()
                    .is_none_or(|needle| row.summary.contains(needle))
        })
        .collect();
    if matched.is_empty() {
        return Ok(Some("No matching conflict row found.".to_string()));
    }
    if matched.len() > 1 {
        return Ok(Some(
            "Multiple conflict rows matched. Add --summary-contains to disambiguate.".to_string(),
        ));
    }
    let row = matched[0];
    let mut entry: FeedbackRecord = JsonMap::new();
    let updated_at = Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true);
    entry.insert("conflict_key".into(), conflict_key(row).into());
    entry.insert("category".into(), req.category.clone().into());
    entry.insert("story".into(), req.story.clone().into());
    entry.insert("title".into(), req.title.clone().into());
    entry.insert("entity_category".into(), row.entity_category.clone().into());
    entry.insert("summary".into(), row.summary.clone().into());
    entry.insert("decision".into(), req.decision.clone().into());
    entry.insert("facet".into(), req.facet.clone().into());
    entry.insert("note".into(), req.note.clone().into());
    entry.insert("updated_at".into(), updated_at.into());
    append_feedback_entry(feedback_path, &entry)?;
    Ok(None)
}

/// `summarize_feedback`。
pub fn summarize_feedback(
    conn: &Connection,
    feedback_path: &Path,
    limit: i64,
    story: Option<&str>,
) -> Result<FeedbackSummary> {
    let feedback_entries = load_feedback_entries(feedback_path)?;
    let feedback_history = read_feedback_history(feedback_path)?;
    let all_conflict_rows = collect_conflict_rows(conn, limit, Some(feedback_path))?;
    let conflict_rows: Vec<ConflictRow> = all_conflict_rows
        .into_iter()
        .filter(|row| match story {
            None => true,
            Some(s) => row.story == s,
        })
        .collect();
    let scoped_history = filter_feedback_history_by_story(&feedback_history, story);

    let mut decision_counter = Ctr::default();
    let mut category_counter = Ctr::default();
    let mut story_counter = Ctr::default();
    let mut facet_counter = Ctr::default();
    let mut unresolved: Vec<ConflictRow> = Vec::new();

    for row in &conflict_rows {
        if !row.feedback_decision.is_empty() {
            decision_counter.add(&row.feedback_decision, 1);
            category_counter.add(&format!("{}::{}", row.category, row.feedback_decision), 1);
            story_counter.add(&format!("{}::{}", row.story, row.feedback_decision), 1);
            if !row.feedback_facet.is_empty() {
                facet_counter.add(
                    &format!("{}::{}", row.feedback_facet, row.feedback_decision),
                    1,
                );
            }
        } else {
            unresolved.push(row.clone());
        }
    }

    let db_root = parent_or_dot(feedback_path);
    let resolved_root = resolve_db_path(&db_root);
    let pending_actions = build_pending_review_actions(&resolved_root, &unresolved, 8);
    let mut unresolved_by_story = Ctr::default();
    for row in &unresolved {
        unresolved_by_story.add(&row.story, 1);
    }
    Ok(FeedbackSummary {
        entries: feedback_entries,
        history: feedback_history,
        conflict_rows,
        decision_counter,
        category_counter,
        story_counter,
        facet_counter,
        unresolved,
        pending_actions,
        backlog: build_feedback_backlog(&scoped_history),
        unresolved_by_story,
        story_filter: story.map(str::to_string),
    })
}

/// `build_story_conflict_snapshot_from_path`（`limit` 缺省 200）。
pub fn build_story_conflict_snapshot_from_path(
    draft_path: &Path,
    limit: i64,
) -> Result<StoryConflictSnapshot> {
    let resolved_path = if draft_path.exists() {
        std::fs::canonicalize(draft_path)?
    } else {
        draft_path.to_path_buf()
    };
    let novel_dir = match find_novel_dir(&resolved_path) {
        Some(dir) => dir,
        None => return Ok(unavailable_snapshot("novel_dir_not_found")),
    };

    let db_path = novel_dir
        .join("research")
        .join("consistency")
        .join("consistency.sqlite3");
    let feedback_path = default_feedback_path_from_db(&db_path);
    if !db_path.exists() {
        return Ok(StoryConflictSnapshot {
            available: false,
            reason: Some("db_not_found".into()),
            novel_dir: Some(novel_dir),
            db_path: Some(db_path),
            feedback_path: Some(feedback_path),
            story: None,
            review_queue_command: None,
            feedback_summary_command: None,
            rows: Vec::new(),
            decision_counter: Ctr::default(),
            category_counter: Ctr::default(),
            facet_counter: Ctr::default(),
            pending_rows: Vec::new(),
            pending_count: 0,
            pending_actions: Vec::new(),
            global_feedback_backlog: Vec::new(),
        });
    }

    let (doc_type, _arc, story, _chapter) = classify_document(&resolved_path, &novel_dir);
    let story = if doc_type != "drafts" { None } else { story };
    let story = match story {
        Some(story) => story,
        None => {
            return Ok(StoryConflictSnapshot {
                available: false,
                reason: Some("story_not_found".into()),
                novel_dir: Some(novel_dir),
                db_path: Some(db_path),
                feedback_path: Some(feedback_path),
                story: None,
                review_queue_command: None,
                feedback_summary_command: None,
                rows: Vec::new(),
                decision_counter: Ctr::default(),
                category_counter: Ctr::default(),
                facet_counter: Ctr::default(),
                pending_rows: Vec::new(),
                pending_count: 0,
                pending_actions: Vec::new(),
                global_feedback_backlog: Vec::new(),
            });
        }
    };

    let conn = open_db(&db_path)?;
    let rows = collect_conflict_rows(&conn, limit, Some(&feedback_path))?;
    let rows: Vec<ConflictRow> = rows.into_iter().filter(|row| row.story == story).collect();
    let feedback_history = read_feedback_history(&feedback_path)?;

    let mut decision_counter = Ctr::default();
    let mut facet_counter = Ctr::default();
    let mut category_counter = Ctr::default();
    let mut pending_rows: Vec<ConflictRow> = Vec::new();
    for row in &rows {
        if !row.feedback_decision.is_empty() {
            decision_counter.add(&row.feedback_decision, 1);
            category_counter.add(&format!("{}::{}", row.category, row.feedback_decision), 1);
            if !row.feedback_facet.is_empty() {
                facet_counter.add(&row.feedback_facet, 1);
            }
        } else {
            pending_rows.push(row.clone());
        }
    }

    let novel_dir_name = novel_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let pending_actions = build_pending_review_actions(&db_path, &pending_rows, 4);
    Ok(StoryConflictSnapshot {
        available: true,
        reason: None,
        novel_dir: Some(novel_dir),
        db_path: Some(db_path),
        feedback_path: Some(feedback_path),
        story: Some(story.clone()),
        review_queue_command: Some(format!(
            "python3 -m consistency review-queue {novel_dir_name} --story {story}",
        )),
        feedback_summary_command: Some(format!(
            "python3 -m consistency feedback-summary {novel_dir_name} --story {story}",
        )),
        rows,
        decision_counter,
        category_counter,
        facet_counter,
        pending_count: pending_rows.len(),
        pending_rows,
        pending_actions,
        global_feedback_backlog: build_feedback_backlog(&feedback_history),
    })
}

/// `build_story_conflict_snapshot`：`build_story_conflict_snapshot_from_path` 的
/// 缺省 `limit=200` 薄封装（供 reports 侧消费）。
pub fn build_story_conflict_snapshot(draft_path: &Path) -> Result<StoryConflictSnapshot> {
    build_story_conflict_snapshot_from_path(draft_path, 200)
}

fn unavailable_snapshot(reason: &str) -> StoryConflictSnapshot {
    StoryConflictSnapshot {
        available: false,
        reason: Some(reason.to_string()),
        novel_dir: None,
        db_path: None,
        feedback_path: None,
        story: None,
        review_queue_command: None,
        feedback_summary_command: None,
        rows: Vec::new(),
        decision_counter: Ctr::default(),
        category_counter: Ctr::default(),
        facet_counter: Ctr::default(),
        pending_rows: Vec::new(),
        pending_count: 0,
        pending_actions: Vec::new(),
        global_feedback_backlog: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// 打印（stdout 逐行对齐 Python）
// ---------------------------------------------------------------------------

/// `print_search_results`。
pub fn print_search_results(conn: &Connection, term: &str, limit: i64) -> Result<()> {
    let sql = "
        SELECT d.path, ps.line_start, ps.line_end, ps.text
        FROM passage_fts f
        JOIN passages ps ON ps.id = f.rowid
        JOIN documents d ON d.id = ps.document_id
        WHERE passage_fts MATCH ?
        LIMIT ?";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map(params![term, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.is_empty() {
        println!("No matches.");
        return Ok(());
    }
    for (path, line_start, line_end, text) in rows {
        println!("{path}:{line_start}-{line_end}");
        println!("{}", normalize_whitespace(&text));
        println!();
    }
    Ok(())
}

/// `print_entity_results`。
pub fn print_entity_results(conn: &Connection, name: &str, limit: i64) -> Result<()> {
    let sql = "
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
        LIMIT ?";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map(params![name, name, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.is_empty() {
        println!("No entity matches.");
        return Ok(());
    }
    let (first_title, first_category, _, _, _, _, _, _) = &rows[0];
    println!("Entity: {first_title} ({first_category})");
    println!();
    for (title, category, matched_name, path, line_start, line_end, text, count) in rows {
        println!("{path}:{line_start}-{line_end} matched=`{matched_name}` count={count}");
        let _ = (title, category);
        println!("{}", normalize_whitespace(&text));
        println!();
    }
    Ok(())
}

/// `print_entity_catalog`。
pub fn print_entity_catalog(conn: &Connection, limit: i64) -> Result<()> {
    let sql = "
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
        LIMIT ?";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map(params![limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (title, category, mentions, names) in rows {
        let names = names.unwrap_or_default();
        println!("{title} [{category}] mentions={mentions} names={names}");
    }
    Ok(())
}

/// `print_entity_facts`。
pub fn print_entity_facts(conn: &Connection, name: &str, limit: i64) -> Result<()> {
    let sql = "
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
        LIMIT ?";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map(params![name, name, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.is_empty() {
        println!("No fact matches.");
        return Ok(());
    }
    let (first_title, first_category, _, _, _, _, _, _, _, _) = &rows[0];
    println!("Entity Facts: {first_title} ({first_category})");
    println!();
    for (title, category, matched_name, path, story, line_start, line_end, text, fact_type, cue) in
        rows
    {
        let story = story.as_deref().unwrap_or("-");
        let _ = (title, category);
        println!(
            "{path}:{line_start}-{line_end} story=`{story}` matched=`{matched_name}` fact=`{fact_type}` cue=`{cue}`"
        );
        println!("{}", normalize_whitespace(&text));
        println!();
    }
    Ok(())
}

/// `print_story_facts`。
pub fn print_story_facts(conn: &Connection, story: &str, limit: i64) -> Result<()> {
    let sql = "
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
        LIMIT ?";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map(params![story, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.is_empty() {
        println!("No story fact rows.");
        return Ok(());
    }
    for (
        title,
        category,
        injury_negative,
        injury_stable,
        equipment_damaged,
        equipment_active,
        goal_assigned,
        goal_changed,
        goal_completed,
        relationship_close,
        relationship_distant,
    ) in rows
    {
        let mut details: Vec<String> = Vec::new();
        let append = |out: &mut Vec<String>, value: Option<&str>, name: &str| {
            if let Some(v) = value {
                if !v.is_empty() {
                    out.push(format!("{name}={v}"));
                }
            }
        };
        append(&mut details, injury_negative.as_deref(), "injury_negative");
        append(&mut details, injury_stable.as_deref(), "injury_stable");
        append(
            &mut details,
            equipment_damaged.as_deref(),
            "equipment_damaged",
        );
        append(
            &mut details,
            equipment_active.as_deref(),
            "equipment_active",
        );
        append(&mut details, goal_assigned.as_deref(), "goal_assigned");
        append(&mut details, goal_changed.as_deref(), "goal_changed");
        append(&mut details, goal_completed.as_deref(), "goal_completed");
        append(
            &mut details,
            relationship_close.as_deref(),
            "relationship_close",
        );
        append(
            &mut details,
            relationship_distant.as_deref(),
            "relationship_distant",
        );
        println!("{title} [{category}] :: {}", details.join(" ; "));
    }
    Ok(())
}

/// `print_story_tension`。
pub fn print_story_tension(conn: &Connection, limit: i64) -> Result<()> {
    let rows = query_story_tension_rows(conn, limit)?;
    if rows.is_empty() {
        println!("No story tension rows.");
        return Ok(());
    }
    for row in &rows {
        let mut details: Vec<String> = Vec::new();
        if !row.injury_negative.as_deref().unwrap_or("").is_empty()
            && !row.injury_stable.as_deref().unwrap_or("").is_empty()
        {
            details.push(format!(
                "injury={} -> {}",
                row.injury_negative.as_deref().unwrap_or(""),
                row.injury_stable.as_deref().unwrap_or("")
            ));
        }
        if !row.equipment_damaged.as_deref().unwrap_or("").is_empty()
            && !row.equipment_active.as_deref().unwrap_or("").is_empty()
        {
            details.push(format!(
                "equipment={} -> {}",
                row.equipment_damaged.as_deref().unwrap_or(""),
                row.equipment_active.as_deref().unwrap_or("")
            ));
        }
        println!(
            "{} :: {} [{}] :: {}",
            row.story,
            row.title,
            row.category,
            details.join(" ; ")
        );
    }
    Ok(())
}

/// `print_conflict_evidence`。
pub fn print_conflict_evidence(conn: &Connection, row: &ConflictRow, indent: &str) -> Result<()> {
    match row.evidence_kind {
        EvidenceKind::Fact if row.title != "-" => {
            let fact_types: Option<Vec<String>> =
                (!row.fact_types.is_empty()).then(|| row.fact_types.clone());
            let evidence_rows = query_story_tension_evidence(
                conn,
                &row.story,
                &row.title,
                4,
                fact_types.as_deref(),
            )?;
            for evidence in &evidence_rows {
                println!(
                    "{indent}{}:{}-{} fact=`{}` cue=`{}`",
                    evidence.path,
                    evidence.line_start,
                    evidence.line_end,
                    evidence.fact_type,
                    evidence.cue
                );
                println!("{indent}  {}", normalize_whitespace(&evidence.text));
            }
        }
        EvidenceKind::Alias => {
            for side in ["plan", "draft"] {
                println!("{indent}{side}:");
                let evidence = query_story_alias_evidence(conn, &row.story, &row.title, side, 2)?;
                for e in &evidence {
                    println!(
                        "{indent}  {}:{}-{} matched=`{}`",
                        e.path, e.line_start, e.line_end, e.matched_name
                    );
                    println!("{indent}    {}", normalize_whitespace(&e.text));
                }
            }
        }
        EvidenceKind::Alignment => {
            let summary = row.summary.clone();
            if let Some(plan_match) = ALIGN_PLAN_ONLY_RE.captures(&summary) {
                let titles = split_pipe_values(plan_match.get(1).map(|m| m.as_str()));
                for entity_title in titles.into_iter().take(3) {
                    println!("{indent}plan_only `{entity_title}`");
                    let evidence =
                        query_story_alignment_evidence(conn, &row.story, &entity_title, "plan", 2)?;
                    for e in &evidence {
                        println!("{indent}  {}:{}-{}", e.path, e.line_start, e.line_end);
                        println!("{indent}    {}", normalize_whitespace(&e.text));
                    }
                }
            }
            if let Some(draft_match) = ALIGN_DRAFT_ONLY_RE.captures(&summary) {
                let titles = split_pipe_values(draft_match.get(1).map(|m| m.as_str()));
                for entity_title in titles.into_iter().take(3) {
                    println!("{indent}draft_only `{entity_title}`");
                    let evidence = query_story_alignment_evidence(
                        conn,
                        &row.story,
                        &entity_title,
                        "draft",
                        2,
                    )?;
                    for e in &evidence {
                        println!("{indent}  {}:{}-{}", e.path, e.line_start, e.line_end);
                        println!("{indent}    {}", normalize_whitespace(&e.text));
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// `print_conflicts`。
pub fn print_conflicts(conn: &Connection, limit: i64, feedback_path: Option<&Path>) -> Result<()> {
    let conflict_rows = collect_conflict_rows(conn, limit, feedback_path)?;
    if conflict_rows.is_empty() {
        println!("No conflict candidates.");
        return Ok(());
    }
    let mut grouped: BTreeMap<String, Vec<ConflictRow>> = BTreeMap::new();
    for row in conflict_rows {
        grouped.entry(row.category.clone()).or_default().push(row);
    }
    for (category, rows) in grouped {
        println!("## {category}");
        for row in &rows {
            println!(
                "{} :: {} [{}] :: confidence={} support={} :: {}",
                row.story,
                row.title,
                row.entity_category,
                row.confidence,
                row.support_note,
                row.summary
            );
            if !row.feedback_decision.is_empty() {
                let mut extra = format!(" feedback={}", row.feedback_decision);
                if !row.feedback_facet.is_empty() {
                    extra.push_str(&format!(" facet={}", row.feedback_facet));
                }
                if !row.feedback_note.is_empty() {
                    extra.push_str(&format!(" note={}", row.feedback_note));
                }
                println!("  -{extra}");
            }
            print_conflict_evidence(conn, row, "  - ")?;
        }
        println!();
    }
    Ok(())
}

/// `print_review_queue`。
pub fn print_review_queue(
    conn: &Connection,
    feedback_path: &Path,
    story: Option<&str>,
    limit: i64,
) -> Result<()> {
    let summary = summarize_feedback(conn, feedback_path, limit * 4, None)?;
    let unresolved: Vec<&ConflictRow> = summary
        .unresolved
        .iter()
        .filter(|row| story.is_none_or(|s| row.story == s))
        .collect();
    if unresolved.is_empty() {
        println!("No pending review rows.");
        return Ok(());
    }
    println!("## Review Queue");
    match story {
        Some(story) => println!("- story: `{story}`"),
        None => println!("- story: `all`"),
    }
    println!("- feedback_log: `{}`", feedback_path.display());
    println!("- pending_total: `{}`", unresolved.len());
    println!();
    let db_root = parent_or_dot(feedback_path);
    let resolved_db = resolve_db_path(&db_root);
    for row in unresolved.into_iter().take(limit as usize) {
        let command = build_feedback_command(&resolved_db, row, None);
        println!(
            "### {} :: {} :: {} :: confidence={}",
            row.story, row.category, row.title, row.confidence
        );
        println!("- focus: {}", build_pending_review_focus(row));
        println!("- summary: {}", row.summary);
        println!("- command: `{command}`");
        println!("- evidence:");
        print_conflict_evidence(conn, row, "  - ")?;
        println!();
    }
    Ok(())
}

/// `print_feedback_summary`。
pub fn print_feedback_summary(
    conn: &Connection,
    feedback_path: &Path,
    limit: i64,
    story: Option<&str>,
) -> Result<()> {
    let summary = summarize_feedback(conn, feedback_path, limit, story)?;
    let entries = summary.entries;
    let decision_counter = summary.decision_counter;
    let category_counter = summary.category_counter;
    let story_counter = summary.story_counter;
    let facet_counter = summary.facet_counter;
    let unresolved = summary.unresolved;
    let pending_actions = summary.pending_actions;
    let backlog = summary.backlog;
    let _ = summary;

    if entries.is_empty() {
        println!("No feedback entries yet.");
        println!();
    }
    if let Some(story) = &summary.story_filter {
        println!("## Story Filter");
        println!("- story: `{story}`");
        println!();
    }
    println!("## Feedback Decisions");
    for decision in FEEDBACK_DECISIONS {
        println!("- {decision}: {}", decision_counter.get(decision));
    }
    println!();
    println!("## By Category");
    if category_counter.is_empty() {
        println!("- 无");
    } else {
        for (name, count) in category_counter.most_common(12) {
            println!("- {name} x{count}");
        }
    }
    println!();
    println!("## By Facet");
    if facet_counter.is_empty() {
        println!("- 无");
    } else {
        for (name, count) in facet_counter.most_common(12) {
            println!("- {name} x{count}");
        }
    }
    println!();
    println!("## Deposition Suggestions");
    if backlog.is_empty() {
        println!("- 无");
    } else {
        for item in &backlog {
            println!("- `{}` {}", item.target, item.reason);
        }
    }
    println!();
    println!("## By Story");
    if story_counter.is_empty() {
        println!("- 无");
    } else {
        for (name, count) in story_counter.most_common(12) {
            println!("- {name} x{count}");
        }
    }
    println!();
    println!("## Pending Review");
    if unresolved.is_empty() {
        println!("- 无");
    } else {
        for row in unresolved.iter().take(12) {
            println!(
                "- {} :: {} :: {} :: confidence={}",
                row.story, row.category, row.title, row.confidence
            );
        }
    }
    println!();
    println!("## Pending Review Actions");
    if pending_actions.is_empty() {
        println!("- 无");
    } else {
        for item in &pending_actions {
            println!(
                "- {} :: {} :: {} :: confidence={} :: {}",
                item.story, item.category, item.title, item.confidence, item.focus
            );
            println!("  {}", item.command);
        }
    }
    println!();
    Ok(())
}

/// `print_suspects`。
pub fn print_suspects(conn: &Connection, limit: i64) -> Result<()> {
    let sql_alias = "
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
        LIMIT ?";
    let mut stmt = conn.prepare(sql_alias)?;
    let alias_rows = stmt
        .query_map(params![limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let sql_draft = "
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
        LIMIT ?";
    let mut stmt = conn.prepare(sql_draft)?;
    let draft_rows = stmt
        .query_map(params![limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let sql_plan_only = "
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
        LIMIT ?";
    let mut stmt = conn.prepare(sql_plan_only)?;
    let plan_only_rows = stmt
        .query_map(params![limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let sql_draft_only = "
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
        LIMIT ?";
    let mut stmt = conn.prepare(sql_draft_only)?;
    let draft_only_rows = stmt
        .query_map(params![limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let fact_rows = query_story_tension_rows(conn, limit)?;
    let story_alignment_rows = query_story_alignment_gap_rows(conn, limit)?;

    if alias_rows.is_empty()
        && draft_rows.is_empty()
        && plan_only_rows.is_empty()
        && draft_only_rows.is_empty()
        && fact_rows.is_empty()
        && story_alignment_rows.is_empty()
    {
        println!("No suspects.");
        return Ok(());
    }

    if !alias_rows.is_empty() {
        println!("## Same Document Alias Mixing");
        for (title, category, path, aliases) in &alias_rows {
            println!(
                "{path} :: {title} [{category}] aliases={}",
                aliases.as_deref().unwrap_or("")
            );
        }
        println!();
    }
    if !draft_rows.is_empty() {
        println!("## Draft vs Upstream Alias Drift");
        for (title, category, draft_aliases, upstream_aliases) in &draft_rows {
            println!(
                "{title} [{category}] draft={} upstream={}",
                draft_aliases.as_deref().unwrap_or(""),
                upstream_aliases.as_deref().unwrap_or("")
            );
        }
        println!();
    }
    if !plan_only_rows.is_empty() {
        println!("## Plan Mentioned But Draft Missing");
        for (title, category, plan_docs, draft_docs, plan_paths) in &plan_only_rows {
            let paths = plan_paths.as_deref().unwrap_or("");
            let preview = split_pipe_values_str(&paths.replace(',', "|"))
                .into_iter()
                .take(3)
                .collect::<Vec<_>>()
                .join(" | ");
            println!(
                "{title} [{category}] plan_docs={plan_docs} draft_docs={draft_docs} paths={preview}"
            );
        }
        println!();
    }
    if !draft_only_rows.is_empty() {
        println!("## Draft Mentioned But Plan Missing");
        for (title, category, draft_docs, plan_docs, draft_paths) in &draft_only_rows {
            let paths = draft_paths.as_deref().unwrap_or("");
            let preview = split_pipe_values_str(&paths.replace(',', "|"))
                .into_iter()
                .take(3)
                .collect::<Vec<_>>()
                .join(" | ");
            println!(
                "{title} [{category}] draft_docs={draft_docs} plan_docs={plan_docs} paths={preview}"
            );
        }
        println!();
    }
    if !fact_rows.is_empty() {
        println!("## Draft State Tension Candidates");
        for row in &fact_rows {
            let mut parts: Vec<String> = Vec::new();
            if !row.injury_negative.as_deref().unwrap_or("").is_empty()
                && !row.injury_stable.as_deref().unwrap_or("").is_empty()
            {
                parts.push(format!(
                    "injury={} -> {}",
                    row.injury_negative.as_deref().unwrap_or(""),
                    row.injury_stable.as_deref().unwrap_or("")
                ));
            }
            if !row.equipment_damaged.as_deref().unwrap_or("").is_empty()
                && !row.equipment_active.as_deref().unwrap_or("").is_empty()
            {
                parts.push(format!(
                    "equipment={} -> {}",
                    row.equipment_damaged.as_deref().unwrap_or(""),
                    row.equipment_active.as_deref().unwrap_or("")
                ));
            }
            println!(
                "{} :: {} [{}] {}",
                row.story,
                row.title,
                row.category,
                parts.join(" ; ")
            );
        }
        println!();
    }
    if !story_alignment_rows.is_empty() {
        println!("## Story Plan / Draft Entity Drift");
        for row in &story_alignment_rows {
            let mut details: Vec<String> = Vec::new();
            if let Some(v) = &row.plan_only_entities {
                details.push(format!("plan_only={v}"));
            }
            if let Some(v) = &row.draft_only_entities {
                details.push(format!("draft_only={v}"));
            }
            println!("{} :: {}", row.story, details.join(" ; "));
        }
        println!();
    }
    Ok(())
}

/// `print_story_alignment`。
pub fn print_story_alignment(conn: &Connection, limit: i64) -> Result<()> {
    let rows = query_story_alignment_rows(conn, limit)?;
    if rows.is_empty() {
        println!("No story alignment rows.");
        return Ok(());
    }
    for row in &rows {
        let mut details = vec![
            format!("plan_entities={}", row.plan_entities),
            format!("draft_entities={}", row.draft_entities),
        ];
        if let Some(v) = &row.plan_only_entities {
            details.push(format!("plan_only={v}"));
        }
        if let Some(v) = &row.draft_only_entities {
            details.push(format!("draft_only={v}"));
        }
        println!("{} :: {}", row.story, details.join(" ; "));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// CLI（clap 子命令面，参数名/默认值与 Python argparse 一致）
// ---------------------------------------------------------------------------

use clap::Subcommand;

/// `consistency` 子命令（与 Python argparse 的 13 个子命令一一对应）。
#[derive(Subcommand)]
pub enum ConsistencyCmd {
    /// 为小说目录构建索引
    Build {
        /// 小说目录（例如 novel1）
        novel_dir: PathBuf,
        /// 输出 SQLite 路径；缺省为 <novel_dir>/research/consistency/consistency.sqlite3
        #[arg(long)]
        db_path: Option<PathBuf>,
    },
    /// 全文检索已索引段落
    Search {
        /// SQLite 数据库路径、consistency 目录或 novel 目录
        db_path: PathBuf,
        /// FTS 检索词或短语
        term: String,
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    /// 查看已知实体的出现段落
    Entity {
        db_path: PathBuf,
        /// 实体标题或别名
        name: String,
        #[arg(long, default_value_t = 30)]
        limit: i64,
    },
    /// 查看已知实体的事实候选
    Facts {
        db_path: PathBuf,
        name: String,
        #[arg(long, default_value_t = 30)]
        limit: i64,
    },
    /// 查看单条 story 的聚合事实候选
    StoryFacts {
        db_path: PathBuf,
        /// story id（例如 story1 或 interlude1）
        story: String,
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// 查看 story 级状态张力候选
    Tension {
        db_path: PathBuf,
        #[arg(long, default_value_t = 100)]
        limit: i64,
    },
    /// 查看冲突候选与证据
    Conflicts {
        db_path: PathBuf,
        #[arg(long, default_value_t = 50)]
        limit: i64,
        /// 反馈 JSONL 路径；缺省为 <db 目录>/review-feedback.jsonl
        #[arg(long)]
        feedback_path: Option<PathBuf>,
    },
    /// 记录某条冲突的人工复核决定
    FeedbackAdd {
        db_path: PathBuf,
        #[arg(long, required = true)]
        category: String,
        #[arg(long, required = true)]
        story: String,
        /// 实体标题；story 级漂移用 -
        #[arg(long, required = true)]
        title: String,
        /// confirmed / false_positive / designed_keep / watch
        #[arg(long, required = true)]
        decision: String,
        /// 可选切面（rhythm / voice / motif / scene_callback / register / naming / irony /
        /// state_progression / extractor_noise / scope_drift）
        #[arg(long, default_value = "")]
        facet: String,
        /// 简短人工备注
        #[arg(long, default_value = "")]
        note: String,
        /// 用于区分同组行的 summary 片段
        #[arg(long)]
        summary_contains: Option<String>,
        /// 反馈 JSONL 路径；缺省为 <db 目录>/review-feedback.jsonl
        #[arg(long)]
        feedback_path: Option<PathBuf>,
    },
    /// 汇总已记录的反馈
    FeedbackSummary {
        db_path: PathBuf,
        #[arg(long, default_value_t = 200)]
        limit: i64,
        /// 可选 story id（例如 story3）
        #[arg(long)]
        story: Option<String>,
        #[arg(long)]
        feedback_path: Option<PathBuf>,
    },
    /// 打印待复核队列（含证据）
    ReviewQueue {
        db_path: PathBuf,
        #[arg(long)]
        story: Option<String>,
        #[arg(long, default_value_t = 8)]
        limit: i64,
        #[arg(long)]
        feedback_path: Option<PathBuf>,
    },
    /// 列出已索引实体与提及数
    Catalog {
        db_path: PathBuf,
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// 打印疑似称呼混用 / 口径漂移
    Suspects {
        db_path: PathBuf,
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// 打印 story 级 plan/draft 实体覆盖
    Alignment {
        db_path: PathBuf,
        #[arg(long, default_value_t = 100)]
        limit: i64,
    },
}

fn resolve_feedback_path(db_path: &Path, explicit: Option<&Path>) -> PathBuf {
    explicit
        .map(PathBuf::from)
        .unwrap_or_else(|| default_feedback_path_from_db(&resolve_db_path(db_path)))
}

/// 子命令分发（对应 Python `main()`；返回进程退出码）。
pub fn run(cmd: &ConsistencyCmd) -> Result<i32> {
    use ConsistencyCmd::*;
    match cmd {
        Build { novel_dir, db_path } => {
            let db_path = db_path.clone().unwrap_or_else(|| {
                novel_dir
                    .join("research")
                    .join("consistency")
                    .join("consistency.sqlite3")
            });
            build_index(novel_dir, &db_path)?;
            println!("{}", db_path.display());
            Ok(0)
        }
        Search {
            db_path,
            term,
            limit,
        } => run_db_query(db_path, |conn| print_search_results(conn, term, *limit)),
        Entity {
            db_path,
            name,
            limit,
        } => run_db_query(db_path, |conn| print_entity_results(conn, name, *limit)),
        Facts {
            db_path,
            name,
            limit,
        } => run_db_query(db_path, |conn| print_entity_facts(conn, name, *limit)),
        StoryFacts {
            db_path,
            story,
            limit,
        } => run_db_query(db_path, |conn| print_story_facts(conn, story, *limit)),
        Catalog { db_path, limit } => {
            run_db_query(db_path, |conn| print_entity_catalog(conn, *limit))
        }
        Suspects { db_path, limit } => run_db_query(db_path, |conn| print_suspects(conn, *limit)),
        Alignment { db_path, limit } => {
            run_db_query(db_path, |conn| print_story_alignment(conn, *limit))
        }
        Tension { db_path, limit } => {
            run_db_query(db_path, |conn| print_story_tension(conn, *limit))
        }
        Conflicts {
            db_path,
            limit,
            feedback_path,
        } => run_db_query(db_path, |conn| {
            let feedback = resolve_feedback_path(db_path, feedback_path.as_deref());
            print_conflicts(conn, *limit, Some(&feedback))
        }),
        FeedbackAdd {
            db_path,
            category,
            story,
            title,
            decision,
            facet,
            note,
            summary_contains,
            feedback_path,
        } => {
            let feedback = resolve_feedback_path(db_path, feedback_path.as_deref());
            let req = FeedbackRequest {
                category: category.clone(),
                story: story.clone(),
                title: title.clone(),
                decision: decision.clone(),
                facet: facet.clone(),
                note: note.clone(),
                summary_contains: summary_contains.clone(),
            };
            let mut rc = 0i32;
            run_db_query(db_path, |conn| {
                match write_feedback(conn, &feedback, &req)? {
                    None => {
                        println!("{}", feedback.display());
                        println!("{category} :: {story} :: {title} :: decision={decision}");
                    }
                    Some(message) => {
                        eprintln!("{message}");
                        rc = 1;
                    }
                }
                Ok(())
            })?;
            Ok(rc)
        }
        FeedbackSummary {
            db_path,
            limit,
            story,
            feedback_path,
        } => run_db_query(db_path, |conn| {
            let feedback = resolve_feedback_path(db_path, feedback_path.as_deref());
            print_feedback_summary(conn, &feedback, *limit, story.as_deref())
        }),
        ReviewQueue {
            db_path,
            story,
            limit,
            feedback_path,
        } => run_db_query(db_path, |conn| {
            let feedback = resolve_feedback_path(db_path, feedback_path.as_deref());
            print_review_queue(conn, &feedback, story.as_deref(), *limit)
        }),
    }
}

fn run_db_query(db_path: &Path, query: impl FnOnce(&Connection) -> Result<()>) -> Result<i32> {
    let resolved = resolve_db_path(db_path);
    let conn = open_db(&resolved)?;
    query(&conn)?;
    Ok(0)
}
