//! `audit.draft` 完整分析移植：Python `analyze_text` 返回的**全部**顶层节。
//!
//! - 9 个规则指标节（tokens/patterns/phrases/modifiers/punctuation/
//!   punctuation_combos/custom_templates/tracked_terms/tracked_term_categories）
//!   复用 [`crate::rules`] 既有实现；
//! - 其余结构分析（summary、warned、句长/对白/场面/语料/疲劳/提醒等）在本模块实现，
//!   语义逐条对齐 `src/audit/draft.py`（含 `Counter.most_common` 平手首现序、
//!   `round(x, n)` 半偶舍入、码点计数、`min/max` 平手取首等语义坑）；
//! - text/markdown 渲染函数已移植（对齐 Python `format_text_report` / `format_markdown_report` / `_render_report`）。
//!
//! JSON 键名与 Python 逐字一致；浮点值经 [`crate::rules::round2`]（2 位）
//! 与 [`round4f`]（4 位）对齐 CPython `round`。

use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fancy_regex::Regex;
use serde::Serialize;

use crate::config::ReviewRules;
use crate::config::{TemplateRule, TrackedTerm};
use crate::input::{resolve_inputs, write_json_line, write_text};
use crate::rules::{
    build_custom_template_metrics, build_rule_metrics, build_template_bank,
    build_tracked_term_metrics, density, round2, CompiledRule, CustomTemplateMetric, Hit,
    RegexMetric,
};
use crate::text::{prose_char_count, quote_ratio, TextSplitter};

// ---------------------------------------------------------------------------
// 常量与共享正则（对齐 draft.py 模块级编译）

/// 重叠词（AA/BB/AABB）正则，对齐 `collect_aa_bb_patterns` 内联正则。
const AA_BB_RE: &str = r"(?:一([\u{4e00}-\u{9fff}])\1|([\u{4e00}-\u{9fff}])\2([\u{4e00}-\u{9fff}])\3|([\u{4e00}-\u{9fff}]{2})\4)";
/// 把字操作片段正则（`FATIGUE_WINDOW_BA_RE`）。
const BA_RE: &str = r"把[^，。！？!?]{1,24}";
/// 分句切分（`CLAUSE_SPLIT`）。
const CLAUSE_SPLIT_RE: &str = r"[，；：]";
/// ngram 清洗：只保留 CJK 与 ASCII 字母（`re.sub(r"[^\u4e00-\u9fffA-Za-z]", "", text)`）。
const NGRAM_KEEP_RE: &str = r"[^一-龥A-Za-z]";
/// 语料正文清洗：列表符前缀（`re.sub(r"^[-*]\s*", "", s)`）。
const CORPUS_BULLET_RE: &str = r"^[-*]\s*";
/// 语料正文清洗：反引号包裹（`re.sub(r"`([^`]+)`", r"\1", s)`）。
const CORPUS_BACKTICK_RE: &str = r"`([^`]+)`";
/// 语料正文清洗：粗体包裹（`re.sub(r"\*\*([^*]+)\*\*", r"\1", s)`）。
const CORPUS_BOLD_RE: &str = r"\*\*([^*]+)\*\*";
/// 语料清洗的分隔符整行（`re.fullmatch(r"[-:| ]+", s)`）。
const CORPUS_TABLE_LINE_RE: &str = r"[-:| ]+";
/// 纯 ASCII 数字字母（`re.fullmatch(r"[A-Za-z0-9]+", s)`）。
const ALPHA_NUMERIC_RE: &str = r"[A-Za-z0-9]+";
/// ngram 字母判定（`phrase.isascii() and phrase.isalpha()`）。
const ASCII_ALPHA_RE: &str = r"[A-Za-z]+";

/// Python 语义：`collections.Counter`（计数 + 首现序；`most_common` 平手按首现序）。
#[derive(Debug, Clone, Default)]
pub struct Counter {
    /// 按 key 首现序排列。
    entries: Vec<(String, usize)>,
    index: HashMap<String, usize>,
}

/// 计数器 JSON 对象：按首现序序列化为 JSON object，对齐 Python `dict(counter)` 形状。
#[derive(Debug, Clone, Default)]
pub struct CountMap(Vec<(String, usize)>);

impl CountMap {
    #[must_use]
    pub fn new(entries: Vec<(String, usize)>) -> Self {
        Self(entries)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, (String, usize)> {
        self.0.iter()
    }
}

impl Serialize for CountMap {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, count) in &self.0 {
            map.serialize_entry(key, count)?;
        }
        map.end()
    }
}

impl Counter {
    /// 新增一次计数；返回该 key 的累计计数。
    pub fn add(&mut self, key: &str) -> usize {
        match self.index.get(key) {
            Some(&i) => {
                self.entries[i].1 += 1;
                self.entries[i].1
            }
            None => {
                self.index.insert(key.to_string(), self.entries.len());
                self.entries.push((key.to_string(), 1));
                1
            }
        }
    }

    /// 新增 `n` 次计数（对齐 Python `counter[key] += n`）；返回该 key 的累计计数。
    pub fn add_n(&mut self, key: &str, n: usize) -> usize {
        match self.index.get(key) {
            Some(&i) => {
                self.entries[i].1 += n;
                self.entries[i].1
            }
            None => {
                self.index.insert(key.to_string(), self.entries.len());
                self.entries.push((key.to_string(), n));
                n
            }
        }
    }

    /// 取累计计数（未出现过为 0）。
    pub fn get(&self, key: &str) -> usize {
        self.index.get(key).map_or(0, |&i| self.entries[i].1)
    }

    /// 按首现序遍历 (key, count)。
    pub fn entries(&self) -> &[(String, usize)] {
        &self.entries
    }

    /// 是否出现过任何 key。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 总计数。
    pub fn total(&self) -> usize {
        self.entries.iter().map(|e| e.1).sum()
    }

    /// `Counter.most_common()`：计数降序；平手保持首现序（稳定排序）。
    pub fn most_common_all(&self) -> Vec<(String, usize)> {
        let mut v = self.entries.clone();
        v.sort_by_key(|x| std::cmp::Reverse(x.1));
        v
    }

    /// `Counter.most_common(n)`。
    pub fn most_common(&self, n: usize) -> Vec<(String, usize)> {
        let mut v = self.entries.clone();
        v.sort_by_key(|x| std::cmp::Reverse(x.1));
        v.truncate(n);
        v
    }
}

// ---------------------------------------------------------------------------
// Python 语义辅助

/// `round(x, n)`：CPython 对二进制精确值做半偶舍入（对齐既有 [`round2`]，推广到 n 位）。
#[must_use]
pub fn round_nd(x: f64, n: u32) -> f64 {
    if x == 0.0 {
        return 0.0;
    }
    let sign = x < 0.0;
    let abs = x.abs();
    let bits = abs.to_bits();
    let mantissa = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
    let exponent = ((bits >> 52) & 0x7ff) as i32 - 1075;
    let scale: u128 = 10u128.pow(n);
    let scaled = (mantissa as u128) * scale; // x * 10^n = scaled * 2^exponent
    let nearest: u128 = if exponent >= 0 {
        scaled << exponent
    } else {
        let shift = (-exponent) as u32;
        if shift >= 128 {
            0
        } else {
            let low = scaled & ((1u128 << shift) - 1);
            let half = 1u128 << (shift - 1);
            let mut quotient = scaled >> shift;
            if low > half || (low == half && quotient & 1 == 1) {
                quotient += 1;
            }
            quotient
        }
    };
    let v = (nearest as f64) / scale as f64;
    if sign {
        -v
    } else {
        v
    }
}

/// `round(x, 4)`。
#[must_use]
pub fn round4f(x: f64) -> f64 {
    round_nd(x, 4)
}

/// `round(x)`（0 位）：CPython 半偶取整。
#[must_use]
pub fn bankers_round_int(x: f64) -> i64 {
    if x == 0.0 {
        return 0;
    }
    let sign = x < 0.0;
    let abs = x.abs();
    let bits = abs.to_bits();
    let mantissa = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
    let exponent = ((bits >> 52) & 0x7ff) as i32 - 1075;
    let nearest: u128 = if exponent >= 0 {
        mantissa as u128 >> exponent
    } else {
        let shift = (-exponent) as u32;
        if shift >= 128 {
            0
        } else {
            let scaled = mantissa as u128;
            let low = scaled & ((1u128 << shift) - 1);
            let half = 1u128 << (shift - 1);
            let mut q = scaled >> shift;
            if low > half || (low == half && q & 1 == 1) {
                q += 1;
            }
            q
        }
    };
    if sign {
        -(nearest as i64)
    } else {
        nearest as i64
    }
}

/// Python `str(float)` 的 f-string 语义：shortest roundtrip，整数值带 `.0`
/// （`str(12.0)` == `"12.0"`，Rust `{}` 是 `"12"`）。
#[must_use]
pub fn py_float_str(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

/// 末尾 n 个码点切片（Python `text[-n:]`）。
fn tail_chars(text: &str, n: usize) -> &str {
    let total = text.chars().count();
    if total <= n {
        return text;
    }
    let skip = total - n;
    let start = text
        .char_indices()
        .nth(skip)
        .map(|(b, _)| b)
        .unwrap_or(text.len());
    &text[start..]
}

/// 码点长度（Python `len(str)`）。
fn code_len(s: &str) -> usize {
    s.chars().count()
}

/// 码点前缀（Python `s[:n]`）。
fn prefix_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// 首尾剥离字符集（Python `s.strip(chars)` / `lstrip(chars)`）。
fn strip_chars<'a>(s: &'a str, set: &str) -> &'a str {
    s.trim_matches(|c: char| set.chars().any(|x| x == c))
}
fn lstrip_chars<'a>(s: &'a str, set: &str) -> &'a str {
    s.trim_start_matches(|c: char| set.chars().any(|x| x == c))
}

/// `LEADING_PUNCT` 词表常量（对齐 draft.py）。
const LEADING_PUNCT: &str = "“”\"'【】《》〈〉（）()[]「」『』，,：:；;、 ";

// ---------------------------------------------------------------------------
// 分析上下文（编译一次，逐篇复用）

/// 一次分析会话所需的配置与编译正则（对应 Python 模块级常量）。
pub struct DraftContext {
    rules: ReviewRules,
    splitter: TextSplitter,
    token_rules: Vec<CompiledRule>,
    pattern_rules: Vec<CompiledRule>,
    phrase_rules: Vec<CompiledRule>,
    modifier_rules: Vec<CompiledRule>,
    punctuation_rules: Vec<CompiledRule>,
    combo_rules: Vec<CompiledRule>,
    speaker_patterns: Vec<Regex>,
    speaker_line_patterns: Vec<Regex>,
    connective_patterns: Vec<(String, Regex)>,
    aa_bb_regex: Regex,
    ba_regex: Regex,
    clause_split_regex: Regex,
    ngram_keep_regex: Regex,
    corpus_bullet_regex: Regex,
    corpus_backtick_regex: Regex,
    corpus_bold_regex: Regex,
    corpus_table_line_regex: Regex,
    corpus_markdown_noise_regex: Regex,
    alpha_numeric_regex: Regex,
    ascii_alpha_regex: Regex,
}

fn compile_rules(list: &[crate::config::RegexRule]) -> Result<Vec<CompiledRule>> {
    list.iter().map(CompiledRule::compile).collect()
}

impl DraftContext {
    /// 构建上下文：编译全部规则与共享正则（正则非法时报错）。
    pub fn new(rules: ReviewRules) -> Result<Self> {
        let draft = &rules.draft;
        let splitter = TextSplitter::new(&draft.markdown_noise_line.pattern)?;
        let speaker_patterns: Vec<Regex> = draft
            .speaker
            .patterns
            .iter()
            .map(|p| Regex::new(p).with_context(|| format!("speaker pattern {p:?} 无法编译")))
            .collect::<Result<Vec<_>>>()?;
        let speaker_line_patterns: Vec<Regex> = draft
            .speaker
            .line_patterns
            .iter()
            .map(|p| Regex::new(p).with_context(|| format!("speaker line pattern {p:?} 无法编译")))
            .collect::<Result<Vec<_>>>()?;
        let connective_patterns: Vec<(String, Regex)> = draft
            .connective_sentence_patterns
            .iter()
            .map(|c| {
                let regex = Regex::new(&c.pattern)
                    .with_context(|| format!("连接词句首 {:?} 无法编译", c.label))?;
                Ok((c.label.clone(), regex))
            })
            .collect::<Result<Vec<_>>>()?;
        let token_rules = compile_rules(&draft.regex_rules.tokens)?;
        let pattern_rules = compile_rules(&draft.regex_rules.patterns)?;
        let phrase_rules = compile_rules(&draft.regex_rules.phrases)?;
        let modifier_rules = compile_rules(&draft.regex_rules.modifiers)?;
        let punctuation_rules = compile_rules(&draft.regex_rules.punctuation)?;
        let combo_rules = compile_rules(&draft.regex_rules.punctuation_combos)?;
        let noise_pattern = draft.markdown_noise_line.pattern.clone();
        Ok(Self {
            rules,
            splitter,
            token_rules,
            pattern_rules,
            phrase_rules,
            modifier_rules,
            punctuation_rules,
            combo_rules,
            speaker_patterns,
            speaker_line_patterns,
            connective_patterns,
            aa_bb_regex: Regex::new(AA_BB_RE).expect("AA/BB 正则应可编译"),
            ba_regex: Regex::new(BA_RE).expect("把字句正则应可编译"),
            clause_split_regex: Regex::new(CLAUSE_SPLIT_RE).expect("分句切分正则应可编译"),
            ngram_keep_regex: Regex::new(NGRAM_KEEP_RE).expect("ngram 清洗正则应可编译"),
            corpus_bullet_regex: Regex::new(CORPUS_BULLET_RE).expect("列表符正则应可编译"),
            corpus_backtick_regex: Regex::new(CORPUS_BACKTICK_RE).expect("反引号正则应可编译"),
            corpus_bold_regex: Regex::new(CORPUS_BOLD_RE).expect("粗体正则应可编译"),
            corpus_table_line_regex: Regex::new(CORPUS_TABLE_LINE_RE)
                .expect("表格分隔行正则应可编译"),
            corpus_markdown_noise_regex: Regex::new(&noise_pattern)
                .expect("markdown 噪音行正则应可编译"),
            alpha_numeric_regex: Regex::new(ALPHA_NUMERIC_RE).expect("ASCII 正则应可编译"),
            ascii_alpha_regex: Regex::new(ASCII_ALPHA_RE).expect("ASCII 字母正则应可编译"),
        })
    }

    /// 草稿规则节（模板库/跟踪词等；stats 子命令需要）。
    pub fn draft_rules(&self) -> &crate::config::DraftConfig {
        &self.rules.draft
    }

    /// 阈值配置。
    pub fn thresholds(&self) -> &crate::config::DraftThresholds {
        &self.rules.draft.thresholds
    }

    /// 语料学习窗口配置。
    pub fn learned_window(&self) -> &crate::config::LearnedTermWindow {
        &self.rules.draft.learned_term_window
    }

    /// 词库。
    pub fn lexicon(&self) -> &crate::config::Lexicon {
        &self.rules.draft.lexicon
    }

    /// 拆分器。
    pub fn splitter(&self) -> &TextSplitter {
        &self.splitter
    }

    /// 语料学习是否被禁用的开关由 CLI 层处理；这里提供 default corpus parts。
    pub fn default_corpus_parts(&self) -> &[Vec<String>] {
        &self.rules.draft.thresholds.default_corpus_parts
    }

    /// `--learn-from` 缺省时的语料发现（对齐 `corpus_paths_for_targets`）。
    pub fn corpus_paths_for_targets(&self, targets: &[PathBuf]) -> Vec<PathBuf> {
        let mut roots: Vec<(String, PathBuf)> = Vec::new();
        for path in targets {
            let parts: Vec<String> = path
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            for marker in [
                "drafts",
                "concept",
                "arc-plan",
                "story-plan",
                "chapter-plan",
            ] {
                let Some(idx) = parts.iter().position(|p| p == marker) else {
                    continue;
                };
                if idx > 0 {
                    let root = path.components().take(idx).collect::<PathBuf>();
                    let key = root.to_string_lossy().into_owned();
                    if !roots.iter().any(|(k, _)| k == &key) {
                        roots.push((key, root));
                    }
                }
                break;
            }
        }
        let mut corpus_paths = Vec::new();
        for (_, root) in &roots {
            for relative in self.default_corpus_parts() {
                let mut candidate = root.clone();
                for part in relative {
                    candidate.push(part);
                }
                if candidate.exists() {
                    corpus_paths.push(candidate);
                }
            }
        }
        corpus_paths
    }
}

// ---------------------------------------------------------------------------
// JSON 输出结构（字段名与 Python 键逐字一致）

/// 短句角色统计行。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ShortRole {
    pub role: String,
    pub count: usize,
}

/// 短句连发块。
#[derive(Debug, Clone, Serialize)]
pub struct ShortRun {
    pub start_index: usize,
    pub end_index: usize,
    pub start_line: usize,
    pub end_line: usize,
    pub avg_chars: f64,
    pub roles: Vec<ShortRole>,
    pub suggestion: String,
    pub sample: Vec<String>,
}

/// 单句长度信息。
#[derive(Debug, Clone, Serialize)]
pub struct ShortSentence {
    pub index: usize,
    pub line_no: usize,
    pub chars: usize,
    pub text: String,
}

/// `sentence_lengths` 节。
#[derive(Debug, Clone, Serialize)]
pub struct SentenceLengths {
    pub count: usize,
    pub min_chars: usize,
    pub p10_chars: usize,
    pub p25_chars: usize,
    pub median_chars: usize,
    pub avg_chars: f64,
    pub max_chars: usize,
    pub short_count: usize,
    pub very_short_count: usize,
    pub short_ratio: f64,
    pub warn: bool,
    pub short_sentences: Vec<ShortSentence>,
    pub very_short_sentences: Vec<ShortSentence>,
    pub short_runs: Vec<ShortRun>,
    pub sentences: Vec<ShortSentence>,
}

/// 局部疲劳窗口。
#[derive(Debug, Clone, Serialize)]
pub struct FatigueWindow {
    pub start_index: usize,
    pub end_index: usize,
    pub start_line: usize,
    pub end_line: usize,
    pub score: usize,
    pub reasons: Vec<String>,
    pub roles: Vec<ShortRole>,
    pub suggestion: String,
    pub sample: Vec<String>,
    pub total_candidates: usize,
}

/// 对白转轴缺口。
#[derive(Debug, Clone, Serialize)]
pub struct DialogueAxisGap {
    pub start_index: usize,
    pub end_index: usize,
    pub start_line: usize,
    pub end_line: usize,
    pub score: usize,
    pub reasons: Vec<String>,
    pub axes: Vec<String>,
    pub suggestion: String,
    pub sample: Vec<String>,
    pub total_candidates: usize,
}

/// 词计数行（judgement_contexts.top_terms）。
#[derive(Debug, Clone, Serialize)]
pub struct TermCount {
    pub term: String,
    pub count: usize,
}

/// 判断词样本。
#[derive(Debug, Clone, Serialize)]
pub struct JudgementSample {
    pub index: usize,
    pub line_no: usize,
    pub terms: Vec<String>,
    pub text: String,
}

/// `judgement_contexts` 行。
#[derive(Debug, Clone, Serialize)]
pub struct JudgementContext {
    pub context: String,
    pub label: String,
    pub count: usize,
    pub warn: bool,
    pub watch: bool,
    pub top_terms: Vec<TermCount>,
    pub samples: Vec<JudgementSample>,
}

/// 把字操作样本。
#[derive(Debug, Clone, Serialize)]
pub struct BaSample {
    pub index: usize,
    pub line_no: usize,
    pub snippet: String,
    pub sentence: String,
}

/// `ba_operation_contexts` 行。
#[derive(Debug, Clone, Serialize)]
pub struct BaContext {
    pub role: String,
    pub count: usize,
    pub samples: Vec<BaSample>,
    pub suggestion: String,
    pub warn: bool,
    pub total: usize,
}

/// 短语计数行（sentence_starts / clause_prefixes / parallel_clauses 等）。
#[derive(Debug, Clone, Serialize)]
pub struct PhraseCount {
    pub phrase: String,
    pub count: usize,
}

/// AA/BB 模式行。
#[derive(Debug, Clone, Serialize)]
pub struct AaBbPattern {
    #[serde(rename = "type")]
    pub pattern_type: String,
    pub name: String,
    pub count: usize,
    pub note: String,
    pub warn: bool,
    pub samples: Vec<String>,
}

/// 修饰/动作压力行。
#[derive(Debug, Clone, Serialize)]
pub struct ModifierPressure {
    pub label: String,
    pub total: usize,
    pub dense_sentences: usize,
    pub warn: bool,
}

/// 对白段连续引用块。
#[derive(Debug, Clone, Serialize)]
pub struct QuoteRun {
    pub start_paragraph: usize,
    pub end_paragraph: usize,
    pub sample: Vec<String>,
}

/// 短对白/引号乒乓块。
#[derive(Debug, Clone, Serialize)]
pub struct ShortQuoteRun {
    pub start_paragraph: usize,
    pub end_paragraph: usize,
    pub avg_len: f64,
    pub sample: Vec<String>,
}

/// A/B 交替说话人。
#[derive(Debug, Clone, Serialize)]
pub struct AbTurn {
    pub paragraph: usize,
    pub pattern: String,
}

/// `dialogue` 节。
#[derive(Debug, Clone, Serialize)]
pub struct DialogueReport {
    pub consecutive_quote_paragraph_runs: Vec<QuoteRun>,
    pub short_quote_runs: Vec<ShortQuoteRun>,
    pub question_ping_pong: Vec<QuoteRun>,
    pub quote_ping_pong: Vec<ShortQuoteRun>,
    pub dialogue_axis_gaps: Vec<DialogueAxisGap>,
    pub alternating_speaker_runs: Vec<AbTurn>,
    pub quote_paragraph_ratio: f64,
    pub dense_quote_run_max: usize,
    pub dense_quote_run_count: usize,
}

/// 场面功能块。
#[derive(Debug, Clone, Serialize)]
pub struct SceneBlock {
    pub role: String,
    pub start_paragraph: usize,
    pub end_paragraph: usize,
    pub start_line: usize,
    pub end_line: usize,
    pub paragraphs: usize,
    pub chars: usize,
    pub sample: Vec<String>,
}

/// `scene_map` 节。
#[derive(Debug, Clone, Serialize)]
pub struct SceneMap {
    pub blocks: Vec<SceneBlock>,
    pub role_counts: CountMap,
    pub dominant_role: String,
    pub dominance_ratio: f64,
    pub warn: bool,
    pub block_count: usize,
    pub switch_count: usize,
}

/// 对白情绪样本。
#[derive(Debug, Clone, Serialize)]
pub struct EmotionSample {
    pub line_no: usize,
    pub label: String,
    pub labels: Vec<String>,
    pub text: String,
}

/// `dialogue_emotions` 节。
#[derive(Debug, Clone, Serialize)]
pub struct DialogueEmotions {
    pub dialogue_sentences: usize,
    pub emotion_counts: CountMap,
    pub dominant_emotion: String,
    pub dominant_ratio: f64,
    pub shift_count: usize,
    pub flatness_warn: bool,
    pub volatility_warn: bool,
    pub samples: Vec<EmotionSample>,
}

/// 角色对白画像样本。
#[derive(Debug, Clone, Serialize)]
pub struct SpeakerSample {
    pub line_no: usize,
    pub text: String,
    pub emotion: String,
}

/// 单个角色的对白画像。
#[derive(Debug, Clone, Serialize)]
pub struct SpeakerProfile {
    pub speaker: String,
    pub lines: usize,
    pub avg_chars: f64,
    pub question_ratio: f64,
    pub exclaim_ratio: f64,
    pub judgement_ratio: f64,
    pub short_ratio: f64,
    pub dominant_emotion: String,
    pub dominant_ratio: f64,
    pub samples: Vec<SpeakerSample>,
}

/// `character_voice` 节。
#[derive(Debug, Clone, Serialize)]
pub struct CharacterVoice {
    pub speaker_count: usize,
    pub identified_lines: usize,
    pub unknown_lines: usize,
    pub coverage_ratio: f64,
    pub dominant_speaker: String,
    pub dominant_ratio: f64,
    pub warn: bool,
    pub homogenized_pairs: Vec<String>,
    pub speakers: Vec<SpeakerProfile>,
}

/// 语气样本。
#[derive(Debug, Clone, Serialize)]
pub struct ToneSample {
    pub paragraph: usize,
    pub line_no: usize,
    pub tone: String,
    pub text: String,
}

/// `tone_profile` 节。
#[derive(Debug, Clone, Serialize)]
pub struct ToneProfile {
    pub tone_counts: CountMap,
    pub dominant_tone: String,
    pub stable_ratio: f64,
    pub switch_count: usize,
    pub samples: Vec<ToneSample>,
    pub warn: bool,
}

/// 战斗段序列。
#[derive(Debug, Clone, Serialize)]
pub struct BattleSequence {
    pub start_index: usize,
    pub end_index: usize,
    pub start_line: usize,
    pub end_line: usize,
    pub sentences: usize,
    pub action_hits: usize,
    pub result_hits: usize,
    pub damage_hits: usize,
    pub movement_hits: usize,
    pub sample: Vec<String>,
    pub warn: bool,
}

/// `battle_profile` 节。
#[derive(Debug, Clone, Serialize)]
pub struct BattleProfile {
    pub sequence_count: usize,
    pub max_sequence_sentences: usize,
    pub action_hits: usize,
    pub result_hits: usize,
    pub damage_hits: usize,
    pub movement_hits: usize,
    pub result_ratio: f64,
    pub warn_sequences: usize,
    pub warn: bool,
    pub samples: Vec<BattleSequence>,
}

/// 视角重叠行。
#[derive(Debug, Clone, Serialize)]
pub struct OverlapEntry {
    pub paragraph: usize,
    pub line_no: usize,
    pub anchors: Vec<String>,
    pub text: String,
}

/// `viewpoint_profile` 节。
#[derive(Debug, Clone, Serialize)]
pub struct ViewpointProfile {
    pub anchor_counts: CountMap,
    pub dominant_anchor: String,
    pub switch_count: usize,
    pub overlap_count: usize,
    pub overlaps: Vec<OverlapEntry>,
    pub warn: bool,
}

/// `ending` 节。
#[derive(Debug, Clone, Serialize)]
pub struct Ending {
    pub tail_excerpt: String,
    pub image_terms: Vec<TermCount>,
    pub flow_terms: Vec<TermCount>,
    pub warn: bool,
}

/// `template_candidates` 行。
#[derive(Debug, Clone, Serialize)]
pub struct TemplateCandidate {
    #[serde(rename = "type")]
    pub candidate_type: String,
    pub name: String,
    pub count: usize,
    pub note: String,
    pub sample: String,
}

/// `hard_flags` 行（`per_10k` 可为 null，对齐 Python `None`）。
#[derive(Debug, Clone, Serialize)]
pub struct HardFlag {
    pub section: String,
    pub name: String,
    pub count: usize,
    pub per_10k: Option<f64>,
    pub note: String,
    pub sample: String,
}

/// `style_fatigue` 行。
#[derive(Debug, Clone, Serialize)]
pub struct FatigueRow {
    pub family: String,
    pub status: String,
    pub count: usize,
    pub risk: String,
    pub reduce: String,
    pub evidence: Vec<String>,
}

/// `review_reminders` 行。
#[derive(Debug, Clone, Serialize)]
pub struct ReviewReminder {
    pub priority: String,
    pub category: String,
    pub title: String,
    pub reason: String,
    pub check: String,
    pub action: String,
    pub evidence: Vec<String>,
}

/// 语料学习得到的模式（对应 Python `LearnedPattern`）。
#[derive(Debug, Clone, Serialize)]
pub struct LearnedPattern {
    pub category: String,
    pub name: String,
    pub count: usize,
    pub corpus_per_10k: f64,
    pub max_per_10k: f64,
    pub note: String,
}

/// 语料学习得到的句首。
#[derive(Debug, Clone, Serialize)]
pub struct LearnedSentenceLead {
    pub phrase: String,
    pub count: usize,
    pub corpus_per_10k: f64,
}

/// 语料学习得到的 AA/BB 形状。
#[derive(Debug, Clone, Serialize)]
pub struct LearnedAaBbShape {
    pub name: String,
    pub count: usize,
    pub note: String,
}

/// 句长基线（`corpus_profile.sentence_length_baseline`）。
#[derive(Debug, Clone, Serialize)]
pub struct SentenceLengthBaseline {
    pub sentence_count: usize,
    pub p10_chars: usize,
    pub p25_chars: usize,
    pub median_chars: usize,
    pub avg_chars: f64,
    pub short_ratio: f64,
}

/// 语料画像（`corpus_profile` 节的数据来源；None 时输出禁用默认值）。
#[derive(Debug, Clone)]
pub struct CorpusProfile {
    pub source_count: usize,
    pub chars: usize,
    pub draft_chars: usize,
    pub learned_terms: Vec<LearnedPattern>,
    pub learned_style_phrases: Vec<LearnedPattern>,
    pub learned_sentence_leads: Vec<LearnedSentenceLead>,
    pub learned_aa_bb_shapes: Vec<LearnedAaBbShape>,
    pub sentence_length_baseline: Option<SentenceLengthBaseline>,
}

/// JSON 里 `sentence_length_baseline` 的两种形态（对齐 Python）：
/// 未启用语料学习 → `{}`（空对象）；语料存在 → 6 键基线对象。
#[derive(Debug, Clone)]
pub enum BaselineJson {
    Empty,
    Values(SentenceLengthBaseline),
}

impl Serialize for BaselineJson {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Empty => {
                let map = serializer.serialize_map(Some(0))?;
                serde::ser::SerializeMap::end(map)
            }
            Self::Values(values) => values.serialize(serializer),
        }
    }
}

/// `corpus_profile` 节。
#[derive(Debug, Clone, Serialize)]
pub struct CorpusProfileJson {
    pub enabled: bool,
    pub source_count: usize,
    pub chars: usize,
    pub draft_chars: usize,
    pub learned_terms: Vec<LearnedTermJson>,
    pub learned_style_phrases: Vec<LearnedTermJson>,
    pub learned_sentence_leads: Vec<LearnedSentenceLead>,
    pub learned_aa_bb_shapes: Vec<LearnedAaBbShape>,
    pub sentence_length_baseline: BaselineJson,
}

/// 语料高频词 JSON 行。
#[derive(Debug, Clone, Serialize)]
pub struct LearnedTermJson {
    pub name: String,
    pub category: String,
    pub count: usize,
    pub corpus_per_10k: f64,
    pub max_per_10k: f64,
}

/// 语料高频词/句法手势指标行（`learned_filters` 节）。
#[derive(Debug, Clone, Serialize)]
pub struct LearnedFilterMetric {
    pub category: String,
    pub name: String,
    pub count: usize,
    pub per_10k: f64,
    pub corpus_per_10k: f64,
    pub max_per_10k: f64,
    pub note: String,
    pub warn: bool,
    pub samples: Vec<Hit>,
}

/// 跟踪词窗口局部行。
#[derive(Debug, Clone, Serialize)]
pub struct TrackedTermWindowTerm {
    pub term: String,
    pub count: usize,
    pub category: String,
    pub note: String,
}

/// `tracked_term_windows` 行。
#[derive(Debug, Clone, Serialize)]
pub struct TrackedTermWindow {
    pub start_index: usize,
    pub end_index: usize,
    pub start_line: usize,
    pub end_line: usize,
    pub window_size: usize,
    pub score: usize,
    pub total_hits: usize,
    pub top_term: String,
    pub top_count: usize,
    pub top_category: String,
    pub reasons: Vec<String>,
    pub terms: Vec<TrackedTermWindowTerm>,
    pub suggestion: String,
    pub sample: Vec<String>,
    pub total_candidates: usize,
}

/// 主要标点（`dominant_punctuation` 行）。
#[derive(Debug, Clone, Serialize)]
pub struct DominantPunctuation {
    pub mark: String,
    pub count: usize,
    pub per_10k: f64,
}

/// `summary` 节。
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub chars: usize,
    pub sentences: usize,
    pub paragraphs: usize,
    pub avg_sentence_chars: f64,
    pub short_sentences: usize,
    pub very_short_sentences: usize,
    pub short_sentence_ratio: f64,
    pub quote_ratio: f64,
    pub warn_sections: usize,
}

/// Python `analyze_text` 返回的完整 analysis 结构（键序与 Python 一致）。
#[derive(Debug, Clone, Serialize)]
pub struct Analysis {
    pub source: String,
    pub summary: Summary,
    pub warned: bool,
    pub tokens: Vec<RegexMetric>,
    pub tracked_terms: Vec<crate::rules::TrackedMetric>,
    pub tracked_term_categories: Vec<crate::rules::CategoryRow>,
    pub tracked_term_windows: Vec<TrackedTermWindow>,
    pub tracked_term_window_count: usize,
    pub ba_operation_contexts: Vec<BaContext>,
    pub patterns: Vec<RegexMetric>,
    pub phrases: Vec<RegexMetric>,
    pub modifiers: Vec<RegexMetric>,
    pub punctuation: Vec<RegexMetric>,
    pub punctuation_combos: Vec<RegexMetric>,
    pub custom_templates: Vec<CustomTemplateMetric>,
    pub learned_filters: Vec<LearnedFilterMetric>,
    pub corpus_profile: CorpusProfileJson,
    pub dominant_punctuation: Vec<DominantPunctuation>,
    pub sentence_starts: Vec<PhraseCount>,
    pub subject_leads: Vec<PhraseCount>,
    pub paragraph_leads: Vec<PhraseCount>,
    pub sentence_patterns: Vec<PhraseCount>,
    pub judgement_endings: Vec<PhraseCount>,
    pub clause_prefixes: Vec<PhraseCount>,
    pub parallel_clauses: Vec<PhraseCount>,
    pub aa_bb_patterns: Vec<AaBbPattern>,
    pub sentence_lengths: SentenceLengths,
    pub fatigue_windows: Vec<FatigueWindow>,
    pub fatigue_window_count: usize,
    pub judgement_contexts: Vec<JudgementContext>,
    pub modifier_pressure: Vec<ModifierPressure>,
    pub terms: Vec<TermCount>,
    pub short_phrases: Vec<TermCount>,
    pub dialogue: DialogueReport,
    pub scene_map: SceneMap,
    pub dialogue_emotions: DialogueEmotions,
    pub character_voice: CharacterVoice,
    pub tone_profile: ToneProfile,
    pub battle_profile: BattleProfile,
    pub viewpoint_profile: ViewpointProfile,
    pub ending: Ending,
    pub template_candidates: Vec<TemplateCandidate>,
    pub hard_flags: Vec<HardFlag>,
    pub style_fatigue: Vec<FatigueRow>,
    pub review_reminders: Vec<ReviewReminder>,
}

// ---------------------------------------------------------------------------
// 输入发现 / 语料清洗（对齐 iter_target_files / clean_corpus_text）

/// 判断路径是否属于生成物或模板目录（对齐 `_is_generated_or_template`）。
fn is_generated_or_template(path: &Path) -> bool {
    let names: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let has = |s: &str| names.iter().any(|n| n == s);
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    has("_templates")
        || has("draft-stats")
        || has("story-plan-stats")
        || has("chapter-plan-stats")
        || has("arc-plan-stats")
        || has("card-stats")
        || filename.to_uppercase().starts_with("README")
        || filename == "progression.md"
}

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk_files(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// 去掉路径中的 `.`（CurrentDir）分量，对齐 Python `pathlib` 的归一化
/// （`Path("./a")` → `a`；`Path(".")` 的 rglob 结果不带 `./` 前缀）。
fn normalize_current_dir(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

/// 展开输入路径为文件列表（对齐 `iter_target_files`：目录递归收 `.md/.txt`
/// 且排除生成物/模板，显式文件原样保留，结果按路径排序；路径按 Python
/// `pathlib` 语义去掉 `.` 分量）。
pub fn iter_target_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for raw in paths {
        let raw = normalize_current_dir(raw);
        if raw.is_dir() {
            let mut found = Vec::new();
            walk_files(&raw, &mut found);
            found.retain(|p| {
                p.extension()
                    .map(|e| e == "md" || e == "txt")
                    .unwrap_or(false)
                    && !is_generated_or_template(p)
            });
            let mut normalized: Vec<PathBuf> = found
                .into_iter()
                .map(|p| normalize_current_dir(&p))
                .collect();
            normalized.sort();
            out.extend(normalized);
        } else if raw.is_file() {
            out.push(raw);
        }
    }
    out
}

/// `re.fullmatch(pattern, text)`：`fancy-regex` 无 `is_whole_match`，用 `find` 起止判定。
fn is_whole_match(regex: &fancy_regex::Regex, text: &str) -> bool {
    regex
        .find(text)
        .ok()
        .flatten()
        .map(|m| m.start() == 0 && m.end() == text.len())
        .unwrap_or(false)
}

/// 语料清洗（对齐 `clean_corpus_text`）。
fn clean_corpus_text(ctx: &DraftContext, path: &Path) -> Result<String> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("无法读取语料文件 {}", path.display()))?;
    let mut lines: Vec<String> = Vec::new();
    for line in raw.lines() {
        let stripped = line.trim();
        if stripped.is_empty() {
            continue;
        }
        if ctx
            .corpus_markdown_noise_regex
            .is_match(stripped)
            .unwrap_or(false)
            || is_whole_match(&ctx.corpus_table_line_regex, stripped)
        {
            continue;
        }
        let mut s = ctx
            .corpus_bullet_regex
            .replace_all(stripped, "")
            .into_owned();
        s = ctx.corpus_backtick_regex.replace_all(&s, "$1").into_owned();
        s = ctx.corpus_bold_regex.replace_all(&s, "$1").into_owned();
        if !s.is_empty() {
            lines.push(s);
        }
    }
    Ok(lines.join("\n"))
}

// ---------------------------------------------------------------------------
// 对白判定与短句角色（对齐 is_dialogue_like / classify_short_sentence_role 等）

/// 是否对白句（对齐 `is_dialogue_like`）。
fn is_dialogue_like(sentence: &str) -> bool {
    let raw = sentence.trim();
    raw.starts_with('“')
        || raw.starts_with('”')
        || raw.starts_with('"')
        || raw.starts_with("【Pi】")
        || raw.starts_with('「')
        || raw.starts_with('『')
        || raw.contains("：“")
        || prefix_chars(raw, 8).contains('】')
}

/// 短句角色分类（对齐 `classify_short_sentence_role`）。
fn classify_short_sentence_role(ctx: &DraftContext, sentence: &str) -> &'static str {
    let raw = sentence.trim();
    let stripped = lstrip_chars(raw, LEADING_PUNCT);
    let lex = ctx.lexicon();
    if is_dialogue_like(raw) {
        return "对白";
    }
    if lex
        .judgement_context_terms
        .iter()
        .any(|t| stripped.contains(t.as_str()))
        || lex
            .judgement_endings
            .iter()
            .any(|e| stripped.ends_with(e.as_str()))
    {
        return "判断";
    }
    if lex
        .short_role_info_terms
        .iter()
        .any(|t| stripped.contains(t.as_str()))
    {
        return "信息";
    }
    if lex
        .short_role_emotion_terms
        .iter()
        .any(|t| stripped.contains(t.as_str()))
    {
        return "情绪";
    }
    if ctx.ba_regex.find_iter(stripped).any(|m| m.is_ok())
        || lex.verb_hints.iter().any(|v| stripped.contains(v.as_str()))
    {
        return "动作";
    }
    if stripped.matches('，').count() + stripped.matches('、').count() >= 2 {
        return "清单";
    }
    "其他"
}

/// 短句角色汇总（对齐 `summarize_short_roles`：固定角色序、计数降序）。
fn summarize_short_roles(
    ctx: &DraftContext,
    items: &[&crate::text::SentenceInfo],
) -> Vec<ShortRole> {
    let mut counts = Counter::default();
    for item in items {
        counts.add(classify_short_sentence_role(ctx, &item.text));
    }
    let order_of = |role: &str| -> u32 {
        match role {
            "对白" => 0,
            "动作" => 1,
            "信息" => 2,
            "判断" => 3,
            "情绪" => 4,
            "清单" => 5,
            "其他" => 6,
            _ => 99,
        }
    };
    let mut rows: Vec<ShortRole> = counts
        .entries()
        .iter()
        .map(|(role, count)| ShortRole {
            role: role.clone(),
            count: *count,
        })
        .collect();
    rows.sort_by(|a, b| {
        order_of(&a.role)
            .cmp(&order_of(&b.role))
            .then(b.count.cmp(&a.count))
    });
    rows
}

/// 由角色汇总给出短句连发建议（对齐 `suggest_short_run_action`，平手取首现）。
fn suggest_short_run_action(roles: &[ShortRole]) -> String {
    let top_role = roles.iter().fold(None, |best: Option<&ShortRole>, r| {
        if best.is_none_or(|b| r.count > b.count) {
            Some(r)
        } else {
            best
        }
    });
    match top_role.map_or("其他", |r| r.role.as_str()) {
        "对白" => "保留最锋利的一两句，其余用动作、环境声或第三方反应打断。".to_string(),
        "动作" => "保留关键动作，补动作因果、阻力或结果，避免操作日志。".to_string(),
        "信息" => "把信息拆成发现、误读、排除和后果，不要连续报材料。".to_string(),
        "判断" => "人物台词可留；旁白判断优先换成证据、动作或误读。".to_string(),
        "情绪" => "用身体反应、声音和场面反馈承载情绪，不要连续短评。".to_string(),
        "清单" => "保留一个清单节奏，其余并入动作过程或视角变化。".to_string(),
        _ => "先判断这些短句是否都必要；只保留一个节奏点，其余展开。".to_string(),
    }
}

/// `format_short_roles`：`角色 x计数` 用分隔符连接。
fn format_short_roles(roles: &[ShortRole], separator: &str) -> String {
    roles
        .iter()
        .map(|r| format!("{} x{}", r.role, r.count))
        .collect::<Vec<_>>()
        .join(separator)
}

// ---------------------------------------------------------------------------
// 跟踪词窗口 / 把字操作 / 句长画像

/// 跟踪词分类标签（对齐 `tracked_term_category_label`）。
fn tracked_term_category_label(category: &str) -> String {
    match category {
        "characters" => "人物名".into(),
        "places" => "地点名".into(),
        "devices" => "设备名".into(),
        "actions" => "动作短语".into(),
        "atmosphere" => "氛围词".into(),
        "style" => "意象词".into(),
        "learned_term" => "语料高频词".into(),
        other => other.to_string(),
    }
}

/// 跟踪词窗口建议（对齐 `suggest_tracked_term_window_action`）。
fn suggest_tracked_term_window_action(category: &str) -> String {
    match category {
        "characters" => "用称谓、站位、动作或视角入口替换连续点名。".into(),
        "places" => "换成具体空间部件、声音、光线或行动路径，不要连续报地点名。".into(),
        "devices" => "让设备通过状态变化、故障后果或人物反应出现，不要连续点屏幕/终端。".into(),
        "actions" => "把重复动作拆成目的、阻力和结果，或换成身体反应。".into(),
        "atmosphere" | "style" => "保留最有用的一处意象，其余改成可见场面变化。".into(),
        "learned_term" => {
            "先判断它是临时角色、物件还是概念；用称谓、位置、动作和后果分担点名。".into()
        }
        _ => "检查同一词是否在替代镜头调度；优先换成动作、物件或视角变化。".into(),
    }
}

/// `format_tracked_term_counts`：`词 x计数(分类标签)` 用分隔符连接。
fn format_tracked_term_counts(terms: &[TrackedTermWindowTerm], separator: &str) -> String {
    terms
        .iter()
        .map(|t| {
            format!(
                "{} x{}({})",
                t.term,
                t.count,
                tracked_term_category_label(&t.category)
            )
        })
        .collect::<Vec<_>>()
        .join(separator)
}

/// 语料学习词能否进入跟踪窗口（对齐 `is_learned_term_window_candidate`）。
fn is_learned_term_window_candidate(
    ctx: &DraftContext,
    term: &str,
    known: &std::collections::HashSet<String>,
) -> bool {
    if code_len(term) < 2 {
        return false;
    }
    let window = ctx.learned_window();
    if window
        .noise_prefixes
        .iter()
        .any(|p| term.starts_with(p.as_str()))
        || window
            .noise_suffixes
            .iter()
            .any(|s| term.ends_with(s.as_str()))
        || window.noise_chars.iter().any(|c| term.contains(c.as_str()))
    {
        return false;
    }
    for known_term in known {
        if !known_term.is_empty() && known_term != term && term.contains(known_term.as_str()) {
            return false;
        }
    }
    true
}

/// 跟踪词局部窗口（对齐 `build_tracked_term_windows`）。
fn build_tracked_term_windows(
    ctx: &DraftContext,
    term_bank: &[TrackedTerm],
    learned_terms: &[LearnedPattern],
    sentence_infos: &[crate::text::SentenceInfo],
    sample_limit: usize,
) -> Vec<TrackedTermWindow> {
    if sentence_infos.len() < 3 {
        return Vec::new();
    }
    let th = ctx.thresholds();
    let window_categories: std::collections::HashSet<&str> = ctx
        .learned_window()
        .categories
        .iter()
        .map(|c| c.as_str())
        .collect();
    let mut rules: Vec<(String, String, String)> = Vec::new();
    let mut seen_terms: std::collections::HashSet<String> = HashSet::new();
    for rule in term_bank {
        if rule.term.is_empty()
            || seen_terms.contains(&rule.term)
            || !window_categories.contains(rule.category.as_str())
        {
            continue;
        }
        rules.push((
            rule.term.clone(),
            rule.category.clone(),
            rule.note.clone().unwrap_or_default(),
        ));
        seen_terms.insert(rule.term.clone());
    }
    for rule in learned_terms {
        if rule.name.is_empty()
            || seen_terms.contains(&rule.name)
            || !window_categories.contains(rule.category.as_str())
            || !is_learned_term_window_candidate(ctx, &rule.name, &seen_terms)
        {
            continue;
        }
        rules.push((rule.name.clone(), rule.category.clone(), rule.note.clone()));
        seen_terms.insert(rule.name.clone());
    }
    if rules.is_empty() {
        return Vec::new();
    }

    let window_size = th.tracked_term_window_size as usize;
    let min_top = th.tracked_term_window_min_top as usize;
    let min_total = th.tracked_term_window_min_total as usize;
    let min_category = th.tracked_term_window_min_category as usize;

    let mut candidates: Vec<TrackedTermWindow> = Vec::new();
    for start in 0..sentence_infos
        .len()
        .saturating_sub(window_size)
        .saturating_add(1)
    {
        let chunk =
            &sentence_infos[start..start.saturating_add(window_size).min(sentence_infos.len())];
        if chunk.len() < 3 {
            continue;
        }
        let mut term_counts = Counter::default();
        let mut term_categories: HashMap<String, String> = HashMap::new();
        let mut term_notes: HashMap<String, String> = HashMap::new();
        for sentence in chunk {
            for (term, category, note) in &rules {
                let count = sentence.text.matches(term.as_str()).count();
                if count == 0 {
                    continue;
                }
                term_counts.add(term.as_str());
                term_categories
                    .entry(term.clone())
                    .or_insert_with(|| category.clone());
                term_notes
                    .entry(term.clone())
                    .or_insert_with(|| note.clone());
            }
        }
        if term_counts.is_empty() {
            continue;
        }
        let most = term_counts.most_common_all();
        let (top_term, top_count) = &most[0];
        let total_hits: usize = most.iter().map(|(_, c)| *c).sum();
        let mut category_counts = Counter::default();
        for (term, count) in &most {
            let category = term_categories
                .get(term)
                .cloned()
                .unwrap_or_else(|| "tracked".into());
            for _ in 0..*count {
                category_counts.add(&category);
            }
        }
        let top_cat = category_counts.most_common_all();
        let (top_category, top_category_count) = if top_cat.is_empty() {
            ("tracked".to_string(), 0usize)
        } else {
            (top_cat[0].0.clone(), top_cat[0].1)
        };

        let mut reasons: Vec<String> = Vec::new();
        if *top_count >= min_top {
            reasons.push(format!("同词 {top_term} x{top_count}/{}", chunk.len()));
        }
        if total_hits >= min_total {
            reasons.push(format!("跟踪词合计 {total_hits}/{}", chunk.len()));
        }
        if top_category_count >= min_category {
            reasons.push(format!(
                "{} x{}/{}",
                tracked_term_category_label(&top_category),
                top_category_count,
                chunk.len()
            ));
        }
        if reasons.is_empty() {
            continue;
        }
        let top_terms: Vec<TrackedTermWindowTerm> = term_counts
            .most_common(5)
            .into_iter()
            .map(|(term, count)| TrackedTermWindowTerm {
                category: term_categories
                    .get(&term)
                    .cloned()
                    .unwrap_or_else(|| "tracked".into()),
                note: term_notes.get(&term).cloned().unwrap_or_default(),
                term,
                count,
            })
            .collect();
        candidates.push(TrackedTermWindow {
            start_index: chunk[0].index,
            end_index: chunk[chunk.len() - 1].index,
            start_line: chunk[0].line_no,
            end_line: chunk[chunk.len() - 1].line_no,
            window_size: chunk.len(),
            score: top_count * 3 + total_hits + top_category_count,
            total_hits,
            top_term: top_term.clone(),
            top_count: *top_count,
            top_category: top_category.clone(),
            reasons,
            terms: top_terms,
            suggestion: suggest_tracked_term_window_action(&top_category),
            sample: chunk.iter().map(|i| i.text.clone()).collect(),
            total_candidates: 0,
        });
    }
    candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(b.top_count.cmp(&a.top_count))
            .then(a.start_index.cmp(&b.start_index))
            .then_with(|| a.top_term.cmp(&b.top_term))
    });
    let total_candidates = candidates.len();
    let mut kept: Vec<TrackedTermWindow> = Vec::new();
    let mut kept_spans: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    for candidate in candidates {
        let span = (candidate.start_index, candidate.end_index);
        let overlaps = kept_spans
            .get(&candidate.top_term)
            .is_some_and(|existing| existing.iter().any(|e| !(span.1 < e.0 || span.0 > e.1)));
        if overlaps {
            continue;
        }
        kept_spans
            .entry(candidate.top_term.clone())
            .or_default()
            .push(span);
        let mut window = candidate;
        window.total_candidates = total_candidates;
        kept.push(window);
        if kept.len() >= sample_limit {
            break;
        }
    }
    kept
}

/// 把字操作角色分类（对齐 `classify_ba_operation`）。
fn classify_ba_operation(ctx: &DraftContext, snippet: &str) -> &'static str {
    let lex = ctx.lexicon();
    if lex
        .ba_emotion_terms
        .iter()
        .any(|t| snippet.contains(t.as_str()))
    {
        return "情绪动作";
    }
    if lex
        .ba_clue_terms
        .iter()
        .any(|t| snippet.contains(t.as_str()))
    {
        return "线索操作";
    }
    if lex
        .ba_scene_terms
        .iter()
        .chain(lex.ba_scene_verbs.iter())
        .any(|t| snippet.contains(t.as_str()))
    {
        return "场面调度";
    }
    if lex
        .ba_tool_terms
        .iter()
        .any(|t| snippet.contains(t.as_str()))
    {
        return "工具操作";
    }
    "动作操作"
}

/// 把字操作建议（对齐 `suggest_ba_operation_action`）。
fn suggest_ba_operation_action(role: &str) -> String {
    match role {
        "工具操作" => "必要工具动作可保留，但连续出现时要补结果、阻力或人物反应。".into(),
        "线索操作" => "把线索操作拆成发现、误读、排除和后果，少写整理流程。".into(),
        "情绪动作" => "优先改成身体反应、声音变化或他人误读，不要只把情绪推来推去。".into(),
        "场面调度" => "保留能改变画面的句子，其余改成环境后果或视角移动。".into(),
        _ => "检查这个把字句是否只是操作日志；能换结果句、被动阻力或场面反馈就换。".into(),
    }
}

/// 把字操作语境（对齐 `build_ba_operation_contexts`）。
fn build_ba_operation_contexts(
    ctx: &DraftContext,
    sentence_infos: &[crate::text::SentenceInfo],
    sample_limit: usize,
) -> Vec<BaContext> {
    struct Bucket {
        count: usize,
        samples: Vec<BaSample>,
    }
    let mut order: Vec<&'static str> = Vec::new();
    let mut buckets: HashMap<&'static str, Bucket> = HashMap::new();
    let mut total = 0usize;
    for sentence in sentence_infos {
        for m in ctx
            .ba_regex
            .find_iter(&sentence.text)
            .filter_map(|m| m.ok())
        {
            let snippet = m.as_str().to_string();
            let role = classify_ba_operation(ctx, &snippet);
            let entry = buckets.entry(role).or_insert_with(|| {
                order.push(role);
                Bucket {
                    count: 0,
                    samples: Vec::new(),
                }
            });
            entry.count += 1;
            total += 1;
            if entry.samples.len() < sample_limit {
                entry.samples.push(BaSample {
                    index: sentence.index,
                    line_no: sentence.line_no,
                    snippet,
                    sentence: sentence.text.clone(),
                });
            }
        }
    }
    let role_order = |role: &str| -> u32 {
        match role {
            "线索操作" => 0,
            "情绪动作" => 1,
            "动作操作" => 2,
            "场面调度" => 3,
            "工具操作" => 4,
            _ => 99,
        }
    };
    let mut rows: Vec<BaContext> = Vec::new();
    for role in order {
        let bucket = &buckets[role];
        let warn = if matches!(role, "线索操作" | "情绪动作" | "动作操作") {
            bucket.count >= 2
        } else {
            bucket.count >= 4
        };
        rows.push(BaContext {
            role: role.to_string(),
            count: bucket.count,
            samples: bucket.samples.clone(),
            suggestion: suggest_ba_operation_action(role),
            warn,
            total,
        });
    }
    rows.sort_by(|a, b| {
        role_order(&a.role)
            .cmp(&role_order(&b.role))
            .then(b.count.cmp(&a.count))
    });
    rows
}

/// 百分位（对齐 `_percentile`：半偶取整下标）。
fn percentile(values: &[usize], pct: f64) -> usize {
    if values.is_empty() {
        return 0;
    }
    let mut ordered = values.to_vec();
    ordered.sort_unstable();
    let index = bankers_round_int((ordered.len() - 1) as f64 * pct).max(0) as usize;
    ordered[index.min(ordered.len() - 1)]
}

fn flush_short_run(
    ctx: &DraftContext,
    run_min: usize,
    runs: &mut Vec<ShortRun>,
    current: &mut Vec<&crate::text::SentenceInfo>,
) {
    if current.len() >= run_min {
        let roles = summarize_short_roles(ctx, &current[..]);
        let total: usize = current.iter().map(|i| i.chars).sum();
        let run = ShortRun {
            start_index: current[0].index,
            end_index: current[current.len() - 1].index,
            start_line: current[0].line_no,
            end_line: current[current.len() - 1].line_no,
            avg_chars: round2(total as f64 / current.len() as f64),
            roles: roles.clone(),
            suggestion: suggest_short_run_action(&roles),
            sample: current.iter().take(5).map(|i| i.text.clone()).collect(),
        };
        runs.push(run);
    }
    current.clear();
}

/// 句长画像（对齐 `build_sentence_length_profile`）。
fn build_sentence_length_profile(
    ctx: &DraftContext,
    sentence_infos: &[crate::text::SentenceInfo],
) -> SentenceLengths {
    let th = ctx.thresholds();
    let short_max = th.short_sentence_max_chars as usize;
    let very_short_max = th.very_short_sentence_max_chars as usize;
    let run_max = th.short_sentence_run_max_chars as usize;
    let run_min = th.short_sentence_run_min as usize;

    let lengths: Vec<usize> = sentence_infos.iter().map(|i| i.chars).collect();
    let short_items: Vec<&crate::text::SentenceInfo> = sentence_infos
        .iter()
        .filter(|i| i.chars <= short_max)
        .collect();
    let very_short_items: Vec<&crate::text::SentenceInfo> = sentence_infos
        .iter()
        .filter(|i| i.chars <= very_short_max)
        .collect();

    let mut runs: Vec<ShortRun> = Vec::new();
    let mut current: Vec<&crate::text::SentenceInfo> = Vec::new();
    let mut previous_index = 0usize;
    for item in sentence_infos {
        if item.chars <= run_max && (current.is_empty() || item.index == previous_index + 1) {
            current.push(item);
        } else {
            flush_short_run(ctx, run_min, &mut runs, &mut current);
            if item.chars <= run_max {
                current.push(item);
            }
        }
        previous_index = item.index;
    }
    flush_short_run(ctx, run_min, &mut runs, &mut current);

    let short_ratio = short_items.len() as f64 / sentence_infos.len().max(1) as f64;
    let warn = very_short_items.len() >= 3 || !runs.is_empty() || short_ratio >= 0.18;
    SentenceLengths {
        count: sentence_infos.len(),
        min_chars: lengths.iter().min().copied().unwrap_or(0),
        p10_chars: percentile(&lengths, 0.10),
        p25_chars: percentile(&lengths, 0.25),
        median_chars: percentile(&lengths, 0.50),
        avg_chars: round2(
            lengths.iter().sum::<usize>() as f64 / sentence_infos.len().max(1) as f64,
        ),
        max_chars: lengths.iter().max().copied().unwrap_or(0),
        short_count: short_items.len(),
        very_short_count: very_short_items.len(),
        short_ratio: round4f(short_ratio),
        warn,
        short_sentences: short_items
            .iter()
            .take(20)
            .map(|i| ShortSentence {
                index: i.index,
                line_no: i.line_no,
                chars: i.chars,
                text: i.text.clone(),
            })
            .collect(),
        very_short_sentences: very_short_items
            .iter()
            .take(20)
            .map(|i| ShortSentence {
                index: i.index,
                line_no: i.line_no,
                chars: i.chars,
                text: i.text.clone(),
            })
            .collect(),
        short_runs: runs.into_iter().take(10).collect(),
        sentences: sentence_infos
            .iter()
            .map(|i| ShortSentence {
                index: i.index,
                line_no: i.line_no,
                chars: i.chars,
                text: i.text.clone(),
            })
            .collect(),
    }
}

/// 局部疲劳窗口（对齐 `build_fatigue_windows`：滑窗 5、块 ≥4 句才参与评分）。
fn build_fatigue_windows(
    ctx: &DraftContext,
    sentence_infos: &[crate::text::SentenceInfo],
    sample_limit: usize,
) -> Vec<FatigueWindow> {
    if sentence_infos.len() < 4 {
        return Vec::new();
    }
    let window_size = 5;
    let short_max = ctx.thresholds().short_sentence_max_chars as usize;
    let very_short_max = ctx.thresholds().very_short_sentence_max_chars as usize;
    let lex = ctx.lexicon();
    let judgement_terms = &lex.fatigue_window_judgement_terms;
    let sticky_terms = &lex.fatigue_window_sticky_terms;

    let mut candidates: Vec<FatigueWindow> = Vec::new();
    for start in 0..sentence_infos
        .len()
        .saturating_sub(window_size)
        .saturating_add(1)
    {
        let chunk =
            &sentence_infos[start..start.saturating_add(window_size).min(sentence_infos.len())];
        if chunk.len() < 4 {
            continue;
        }
        let short_count = chunk.iter().filter(|i| i.chars <= short_max).count();
        let very_short_count = chunk.iter().filter(|i| i.chars <= very_short_max).count();
        let mut judgement_count = 0usize;
        let mut ba_count = 0usize;
        let mut sticky_count = 0usize;
        let mut role_lead_count = 0usize;
        let mut dialogue_count = 0usize;
        let mut listish_count = 0usize;
        for item in chunk {
            let stripped = lstrip_chars(&item.text, LEADING_PUNCT);
            if judgement_terms
                .iter()
                .any(|t| stripped.contains(t.as_str()))
            {
                judgement_count += 1;
            }
            if ctx.ba_regex.is_match(stripped).unwrap_or(false) {
                ba_count += 1;
            }
            if sticky_terms.iter().any(|t| stripped.contains(t.as_str())) {
                sticky_count += 1;
            }
            if lex
                .subject_leads
                .iter()
                .any(|p| stripped.starts_with(p.as_str()))
            {
                role_lead_count += 1;
            }
            if stripped.starts_with('\u{201c}')
                || stripped.starts_with("【Pi】")
                || stripped.contains("\u{ff1a}\u{201c}")
            {
                dialogue_count += 1;
            }
            if stripped.matches('，').count() + stripped.matches('、').count() >= 3 {
                listish_count += 1;
            }
        }
        let mut reasons: Vec<String> = Vec::new();
        if short_count >= 3 {
            reasons.push(format!("短句 {short_count}/5"));
        }
        if very_short_count >= 2 {
            reasons.push(format!("极短句 {very_short_count}/5"));
        }
        if judgement_count >= 2 {
            reasons.push(format!("判断解释 {judgement_count}/5"));
        }
        if ba_count >= 2 {
            reasons.push(format!("把字操作 {ba_count}/5"));
        }
        if sticky_count >= 2 {
            reasons.push(format!("黏糊词 {sticky_count}/5"));
        }
        if role_lead_count >= 3 {
            reasons.push(format!("角色起手 {role_lead_count}/5"));
        }
        if dialogue_count >= 4 {
            reasons.push(format!("对白挤压 {dialogue_count}/5"));
        }
        if listish_count >= 2 {
            reasons.push(format!("清单分句 {listish_count}/5"));
        }
        if reasons.is_empty() {
            continue;
        }
        let score = short_count * 2
            + very_short_count
            + judgement_count * 2
            + ba_count * 2
            + sticky_count
            + role_lead_count
            + dialogue_count
            + listish_count;
        let roles = summarize_short_roles(ctx, &chunk.iter().collect::<Vec<_>>());
        candidates.push(FatigueWindow {
            start_index: chunk[0].index,
            end_index: chunk[chunk.len() - 1].index,
            start_line: chunk[0].line_no,
            end_line: chunk[chunk.len() - 1].line_no,
            score,
            reasons,
            roles: roles.clone(),
            suggestion: suggest_short_run_action(&roles),
            sample: chunk.iter().map(|i| i.text.clone()).collect(),
            total_candidates: 0,
        });
    }
    candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.start_index.cmp(&b.start_index))
    });
    let total_candidates = candidates.len();
    let mut kept: Vec<FatigueWindow> = Vec::new();
    let mut occupied: HashSet<usize> = HashSet::new();
    for c in candidates {
        if (c.start_index..=c.end_index).any(|i| occupied.contains(&i)) {
            continue;
        }
        for i in c.start_index..=c.end_index {
            occupied.insert(i);
        }
        let mut w = c;
        w.total_candidates = total_candidates;
        kept.push(w);
        if kept.len() >= sample_limit {
            break;
        }
    }
    kept
}

/// 对白轴分类（对齐 `classify_dialogue_axis`）。
fn classify_dialogue_axis(ctx: &DraftContext, sentence: &str) -> &'static str {
    let stripped = lstrip_chars(sentence.trim(), LEADING_PUNCT);
    let lex = ctx.lexicon();
    if lex
        .dialogue_axis_device_terms
        .iter()
        .any(|t| stripped.contains(t.as_str()))
    {
        return "设备声";
    }
    if lex
        .dialogue_axis_third_party_terms
        .iter()
        .any(|t| stripped.contains(t.as_str()))
    {
        return "第三方";
    }
    if lex
        .dialogue_axis_env_terms
        .iter()
        .any(|t| stripped.contains(t.as_str()))
    {
        return "环境";
    }
    if lex
        .dialogue_axis_action_terms
        .iter()
        .any(|t| stripped.contains(t.as_str()))
    {
        return "动作";
    }
    ""
}

/// 对白轴转轴建议（对齐 `suggest_dialogue_axis_action`）。
fn suggest_dialogue_axis_action(axes: &[String]) -> String {
    if axes.is_empty() {
        return "插入动作、环境变化、第三方打断或设备声，让对白改变场面。".into();
    }
    if !axes.iter().any(|a| a == "动作") {
        return "补一个能改变站位或物件状态的动作，不要只让角色继续接话。".into();
    }
    if !axes.iter().any(|a| a == "环境") && !axes.iter().any(|a| a == "设备声") {
        return "补环境声、设备反馈或空间变化，把话题从互答里拨出来。".into();
    }
    "保留已有转轴，再压掉重复问答或合并台词。".into()
}

/// 对白转轴缺口（对齐 `build_dialogue_axis_gaps`）。
fn build_dialogue_axis_gaps(
    ctx: &DraftContext,
    sentence_infos: &[crate::text::SentenceInfo],
    sample_limit: usize,
) -> Vec<DialogueAxisGap> {
    if sentence_infos.len() < 4 {
        return Vec::new();
    }
    let window_size = 4;
    let run_max = ctx.thresholds().short_sentence_run_max_chars as usize;
    let mut candidates: Vec<DialogueAxisGap> = Vec::new();
    for start in 0..sentence_infos
        .len()
        .saturating_sub(window_size)
        .saturating_add(1)
    {
        let chunk =
            &sentence_infos[start..start.saturating_add(window_size).min(sentence_infos.len())];
        if chunk.len() < window_size {
            continue;
        }
        let dialogue_count = chunk.iter().filter(|i| is_dialogue_like(&i.text)).count();
        if dialogue_count < window_size {
            continue;
        }
        let axes: Vec<String> = chunk
            .iter()
            .filter_map(|i| {
                let a = classify_dialogue_axis(ctx, &i.text);
                (!a.is_empty()).then_some(a.to_string())
            })
            .collect();
        if !axes.is_empty() {
            continue;
        }
        let question_count = chunk
            .iter()
            .filter(|i| i.text.contains('？') || i.text.contains('?'))
            .count();
        let short_count = chunk.iter().filter(|i| i.chars <= run_max).count();
        let mut reasons = vec![
            format!("纯对白 {dialogue_count}/{window_size}"),
            format!("短句 {short_count}/{window_size}"),
        ];
        if question_count > 0 {
            reasons.push(format!("问句 {question_count}/{window_size}"));
        }
        candidates.push(DialogueAxisGap {
            start_index: chunk[0].index,
            end_index: chunk[chunk.len() - 1].index,
            start_line: chunk[0].line_no,
            end_line: chunk[chunk.len() - 1].line_no,
            score: window_size * 2 + short_count + question_count,
            reasons,
            axes: Vec::new(),
            suggestion: suggest_dialogue_axis_action(&[]),
            sample: chunk.iter().map(|i| i.text.clone()).collect(),
            total_candidates: 0,
        });
    }
    candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.start_index.cmp(&b.start_index))
    });
    let total_candidates = candidates.len();
    let mut kept: Vec<DialogueAxisGap> = Vec::new();
    let mut occupied: HashSet<usize> = HashSet::new();
    for c in candidates {
        if (c.start_index..=c.end_index).any(|i| occupied.contains(&i)) {
            continue;
        }
        for i in c.start_index..=c.end_index {
            occupied.insert(i);
        }
        let mut g = c;
        g.total_candidates = total_candidates;
        kept.push(g);
        if kept.len() >= sample_limit {
            break;
        }
    }
    kept
}

/// 句上下文标签（对齐 `sentence_context_label`）。
fn sentence_context_label(sentence: &str) -> &'static str {
    let raw = sentence.trim();
    if is_dialogue_like(raw) {
        return "dialogue";
    }
    let stripped = lstrip_chars(raw, LEADING_PUNCT);
    if stripped.starts_with("【Pi】") || stripped.starts_with("Pi") {
        "dialogue"
    } else {
        "narration"
    }
}

/// 判断词语境（对齐 `collect_judgement_contexts`；term 去重保持 matched 原序）。
fn collect_judgement_contexts(
    ctx: &DraftContext,
    sentence_infos: &[crate::text::SentenceInfo],
    sample_limit: usize,
) -> Vec<JudgementContext> {
    struct Bucket {
        label: &'static str,
        count: usize,
        terms: Counter,
        samples: Vec<JudgementSample>,
    }
    let mut buckets: [Option<Bucket>; 2] = [
        Some(Bucket {
            label: "旁白判断",
            count: 0,
            terms: Counter::default(),
            samples: Vec::new(),
        }),
        Some(Bucket {
            label: "对白判断",
            count: 0,
            terms: Counter::default(),
            samples: Vec::new(),
        }),
    ];
    let judgement_terms = &ctx.lexicon().judgement_context_terms;
    for item in sentence_infos {
        let stripped = lstrip_chars(&item.text, LEADING_PUNCT);
        let matched: Vec<&str> = judgement_terms
            .iter()
            .filter(|t| stripped.contains(t.as_str()))
            .map(|t| t.as_str())
            .collect();
        if matched.is_empty() {
            continue;
        }
        let context = sentence_context_label(&item.text);
        let bucket = &mut buckets[if context == "dialogue" { 1 } else { 0 }]
            .as_mut()
            .unwrap();
        bucket.count += 1;
        let mut seen: Vec<&str> = Vec::new();
        for term in &matched {
            if !seen.contains(term) {
                seen.push(term);
                bucket.terms.add(term);
            }
        }
        if bucket.samples.len() < sample_limit {
            bucket.samples.push(JudgementSample {
                index: item.index,
                line_no: item.line_no,
                terms: matched.iter().map(|s| s.to_string()).collect(),
                text: item.text.clone(),
            });
        }
    }
    let mut out: Vec<JudgementContext> = Vec::new();
    for (i, &context) in ["narration", "dialogue"].iter().enumerate() {
        let bucket = &buckets[i].as_ref().unwrap();
        if bucket.count == 0 {
            continue;
        }
        let warn = context == "narration" && bucket.count >= 3;
        let watch = (context == "narration" && bucket.count >= 2)
            || (context == "dialogue" && bucket.count >= 8);
        let top_terms = bucket
            .terms
            .most_common(6)
            .into_iter()
            .map(|(term, count)| TermCount { term, count })
            .collect();
        out.push(JudgementContext {
            context: context.to_string(),
            label: bucket.label.to_string(),
            count: bucket.count,
            warn,
            watch,
            top_terms,
            samples: bucket.samples.clone(),
        });
    }
    out
}

/// 句首短语（对齐 `leading_phrase`：剥前缀后取前 max_len 码点）。
fn leading_phrase(sentence: &str, max_len: usize) -> String {
    prefix_chars(lstrip_chars(sentence, LEADING_PUNCT), max_len)
}

/// 重复句首（对齐 `collect_sentence_starts`）。
fn collect_sentence_starts(sentences: &[String]) -> Vec<(String, usize)> {
    let mut counts = Counter::default();
    for sentence in sentences {
        let lead = leading_phrase(sentence, 8);
        if code_len(&lead) < 2 {
            continue;
        }
        counts.add(&lead);
    }
    counts.most_common_all()
}

/// 主语起手（对齐 `collect_subject_leads`，≥3 才保留）。
fn collect_subject_leads(ctx: &DraftContext, sentences: &[String]) -> Vec<(String, usize)> {
    let mut counts = Counter::default();
    for sentence in sentences {
        let lead = lstrip_chars(sentence, LEADING_PUNCT);
        for candidate in &ctx.lexicon().subject_leads {
            if lead.starts_with(candidate.as_str()) {
                counts.add(candidate);
                break;
            }
        }
    }
    counts
        .most_common_all()
        .into_iter()
        .filter(|(_, c)| *c >= 3)
        .collect()
}

/// 段首起手（对齐 `collect_paragraph_leads`，≥3 才保留）。
fn collect_paragraph_leads(ctx: &DraftContext, paragraphs: &[String]) -> Vec<(String, usize)> {
    let mut counts = Counter::default();
    for paragraph in paragraphs {
        let lead = lstrip_chars(paragraph, LEADING_PUNCT);
        for candidate in &ctx.lexicon().paragraph_leads {
            if lead.starts_with(candidate.as_str()) {
                counts.add(candidate);
                break;
            }
        }
    }
    counts
        .most_common_all()
        .into_iter()
        .filter(|(_, c)| *c >= 3)
        .collect()
}

/// 连接词句首（对齐 `collect_connective_sentence_patterns`）。
fn collect_connective_sentence_patterns(
    ctx: &DraftContext,
    sentences: &[String],
) -> Vec<(String, usize)> {
    let mut counts = Counter::default();
    for sentence in sentences {
        let lead = lstrip_chars(sentence, LEADING_PUNCT);
        for (label, regex) in &ctx.connective_patterns {
            if regex.find(lead).ok().flatten().is_some() {
                counts.add(label);
                break;
            }
        }
    }
    counts.most_common_all()
}

/// 分句骨架（对齐 `collect_clause_prefixes`，取分句前 4 码点，≥4 才保留）。
fn collect_clause_prefixes(ctx: &DraftContext, sentences: &[String]) -> Vec<(String, usize)> {
    let mut counts = Counter::default();
    for sentence in sentences {
        for clause in split_clauses(ctx, sentence) {
            let lead = lstrip_chars(&clause, LEADING_PUNCT);
            if code_len(lead) < 2 {
                continue;
            }
            counts.add(&prefix_chars(lead, 4));
        }
    }
    counts
        .most_common_all()
        .into_iter()
        .filter(|(_, c)| *c >= 4)
        .collect()
}

/// 分句切分（`CLAUSE_SPLIT`：`[，；：]`）。
fn split_clauses(ctx: &DraftContext, sentence: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut last = 0usize;
    for m in ctx
        .clause_split_regex
        .find_iter(sentence)
        .filter_map(|m| m.ok())
    {
        out.push(sentence[last..m.start()].to_string());
        last = m.end();
    }
    out.push(sentence[last..].to_string());
    out
}

/// 相邻分句骨架对（对齐 `collect_parallel_clauses`，≥4 才保留）。
fn collect_parallel_clauses(ctx: &DraftContext, sentences: &[String]) -> Vec<(String, usize)> {
    let mut counts = Counter::default();
    for sentence in sentences {
        if !sentence.contains('，') {
            continue;
        }
        let clauses: Vec<String> = split_clauses(ctx, sentence)
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        for pair in clauses.windows(2) {
            let left = &pair[0];
            let right = &pair[1];
            let left_lead = prefix_chars(left, 2);
            let right_lead = prefix_chars(right, 2);
            if code_len(&left_lead) < 2 || code_len(&right_lead) < 2 {
                continue;
            }
            counts.add(&format!("{left_lead}/{right_lead}"));
        }
    }
    counts
        .most_common_all()
        .into_iter()
        .filter(|(_, c)| *c >= 4)
        .collect()
}

/// 修饰/动作词压力（对齐 `collect_modifier_pressure`）。
fn collect_modifier_pressure(ctx: &DraftContext, sentences: &[String]) -> Vec<ModifierPressure> {
    let lex = ctx.lexicon();
    let mut findings: Vec<ModifierPressure> = Vec::new();
    for (label, hints) in [
        ("形容词提示", &lex.adjective_hints),
        ("动词提示", &lex.verb_hints),
    ] {
        let mut total = 0usize;
        let mut hit_sentences = 0usize;
        for sentence in sentences {
            let count = hints
                .iter()
                .map(|h| sentence.matches(h.as_str()).count())
                .sum::<usize>();
            total += count;
            if count >= 3 {
                hit_sentences += 1;
            }
        }
        if total > 0 {
            findings.push(ModifierPressure {
                label: label.to_string(),
                total,
                dense_sentences: hit_sentences,
                warn: hit_sentences >= 4,
            });
        }
    }
    findings
}

/// 判断句收束（对齐 `collect_judgement_endings`，≥2 才保留）。
fn collect_judgement_endings(ctx: &DraftContext, sentences: &[String]) -> Vec<(String, usize)> {
    let mut counts = Counter::default();
    for sentence in sentences {
        let stripped = sentence.trim();
        for ending in &ctx.lexicon().judgement_endings {
            if stripped.ends_with(ending.as_str()) {
                counts.add(ending);
                break;
            }
        }
    }
    counts
        .most_common_all()
        .into_iter()
        .filter(|(_, c)| *c >= 2)
        .collect()
}

/// AA/BB/重叠词模式（对齐 `collect_aa_bb_patterns`）。
fn collect_aa_bb_patterns(
    ctx: &DraftContext,
    sentences: &[String],
    sample_limit: usize,
) -> Vec<AaBbPattern> {
    let mut balanced_samples: Vec<(String, Vec<String>)> = Vec::new();
    let mut redup_counts = Counter::default();
    let mut redup_samples: Vec<(String, Vec<String>)> = Vec::new();
    for sentence in sentences {
        let stripped = sentence.trim();
        if stripped.is_empty() || code_len(stripped) > 120 {
            continue;
        }
        for m in ctx.aa_bb_regex.find_iter(stripped).filter_map(|m| m.ok()) {
            let token = m.as_str().to_string();
            redup_counts.add(&token);
            let entry = redup_samples.iter_mut().find(|(name, _)| name == &token);
            if let Some(e) = entry {
                if e.1.len() < sample_limit {
                    e.1.push(stripped.to_string());
                }
            } else {
                redup_samples.push((
                    token,
                    if sample_limit > 0 {
                        vec![stripped.to_string()]
                    } else {
                        Vec::new()
                    },
                ));
            }
        }
        if !stripped.contains('，') {
            continue;
        }
        let clauses: Vec<String> = split_clauses(ctx, stripped)
            .into_iter()
            .map(|c| strip_chars(&c, LEADING_PUNCT).to_string())
            .filter(|c| !c.is_empty())
            .collect();
        if clauses.len() < 3 {
            continue;
        }
        let clause_lengths: Vec<usize> = clauses.iter().map(|c| prose_char_count(c)).collect();
        for start in 0..clauses.len().saturating_sub(2) {
            for end in (start + 3)..=clauses.len().min(start + 5) {
                let window = &clause_lengths[start..end];
                let lo = window.iter().min().copied().unwrap_or(0);
                let hi = window.iter().max().copied().unwrap_or(0);
                if lo < 2 || hi > 10 || hi - lo > 2 {
                    continue;
                }
                let shape = window
                    .iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join("/");
                let label = format!("短分句排比 {shape}");
                let entry = balanced_samples.iter_mut().find(|(name, _)| name == &label);
                if let Some(e) = entry {
                    if e.1.len() < sample_limit {
                        e.1.push(stripped.to_string());
                    }
                } else {
                    balanced_samples.push((
                        label,
                        if sample_limit > 0 {
                            vec![stripped.to_string()]
                        } else {
                            Vec::new()
                        },
                    ));
                }
                break;
            }
        }
    }
    let mut findings: Vec<AaBbPattern> = Vec::new();
    {
        let mut sorted = balanced_samples.clone();
        sorted.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
        for (label, samples) in sorted {
            findings.push(AaBbPattern {
                pattern_type: "balanced_clauses".into(),
                name: label,
                count: samples.len(),
                note: "AA/BB式短分句排比，密集时会把画面写成清单".into(),
                warn: samples.len() >= 2,
                samples,
            });
        }
    }
    for (token, count) in redup_counts.most_common(12) {
        let samples = redup_samples
            .iter()
            .find(|(name, _)| name == &token)
            .map(|(_, s)| s.clone())
            .unwrap_or_default();
        findings.push(AaBbPattern {
            pattern_type: "reduplicative_word".into(),
            name: token,
            count,
            note: "重叠词节奏，重复后会暴露手癖".into(),
            warn: count >= 4,
            samples,
        });
    }
    findings
}

/// `QUOTE_LINE`（`^\s*[“"【].*`）：对白行判定（对齐 Python `QUOTE_LINE.match`）。
fn is_quote_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with('\u{201c}') || trimmed.starts_with('"') || trimmed.starts_with('\u{3010}')
}

/// 逐段对白判定（Python 检测函数共用：全行对白或单行引号段）。
fn paragraph_is_dialogue(paragraph: &str) -> Option<String> {
    let stripped = paragraph.trim();
    if stripped.is_empty() {
        return None;
    }
    let lines: Vec<&str> = stripped
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    let all = lines.iter().all(|l| is_quote_line(l));
    let single = lines.len() == 1 && quote_ratio(lines[0]) > 0.02 && lines[0].contains('\u{201c}');
    if all || single {
        Some(lines[0].to_string())
    } else {
        None
    }
}

/// 连续引用段（对齐 `detect_dialogue_runs`：≥4 段，1-based 段号）。
fn detect_dialogue_runs(text: &str) -> Vec<(usize, usize, Vec<String>)> {
    let mut runs: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut start_idx = 0usize;
    let paragraphs: Vec<String> = text.split("\n\n").map(|s| s.to_string()).collect();
    let flush = |runs: &mut Vec<(usize, usize, Vec<String>)>,
                 current: &mut Vec<String>,
                 start_idx: usize,
                 end_idx: usize| {
        if current.len() >= 4 {
            let sample = current.iter().take(4).cloned().collect();
            runs.push((start_idx + 1, end_idx, sample));
        }
        current.clear();
    };
    for (idx, para) in paragraphs.iter().enumerate() {
        match paragraph_is_dialogue(para) {
            Some(first) => {
                if current.is_empty() {
                    start_idx = idx;
                }
                current.push(first);
            }
            None => {
                flush(&mut runs, &mut current, start_idx, idx);
            }
        }
    }
    flush(&mut runs, &mut current, start_idx, paragraphs.len());
    runs
}

/// 短对白连续段（对齐 `detect_short_dialogue_runs`：≥4 段且平均 ≤15 字）。
fn detect_short_dialogue_runs(text: &str) -> Vec<(usize, usize, f64, Vec<String>)> {
    let mut flagged: Vec<(usize, usize, f64, Vec<String>)> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut start_idx = 0usize;
    let paragraphs: Vec<String> = text.split("\n\n").map(|s| s.to_string()).collect();
    let flush = |flagged: &mut Vec<(usize, usize, f64, Vec<String>)>,
                 current: &mut Vec<String>,
                 start_idx: usize,
                 end_idx: usize| {
        if current.len() >= 4 {
            let total: usize = current
                .iter()
                .map(|item| code_len(strip_chars(item, "\u{201c}\u{201d}\"")))
                .sum();
            let avg_len = total as f64 / current.len() as f64;
            if avg_len <= 15.0 {
                let sample = current.iter().take(4).cloned().collect();
                flagged.push((start_idx + 1, end_idx, round2(avg_len), sample));
            }
        }
        current.clear();
    };
    for (idx, para) in paragraphs.iter().enumerate() {
        match paragraph_is_dialogue(para) {
            Some(first) => {
                if current.is_empty() {
                    start_idx = idx;
                }
                current.push(first);
            }
            None => {
                flush(&mut flagged, &mut current, start_idx, idx);
            }
        }
    }
    flush(&mut flagged, &mut current, start_idx, paragraphs.len());
    flagged
}

/// 短问句乒乓（对齐 `detect_question_ping_pong`：≥3 段，问句且 ≤18 字）。
fn detect_question_ping_pong(text: &str) -> Vec<(usize, usize, Vec<String>)> {
    let mut flagged: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut start_idx = 0usize;
    let paragraphs: Vec<String> = text.split("\n\n").map(|s| s.to_string()).collect();
    let flush = |flagged: &mut Vec<(usize, usize, Vec<String>)>,
                 current: &mut Vec<String>,
                 start_idx: usize,
                 end_idx: usize| {
        if current.len() >= 3 {
            let sample = current.iter().take(4).cloned().collect();
            flagged.push((start_idx + 1, end_idx, sample));
        }
        current.clear();
    };
    for (idx, para) in paragraphs.iter().enumerate() {
        let stripped = para.trim();
        if stripped.is_empty() {
            flush(&mut flagged, &mut current, start_idx, idx);
            continue;
        }
        let lines: Vec<&str> = stripped
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();
        if lines.len() != 1 || !lines[0].contains('\u{201c}') {
            flush(&mut flagged, &mut current, start_idx, idx);
            continue;
        }
        let line = lines[0];
        let is_question = line.contains('？');
        let short_line = code_len(strip_chars(line, "\u{201c}\u{201d}\"")) <= 18;
        if is_question && short_line {
            if current.is_empty() {
                start_idx = idx;
            }
            current.push(line.to_string());
        } else {
            flush(&mut flagged, &mut current, start_idx, idx);
        }
    }
    flush(&mut flagged, &mut current, start_idx, paragraphs.len());
    flagged
}

/// 引号乒乓（对齐 `detect_quote_ping_pong`：≥4 段且平均 ≤22 字）。
fn detect_quote_ping_pong(text: &str) -> Vec<(usize, usize, f64, Vec<String>)> {
    let mut flagged: Vec<(usize, usize, f64, Vec<String>)> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut start_idx = 0usize;
    let paragraphs: Vec<String> = text.split("\n\n").map(|s| s.to_string()).collect();
    let flush = |flagged: &mut Vec<(usize, usize, f64, Vec<String>)>,
                 current: &mut Vec<String>,
                 start_idx: usize,
                 end_idx: usize| {
        if current.len() >= 4 {
            let total: usize = current
                .iter()
                .map(|item| code_len(strip_chars(item, "\u{201c}\u{201d}\"")))
                .sum();
            let avg_len = total as f64 / current.len() as f64;
            if avg_len <= 22.0 {
                let sample = current.iter().take(6).cloned().collect();
                flagged.push((start_idx + 1, end_idx, round2(avg_len), sample));
            }
        }
        current.clear();
    };
    for (idx, para) in paragraphs.iter().enumerate() {
        let stripped = para.trim();
        if stripped.is_empty() {
            flush(&mut flagged, &mut current, start_idx, idx);
            continue;
        }
        let lines: Vec<&str> = stripped
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();
        if lines.len() != 1 || !lines[0].contains('\u{201c}') {
            flush(&mut flagged, &mut current, start_idx, idx);
            continue;
        }
        if current.is_empty() {
            start_idx = idx;
        }
        current.push(lines[0].to_string());
    }
    flush(&mut flagged, &mut current, start_idx, paragraphs.len());
    flagged
}

/// A/B 交替说话人（对齐 `detect_a_b_turns`：4 连 `A->B->A->B`，1-based 段号）。
fn detect_a_b_turns(ctx: &DraftContext, text: &str) -> Vec<AbTurn> {
    let paragraphs: Vec<String> = text.split("\n\n").map(|s| s.to_string()).collect();
    let mut speakers: Vec<(usize, String)> = Vec::new();
    for (i, para) in paragraphs.iter().enumerate() {
        let stripped = para.trim();
        if stripped.is_empty() {
            continue;
        }
        let mut found: Option<String> = None;
        for pattern in &ctx.speaker_patterns {
            if let Ok(Some(caps)) = pattern.captures(stripped) {
                let mut expanded = String::new();
                caps.expand("$1", &mut expanded);
                found = Some(expanded);
                break;
            }
        }
        if let Some(speaker) = found {
            if !speaker.is_empty() {
                speakers.push((i + 1, speaker));
            }
        }
    }
    let mut flagged: Vec<AbTurn> = Vec::new();
    for idx in 0..speakers.len().saturating_sub(3) {
        let seq = &speakers[idx..idx + 4];
        let names: Vec<&str> = seq.iter().map(|(_, s)| s.as_str()).collect();
        if names[0] == names[2] && names[1] == names[3] && names[0] != names[1] {
            flagged.push(AbTurn {
                paragraph: seq[0].0,
                pattern: names.join(" -> "),
            });
        }
    }
    flagged
}

/// ngram 高频词（对齐 `collect_ngram_terms`：sizes 按传入顺序扫描，
/// 平手按首现序；covered = 被更长且计数不少于自身的已收短语包含）。
fn collect_ngram_terms(
    ctx: &DraftContext,
    text: &str,
    min_count_by_size: &[(usize, usize)],
    require_structure: bool,
) -> Vec<(String, usize)> {
    let cleaned: String = ctx.ngram_keep_regex.replace_all(text, "").into_owned();
    let chars: Vec<char> = cleaned.chars().collect();
    let n = chars.len();
    let mut counts = Counter::default();
    let word_stoplist: std::collections::HashSet<&str> = ctx
        .lexicon()
        .word_stoplist
        .iter()
        .map(|s| s.as_str())
        .collect();
    let structure_chars: std::collections::HashSet<char> =
        ctx.lexicon().structure_chars.chars().collect();
    for (size, _min) in min_count_by_size {
        if n < *size {
            continue;
        }
        for idx in 0..=n - size {
            let phrase: String = chars[idx..idx + size].iter().collect();
            if word_stoplist.contains(phrase.as_str()) {
                continue;
            }
            if is_whole_match(&ctx.ascii_alpha_regex, &phrase) {
                continue;
            }
            if chars[idx..idx + size].iter().all(|c| *c == '一') {
                continue;
            }
            if require_structure
                && !chars[idx..idx + size]
                    .iter()
                    .any(|c| structure_chars.contains(c))
            {
                continue;
            }
            counts.add(&phrase);
        }
    }
    let min_for = |len: usize| -> usize {
        min_count_by_size
            .iter()
            .find(|(size, _)| *size == len)
            .map(|(_, min)| *min)
            .unwrap_or(99)
    };
    let mut filtered: Vec<(String, usize)> = counts
        .entries()
        .iter()
        .filter(|(phrase, count)| *count >= min_for(code_len(phrase)))
        .map(|(phrase, count)| (phrase.clone(), *count))
        .collect();
    filtered.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then(code_len(&b.0).cmp(&code_len(&a.0)))
            .then_with(|| a.0.cmp(&b.0))
    });
    let mut deduped: Vec<(String, usize)> = Vec::new();
    for (phrase, count) in filtered {
        let covered = deduped.iter().any(|(kept_phrase, kept_count)| {
            phrase != *kept_phrase && phrase.contains(kept_phrase.as_str()) && kept_count >= &count
        });
        if !covered {
            deduped.push((phrase, count));
        }
    }
    deduped
}

/// 语料词是否有用（对齐 `_is_useful_corpus_term`）。
fn is_useful_corpus_term(ctx: &DraftContext, term: &str, category: &str) -> bool {
    let lex = ctx.lexicon();
    if lex.corpus_stop_terms.iter().any(|t| t == term) {
        return false;
    }
    if code_len(term) < 2 {
        return false;
    }
    if is_whole_match(&ctx.alpha_numeric_regex, term) {
        return false;
    }
    if !term.is_empty() && term.chars().all(|c| c == term.chars().next().unwrap()) {
        return false;
    }
    !(category == "learned_term"
        && code_len(term) == 2
        && !lex.allowed_short_corpus_terms.iter().any(|t| t == term))
}

/// 由 ngram 词表生成学习模式（对齐 `_learned_patterns_from_terms`，截到 learned_filter_limit）。
fn learned_patterns_from_terms(
    ctx: &DraftContext,
    category: &str,
    raw_terms: &[(String, usize)],
    corpus_chars: usize,
    min_per_10k_floor: f64,
    multiplier: f64,
    note: &str,
) -> Vec<LearnedPattern> {
    let limit = ctx.thresholds().learned_filter_limit as usize;
    let mut patterns: Vec<LearnedPattern> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (term, count) in raw_terms {
        if seen.contains(term.as_str()) || !is_useful_corpus_term(ctx, term, category) {
            continue;
        }
        let corpus_per_10k = density(*count, corpus_chars);
        patterns.push(LearnedPattern {
            category: category.to_string(),
            name: term.clone(),
            count: *count,
            corpus_per_10k: round2(corpus_per_10k),
            max_per_10k: round2((corpus_per_10k * multiplier).max(min_per_10k_floor)),
            note: note.to_string(),
        });
        seen.insert(term.as_str());
        if patterns.len() >= limit {
            break;
        }
    }
    patterns
}

/// 语料画像（对齐 `build_corpus_profile`；无可用语料时返回 None）。
pub fn build_corpus_profile(
    ctx: &DraftContext,
    paths: &[PathBuf],
) -> Result<Option<CorpusProfile>> {
    let files: Vec<PathBuf> = iter_target_files(paths)
        .into_iter()
        .filter(|p| {
            p.extension()
                .map(|e| e == "md" || e == "txt")
                .unwrap_or(false)
                && !is_generated_or_template(p)
        })
        .collect();
    if files.is_empty() {
        return Ok(None);
    }
    let mut all_text_parts: Vec<String> = Vec::new();
    let mut draft_text_parts: Vec<String> = Vec::new();
    for path in &files {
        let cleaned = clean_corpus_text(ctx, path)?;
        if cleaned.is_empty() {
            continue;
        }
        all_text_parts.push(cleaned.clone());
        if path.components().any(|c| c.as_os_str() == "drafts") {
            draft_text_parts.push(cleaned);
        }
    }
    let all_text = all_text_parts.join("\n\n");
    let draft_text = if draft_text_parts.is_empty() {
        all_text.clone()
    } else {
        draft_text_parts.join("\n\n")
    };
    let corpus_chars = all_text.chars().filter(|c| *c != '\n').count();
    let draft_chars = draft_text.chars().filter(|c| *c != '\n').count();
    if corpus_chars == 0 {
        return Ok(None);
    }
    let raw_terms = collect_ngram_terms(ctx, &all_text, &[(2, 30), (3, 18), (4, 12)], false);
    let mut style_raw_terms =
        collect_ngram_terms(ctx, &draft_text, &[(2, 24), (3, 14), (4, 10)], true);
    let style_chars: std::collections::HashSet<char> = "得像把还说看没不只更在就".chars().collect();
    style_raw_terms.retain(|(term, _)| term.chars().any(|c| style_chars.contains(&c)));
    let learned_terms = learned_patterns_from_terms(
        ctx,
        "learned_term",
        &raw_terms,
        corpus_chars,
        10.0,
        1.25,
        "从卡片/大纲/草稿语料学到的高频实体或动作词，当前章过线时要查是否点名过密",
    );
    let learned_style_phrases = learned_patterns_from_terms(
        ctx,
        "learned_style_phrase",
        &style_raw_terms,
        std::cmp::max(draft_chars, 1),
        5.0,
        1.15,
        "从现有草稿学到的高频句法手势，当前章过线时优先改写",
    );
    let draft_sentences = ctx.splitter().split_sentences(&draft_text);
    let limit = ctx.thresholds().learned_filter_limit as usize;
    let sentence_leads: Vec<LearnedSentenceLead> = collect_sentence_starts(&draft_sentences)
        .into_iter()
        .filter(|(phrase, count)| {
            *count >= 6
                && !ctx
                    .corpus_markdown_noise_regex
                    .is_match(phrase)
                    .unwrap_or(false)
        })
        .map(|(phrase, count)| LearnedSentenceLead {
            phrase,
            count,
            corpus_per_10k: round2(density(count, draft_chars.max(1))),
        })
        .take(limit)
        .collect();
    let aa_bb_shapes: Vec<LearnedAaBbShape> = collect_aa_bb_patterns(ctx, &draft_sentences, 1)
        .into_iter()
        .filter(|item| item.count >= 2)
        .take(limit)
        .map(|item| LearnedAaBbShape {
            name: item.name,
            count: item.count,
            note: item.note,
        })
        .collect();
    let baseline_profile =
        build_sentence_length_profile(ctx, &ctx.splitter().split_sentence_infos(&draft_text));
    Ok(Some(CorpusProfile {
        source_count: files.len(),
        chars: corpus_chars,
        draft_chars,
        learned_terms,
        learned_style_phrases,
        learned_sentence_leads: sentence_leads,
        learned_aa_bb_shapes: aa_bb_shapes,
        sentence_length_baseline: Some(SentenceLengthBaseline {
            sentence_count: baseline_profile.count,
            p10_chars: baseline_profile.p10_chars,
            p25_chars: baseline_profile.p25_chars,
            median_chars: baseline_profile.median_chars,
            avg_chars: baseline_profile.avg_chars,
            short_ratio: baseline_profile.short_ratio,
        }),
    }))
}

/// 语料学习词/句法手势的当前章指标（对齐 `build_learned_filter_metrics`：
/// 计数 ≥2 才纳入；排序 = warn 优先、计数降序、category/name 升序）。
pub fn build_learned_filter_metrics(
    corpus: Option<&CorpusProfile>,
    lines: &[String],
    chars: usize,
    sample_limit: usize,
) -> Vec<LearnedFilterMetric> {
    let Some(corpus) = corpus else {
        return Vec::new();
    };
    let mut metrics: Vec<LearnedFilterMetric> = Vec::new();
    for rule in corpus
        .learned_terms
        .iter()
        .chain(corpus.learned_style_phrases.iter())
    {
        let escaped = fancy_regex::escape(&rule.name);
        let Ok(re) = fancy_regex::Regex::new(&escaped) else {
            continue;
        };
        let (count, hits) = crate::rules::find_hits(&re, lines, sample_limit);
        if count < 2 {
            continue;
        }
        let per_10k = crate::rules::density(count, chars);
        let flag = count >= 2 && per_10k > rule.max_per_10k;
        metrics.push(LearnedFilterMetric {
            category: rule.category.clone(),
            name: rule.name.clone(),
            count,
            per_10k: round2(per_10k),
            corpus_per_10k: rule.corpus_per_10k,
            max_per_10k: rule.max_per_10k,
            note: rule.note.clone(),
            warn: flag,
            samples: hits,
        });
    }
    metrics.sort_by(|a, b| {
        b.warn
            .cmp(&a.warn)
            .then(b.count.cmp(&a.count))
            .then_with(|| a.category.cmp(&b.category))
            .then_with(|| a.name.cmp(&b.name))
    });
    metrics
}

/// 词表命中总数（对齐 `_count_term_hits`：`text.count(term)` 求和）。
fn count_term_hits(text: &str, terms: &[String]) -> usize {
    terms.iter().map(|t| text.matches(t.as_str()).count()).sum()
}

/// 段落功能分类（对齐 `classify_paragraph_role`）。
fn classify_paragraph_role(
    ctx: &DraftContext,
    paragraph: &crate::text::ParagraphInfo,
) -> &'static str {
    let stripped = lstrip_chars(paragraph.text.trim(), LEADING_PUNCT);
    if paragraph.is_dialogue {
        return "dialogue";
    }
    let lex = ctx.lexicon();
    let battle_terms: Vec<String> = lex
        .battle_action_terms
        .iter()
        .chain(lex.battle_damage_terms.iter())
        .chain(lex.battle_result_terms.iter())
        .cloned()
        .collect();
    let battle_hits = count_term_hits(stripped, &battle_terms);
    let action_hits = count_term_hits(stripped, &lex.verb_hints)
        + usize::from(ctx.ba_regex.is_match(stripped).unwrap_or(false));
    let info_hits =
        count_term_hits(stripped, &lex.paragraph_info_terms) + stripped.matches('\u{ff1a}').count();
    let emotion_terms: Vec<String> = lex
        .short_role_emotion_terms
        .iter()
        .chain(lex.mental_state_terms.iter())
        .cloned()
        .collect();
    let emotion_hits = count_term_hits(stripped, &emotion_terms);
    let tone_hits: usize = lex
        .tone_rules
        .rules
        .iter()
        .map(|(_, terms)| count_term_hits(stripped, terms))
        .sum();
    if battle_hits >= 2 {
        return "battle";
    }
    if info_hits >= 3 && info_hits >= action_hits {
        return "info";
    }
    if action_hits >= 3 && action_hits >= emotion_hits {
        return "action";
    }
    if emotion_hits >= 2 {
        return "emotion";
    }
    if tone_hits >= 2 {
        return "environment";
    }
    "mixed"
}

/// 收尾一个功能块（对齐 `build_scene_map` 内部 `flush`；拆成函数避免借用冲突）。
fn flush_scene_block(
    blocks: &mut Vec<SceneBlock>,
    current_role: &mut String,
    current_items: &mut Vec<&crate::text::ParagraphInfo>,
) {
    if current_items.is_empty() {
        return;
    }
    blocks.push(SceneBlock {
        role: if current_role.is_empty() {
            "mixed".to_string()
        } else {
            current_role.clone()
        },
        start_paragraph: current_items[0].index,
        end_paragraph: current_items.last().unwrap().index,
        start_line: current_items[0].line_start,
        end_line: current_items.last().unwrap().line_end,
        paragraphs: current_items.len(),
        chars: current_items.iter().map(|i| i.chars).sum(),
        sample: current_items
            .iter()
            .take(2)
            .map(|i| prefix_chars(i.text.replace('\n', " ").as_str(), 80))
            .collect(),
    });
    current_items.clear();
    current_role.clear();
}

/// 粗分块功能地图（对齐 `build_scene_map`：换功能 ≥2 块才切断；
/// 对白→非对白立即切；`scene_break_leads` 起手强切）。
pub fn build_scene_map(
    ctx: &DraftContext,
    paragraph_infos: &[crate::text::ParagraphInfo],
    sample_limit: usize,
) -> SceneMap {
    if paragraph_infos.is_empty() {
        return SceneMap {
            blocks: Vec::new(),
            role_counts: CountMap::default(),
            dominant_role: "mixed".into(),
            dominance_ratio: 0.0,
            warn: false,
            block_count: 0,
            switch_count: 0,
        };
    }
    let mut blocks: Vec<SceneBlock> = Vec::new();
    let mut current_role = String::new();
    let mut current_items: Vec<&crate::text::ParagraphInfo> = Vec::new();
    for info in paragraph_infos {
        let role = classify_paragraph_role(ctx, info);
        let stripped = lstrip_chars(info.text.trim(), LEADING_PUNCT);
        let lex = ctx.lexicon();
        let force_break = !current_items.is_empty()
            && (lex
                .scene_break_leads
                .iter()
                .any(|term| stripped.starts_with(term.as_str()))
                || (current_role == "dialogue" && role != "dialogue")
                || (current_role != role && current_items.len() >= 2));
        if force_break {
            flush_scene_block(&mut blocks, &mut current_role, &mut current_items);
        }
        if current_items.is_empty() {
            current_role = role.to_string();
        }
        current_items.push(info);
    }
    flush_scene_block(&mut blocks, &mut current_role, &mut current_items);
    let mut role_counter = Counter::default();
    for block in &blocks {
        role_counter.add(&block.role);
    }
    let (dominant_role, dominant_count) = role_counter
        .most_common(1)
        .into_iter()
        .next()
        .unwrap_or_else(|| ("mixed".to_string(), 0));
    let dominance_ratio = round4f(dominant_count as f64 / blocks.len().max(1) as f64);
    let warn = blocks.len() >= 4
        && (dominant_role == "dialogue" || dominant_role == "info")
        && dominance_ratio >= 0.6;
    let block_count = blocks.len();
    SceneMap {
        blocks: blocks.into_iter().take(sample_limit * 2).collect(),
        role_counts: CountMap::new(role_counter.entries().to_vec()),
        dominant_role,
        dominance_ratio,
        warn,
        block_count,
        switch_count: block_count.saturating_sub(1),
    }
}

/// 情绪规则标签（对齐 `classify_dialogue_emotion`：首条命中规则优先）。
fn classify_dialogue_emotion<'a>(ctx: &'a DraftContext, text: &str) -> &'a str {
    let lex = ctx.lexicon();
    for (label, terms) in &lex.dialogue_emotion_rules.rules {
        if terms.iter().any(|term| text.contains(term)) {
            return label.as_str();
        }
    }
    if text.contains('？') || text.contains('?') {
        return "pressure";
    }
    if text.contains('\u{ff01}') {
        return "hostility";
    }
    "neutral"
}

/// 对白情绪曲线（对齐 `build_dialogue_emotion_profile`）。
pub fn build_dialogue_emotion_profile(
    ctx: &DraftContext,
    sentence_infos: &[crate::text::SentenceInfo],
    sample_limit: usize,
) -> DialogueEmotions {
    let dialogue_items: Vec<&crate::text::SentenceInfo> = sentence_infos
        .iter()
        .filter(|item| is_dialogue_like(&item.text))
        .collect();
    let mut emotion_counter = Counter::default();
    let mut samples: Vec<EmotionSample> = Vec::new();
    let mut last_label = String::new();
    let mut shift_count = 0usize;
    for item in &dialogue_items {
        let text = item.text.trim();
        let lex = ctx.lexicon();
        let mut labels: Vec<String> = lex
            .dialogue_emotion_rules
            .rules
            .iter()
            .filter(|(_, terms)| terms.iter().any(|term| text.contains(term)))
            .map(|(label, _)| label.clone())
            .collect();
        if labels.is_empty() {
            if text.contains('？') || text.contains('?') {
                labels = vec!["pressure".to_string()];
            } else if text.contains('\u{ff01}') {
                labels = vec!["hostility".to_string()];
            }
        }
        let primary = labels.first().map_or("neutral", |s| s.as_str());
        emotion_counter.add(primary);
        if !samples.is_empty()
            && primary != "neutral"
            && !last_label.is_empty()
            && primary != last_label
        {
            shift_count += 1;
        }
        if primary != "neutral" {
            last_label = primary.to_string();
        }
        if samples.len() < sample_limit {
            samples.push(EmotionSample {
                line_no: item.line_no,
                label: primary.to_string(),
                labels: if labels.is_empty() {
                    vec!["neutral".to_string()]
                } else {
                    labels.clone()
                },
                text: text.to_string(),
            });
        }
    }
    let (dominant_label, dominant_count) = emotion_counter
        .most_common(1)
        .into_iter()
        .next()
        .unwrap_or_else(|| ("neutral".to_string(), 0));
    let non_neutral: usize = emotion_counter
        .entries()
        .iter()
        .filter(|(label, _)| label.as_str() != "neutral")
        .map(|(_, count)| *count)
        .sum();
    let flatness_warn = dialogue_items.len() >= 6
        && dominant_label != "neutral"
        && dominant_count as f64 / dialogue_items.len().max(1) as f64 >= 0.7;
    let volatility_warn = shift_count >= 4 && non_neutral >= 5;
    DialogueEmotions {
        dialogue_sentences: dialogue_items.len(),
        emotion_counts: CountMap::new(emotion_counter.entries().to_vec()),
        dominant_emotion: dominant_label,
        dominant_ratio: if dialogue_items.is_empty() {
            0.0
        } else {
            round4f(dominant_count as f64 / dialogue_items.len().max(1) as f64)
        },
        shift_count,
        flatness_warn,
        volatility_warn,
        samples,
    }
}

/// 从对白句提取说话人名（对齐 `extract_speaker_name`）。
fn extract_speaker_name(ctx: &DraftContext, text: &str) -> String {
    let cleaned = text.trim();
    let lex = ctx.lexicon();
    let suffixes = &ctx.rules.draft.speaker.suffixes;
    for pattern in &ctx.speaker_line_patterns {
        let Ok(Some(caps)) = pattern.captures(cleaned) else {
            continue;
        };
        let mut raw = String::new();
        caps.expand("$1", &mut raw);
        let mut name = strip_chars(&raw, LEADING_PUNCT).to_string();
        for sfx in suffixes {
            if name.ends_with(sfx.as_str()) && code_len(&name) > code_len(sfx) {
                name = prefix_chars(&name, code_len(&name) - code_len(sfx))
                    .trim()
                    .to_string();
                break;
            }
        }
        let first = name.chars().next();
        let rest: String = name.chars().skip(1).collect();
        if code_len(&name) >= 2
            && matches!(
                first,
                Some('\u{4ed6}') | Some('\u{5979}') | Some('\u{6211}') | Some('\u{4f60}')
            )
            && suffixes.iter().any(|s| s == &rest)
        {
            name = first.unwrap().to_string();
        }
        if !name.is_empty()
            && !lex.character_name_stoplist.iter().any(|s| s == &name)
            && code_len(&name) <= 12
        {
            return name;
        }
    }
    String::new()
}

/// 角色对白画像（对齐 `build_character_voice_profile`：
/// 每角色行级统计 + 同质化对检测）。
pub fn build_character_voice_profile(
    ctx: &DraftContext,
    sentence_infos: &[crate::text::SentenceInfo],
    sample_limit: usize,
) -> CharacterVoice {
    #[derive(Debug)]
    struct SpeakerStats {
        lines: usize,
        chars: usize,
        questions: usize,
        exclaims: usize,
        judgements: usize,
        short_lines: usize,
        emotion_counter: Counter,
        samples: Vec<SpeakerSample>,
    }
    let mut speaker_counter = Counter::default();
    let mut speaker_stats: Vec<(String, SpeakerStats)> = Vec::new();
    let mut unknown_count = 0usize;
    let lex = ctx.lexicon();
    let run_max = ctx.thresholds().short_sentence_run_max_chars as usize;
    for item in sentence_infos {
        if !is_dialogue_like(&item.text) {
            continue;
        }
        let text = item.text.trim();
        let speaker = extract_speaker_name(ctx, text);
        if speaker.is_empty() {
            unknown_count += 1;
            continue;
        }
        speaker_counter.add(&speaker);
        let idx = match speaker_stats.iter().position(|(s, _)| *s == speaker) {
            Some(idx) => idx,
            None => {
                speaker_stats.push((
                    speaker.clone(),
                    SpeakerStats {
                        lines: 0,
                        chars: 0,
                        questions: 0,
                        exclaims: 0,
                        judgements: 0,
                        short_lines: 0,
                        emotion_counter: Counter::default(),
                        samples: Vec::new(),
                    },
                ));
                speaker_stats.len() - 1
            }
        };
        let entry = &mut speaker_stats[idx].1;
        entry.lines += 1;
        let pcc = prose_char_count(text);
        entry.chars += pcc;
        entry.questions += usize::from(text.contains('？') || text.contains('?'));
        entry.exclaims += usize::from(text.contains('\u{ff01}'));
        entry.judgements += usize::from(
            lex.judgement_context_terms
                .iter()
                .any(|term| text.contains(term.as_str())),
        );
        entry.short_lines += usize::from(pcc <= run_max);
        let emotion = classify_dialogue_emotion(ctx, text);
        entry.emotion_counter.add(emotion);
        if entry.samples.len() < sample_limit {
            entry.samples.push(SpeakerSample {
                line_no: item.line_no,
                text: text.to_string(),
                emotion: emotion.to_string(),
            });
        }
    }
    let mut speakers: Vec<SpeakerProfile> = Vec::new();
    for (speaker, _count) in speaker_counter.most_common(sample_limit * 2) {
        let stats = speaker_stats
            .iter()
            .find(|(s, _)| *s == speaker)
            .map(|(_, st)| st)
            .unwrap();
        let lines = stats.lines.max(1);
        let (dominant_emotion, dominant_count) = stats
            .emotion_counter
            .most_common(1)
            .into_iter()
            .next()
            .unwrap_or_else(|| ("neutral".to_string(), 0));
        speakers.push(SpeakerProfile {
            speaker,
            lines: stats.lines,
            avg_chars: round2(stats.chars as f64 / lines as f64),
            question_ratio: round4f(stats.questions as f64 / lines as f64),
            exclaim_ratio: round4f(stats.exclaims as f64 / lines as f64),
            judgement_ratio: round4f(stats.judgements as f64 / lines as f64),
            short_ratio: round4f(stats.short_lines as f64 / lines as f64),
            dominant_emotion,
            dominant_ratio: round4f(dominant_count as f64 / lines as f64),
            samples: stats.samples.iter().take(sample_limit).cloned().collect(),
        });
    }
    let identifiable_lines: usize = speakers.iter().map(|s| s.lines).sum();
    let coverage_ratio =
        round4f(identifiable_lines as f64 / (identifiable_lines + unknown_count).max(1) as f64);
    let comparable: Vec<&SpeakerProfile> = speakers.iter().filter(|s| s.lines >= 3).collect();
    let mut homogenized_pairs: Vec<String> = Vec::new();
    for (i, left) in comparable.iter().enumerate() {
        for right in &comparable[i + 1..] {
            if left.dominant_emotion == right.dominant_emotion
                && (left.avg_chars - right.avg_chars).abs() <= 3.0
                && (left.question_ratio - right.question_ratio).abs() <= 0.2
                && (left.short_ratio - right.short_ratio).abs() <= 0.2
            {
                homogenized_pairs.push(format!(
                    "{}~{} emotion={} avg={}/{}",
                    left.speaker,
                    right.speaker,
                    left.dominant_emotion,
                    py_float_str(left.avg_chars),
                    py_float_str(right.avg_chars),
                ));
            }
        }
    }
    let warn = !homogenized_pairs.is_empty() && comparable.len() >= 2;
    let dominant_speaker = speakers
        .first()
        .map(|s| s.speaker.clone())
        .unwrap_or_default();
    let dominant_ratio = if speakers.is_empty() {
        0.0
    } else {
        round4f(speakers[0].lines as f64 / identifiable_lines.max(1) as f64)
    };
    CharacterVoice {
        speaker_count: speakers.len(),
        identified_lines: identifiable_lines,
        unknown_lines: unknown_count,
        coverage_ratio,
        dominant_speaker,
        dominant_ratio,
        warn,
        homogenized_pairs: homogenized_pairs.into_iter().take(sample_limit).collect(),
        speakers: speakers.into_iter().take(sample_limit).collect(),
    }
}

/// 语气画像（对齐 `build_tone_profile`：活跃 label 取计数最高，平手取 label 序）。
pub fn build_tone_profile(
    ctx: &DraftContext,
    paragraph_infos: &[crate::text::ParagraphInfo],
    sample_limit: usize,
) -> ToneProfile {
    let mut tone_counter = Counter::default();
    let mut paragraph_tones: Vec<String> = Vec::new();
    let mut samples: Vec<ToneSample> = Vec::new();
    let mut switch_count = 0usize;
    let mut last_tone = String::new();
    for info in paragraph_infos {
        let stripped = info.text.trim();
        let lex = ctx.lexicon();
        let mut active: Vec<(&str, usize)> = Vec::new();
        for (label, terms) in &lex.tone_rules.rules {
            let count = count_term_hits(stripped, terms);
            if count > 0 {
                active.push((label.as_str(), count));
            }
        }
        if active.is_empty() {
            continue;
        }
        // Python `min(active, key=lambda item: (-item[1], item[0]))`：
        // 计数最高，平手按 label 升序取首。
        let best = active
            .iter()
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
            .unwrap();
        tone_counter.add_n(best.0, best.1);
        paragraph_tones.push(best.0.to_string());
        if !last_tone.is_empty() && best.0 != last_tone {
            switch_count += 1;
        }
        last_tone = best.0.to_string();
        if samples.len() < sample_limit {
            samples.push(ToneSample {
                paragraph: info.index,
                line_no: info.line_start,
                tone: best.0.to_string(),
                text: prefix_chars(stripped, 80),
            });
        }
    }
    let (dominant_tone, dominant_count) = tone_counter
        .most_common(1)
        .into_iter()
        .next()
        .unwrap_or_else(|| ("none".to_string(), 0));
    let total = tone_counter.total();
    let stable_ratio = if total == 0 {
        0.0
    } else {
        round4f(dominant_count as f64 / total as f64)
    };
    let distinct_count: std::collections::HashSet<&String> =
        std::collections::HashSet::from_iter(paragraph_tones.iter());
    let warn = !paragraph_tones.is_empty()
        && ((distinct_count.len() >= 4 && switch_count >= (paragraph_tones.len() / 2).max(2))
            || dominant_tone == "none");
    ToneProfile {
        tone_counts: CountMap::new(tone_counter.entries().to_vec()),
        dominant_tone,
        stable_ratio,
        switch_count,
        samples,
        warn,
    }
}

/// 战斗段画像（对齐 `build_battle_profile`：连续战斗句成段，
/// warn = 动作 ≥3 且无结果且无伤害反馈）。
pub fn build_battle_profile(
    ctx: &DraftContext,
    sentence_infos: &[crate::text::SentenceInfo],
    sample_limit: usize,
) -> BattleProfile {
    let lex = ctx.lexicon();
    let all_battle: Vec<String> = lex
        .battle_action_terms
        .iter()
        .chain(lex.battle_damage_terms.iter())
        .chain(lex.battle_result_terms.iter())
        .cloned()
        .collect();
    let mut sequences: Vec<BattleSequence> = Vec::new();
    let mut current: Vec<&crate::text::SentenceInfo> = Vec::new();
    let mut previous_index = 0usize;
    for item in sentence_infos {
        if count_term_hits(&item.text, &all_battle) >= 1 {
            if !current.is_empty() && item.index != previous_index + 1 {
                if current.len() >= 2 {
                    let joined: String = current
                        .iter()
                        .map(|i| i.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    sequences.push(BattleSequence {
                        start_index: current[0].index,
                        end_index: current.last().unwrap().index,
                        start_line: current[0].line_no,
                        end_line: current.last().unwrap().line_no,
                        sentences: current.len(),
                        action_hits: count_term_hits(&joined, &lex.battle_action_terms),
                        result_hits: count_term_hits(&joined, &lex.battle_result_terms),
                        damage_hits: count_term_hits(&joined, &lex.battle_damage_terms),
                        movement_hits: count_term_hits(&joined, &lex.battle_movement_terms),
                        sample: current.iter().take(4).map(|i| i.text.clone()).collect(),
                        warn: false,
                    });
                    let last = sequences.last_mut().unwrap();
                    last.warn =
                        last.action_hits >= 3 && last.result_hits == 0 && last.damage_hits == 0;
                }
                current.clear();
            }
            current.push(item);
            previous_index = item.index;
        } else {
            if current.len() >= 2 {
                let joined: String = current
                    .iter()
                    .map(|i| i.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                sequences.push(BattleSequence {
                    start_index: current[0].index,
                    end_index: current.last().unwrap().index,
                    start_line: current[0].line_no,
                    end_line: current.last().unwrap().line_no,
                    sentences: current.len(),
                    action_hits: count_term_hits(&joined, &lex.battle_action_terms),
                    result_hits: count_term_hits(&joined, &lex.battle_result_terms),
                    damage_hits: count_term_hits(&joined, &lex.battle_damage_terms),
                    movement_hits: count_term_hits(&joined, &lex.battle_movement_terms),
                    sample: current.iter().take(4).map(|i| i.text.clone()).collect(),
                    warn: false,
                });
                let last = sequences.last_mut().unwrap();
                last.warn = last.action_hits >= 3 && last.result_hits == 0 && last.damage_hits == 0;
            }
            current.clear();
            previous_index = item.index;
        }
    }
    if current.len() >= 2 {
        let joined: String = current
            .iter()
            .map(|i| i.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        sequences.push(BattleSequence {
            start_index: current[0].index,
            end_index: current.last().unwrap().index,
            start_line: current[0].line_no,
            end_line: current.last().unwrap().line_no,
            sentences: current.len(),
            action_hits: count_term_hits(&joined, &lex.battle_action_terms),
            result_hits: count_term_hits(&joined, &lex.battle_result_terms),
            damage_hits: count_term_hits(&joined, &lex.battle_damage_terms),
            movement_hits: count_term_hits(&joined, &lex.battle_movement_terms),
            sample: current.iter().take(4).map(|i| i.text.clone()).collect(),
            warn: false,
        });
        let last = sequences.last_mut().unwrap();
        last.warn = last.action_hits >= 3 && last.result_hits == 0 && last.damage_hits == 0;
    }
    let total_action: usize = sequences.iter().map(|s| s.action_hits).sum();
    let total_result: usize = sequences.iter().map(|s| s.result_hits).sum();
    let total_damage: usize = sequences.iter().map(|s| s.damage_hits).sum();
    let total_movement: usize = sequences.iter().map(|s| s.movement_hits).sum();
    let warn_sequences = sequences.iter().filter(|s| s.warn).count();
    let result_ratio = if total_action == 0 {
        0.0
    } else {
        round4f((total_result + total_damage) as f64 / total_action.max(1) as f64)
    };
    let warn = warn_sequences >= 2
        || (sequences.len() >= 3
            && warn_sequences as f64 / sequences.len().max(1) as f64 >= 0.5
            && total_action >= 6);
    BattleProfile {
        sequence_count: sequences.len(),
        max_sequence_sentences: sequences.iter().map(|s| s.sentences).max().unwrap_or(0),
        action_hits: total_action,
        result_hits: total_result,
        damage_hits: total_damage,
        movement_hits: total_movement,
        result_ratio,
        warn_sequences,
        warn,
        samples: sequences.into_iter().take(sample_limit).collect(),
    }
}

/// 视角锚点画像（对齐 `build_viewpoint_profile`：
/// 段内多锚 + 心理词 = 重叠；换锚计切）。
pub fn build_viewpoint_profile(
    ctx: &DraftContext,
    paragraph_infos: &[crate::text::ParagraphInfo],
    sample_limit: usize,
) -> ViewpointProfile {
    let lex = ctx.lexicon();
    let anchors = &lex.subject_leads;
    let mut overlaps: Vec<OverlapEntry> = Vec::new();
    let mut switches = 0usize;
    let mut last_anchor = String::new();
    let mut anchor_counter = Counter::default();
    let has_mental = lex.mental_state_terms.iter().any(|t| {
        let _ = t;
        false
    });
    let _ = has_mental;
    for info in paragraph_infos {
        let text = info.text.trim();
        let has_mental = lex
            .mental_state_terms
            .iter()
            .any(|t| text.contains(t.as_str()));
        let mut paragraph_anchors: Vec<String> = anchors
            .iter()
            .filter(|anchor| text.contains(anchor.as_str()) && has_mental)
            .cloned()
            .collect();
        paragraph_anchors.dedup();
        paragraph_anchors.sort();
        if paragraph_anchors.len() >= 2 && overlaps.len() < sample_limit {
            overlaps.push(OverlapEntry {
                paragraph: info.index,
                line_no: info.line_start,
                anchors: paragraph_anchors.clone(),
                text: prefix_chars(text, 100),
            });
        }
        if !paragraph_anchors.is_empty() {
            let primary = &paragraph_anchors[0];
            anchor_counter.add(primary);
            if !last_anchor.is_empty() && primary != &last_anchor {
                switches += 1;
            }
            last_anchor = primary.clone();
        }
    }
    let dominant_anchor = anchor_counter
        .most_common(1)
        .into_iter()
        .next()
        .map(|(k, _)| k)
        .unwrap_or_default();
    let warn = !overlaps.is_empty() || switches >= 3;
    ViewpointProfile {
        anchor_counts: CountMap::new(anchor_counter.entries().to_vec()),
        dominant_anchor,
        switch_count: switches,
        overlap_count: overlaps.len(),
        overlaps,
        warn,
    }
}

// ---------------------------------------------------------------------------
// 风格疲劳与审查提醒（对齐 build_style_fatigue / build_review_reminders）

/// 指标证据行（对齐 `_metric_evidence`：样本优先，否则 `count=/per_10k=`）。
fn metric_evidence(count: usize, per_10k: f64, samples: &[Hit]) -> String {
    if let Some(sample) = samples.first() {
        format!("L{} {}", sample.line_no, sample.snippet)
    } else {
        format!("count={count}, per_10k={}", py_float_str(per_10k))
    }
}

/// 按展示名过滤规则指标（对齐 `_metrics_named`：name ∈ 集合且 count > 0）。
fn regex_metrics_named(list: &[RegexMetric], names: &[&str]) -> Vec<RegexMetric> {
    list.iter()
        .filter(|m| names.contains(&m.name.as_str()) && m.count > 0)
        .cloned()
        .collect()
}

/// 按展示名过滤跟踪词指标（对齐 `_metrics_named` 在 tracked_terms 节）。
fn tracked_metrics_named(
    list: &[crate::rules::TrackedMetric],
    names: &[&str],
) -> Vec<crate::rules::TrackedMetric> {
    list.iter()
        .filter(|m| names.contains(&m.name.as_str()) && m.count > 0)
        .cloned()
        .collect()
}

/// 疲劳状态（对齐 `_fatigue_status`）。
fn fatigue_status(warn: bool, count: usize, watch_at: usize) -> &'static str {
    if warn {
        "WARN"
    } else if count >= watch_at {
        "WATCH"
    } else {
        "OK"
    }
}

/// 指标计数合计（对齐 `_metric_count`）。
fn metric_count(list: &[RegexMetric]) -> usize {
    list.iter().map(|m| m.count).sum()
}

/// 去重且保序的证据（对齐 `_unique_evidence`：空串丢弃，限长）。
fn unique_evidence(items: &[String], limit: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in items {
        if item.is_empty() || !seen.insert(item) {
            continue;
        }
        out.push(item.clone());
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// 风格疲劳行（对齐 `build_style_fatigue`；基于已装配的 analysis 各节汇总）。
pub fn build_style_fatigue(a: &Analysis) -> Vec<FatigueRow> {
    let mut rows: Vec<FatigueRow> = Vec::new();
    let mut add = |family: &str,
                   status: &str,
                   count: usize,
                   risk: &str,
                   reduce: &str,
                   evidence: Vec<String>| {
        rows.push(FatigueRow {
            family: family.to_string(),
            status: status.to_string(),
            count,
            risk: risk.to_string(),
            reduce: reduce.to_string(),
            evidence: unique_evidence(&evidence, 3),
        });
    };

    let pi_metrics = regex_metrics_named(&a.patterns, &["Pi竖线状态栏", "Pi是否菜单"]);
    add(
        "Pi UI/菜单句",
        fatigue_status(
            pi_metrics.iter().any(|m| m.warn),
            metric_count(&pi_metrics),
            1,
        ),
        metric_count(&pi_metrics),
        "Pi 像系统面板，会削弱搭档感和人物反应。",
        "只留最有角色感的一处，其余改成卡顿、延迟、误读或人物自行判断。",
        pi_metrics
            .iter()
            .map(|m| metric_evidence(m.count, m.per_10k, &m.samples))
            .collect(),
    );

    let clue_metrics = regex_metrics_named(&a.patterns, &["线索面板词"]);
    add(
        "线索面板句",
        fatigue_status(
            clue_metrics.iter().any(|m| m.warn),
            metric_count(&clue_metrics),
            3,
        ),
        metric_count(&clue_metrics),
        "线索被归档、首屏、标签、坐标、重合等词收拢，读感像任务列表。",
        "把完整结论拆成发现、排除、误判、半确认，章末用行动阻力收束。",
        clue_metrics
            .iter()
            .map(|m| metric_evidence(m.count, m.per_10k, &m.samples))
            .collect(),
    );

    let conclusion_metrics = regex_metrics_named(&a.patterns, &["这不是X是Y"]);
    let negation_metrics = regex_metrics_named(
        &a.patterns,
        &[
            "不是A而是B",
            "不是A只是B/更像B",
            "肯定后否定",
            "否定后肯定",
            "问题在于/这就是",
        ],
    );
    let negation_tokens = regex_metrics_named(&a.tokens, &["不是", "只是", "而是"]);
    let negation_total = metric_count(
        &conclusion_metrics
            .iter()
            .chain(negation_metrics.iter())
            .chain(negation_tokens.iter())
            .cloned()
            .collect::<Vec<_>>(),
    );
    add(
        "否定/肯定判断句",
        fatigue_status(
            !conclusion_metrics.is_empty() || negation_metrics.iter().any(|m| m.warn),
            negation_total,
            4,
        ),
        negation_total,
        "不是/只是/而是/这就是一类句子会让旁白替读者解释。",
        "人物台词可保留；旁白判断改成动作、证据、误读或后果。",
        conclusion_metrics
            .iter()
            .chain(negation_metrics.iter())
            .chain(negation_tokens.iter())
            .map(|m| metric_evidence(m.count, m.per_10k, &m.samples))
            .collect(),
    );

    let narration_context = a
        .judgement_contexts
        .iter()
        .find(|item| item.context == "narration");
    if let Some(nc) = narration_context {
        add(
            "旁白判断句",
            fatigue_status(nc.warn, nc.count, 2),
            nc.count,
            "判断词集中在旁白里时，作者会替读者完成理解。",
            "人物台词可保留；旁白判断优先换成动作、证据、误读或后果。",
            nc.samples
                .iter()
                .take(3)
                .map(|s| format!("L{} {}：{}", s.line_no, s.terms.join(","), s.text))
                .collect(),
        );
    }

    let assertive_metrics = regex_metrics_named(&a.patterns, &["肯定判断/解释腔"]);
    let cliche_metrics = regex_metrics_named(
        &a.phrases,
        &["不是因为", "问题不在", "看起来", "更像", "像是", "至少"],
    );
    let cliche_total = metric_count(
        &assertive_metrics
            .iter()
            .chain(cliche_metrics.iter())
            .cloned()
            .collect::<Vec<_>>(),
    );
    add(
        "陈词/解释腔",
        fatigue_status(
            assertive_metrics.iter().any(|m| m.warn) || cliche_metrics.iter().any(|m| m.warn),
            cliche_total,
            3,
        ),
        cliche_total,
        "真正、其实、显然、更像、至少等解释词过密时，旁白会变成评语。",
        "删掉只负责解释的句子，改成角色误读、物件变化或场面后果。",
        assertive_metrics
            .iter()
            .chain(cliche_metrics.iter())
            .map(|m| metric_evidence(m.count, m.per_10k, &m.samples))
            .collect(),
    );

    let sl = &a.sentence_lengths;
    add(
        "短句/极短句",
        if sl.warn { "WARN" } else { "OK" },
        sl.short_count,
        "短句连发会把动作、情绪和信息压成碎拍。",
        "每个短句连发块只保留一个节拍点，其余改成动作因果或场面阻力。",
        {
            let base = format!(
                "short={}, very_short={}, ratio={}, runs={}",
                sl.short_count,
                sl.very_short_count,
                py_float_str(sl.short_ratio),
                sl.short_runs.len()
            );
            if let Some(first) = sl.short_runs.first() {
                vec![
                    base,
                    format!(
                        "短句连发类型：{}；建议：{}",
                        format_short_roles(&first.roles, "，"),
                        first.suggestion
                    ),
                ]
            } else {
                vec![base]
            }
        },
    );

    add(
        "局部疲劳窗口",
        fatigue_status(!a.fatigue_windows.is_empty(), a.fatigue_windows.len(), 1),
        a.fatigue_windows.len(),
        "短句、解释、把字操作和角色起手在同一小段叠加时，读感会突然变累。",
        "先改分数最高的窗口：保留一个节奏点，其余改成动作因果、环境反应或视角切换。",
        a.fatigue_windows
            .iter()
            .take(3)
            .map(|item| {
                format!(
                    "S{}-{} L{}-{} {}；类型：{}；建议：{}",
                    item.start_index,
                    item.end_index,
                    item.start_line,
                    item.end_line,
                    item.reasons.join("、"),
                    format_short_roles(&item.roles, "，"),
                    item.suggestion
                )
            })
            .collect(),
    );

    let ba_metrics = regex_metrics_named(&a.patterns, &["把字操作句"]);
    let ba_contexts = &a.ba_operation_contexts;
    let mut ba_evidence: Vec<String> = ba_metrics
        .iter()
        .map(|m| metric_evidence(m.count, m.per_10k, &m.samples))
        .collect();
    for item in ba_contexts.iter().take(3) {
        let mut sample = String::new();
        if let Some(first) = item.samples.first() {
            sample = format!("L{} {}", first.line_no, first.snippet);
        }
        let mut line = format!("{} x{}；建议：{}", item.role, item.count, item.suggestion);
        if !sample.is_empty() {
            line.push('；');
            line.push_str(&sample);
        }
        ba_evidence.push(line);
    }
    let ba_total = std::cmp::max(
        metric_count(&ba_metrics),
        ba_contexts.iter().map(|c| c.count).sum(),
    );
    add(
        "把字操作句",
        fatigue_status(
            ba_metrics.iter().any(|m| m.warn) || ba_contexts.iter().any(|c| c.warn),
            ba_total,
            10,
        ),
        ba_total,
        "把 X 拖上/放进/压住/推过去过密，会像操作日志。",
        "工具操作保留；情绪和线索操作改成视觉结果、环境反应或被动阻力。",
        ba_evidence,
    );

    let simile_metrics = regex_metrics_named(&a.patterns, &["像/活像模板"]);
    add(
        "像/活像比喻",
        fatigue_status(
            simile_metrics.iter().any(|m| m.warn),
            metric_count(&simile_metrics),
            3,
        ),
        metric_count(&simile_metrics),
        "比喻模板过密会替代真实动作，让旁白解释气氛。",
        "每章只保留少数最有新意的比喻，其余改成具体动作、声音或物件变化。",
        simile_metrics
            .iter()
            .map(|m| metric_evidence(m.count, m.per_10k, &m.samples))
            .collect(),
    );

    let sticky_metrics = regex_metrics_named(
        &a.modifiers,
        &[
            "微微",
            "轻轻",
            "慢慢",
            "有点",
            "一点点",
            "显得",
            "过于",
            "几乎",
            "几乎没有",
        ],
    );
    add(
        "黏糊词/弱判断",
        fatigue_status(
            sticky_metrics.iter().any(|m| m.warn),
            metric_count(&sticky_metrics),
            4,
        ),
        metric_count(&sticky_metrics),
        "轻轻、微微、有点、显得等词过密时，动作力度会被磨软。",
        "优先删弱判断词；用动作幅度、声音、阻力来表示轻重。",
        sticky_metrics
            .iter()
            .map(|m| metric_evidence(m.count, m.per_10k, &m.samples))
            .collect(),
    );

    let aa_bb_warns: Vec<&AaBbPattern> = a.aa_bb_patterns.iter().filter(|i| i.warn).collect();
    let aa_bb_count: usize = a.aa_bb_patterns.iter().map(|i| i.count).sum();
    add(
        "AA/BB 短排比",
        fatigue_status(!aa_bb_warns.is_empty(), aa_bb_count, 2),
        aa_bb_count,
        "短分句排比会把画面写成清单。",
        "保留一个节奏点，其余并入动作过程或拆给人物反应。",
        aa_bb_warns
            .iter()
            .filter_map(|item| item.samples.first().cloned())
            .collect(),
    );

    let lead_count = a
        .paragraph_leads
        .iter()
        .chain(a.subject_leads.iter())
        .map(|i| i.count)
        .max()
        .unwrap_or(0);
    let mut lead_evidence: Vec<String> = Vec::new();
    if !a.paragraph_leads.is_empty() {
        lead_evidence.push(format!(
            "段首 {}",
            a.paragraph_leads
                .iter()
                .take(3)
                .map(|i| format!("{} x{}", i.phrase, i.count))
                .collect::<Vec<_>>()
                .join("，")
        ));
    }
    if !a.subject_leads.is_empty() {
        lead_evidence.push(format!(
            "主语 {}",
            a.subject_leads
                .iter()
                .take(3)
                .map(|i| format!("{} x{}", i.phrase, i.count))
                .collect::<Vec<_>>()
                .join("，")
        ));
    }
    add(
        "角色名/他她起手",
        fatigue_status(lead_count >= 8, lead_count, 4),
        lead_count,
        "段落总从角色名或他她起步，会让镜头调度单一。",
        "每三到四个角色起手里，换一个空间、物件、声音或证据变化起手。",
        lead_evidence,
    );

    let tracked_warns = tracked_metrics_named(&a.tracked_terms, &[]);
    let _ = tracked_warns;
    let tracked_warn_list: Vec<&crate::rules::TrackedMetric> =
        a.tracked_terms.iter().filter(|m| m.warn).collect();
    let tracked_total: usize =
        tracked_warn_list.iter().map(|m| m.count).sum::<usize>() + a.tracked_term_windows.len();
    let mut tracked_evidence: Vec<String> = tracked_warn_list
        .iter()
        .take(4)
        .map(|m| metric_evidence(m.count, m.per_10k, &m.samples))
        .collect();
    for item in a.tracked_term_windows.iter().take(3) {
        tracked_evidence.push(format!(
            "S{}-{} L{}-{} {}；{}；建议：{}",
            item.start_index,
            item.end_index,
            item.start_line,
            item.end_line,
            item.reasons.join("、"),
            format_tracked_term_counts(
                &item.terms.iter().take(3).cloned().collect::<Vec<_>>(),
                "，"
            ),
            item.suggestion
        ));
    }
    add(
        "高频词/点名册",
        fatigue_status(
            !tracked_warn_list.is_empty() || !a.tracked_term_windows.is_empty(),
            tracked_total,
            8,
        ),
        tracked_total,
        "人物名、地名、设备名过密时，叙述会像点名册或设定表。",
        "用动作、称谓、空间位置和具体物件轮换，不要只靠同一个名词推进。",
        tracked_evidence,
    );

    let d = &a.dialogue;
    let dialogue_count = d.short_quote_runs.len()
        + d.question_ping_pong.len()
        + d.quote_ping_pong.len()
        + d.dialogue_axis_gaps.len();
    let mut dialogue_evidence: Vec<String> = Vec::new();
    if let Some(first) = d.short_quote_runs.first() {
        dialogue_evidence.push(first.sample.join(" | "));
    }
    if let Some(first) = d.question_ping_pong.first() {
        dialogue_evidence.push(first.sample.join(" | "));
    }
    if let Some(first) = d.quote_ping_pong.first() {
        dialogue_evidence.push(first.sample.join(" | "));
    }
    for item in d.dialogue_axis_gaps.iter().take(2) {
        dialogue_evidence.push(format!(
            "S{}-{} L{}-{} {}；建议：{}",
            item.start_index,
            item.end_index,
            item.start_line,
            item.end_line,
            item.reasons.join("、"),
            item.suggestion
        ));
    }
    add(
        "对白乒乓",
        fatigue_status(dialogue_count >= 1, dialogue_count, 1),
        dialogue_count,
        "短对白连续互顶时，动作和场面会消失。",
        "每四句对白至少插入一个动作、环境变化或人物误读作为转轴。",
        dialogue_evidence,
    );

    let sm = &a.scene_map;
    let role_summary = if sm.role_counts.is_empty() {
        "无".to_string()
    } else {
        sm.role_counts
            .iter()
            .map(|(name, count)| format!("{name} x{count}"))
            .collect::<Vec<_>>()
            .join("，")
    };
    add(
        "场面功能分布",
        fatigue_status(sm.warn, sm.block_count, 4),
        sm.block_count,
        "如果整章长时间停在对白块或说明块，场面会失去功能切换。",
        "让信息、动作、环境和关系推进互相接力，不要让单一功能吃满整章。",
        vec![format!(
            "dominant={} ratio={}；{role_summary}",
            sm.dominant_role,
            py_float_str(sm.dominance_ratio)
        )],
    );

    let de = &a.dialogue_emotions;
    let emotion_summary = if de.emotion_counts.is_empty() {
        "无".to_string()
    } else {
        de.emotion_counts
            .iter()
            .map(|(name, count)| format!("{name} x{count}"))
            .collect::<Vec<_>>()
            .join("，")
    };
    add(
        "对白情绪曲线",
        fatigue_status(de.flatness_warn || de.volatility_warn, de.shift_count, 2),
        de.dialogue_sentences,
        "对白如果长期只剩一种情绪，或情绪标签频繁横跳，关系推进会发假。",
        "检查台词是在逼问、回避、防御还是安抚，并补动作或停顿让情绪转折落地。",
        vec![format!(
            "dominant={} ratio={} shift={}；{emotion_summary}",
            de.dominant_emotion,
            py_float_str(de.dominant_ratio),
            de.shift_count
        )],
    );

    let cv = &a.character_voice;
    let mut voice_evidence: Vec<String> = Vec::new();
    for item in cv.speakers.iter().take(3) {
        voice_evidence.push(format!(
            "{} line={} avg={} q={} short={} emotion={}",
            item.speaker,
            item.lines,
            py_float_str(item.avg_chars),
            py_float_str(item.question_ratio),
            py_float_str(item.short_ratio),
            item.dominant_emotion
        ));
    }
    voice_evidence.extend(cv.homogenized_pairs.iter().take(2).cloned());
    add(
        "角色声音",
        fatigue_status(cv.warn, cv.speaker_count, 2),
        cv.identified_lines,
        "如果多名角色的对白节拍、问句率和情绪主导长期接近，人物会越来越像同一个人在说话。",
        "让不同角色在句长、追问强度、判断习惯和情绪入口上拉开距离。",
        voice_evidence,
    );

    let bp = &a.battle_profile;
    add(
        "动作结果链",
        fatigue_status(bp.warn, bp.sequence_count, 1),
        bp.action_hits,
        "冲突段如果只有动作没有结果、伤害或位移反馈，会像挥空的动作脚本。",
        "每段冲突至少补一个结果句：谁退了、谁失衡了、什么东西坏了、谁被迫改动作。",
        vec![format!(
            "sequences={} action={} result={} damage={} ratio={}",
            bp.sequence_count,
            bp.action_hits,
            bp.result_hits,
            bp.damage_hits,
            py_float_str(bp.result_ratio)
        )],
    );

    let vp = &a.viewpoint_profile;
    let anchor_summary = if vp.anchor_counts.is_empty() {
        "无".to_string()
    } else {
        vp.anchor_counts
            .iter()
            .map(|(name, count)| format!("{name} x{count}"))
            .collect::<Vec<_>>()
            .join("，")
    };
    add(
        "视角锚点",
        fatigue_status(vp.warn, vp.overlap_count, 1),
        vp.switch_count,
        "同段多人物心理暴露或近距离切锚偏多时，镜头中心会发飘。",
        "近距离视角段先固定一个感知中心；别在同段同时替两个人解释内心。",
        vec![format!(
            "anchors={anchor_summary} switch={} overlap={}",
            vp.switch_count, vp.overlap_count
        )],
    );

    let en = &a.ending;
    let ending_count = en.image_terms.len() + en.flow_terms.len();
    let mut ending_evidence: Vec<String> = Vec::new();
    if !en.flow_terms.is_empty() {
        ending_evidence.push(format!(
            "流程词 {}",
            en.flow_terms
                .iter()
                .map(|t| format!("{} x{}", t.term, t.count))
                .collect::<Vec<_>>()
                .join("，")
        ));
    }
    if !en.image_terms.is_empty() {
        ending_evidence.push(format!(
            "意象词 {}",
            en.image_terms
                .iter()
                .map(|t| format!("{} x{}", t.term, t.count))
                .collect::<Vec<_>>()
                .join("，")
        ));
    }
    if !en.tail_excerpt.is_empty() {
        ending_evidence.push(prefix_chars(&en.tail_excerpt, 120));
    }
    add(
        "章末模板",
        fatigue_status(en.warn, ending_count, 2),
        ending_count,
        "章末反复用流程词或同类意象收束，会让钩子同质。",
        "在动作余波、关系变化、外部阻力三类里换一种收束手势。",
        ending_evidence,
    );

    let modifier_warns: Vec<&ModifierPressure> =
        a.modifier_pressure.iter().filter(|i| i.warn).collect();
    let modifier_total: usize = a.modifier_pressure.iter().map(|i| i.total).sum();
    add(
        "修饰/动作压力",
        fatigue_status(!modifier_warns.is_empty(), modifier_total, 80),
        modifier_total,
        "同类修饰词和动作词过密时，画面会发僵。",
        "优先改 dense_sentences，不要只替换同义词。",
        a.modifier_pressure
            .iter()
            .filter(|i| i.total > 0)
            .map(|i| format!("{} total={} dense={}", i.label, i.total, i.dense_sentences))
            .collect(),
    );

    let status_order = |status: &str| -> i32 {
        match status {
            "WARN" => 0,
            "WATCH" => 1,
            "OK" => 2,
            _ => 9,
        }
    };
    rows.sort_by(|a, b| {
        status_order(&a.status)
            .cmp(&status_order(&b.status))
            .then_with(|| a.family.cmp(&b.family))
    });
    rows
}

/// 审查提醒（对齐 `build_review_reminders`：把原始告警翻成复核问题）。
pub fn build_review_reminders(a: &Analysis) -> Vec<ReviewReminder> {
    let mut reminders: Vec<ReviewReminder> = Vec::new();
    let mut add = |priority: &str,
                   category: &str,
                   title: &str,
                   reason: &str,
                   check: &str,
                   action: &str,
                   evidence: Vec<String>| {
        reminders.push(ReviewReminder {
            priority: priority.to_string(),
            category: category.to_string(),
            title: title.to_string(),
            reason: reason.to_string(),
            check: check.to_string(),
            action: action.to_string(),
            evidence: unique_evidence(&evidence, 4),
        });
    };

    let pi_metrics = regex_metrics_named(&a.patterns, &["Pi竖线状态栏", "Pi是否菜单"]);
    if !pi_metrics.is_empty() {
        add(
            "P1",
            "Pi",
            "Pi 输出正在滑向 UI/菜单",
            "Pi 负责给结论或按钮提示时，会从搭档变成系统面板。",
            "检查 Pi 输出后是否还有人物误读、停顿、拒绝配合或行动后果。",
            "保留最有角色感的一处 Pi 文本，其余改成蓝字卡顿、反应延迟或人物自己判断。",
            pi_metrics
                .iter()
                .map(|m| metric_evidence(m.count, m.per_10k, &m.samples))
                .collect(),
        );
    }

    let clue_metrics = regex_metrics_named(&a.patterns, &["线索面板词"]);
    if !clue_metrics.is_empty() {
        add(
            "P1",
            "信息",
            "线索被面板词收拢",
            "归档、首屏、标签、坐标、重合等词密集时，章节会像任务列表。",
            "检查本章结论是否由场面冲突推出，而不是由屏幕/图表替读者盖章。",
            "把一次完整结论拆成发现、排除、误判、半确认；章末改用行动阻力收束。",
            clue_metrics
                .iter()
                .map(|m| metric_evidence(m.count, m.per_10k, &m.samples))
                .collect(),
        );
    }

    let conclusion_metrics = regex_metrics_named(&a.patterns, &["这不是X是Y"]);
    let negation_metrics = regex_metrics_named(
        &a.patterns,
        &[
            "不是A而是B",
            "不是A只是B/更像B",
            "肯定后否定",
            "否定后肯定",
            "问题在于/这就是",
        ],
    );
    let negation_tokens = regex_metrics_named(&a.tokens, &["不是", "只是", "而是"]);
    let negation_count: usize = negation_metrics
        .iter()
        .chain(negation_tokens.iter())
        .map(|m| m.count)
        .sum();
    if !conclusion_metrics.is_empty() || negation_count >= 5 {
        add(
            if !conclusion_metrics.is_empty() {
                "P1"
            } else {
                "P2"
            },
            "句式",
            "否定/肯定判断句过密",
            "不是/只是/而是/这就是一类句子会让旁白替读者解释。",
            "区分人物台词和作者旁白；人物声音可保留，旁白判断优先改。",
            "把结论改成证据、动作或误读；保留一处关键否定，其余让读者自己推出来。",
            conclusion_metrics
                .iter()
                .chain(negation_metrics.iter())
                .chain(negation_tokens.iter())
                .take(4)
                .map(|m| metric_evidence(m.count, m.per_10k, &m.samples))
                .collect(),
        );
    }

    let narration_context = a
        .judgement_contexts
        .iter()
        .find(|item| item.context == "narration");
    if let Some(nc) = narration_context {
        if nc.warn {
            add(
                if nc.count >= 5 { "P1" } else { "P2" },
                "句式",
                "旁白判断句偏密",
                "判断词集中在旁白里时，作者会替读者完成理解。",
                "先把人物台词和旁白判断分开；只处理旁白里负责下结论的句子。",
                "把旁白判断改成动作、证据、误读、后果或第三方反应。",
                nc.samples
                    .iter()
                    .take(4)
                    .map(|s| format!("L{} {}：{}", s.line_no, s.terms.join(","), s.text))
                    .collect(),
            );
        }
    }

    let assertive_metrics = regex_metrics_named(&a.patterns, &["肯定判断/解释腔"]);
    let cliche_metrics = regex_metrics_named(
        &a.phrases,
        &["不是因为", "问题不在", "看起来", "更像", "像是", "至少"],
    );
    let cliche_count: usize = assertive_metrics
        .iter()
        .chain(cliche_metrics.iter())
        .map(|m| m.count)
        .sum();
    let cliche_warn =
        assertive_metrics.iter().any(|m| m.warn) || cliche_metrics.iter().any(|m| m.warn);
    if cliche_warn || cliche_count >= 3 {
        add(
            "P2",
            "文风",
            "陈词/解释腔偏密",
            "真正、其实、显然、更像、至少等词会让旁白像评语。",
            "检查这些句子是不是只在解释观感，而没有制造动作或阻力。",
            "删掉只负责解释的句子，或改成角色误读、物件变化、场面后果。",
            assertive_metrics
                .iter()
                .chain(cliche_metrics.iter())
                .take(4)
                .map(|m| metric_evidence(m.count, m.per_10k, &m.samples))
                .collect(),
        );
    }

    let sl = &a.sentence_lengths;
    if sl.warn {
        let priority = if sl.short_ratio >= 0.25 || sl.short_runs.len() >= 5 {
            "P1"
        } else {
            "P2"
        };
        let mut evidence = vec![format!(
            "short={}, very_short={}, ratio={}, runs={}",
            sl.short_count,
            sl.very_short_count,
            py_float_str(sl.short_ratio),
            sl.short_runs.len()
        )];
        if let Some(first) = sl.short_runs.first() {
            evidence.push(first.sample.join(" | "));
            let roles = format_short_roles(&first.roles, "，");
            if !roles.is_empty() {
                evidence.push(format!("类型：{roles}；建议：{}", first.suggestion));
            }
        }
        add(
            priority,
            "节奏",
            "短句正在变成默认节拍",
            "连续短句会把动作、情绪和信息压成碎拍。",
            "检查短句是在制造节奏，还是在把应展开的过程写成提纲。",
            "每个短句连发块只保留一个节拍点，其余改成动作因果或场面阻力。",
            evidence,
        );
    }

    if !a.fatigue_windows.is_empty() {
        let first_window = &a.fatigue_windows[0];
        add(
            if first_window.score >= 10 { "P1" } else { "P2" },
            "定位",
            "局部句式疲劳窗口",
            "同一小段里短句、判断解释、把字操作或角色起手叠加，会比单项总数更影响观感。",
            "优先看分数最高的窗口，不要平均用力改全章。",
            "保留一个最有用的节奏点；其余改成动作因果、环境反应、人物误读或视角入口。",
            a.fatigue_windows
                .iter()
                .take(3)
                .map(|item| {
                    format!(
                        "S{}-{} L{}-{} score={} {}：{}",
                        item.start_index,
                        item.end_index,
                        item.start_line,
                        item.end_line,
                        item.score,
                        item.reasons.join("、"),
                        item.sample
                            .iter()
                            .take(5)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" | ")
                    )
                })
                .collect(),
        );
    }

    let ba_metrics = regex_metrics_named(&a.patterns, &["把字操作句"]);
    let ba_contexts = &a.ba_operation_contexts;
    if !ba_metrics.is_empty() || !ba_contexts.is_empty() {
        let mut evidence: Vec<String> = ba_metrics
            .iter()
            .map(|m| metric_evidence(m.count, m.per_10k, &m.samples))
            .collect();
        for item in ba_contexts.iter().take(3) {
            let mut sample = String::new();
            if let Some(first) = item.samples.first() {
                sample = format!("L{} {}", first.line_no, first.snippet);
            }
            let mut line = format!("{} x{}；建议：{}", item.role, item.count, item.suggestion);
            if !sample.is_empty() {
                line.push('；');
                line.push_str(&sample);
            }
            evidence.push(line);
        }
        add(
            "P2",
            "动作",
            "把字句过密",
            "把 X 拖上/放进/压住/推过去连续出现时，场面像操作日志。",
            "检查这些把字句属于工具操作、线索操作、情绪动作还是场面调度。",
            "工具操作可保留必要句；线索操作拆发现-误读-后果，情绪动作改身体反应或他人误读。",
            evidence,
        );
    }

    let simile_metrics = regex_metrics_named(&a.patterns, &["像/活像模板"]);
    if !simile_metrics.is_empty() {
        add(
            "P2",
            "文风",
            "比喻模板过密",
            "像/活像类句式能快速给气氛，但过密时会替代真实动作。",
            "检查比喻是否提供新信息；只解释气氛的比喻优先删。",
            "每章保留少数最有新意的比喻，其余改成具体动作、声音、物件变化。",
            simile_metrics
                .iter()
                .map(|m| metric_evidence(m.count, m.per_10k, &m.samples))
                .collect(),
        );
    }

    let sticky_metrics = regex_metrics_named(
        &a.modifiers,
        &[
            "微微",
            "轻轻",
            "慢慢",
            "有点",
            "一点点",
            "显得",
            "过于",
            "几乎",
            "几乎没有",
        ],
    );
    if sticky_metrics.iter().map(|m| m.count).sum::<usize>() >= 4
        || sticky_metrics.iter().any(|m| m.warn)
    {
        add(
            "P2",
            "文风",
            "黏糊词/弱判断偏密",
            "轻轻、微微、有点、显得等词会削弱动作力度。",
            "检查这些词是否在替代动作幅度、声音、阻力或人物状态。",
            "优先删弱判断词；用可见动作和场面反应表达轻重。",
            sticky_metrics
                .iter()
                .take(4)
                .map(|m| metric_evidence(m.count, m.per_10k, &m.samples))
                .collect(),
        );
    }

    let tracked_warn_list: Vec<&crate::rules::TrackedMetric> =
        a.tracked_terms.iter().filter(|m| m.warn).collect();
    if !tracked_warn_list.is_empty() || !a.tracked_term_windows.is_empty() {
        let mut evidence: Vec<String> = tracked_warn_list
            .iter()
            .take(4)
            .map(|m| metric_evidence(m.count, m.per_10k, &m.samples))
            .collect();
        for item in a.tracked_term_windows.iter().take(3) {
            evidence.push(format!(
                "S{}-{} L{}-{} {}；{}；建议：{}",
                item.start_index,
                item.end_index,
                item.start_line,
                item.end_line,
                item.reasons.join("、"),
                format_tracked_term_counts(
                    &item.terms.iter().take(3).cloned().collect::<Vec<_>>(),
                    "，"
                ),
                item.suggestion
            ));
        }
        add(
            "P2",
            "词汇",
            "高频词/点名过密",
            "人物名、地名、设备名过密时，叙述会像点名册或设定表。",
            "检查同一名词是否在局部窗口里连续点名，或是否可以用动作、称谓、空间位置、具体物件轮换。",
            "先改最密的 1-2 个窗口，不要只做同义词替换。",
            evidence,
        );
    }

    let paragraph_leads = &a.paragraph_leads;
    let subject_leads = &a.subject_leads;
    let mut lead_evidence: Vec<String> = Vec::new();
    if !paragraph_leads.is_empty() {
        lead_evidence.push(format!(
            "段首 {}",
            paragraph_leads
                .iter()
                .take(3)
                .map(|i| format!("{} x{}", i.phrase, i.count))
                .collect::<Vec<_>>()
                .join("，")
        ));
    }
    if !subject_leads.is_empty() {
        lead_evidence.push(format!(
            "主语 {}",
            subject_leads
                .iter()
                .take(3)
                .map(|i| format!("{} x{}", i.phrase, i.count))
                .collect::<Vec<_>>()
                .join("，")
        ));
    }
    if !lead_evidence.is_empty() {
        let lead_count = paragraph_leads
            .iter()
            .chain(subject_leads.iter())
            .map(|i| i.count)
            .max()
            .unwrap_or(0);
        if lead_count >= 8 {
            add(
                "P2",
                "镜头",
                "角色名/他她起手过密",
                "段落总从角色名或他她起步，会让镜头调度单一。",
                "检查连续段落是否都是角色先出现、再动作、再判断。",
                "每三到四个角色起手里，至少换一个空间、物件、声音或证据变化起手。",
                lead_evidence,
            );
        }
    }

    let d = &a.dialogue;
    let mut dialogue_evidence: Vec<String> = Vec::new();
    if let Some(first) = d.short_quote_runs.first() {
        dialogue_evidence.push(first.sample.join(" | "));
    }
    if let Some(first) = d.quote_ping_pong.first() {
        dialogue_evidence.push(first.sample.join(" | "));
    }
    for item in d.dialogue_axis_gaps.iter().take(2) {
        dialogue_evidence.push(format!(
            "S{}-{} L{}-{} {}；建议：{}",
            item.start_index,
            item.end_index,
            item.start_line,
            item.end_line,
            item.reasons.join("、"),
            item.suggestion
        ));
    }
    if !dialogue_evidence.is_empty() {
        add(
            "P2",
            "对话",
            "对白像互答录音",
            "短对白连续互顶时，场面动作会消失。",
            "检查每四句对白里是否有动作、环境变化、第三方打断或设备声作为转轴。",
            "保留最有锋芒的两句，其余用动作、环境声、第三方反应或设备反馈打断。",
            dialogue_evidence,
        );
    }

    let sm = &a.scene_map;
    if sm.warn {
        let role_summary = if sm.role_counts.is_empty() {
            "无".to_string()
        } else {
            sm.role_counts
                .iter()
                .map(|(name, count)| format!("{name} x{count}"))
                .collect::<Vec<_>>()
                .join("，")
        };
        add(
            "P2",
            "场面",
            "章节长时间停在同一种功能块",
            "如果整章大部分粗分块都在对白或说明，场面会失去切换和推进。",
            "检查这章有没有让动作、环境、关系和信息交替接力，而不是一直停在解释或接话。",
            "补一个改变站位、空间、外部阻力或关系温度的块，不要只扩写原功能。",
            vec![format!(
                "dominant={} ratio={}；{role_summary}",
                sm.dominant_role,
                py_float_str(sm.dominance_ratio)
            )],
        );
    }

    let de = &a.dialogue_emotions;
    if de.flatness_warn || de.volatility_warn {
        let emotion_summary = if de.emotion_counts.is_empty() {
            "无".to_string()
        } else {
            de.emotion_counts
                .iter()
                .map(|(name, count)| format!("{name} x{count}"))
                .collect::<Vec<_>>()
                .join("，")
        };
        add(
            "P2",
            "对话",
            "对白情绪曲线失衡",
            "对白如果长期只剩逼问/防御一种温度，或情绪标签频繁横跳，人物关系会发假。",
            "检查这段对话是在升级关系、回避真问题，还是只在重复情绪姿态。",
            "给情绪转折补动作、停顿、误读或第三方干扰，让变化落到场面上。",
            vec![format!(
                "dominant={} ratio={} shift={}；{emotion_summary}",
                de.dominant_emotion,
                py_float_str(de.dominant_ratio),
                de.shift_count
            )],
        );
    }

    let cv = &a.character_voice;
    if cv.warn {
        let mut evidence = vec![format!(
            "coverage={} dominant={} ratio={}",
            py_float_str(cv.coverage_ratio),
            cv.dominant_speaker,
            py_float_str(cv.dominant_ratio)
        )];
        for item in cv.speakers.iter().take(3) {
            evidence.push(format!(
                "{} line={} avg={} q={} short={} emotion={}",
                item.speaker,
                item.lines,
                py_float_str(item.avg_chars),
                py_float_str(item.question_ratio),
                py_float_str(item.short_ratio),
                item.dominant_emotion
            ));
        }
        evidence.extend(cv.homogenized_pairs.iter().take(2).cloned());
        add(
            "P2",
            "角色",
            "角色对白开始同腔",
            "多名角色的问句率、短句率、判断姿态和主导情绪过近时，人物声音会并轨。",
            "检查这些角色是不是都在用同一种追问、回避或判断手势说话。",
            "至少给核心角色拉开一项稳定差异：句长、问句密度、脏话/判断句习惯、安抚还是施压入口。",
            evidence,
        );
    }

    let bp = &a.battle_profile;
    if bp.warn {
        add(
            "P2",
            "动作",
            "冲突段动作多但结果少",
            "动作和碰撞已经出现，但结果句、受伤反馈或位移后果不足时，冲突会像挥空。",
            "检查每段动作后，是否有人被逼退、卡住、受伤、失手或改变目标。",
            "每段冲突至少补一个结果句，不要只累计动作动词。",
            vec![format!(
                "sequences={} action={} result={} damage={} ratio={}",
                bp.sequence_count,
                bp.action_hits,
                bp.result_hits,
                bp.damage_hits,
                py_float_str(bp.result_ratio)
            )],
        );
    }

    let vp = &a.viewpoint_profile;
    if vp.warn {
        let anchor_summary = if vp.anchor_counts.is_empty() {
            "无".to_string()
        } else {
            vp.anchor_counts
                .iter()
                .map(|(name, count)| format!("{name} x{count}"))
                .collect::<Vec<_>>()
                .join("，")
        };
        let mut evidence = vec![format!(
            "anchors={anchor_summary} switch={} overlap={}",
            vp.switch_count, vp.overlap_count
        )];
        for item in vp.overlaps.iter().take(2) {
            evidence.push(format!(
                "L{} {}：{}",
                item.line_no,
                item.anchors.join(","),
                item.text
            ));
        }
        add(
            "P2",
            "视角",
            "近距离视角锚点漂移",
            "同段多人物心理暴露或近距离切锚偏多时，读者会丢当前镜头中心。",
            "检查这些段落是不是同时替两个人解释内心，或刚贴近一个人就跳去另一个人。",
            "近距离段先固定一个感知中心，其余人物只通过动作、台词和误读出现。",
            evidence,
        );
    }

    let en = &a.ending;
    if en.warn {
        let mut evidence: Vec<String> = Vec::new();
        if !en.flow_terms.is_empty() {
            evidence.push(format!(
                "流程词 {}",
                en.flow_terms
                    .iter()
                    .map(|t| format!("{} x{}", t.term, t.count))
                    .collect::<Vec<_>>()
                    .join("，")
            ));
        }
        if !en.image_terms.is_empty() {
            evidence.push(format!(
                "意象词 {}",
                en.image_terms
                    .iter()
                    .map(|t| format!("{} x{}", t.term, t.count))
                    .collect::<Vec<_>>()
                    .join("，")
            ));
        }
        evidence.push(prefix_chars(&en.tail_excerpt, 120));
        add(
            "P2",
            "章末",
            "章末收束可能模板化",
            "章末反复用冷光、夜、首屏、继续、下一步等词，会让钩子同质。",
            "检查结尾是在打开新行动，还是只把本章线索摆整齐。",
            "在动作余波、关系变化、外部阻力三类里换一种收束手势。",
            evidence,
        );
    }

    let priority_order = |priority: &str| -> i32 {
        match priority {
            "P1" => 0,
            "P2" => 1,
            "P3" => 2,
            _ => 9,
        }
    };
    reminders.sort_by(|a, b| {
        priority_order(&a.priority)
            .cmp(&priority_order(&b.priority))
            .then_with(|| a.category.cmp(&b.category))
            .then_with(|| a.title.cmp(&b.title))
    });
    reminders
}

// ---------------------------------------------------------------------------
// analyze_text / analyze_path / CLI（对齐 Python analyze_text / analyze_path / main）

/// 报告输出格式（对齐 `--format`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    /// 纯文本报告（对齐 Python `format_text_report`）。
    Text,
    /// JSON 报告（与 Python `--format json` 完整对齐）。
    Json,
    /// Markdown 报告（对齐 Python `format_markdown_report`）。
    Markdown,
}

/// 语料画像 JSON 节（对齐 Python analysis["corpus_profile"] 组装）。
fn corpus_profile_json(corpus: Option<&CorpusProfile>) -> CorpusProfileJson {
    let to_json = |item: &LearnedPattern| LearnedTermJson {
        name: item.name.clone(),
        category: item.category.clone(),
        count: item.count,
        corpus_per_10k: item.corpus_per_10k,
        max_per_10k: item.max_per_10k,
    };
    match corpus {
        Some(c) => CorpusProfileJson {
            enabled: true,
            source_count: c.source_count,
            chars: c.chars,
            draft_chars: c.draft_chars,
            learned_terms: c.learned_terms.iter().map(to_json).collect(),
            learned_style_phrases: c.learned_style_phrases.iter().map(to_json).collect(),
            learned_sentence_leads: c.learned_sentence_leads.clone(),
            learned_aa_bb_shapes: c.learned_aa_bb_shapes.clone(),
            sentence_length_baseline: match &c.sentence_length_baseline {
                Some(baseline) => BaselineJson::Values(baseline.clone()),
                None => BaselineJson::Empty,
            },
        },
        None => CorpusProfileJson {
            enabled: false,
            source_count: 0,
            chars: 0,
            draft_chars: 0,
            learned_terms: Vec::new(),
            learned_style_phrases: Vec::new(),
            learned_sentence_leads: Vec::new(),
            learned_aa_bb_shapes: Vec::new(),
            sentence_length_baseline: BaselineJson::Empty,
        },
    }
}

/// 指标证据（对齐 Python `metric["samples"][0]["snippet"] if metric["samples"] else ""`）。
fn first_snippet(samples: &[Hit]) -> String {
    samples
        .first()
        .map(|s| s.snippet.clone())
        .unwrap_or_default()
}

/// Python `analyze_text` 的完整移植：装配全部顶层节并计算 warned/warn_sections。
pub fn analyze_text(
    ctx: &DraftContext,
    text: &str,
    source: &str,
    template_bank: &[TemplateRule],
    term_bank: &[TrackedTerm],
    corpus_profile: Option<&CorpusProfile>,
    sample_limit: usize,
) -> Result<Analysis> {
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let splitter = ctx.splitter();
    let sentences = splitter.split_sentences(text);
    let sentence_infos = splitter.split_sentence_infos(text);
    let paragraph_infos = splitter.split_paragraph_infos(text);
    let paragraphs: Vec<String> = text
        .split("\n\n")
        .filter(|para| !para.trim().is_empty())
        .map(str::to_string)
        .collect();
    let chars = code_len(&text.replace('\n', ""));
    let quote_runs = detect_dialogue_runs(text);
    let short_quote_runs = detect_short_dialogue_runs(text);
    let question_ping_pong = detect_question_ping_pong(text);
    let quote_ping_pong = detect_quote_ping_pong(text);
    let dialogue_axis_gaps = build_dialogue_axis_gaps(ctx, &sentence_infos, sample_limit * 2);
    let ab_turns = detect_a_b_turns(ctx, text);

    let (token_metrics, token_warn) =
        build_rule_metrics(&ctx.token_rules, &lines, chars, false, sample_limit);
    let (pattern_metrics, pattern_warn) =
        build_rule_metrics(&ctx.pattern_rules, &lines, chars, true, sample_limit);
    let (phrase_metrics, phrase_warn) =
        build_rule_metrics(&ctx.phrase_rules, &lines, chars, false, sample_limit);
    let (modifier_metrics, modifier_warn) =
        build_rule_metrics(&ctx.modifier_rules, &lines, chars, false, sample_limit);
    let (punctuation_metrics, punctuation_warn) =
        build_rule_metrics(&ctx.punctuation_rules, &lines, chars, false, sample_limit);
    let (combo_metrics, combo_warn) =
        build_rule_metrics(&ctx.combo_rules, &lines, chars, false, sample_limit);
    let (custom_template_metrics, custom_warn) =
        build_custom_template_metrics(template_bank, &lines, chars, sample_limit)?;
    let (tracked_term_metrics, tracked_term_categories, tracked_term_warn) =
        build_tracked_term_metrics(term_bank, &lines, chars, sample_limit)?;
    let learned_filter_metrics =
        build_learned_filter_metrics(corpus_profile, &lines, chars, sample_limit);
    let learned_filter_warn = learned_filter_metrics.iter().any(|m| m.warn);

    let sentence_starts = collect_sentence_starts(&sentences);
    let subject_leads = collect_subject_leads(ctx, &sentences);
    let paragraph_leads = collect_paragraph_leads(ctx, &paragraphs);
    let repeated_starts: Vec<PhraseCount> = sentence_starts
        .into_iter()
        .filter(|(_, count)| *count >= 3)
        .map(|(phrase, count)| PhraseCount { phrase, count })
        .collect();
    let subject_leads_rows: Vec<PhraseCount> = subject_leads
        .into_iter()
        .map(|(phrase, count)| PhraseCount { phrase, count })
        .collect();
    let paragraph_leads_rows: Vec<PhraseCount> = paragraph_leads
        .into_iter()
        .map(|(phrase, count)| PhraseCount { phrase, count })
        .collect();
    let sentence_patterns_all = collect_connective_sentence_patterns(ctx, &sentences);
    let sentence_patterns: Vec<PhraseCount> = sentence_patterns_all
        .iter()
        .filter(|(_, count)| *count >= 3)
        .map(|(phrase, count)| PhraseCount {
            phrase: phrase.clone(),
            count: *count,
        })
        .collect();
    let judgement_endings: Vec<PhraseCount> = collect_judgement_endings(ctx, &sentences)
        .into_iter()
        .map(|(phrase, count)| PhraseCount { phrase, count })
        .collect();
    let clause_prefixes: Vec<PhraseCount> = collect_clause_prefixes(ctx, &sentences)
        .into_iter()
        .map(|(phrase, count)| PhraseCount { phrase, count })
        .collect();
    let parallel_clauses: Vec<PhraseCount> = collect_parallel_clauses(ctx, &sentences)
        .into_iter()
        .map(|(phrase, count)| PhraseCount { phrase, count })
        .collect();
    let aa_bb_patterns = collect_aa_bb_patterns(ctx, &sentences, sample_limit);
    let aa_bb_warn = aa_bb_patterns.iter().any(|p| p.warn);
    let ba_operation_contexts = build_ba_operation_contexts(ctx, &sentence_infos, sample_limit);
    let ba_operation_context_warn = ba_operation_contexts.iter().any(|c| c.warn);
    let modifier_pressure = collect_modifier_pressure(ctx, &sentences);
    let sentence_lengths = build_sentence_length_profile(ctx, &sentence_infos);
    let fatigue_windows = build_fatigue_windows(ctx, &sentence_infos, sample_limit * 2);
    let fatigue_window_count = fatigue_windows
        .first()
        .map(|w| w.total_candidates)
        .unwrap_or(0);
    let judgement_contexts = collect_judgement_contexts(ctx, &sentence_infos, sample_limit);
    let judgement_context_warn = judgement_contexts.iter().any(|c| c.warn);
    let scene_map = build_scene_map(ctx, &paragraph_infos, sample_limit);
    let dialogue_emotions = build_dialogue_emotion_profile(ctx, &sentence_infos, sample_limit);
    let character_voice = build_character_voice_profile(ctx, &sentence_infos, sample_limit);
    let tone_profile = build_tone_profile(ctx, &paragraph_infos, sample_limit);
    let battle_profile = build_battle_profile(ctx, &sentence_infos, sample_limit);
    let viewpoint_profile = build_viewpoint_profile(ctx, &paragraph_infos, sample_limit);
    let tracked_term_windows = build_tracked_term_windows(
        ctx,
        term_bank,
        corpus_profile
            .map(|c| c.learned_terms.as_slice())
            .unwrap_or(&[]),
        &sentence_infos,
        sample_limit * 2,
    );
    let tracked_term_window_count = tracked_term_windows
        .first()
        .map(|w| w.total_candidates)
        .unwrap_or(0);
    let terms: Vec<TermCount> = collect_ngram_terms(ctx, text, &[(2, 8), (3, 5), (4, 4)], false)
        .into_iter()
        .map(|(term, count)| TermCount { term, count })
        .collect();
    let short_phrases: Vec<TermCount> =
        collect_ngram_terms(ctx, text, &[(2, 6), (3, 5), (4, 4)], true)
            .into_iter()
            .map(|(term, count)| TermCount { term, count })
            .collect();

    let mut dominant_punctuation: Vec<DominantPunctuation> = punctuation_metrics
        .iter()
        .filter(|m| m.count > 0)
        .map(|m| DominantPunctuation {
            mark: m.name.clone(),
            count: m.count,
            per_10k: m.per_10k,
        })
        .collect();
    dominant_punctuation.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.mark.cmp(&b.mark)));

    let dialogue = DialogueReport {
        consecutive_quote_paragraph_runs: quote_runs
            .iter()
            .map(|(start, end, sample)| QuoteRun {
                start_paragraph: *start,
                end_paragraph: *end,
                sample: sample.clone(),
            })
            .collect(),
        short_quote_runs: short_quote_runs
            .iter()
            .map(|(start, end, avg_len, sample)| ShortQuoteRun {
                start_paragraph: *start,
                end_paragraph: *end,
                avg_len: *avg_len,
                sample: sample.clone(),
            })
            .collect(),
        question_ping_pong: question_ping_pong
            .iter()
            .map(|(start, end, sample)| QuoteRun {
                start_paragraph: *start,
                end_paragraph: *end,
                sample: sample.clone(),
            })
            .collect(),
        quote_ping_pong: quote_ping_pong
            .iter()
            .map(|(start, end, avg_len, sample)| ShortQuoteRun {
                start_paragraph: *start,
                end_paragraph: *end,
                avg_len: *avg_len,
                sample: sample.clone(),
            })
            .collect(),
        dialogue_axis_gaps,
        alternating_speaker_runs: ab_turns,
        quote_paragraph_ratio: round4f(quote_runs.len() as f64 / paragraphs.len().max(1) as f64),
        dense_quote_run_max: quote_runs
            .iter()
            .map(|(start, end, _)| end - start + 1)
            .max()
            .unwrap_or(0),
        dense_quote_run_count: quote_runs
            .iter()
            .filter(|(start, end, _)| end - start + 1 >= 5)
            .count(),
    };
    let dialogue_warn = !dialogue.consecutive_quote_paragraph_runs.is_empty()
        || !dialogue.alternating_speaker_runs.is_empty()
        || !dialogue.dialogue_axis_gaps.is_empty();

    let lex = ctx.lexicon();
    let tail_text = tail_chars(text, 180).to_string();
    let ending_images: Vec<TermCount> = lex
        .ending_image_terms
        .iter()
        .filter(|term| tail_text.contains(term.as_str()))
        .map(|term| TermCount {
            term: term.clone(),
            count: tail_text.matches(term.as_str()).count(),
        })
        .collect();
    let ending_flows: Vec<TermCount> = lex
        .ending_flow_terms
        .iter()
        .filter(|term| tail_text.contains(term.as_str()))
        .map(|term| TermCount {
            term: term.clone(),
            count: tail_text.matches(term.as_str()).count(),
        })
        .collect();
    let ending_warn = ending_images.len() >= 3 || ending_flows.len() >= 2;
    let ending = Ending {
        tail_excerpt: tail_text.trim().to_string(),
        image_terms: ending_images,
        flow_terms: ending_flows,
        warn: ending_warn,
    };

    // template_candidates（顺序与 Python 逐条一致）。
    let mut template_candidates: Vec<TemplateCandidate> = Vec::new();
    for m in &tracked_term_metrics {
        if m.warn {
            template_candidates.push(TemplateCandidate {
                candidate_type: "tracked_term".to_string(),
                name: m.name.clone(),
                count: m.count,
                note: m.note.clone(),
                sample: first_snippet(&m.samples),
            });
        }
    }
    for (section, metrics) in [
        ("tokens", &token_metrics),
        ("patterns", &pattern_metrics),
        ("phrases", &phrase_metrics),
        ("modifiers", &modifier_metrics),
        ("punctuation", &punctuation_metrics),
        ("punctuation_combo", &combo_metrics),
    ] {
        for m in metrics {
            if m.warn {
                template_candidates.push(TemplateCandidate {
                    candidate_type: section.to_string(),
                    name: m.name.clone(),
                    count: m.count,
                    note: m.note.clone(),
                    sample: first_snippet(&m.samples),
                });
            }
        }
    }
    for m in &custom_template_metrics {
        if m.warn {
            template_candidates.push(TemplateCandidate {
                candidate_type: "custom_template".to_string(),
                name: m.name.clone(),
                count: m.count,
                note: m.note.clone(),
                sample: first_snippet(&m.samples),
            });
        }
    }
    for m in &learned_filter_metrics {
        if m.warn {
            template_candidates.push(TemplateCandidate {
                candidate_type: "learned_filter".to_string(),
                name: m.name.clone(),
                count: m.count,
                note: m.note.clone(),
                sample: first_snippet(&m.samples),
            });
        }
    }
    for item in &sentence_patterns {
        template_candidates.push(TemplateCandidate {
            candidate_type: "sentence_pattern".to_string(),
            name: item.phrase.clone(),
            count: item.count,
            note: "句首骨架重复".to_string(),
            sample: String::new(),
        });
    }
    for item in &aa_bb_patterns {
        if item.warn {
            template_candidates.push(TemplateCandidate {
                candidate_type: "aa_bb_pattern".to_string(),
                name: item.name.clone(),
                count: item.count,
                note: item.note.clone(),
                sample: item.samples.first().cloned().unwrap_or_default(),
            });
        }
    }
    for item in &ba_operation_contexts {
        if !item.warn {
            continue;
        }
        let sample = item
            .samples
            .first()
            .map(|s| s.sentence.clone())
            .unwrap_or_default();
        template_candidates.push(TemplateCandidate {
            candidate_type: "ba_operation_context".to_string(),
            name: item.role.clone(),
            count: item.count,
            note: format!("把字句类型偏密；{}", item.suggestion),
            sample,
        });
    }
    if sentence_lengths.warn {
        let sample = sentence_lengths
            .short_sentences
            .first()
            .map(|s| format!("L{} {}字：{}", s.line_no, s.chars, s.text))
            .unwrap_or_default();
        template_candidates.push(TemplateCandidate {
            candidate_type: "sentence_length".to_string(),
            name: "短句密度".to_string(),
            count: sentence_lengths.short_count,
            note: "短句过多或连发，会让草稿像节拍器或对白录音".to_string(),
            sample,
        });
    }
    for item in short_phrases.iter().take(5) {
        template_candidates.push(TemplateCandidate {
            candidate_type: "short_phrase".to_string(),
            name: item.term.clone(),
            count: item.count,
            note: "短语手感重复".to_string(),
            sample: String::new(),
        });
    }
    if let Some(first) = dialogue.consecutive_quote_paragraph_runs.first() {
        template_candidates.push(TemplateCandidate {
            candidate_type: "dialogue".to_string(),
            name: "连续短对白".to_string(),
            count: dialogue.consecutive_quote_paragraph_runs.len(),
            note: "A/B 乒乓过长".to_string(),
            sample: first.sample.join(" | "),
        });
    }
    if let Some(first) = dialogue.short_quote_runs.first() {
        template_candidates.push(TemplateCandidate {
            candidate_type: "dialogue".to_string(),
            name: "短句对白块".to_string(),
            count: dialogue.short_quote_runs.len(),
            note: "对白短句过密，容易写成互答录音".to_string(),
            sample: first.sample.join(" | "),
        });
    }
    if let Some(first) = dialogue.quote_ping_pong.first() {
        template_candidates.push(TemplateCandidate {
            candidate_type: "dialogue".to_string(),
            name: "对白乒乓".to_string(),
            count: dialogue.quote_ping_pong.len(),
            note: "纯对白互顶过长，缺少动作或场面转轴".to_string(),
            sample: first.sample.join(" | "),
        });
    }
    if let Some(first_gap) = dialogue.dialogue_axis_gaps.first() {
        template_candidates.push(TemplateCandidate {
            candidate_type: "dialogue_axis_gap".to_string(),
            name: "对白转轴缺口".to_string(),
            count: dialogue.dialogue_axis_gaps.len(),
            note: format!(
                "连续对白缺少动作、环境、第三方或设备转轴；{}",
                first_gap.suggestion
            ),
            sample: first_gap
                .sample
                .iter()
                .take(4)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | "),
        });
    }
    if scene_map.warn {
        let first_block = scene_map.blocks.first();
        template_candidates.push(TemplateCandidate {
            candidate_type: "scene_map".to_string(),
            name: "场面功能失衡".to_string(),
            count: scene_map.block_count,
            note: format!(
                "粗分块里 `{}` 占比偏高，检查这章是否长时间停在同一种叙事功能里。",
                scene_map.dominant_role
            ),
            sample: first_block
                .map(|b| {
                    b.sample
                        .iter()
                        .take(2)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" | ")
                })
                .unwrap_or_default(),
        });
    }
    if dialogue_emotions.flatness_warn || dialogue_emotions.volatility_warn {
        let first_sample = dialogue_emotions.samples.first();
        template_candidates.push(TemplateCandidate {
            candidate_type: "dialogue_emotion".to_string(),
            name: "对白情绪单一/横跳".to_string(),
            count: if dialogue_emotions.shift_count > 0 {
                dialogue_emotions.shift_count
            } else {
                dialogue_emotions.dialogue_sentences
            },
            note: format!(
                "dominant={} shift={}，检查对白是否只在重复顶回去。",
                dialogue_emotions.dominant_emotion, dialogue_emotions.shift_count
            ),
            sample: first_sample.map(|s| s.text.clone()).unwrap_or_default(),
        });
    }
    if character_voice.warn {
        let first_speaker = character_voice.speakers.first();
        template_candidates.push(TemplateCandidate {
            candidate_type: "character_voice".to_string(),
            name: "角色对白同质化".to_string(),
            count: character_voice.speaker_count,
            note: "多名角色的对白节拍、问句率和情绪主导过近，检查是否越来越像同一个人在说话。"
                .to_string(),
            sample: first_speaker.map(|s| s.speaker.clone()).unwrap_or_default(),
        });
    }
    if battle_profile.warn {
        let first_sample = battle_profile.samples.first();
        template_candidates.push(TemplateCandidate {
            candidate_type: "battle_profile".to_string(),
            name: "动作链缺结果".to_string(),
            count: battle_profile.sequence_count,
            note: "动作/冲突段有推进，但结果句、受伤反馈或位移后果不足，容易只剩挥打。".to_string(),
            sample: first_sample
                .map(|s| {
                    s.sample
                        .iter()
                        .take(4)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" | ")
                })
                .unwrap_or_default(),
        });
    }
    if viewpoint_profile.warn {
        let first_overlap = viewpoint_profile.overlaps.first();
        template_candidates.push(TemplateCandidate {
            candidate_type: "viewpoint_profile".to_string(),
            name: "视角锚点漂移".to_string(),
            count: if viewpoint_profile.overlap_count > 0 {
                viewpoint_profile.overlap_count
            } else {
                viewpoint_profile.switch_count
            },
            note: "同段多人物心理暴露或近距离视角反复换锚，读者容易丢当前镜头中心。".to_string(),
            sample: first_overlap.map(|o| o.text.clone()).unwrap_or_default(),
        });
    }
    if ending_warn {
        let sample_source = ending.tail_excerpt.replace('\n', " ");
        template_candidates.push(TemplateCandidate {
            candidate_type: "ending".to_string(),
            name: "章末模板".to_string(),
            count: ending.image_terms.len() + ending.flow_terms.len(),
            note: "章末意象或流程词偏密，检查是否又在模板化收尾".to_string(),
            sample: prefix_chars(sample_source.as_str(), 80),
        });
    }
    if let Some(first_window) = fatigue_windows.first() {
        template_candidates.push(TemplateCandidate {
            candidate_type: "fatigue_window".to_string(),
            name: "局部疲劳窗口".to_string(),
            count: fatigue_windows.len(),
            note: "短句、判断、把字句、角色起手等问题在局部连续叠加".to_string(),
            sample: first_window
                .sample
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | "),
        });
    }
    if let Some(first_window) = tracked_term_windows.first() {
        template_candidates.push(TemplateCandidate {
            candidate_type: "tracked_term_window".to_string(),
            name: "点名局部密度".to_string(),
            count: tracked_term_windows.len(),
            note: format!(
                "同一名词或同类跟踪词在局部窗口内密集出现；{}",
                format_tracked_term_counts(
                    &first_window.terms[..first_window.terms.len().min(3)],
                    "，"
                )
            ),
            sample: first_window
                .sample
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | "),
        });
    }
    for item in &judgement_contexts {
        if !item.warn {
            continue;
        }
        let sample = item
            .samples
            .first()
            .map(|s| format!("L{} {}", s.line_no, s.text))
            .unwrap_or_default();
        template_candidates.push(TemplateCandidate {
            candidate_type: "judgement_context".to_string(),
            name: item.label.clone(),
            count: item.count,
            note: "旁白判断句偏密，容易替场面下结论".to_string(),
            sample,
        });
    }

    // hard_flags（顺序与 Python 一致：先 9 指标节，再其余固定行，最后整体排序）。
    let mut hard_flags: Vec<HardFlag> = Vec::new();
    let extend_flags = |section: &str, metrics: &[RegexMetric], flags: &mut Vec<HardFlag>| {
        for m in metrics {
            if !m.warn {
                continue;
            }
            flags.push(HardFlag {
                section: section.to_string(),
                name: m.name.clone(),
                count: m.count,
                per_10k: Some(m.per_10k),
                note: m.note.clone(),
                sample: first_snippet(&m.samples),
            });
        }
    };
    for m in &tracked_term_metrics {
        if m.warn {
            hard_flags.push(HardFlag {
                section: "tracked_terms".to_string(),
                name: m.name.clone(),
                count: m.count,
                per_10k: Some(m.per_10k),
                note: m.note.clone(),
                sample: first_snippet(&m.samples),
            });
        }
    }
    extend_flags("tokens", &token_metrics, &mut hard_flags);
    extend_flags("patterns", &pattern_metrics, &mut hard_flags);
    extend_flags("phrases", &phrase_metrics, &mut hard_flags);
    extend_flags("modifiers", &modifier_metrics, &mut hard_flags);
    extend_flags("punctuation", &punctuation_metrics, &mut hard_flags);
    extend_flags("punctuation_combos", &combo_metrics, &mut hard_flags);
    for m in &custom_template_metrics {
        if m.warn {
            hard_flags.push(HardFlag {
                section: "custom_templates".to_string(),
                name: m.name.clone(),
                count: m.count,
                per_10k: Some(m.per_10k),
                note: m.note.clone(),
                sample: first_snippet(&m.samples),
            });
        }
    }
    for m in &learned_filter_metrics {
        if m.warn {
            hard_flags.push(HardFlag {
                section: "learned_filters".to_string(),
                name: m.name.clone(),
                count: m.count,
                per_10k: Some(m.per_10k),
                note: m.note.clone(),
                sample: first_snippet(&m.samples),
            });
        }
    }
    let push_flag = |section: &str,
                     name: String,
                     count: usize,
                     note: String,
                     sample: String,
                     flags: &mut Vec<HardFlag>| {
        flags.push(HardFlag {
            section: section.to_string(),
            name,
            count,
            per_10k: None,
            note,
            sample,
        });
    };
    for item in &sentence_patterns {
        push_flag(
            "sentence_patterns",
            item.phrase.clone(),
            item.count,
            "句首骨架重复".to_string(),
            String::new(),
            &mut hard_flags,
        );
    }
    for item in &subject_leads_rows {
        push_flag(
            "subject_leads",
            item.phrase.clone(),
            item.count,
            "主语起手重复".to_string(),
            String::new(),
            &mut hard_flags,
        );
    }
    for item in &paragraph_leads_rows {
        push_flag(
            "paragraph_leads",
            item.phrase.clone(),
            item.count,
            "段首起手重复".to_string(),
            String::new(),
            &mut hard_flags,
        );
    }
    for item in &clause_prefixes {
        push_flag(
            "clause_prefixes",
            item.phrase.clone(),
            item.count,
            "分句骨架重复".to_string(),
            String::new(),
            &mut hard_flags,
        );
    }
    for item in short_phrases.iter().take(10) {
        push_flag(
            "short_phrases",
            item.term.clone(),
            item.count,
            "结构短语重复".to_string(),
            String::new(),
            &mut hard_flags,
        );
    }
    for item in &aa_bb_patterns {
        if !item.warn {
            continue;
        }
        push_flag(
            "aa_bb_patterns",
            item.name.clone(),
            item.count,
            item.note.clone(),
            item.samples.first().cloned().unwrap_or_default(),
            &mut hard_flags,
        );
    }
    for item in &ba_operation_contexts {
        if !item.warn {
            continue;
        }
        let sample = item
            .samples
            .first()
            .map(|s| s.sentence.clone())
            .unwrap_or_default();
        push_flag(
            "ba_operation_contexts",
            item.role.clone(),
            item.count,
            format!("把字句类型偏密；{}", item.suggestion),
            sample,
            &mut hard_flags,
        );
    }
    if sentence_lengths.warn {
        if let Some(first_run) = sentence_lengths.short_runs.first() {
            push_flag(
                "sentence_lengths",
                "短句连发".to_string(),
                sentence_lengths.short_runs.len(),
                format!(
                    "连续短句会把叙述切成机械节拍；类型：{}；建议：{}",
                    format_short_roles(&first_run.roles, "，"),
                    first_run.suggestion
                ),
                first_run.sample.join(" | "),
                &mut hard_flags,
            );
        }
        if let Some(first_short) = sentence_lengths.short_sentences.first() {
            push_flag(
                "sentence_lengths",
                "短句密度".to_string(),
                sentence_lengths.short_count,
                "短句过多时需要判断是节奏控制还是内容没写开".to_string(),
                format!(
                    "L{} {}字：{}",
                    first_short.line_no, first_short.chars, first_short.text
                ),
                &mut hard_flags,
            );
        }
    }
    if let Some(first) = dialogue.consecutive_quote_paragraph_runs.first() {
        push_flag(
            "dialogue",
            "连续短对白块".to_string(),
            dialogue.consecutive_quote_paragraph_runs.len(),
            "对话过长且缺少动作转轴".to_string(),
            first.sample.join(" | "),
            &mut hard_flags,
        );
    }
    if let Some(first) = dialogue.short_quote_runs.first() {
        push_flag(
            "dialogue",
            "短句对白块".to_string(),
            dialogue.short_quote_runs.len(),
            "对白像互答录音".to_string(),
            first.sample.join(" | "),
            &mut hard_flags,
        );
    }
    if let Some(first) = dialogue.question_ping_pong.first() {
        push_flag(
            "dialogue",
            "问答互顶".to_string(),
            dialogue.question_ping_pong.len(),
            "问一句顶一句，像脚本对白".to_string(),
            first.sample.join(" | "),
            &mut hard_flags,
        );
    }
    if let Some(first) = dialogue.quote_ping_pong.first() {
        push_flag(
            "dialogue",
            "对白乒乓".to_string(),
            dialogue.quote_ping_pong.len(),
            "纯对白来回互顶".to_string(),
            first.sample.join(" | "),
            &mut hard_flags,
        );
    }
    if let Some(first_gap) = dialogue.dialogue_axis_gaps.first() {
        push_flag(
            "dialogue_axis_gaps",
            "对白转轴缺口".to_string(),
            dialogue.dialogue_axis_gaps.len(),
            format!(
                "连续对白缺少动作、环境、第三方或设备转轴；{}",
                first_gap.suggestion
            ),
            first_gap
                .sample
                .iter()
                .take(4)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | "),
            &mut hard_flags,
        );
    }
    if scene_map.warn {
        let first_block = scene_map.blocks.first();
        push_flag(
            "scene_map",
            "场面功能失衡".to_string(),
            scene_map.block_count,
            format!(
                "粗分块里 `{}` 占比 `{}`，场面功能切换偏少。",
                scene_map.dominant_role,
                py_float_str(scene_map.dominance_ratio)
            ),
            first_block
                .map(|b| {
                    b.sample
                        .iter()
                        .take(2)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" | ")
                })
                .unwrap_or_default(),
            &mut hard_flags,
        );
    }
    if dialogue_emotions.flatness_warn || dialogue_emotions.volatility_warn {
        let first_sample = dialogue_emotions.samples.first();
        push_flag(
            "dialogue_emotions",
            "对白情绪单一/横跳".to_string(),
            if dialogue_emotions.shift_count > 0 {
                dialogue_emotions.shift_count
            } else {
                dialogue_emotions.dialogue_sentences
            },
            format!(
                "dominant={} ratio={} shift={}",
                dialogue_emotions.dominant_emotion,
                py_float_str(dialogue_emotions.dominant_ratio),
                dialogue_emotions.shift_count
            ),
            first_sample.map(|s| s.text.clone()).unwrap_or_default(),
            &mut hard_flags,
        );
    }
    if character_voice.warn {
        push_flag(
            "character_voice",
            "角色对白同质化".to_string(),
            character_voice.speaker_count,
            format!(
                "dominant={} coverage={}，多名角色对白画像过近。",
                character_voice.dominant_speaker,
                py_float_str(character_voice.coverage_ratio)
            ),
            character_voice
                .homogenized_pairs
                .iter()
                .take(2)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | "),
            &mut hard_flags,
        );
    }
    if battle_profile.warn {
        let first_sample = battle_profile.samples.first();
        push_flag(
            "battle_profile",
            "动作链缺结果".to_string(),
            battle_profile.sequence_count,
            format!(
                "result_ratio={}，动作句已有堆积，但结果/伤害反馈不足。",
                py_float_str(battle_profile.result_ratio)
            ),
            first_sample
                .map(|s| {
                    s.sample
                        .iter()
                        .take(4)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" | ")
                })
                .unwrap_or_default(),
            &mut hard_flags,
        );
    }
    if viewpoint_profile.warn {
        let first_overlap = viewpoint_profile.overlaps.first();
        push_flag(
            "viewpoint_profile",
            "视角锚点漂移".to_string(),
            if viewpoint_profile.overlap_count > 0 {
                viewpoint_profile.overlap_count
            } else {
                viewpoint_profile.switch_count
            },
            "同段多人物心理暴露或近距离切锚偏多。".to_string(),
            first_overlap.map(|o| o.text.clone()).unwrap_or_default(),
            &mut hard_flags,
        );
    }
    if ending.warn {
        let sample_source = ending.tail_excerpt.replace('\n', " ");
        push_flag(
            "ending",
            "章末模板".to_string(),
            ending.image_terms.len() + ending.flow_terms.len(),
            "章末意象或流程词偏密".to_string(),
            prefix_chars(sample_source.as_str(), 80),
            &mut hard_flags,
        );
    }
    if let Some(first_window) = fatigue_windows.first() {
        push_flag(
            "fatigue_windows",
            "局部疲劳窗口".to_string(),
            fatigue_windows.len(),
            format!(
                "短句、判断、把字句、角色起手等问题在局部连续叠加；类型：{}；建议：{}",
                format_short_roles(&first_window.roles, "，"),
                first_window.suggestion
            ),
            first_window
                .sample
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | "),
            &mut hard_flags,
        );
    }
    if let Some(first_window) = tracked_term_windows.first() {
        push_flag(
            "tracked_term_windows",
            "点名局部密度".to_string(),
            tracked_term_windows.len(),
            format!(
                "同一名词或同类跟踪词在局部窗口内密集出现；{}；建议：{}",
                format_tracked_term_counts(
                    &first_window.terms[..first_window.terms.len().min(3)],
                    "，"
                ),
                first_window.suggestion
            ),
            first_window
                .sample
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | "),
            &mut hard_flags,
        );
    }
    for item in &judgement_contexts {
        if !item.warn {
            continue;
        }
        let sample = item
            .samples
            .first()
            .map(|s| format!("L{} {}", s.line_no, s.text))
            .unwrap_or_default();
        push_flag(
            "judgement_contexts",
            item.label.clone(),
            item.count,
            "旁白判断句偏密，容易替场面下结论".to_string(),
            sample,
            &mut hard_flags,
        );
    }
    hard_flags.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.section.cmp(&b.section))
            .then_with(|| a.name.cmp(&b.name))
    });

    let warned = token_warn
        || pattern_warn
        || phrase_warn
        || modifier_warn
        || punctuation_warn
        || combo_warn
        || custom_warn
        || tracked_term_warn
        || learned_filter_warn
        || !repeated_starts.is_empty()
        || !subject_leads_rows.is_empty()
        || !paragraph_leads_rows.is_empty()
        || !sentence_patterns.is_empty()
        || !judgement_endings.is_empty()
        || !clause_prefixes.is_empty()
        || !parallel_clauses.is_empty()
        || aa_bb_warn
        || ba_operation_context_warn
        || sentence_lengths.warn
        || modifier_pressure.iter().any(|m| m.warn)
        || !short_phrases.is_empty()
        || dialogue_warn
        || scene_map.warn
        || dialogue_emotions.flatness_warn
        || dialogue_emotions.volatility_warn
        || battle_profile.warn
        || viewpoint_profile.warn
        || !dialogue.short_quote_runs.is_empty()
        || !dialogue.question_ping_pong.is_empty()
        || !dialogue.quote_ping_pong.is_empty()
        || ending_warn
        || !fatigue_windows.is_empty()
        || judgement_context_warn
        || !tracked_term_windows.is_empty();
    let warn_sections = usize::from(token_warn)
        + usize::from(pattern_warn)
        + usize::from(phrase_warn)
        + usize::from(modifier_warn)
        + usize::from(punctuation_warn)
        + usize::from(combo_warn)
        + usize::from(custom_warn)
        + usize::from(tracked_term_warn)
        + usize::from(learned_filter_warn)
        + usize::from(!repeated_starts.is_empty())
        + usize::from(!subject_leads_rows.is_empty())
        + usize::from(!paragraph_leads_rows.is_empty())
        + usize::from(!sentence_patterns.is_empty())
        + usize::from(!judgement_endings.is_empty())
        + usize::from(!clause_prefixes.is_empty())
        + usize::from(!parallel_clauses.is_empty())
        + usize::from(aa_bb_warn)
        + usize::from(ba_operation_context_warn)
        + usize::from(sentence_lengths.warn)
        + usize::from(modifier_pressure.iter().any(|m| m.warn))
        + usize::from(!short_phrases.is_empty())
        + usize::from(dialogue_warn)
        + usize::from(scene_map.warn)
        + usize::from(dialogue_emotions.flatness_warn || dialogue_emotions.volatility_warn)
        + usize::from(battle_profile.warn)
        + usize::from(viewpoint_profile.warn)
        + usize::from(!dialogue.short_quote_runs.is_empty())
        + usize::from(!dialogue.question_ping_pong.is_empty())
        + usize::from(!dialogue.quote_ping_pong.is_empty())
        + usize::from(ending_warn)
        + usize::from(!fatigue_windows.is_empty())
        + usize::from(judgement_context_warn)
        + usize::from(!tracked_term_windows.is_empty());
    let summary = Summary {
        chars,
        sentences: sentence_infos.len(),
        paragraphs: paragraphs.len(),
        avg_sentence_chars: round2(chars as f64 / sentence_infos.len().max(1) as f64),
        short_sentences: sentence_lengths.short_count,
        very_short_sentences: sentence_lengths.very_short_count,
        short_sentence_ratio: sentence_lengths.short_ratio,
        quote_ratio: round4f(quote_ratio(text)),
        warn_sections,
    };

    let mut analysis = Analysis {
        source: source.to_string(),
        summary,
        warned,
        tokens: token_metrics,
        tracked_terms: tracked_term_metrics,
        tracked_term_categories,
        tracked_term_windows,
        tracked_term_window_count,
        ba_operation_contexts,
        patterns: pattern_metrics,
        phrases: phrase_metrics,
        modifiers: modifier_metrics,
        punctuation: punctuation_metrics,
        punctuation_combos: combo_metrics,
        custom_templates: custom_template_metrics,
        learned_filters: learned_filter_metrics,
        corpus_profile: corpus_profile_json(corpus_profile),
        dominant_punctuation,
        sentence_starts: repeated_starts,
        subject_leads: subject_leads_rows,
        paragraph_leads: paragraph_leads_rows,
        sentence_patterns,
        judgement_endings,
        clause_prefixes,
        parallel_clauses,
        aa_bb_patterns,
        sentence_lengths,
        fatigue_windows,
        fatigue_window_count,
        judgement_contexts,
        modifier_pressure,
        terms,
        short_phrases,
        dialogue,
        scene_map,
        dialogue_emotions,
        character_voice,
        tone_profile,
        battle_profile,
        viewpoint_profile,
        ending,
        template_candidates,
        hard_flags,
        style_fatigue: Vec::new(),
        review_reminders: Vec::new(),
    };
    analysis.style_fatigue = build_style_fatigue(&analysis);
    analysis.review_reminders = build_review_reminders(&analysis);
    Ok(analysis)
}

/// 对齐 Python `analyze_path`：读文件文本后走 `analyze_text`（source 为路径字符串）。
pub fn analyze_path(
    ctx: &DraftContext,
    path: &Path,
    template_bank: &[TemplateRule],
    term_bank: &[TrackedTerm],
    corpus_profile: Option<&CorpusProfile>,
    sample_limit: usize,
) -> Result<Analysis> {
    let text =
        fs::read_to_string(path).with_context(|| format!("无法读取文件 {}", path.display()))?;
    analyze_text(
        ctx,
        &text,
        &path.display().to_string(),
        template_bank,
        term_bank,
        corpus_profile,
        sample_limit,
    )
}
// ---------------------------------------------------------------------------
// 文本报告（对齐 Python `format_text_report` / `_format_metric_block`）

/// 文本指标行（4 类指标的渲染字段形状一致）。
struct TextMetric<'a> {
    name: &'a str,
    count: usize,
    per_10k: f64,
    max_per_10k: f64,
    note: &'a str,
    warn: bool,
    samples: &'a [Hit],
}

fn text_metric<'a>(
    name: &'a str,
    count: usize,
    per_10k: f64,
    max_per_10k: f64,
    note: &'a str,
    warn: bool,
    samples: &'a [Hit],
) -> TextMetric<'a> {
    TextMetric {
        name,
        count,
        per_10k,
        max_per_10k,
        note,
        warn,
        samples,
    }
}

/// 渲染一个规则指标小节（对齐 Python `_format_metric_block`）。
fn metric_block_lines(title: &str, metrics: &[TextMetric<'_>], sample_limit: usize) -> Vec<String> {
    let mut out = vec![format!("{title}:")];
    for metric in metrics {
        let status = if metric.warn { "WARN" } else { "OK" };
        out.push(format!(
            "  [{status}] {name}: count={count}, per_10k={per_10k:.2}, max={max:.2}  # {note}",
            name = metric.name,
            count = metric.count,
            per_10k = metric.per_10k,
            max = metric.max_per_10k,
            note = metric.note,
        ));
        for sample in metric.samples.iter().take(sample_limit) {
            out.push(format!("    L{}: {}", sample.line_no, sample.snippet));
        }
    }
    out
}

/// 四类指标 → 文本行（字段形状一致；`name/count/per_10k/max_per_10k/note/warn/samples`）。
fn regex_text_rows(list: &[RegexMetric]) -> Vec<TextMetric<'_>> {
    list.iter()
        .map(|m| {
            text_metric(
                &m.name,
                m.count,
                m.per_10k,
                m.max_per_10k,
                &m.note,
                m.warn,
                &m.samples,
            )
        })
        .collect()
}

fn tracked_text_rows(list: &[crate::rules::TrackedMetric]) -> Vec<TextMetric<'_>> {
    list.iter()
        .map(|m| {
            text_metric(
                &m.name,
                m.count,
                m.per_10k,
                m.max_per_10k,
                &m.note,
                m.warn,
                &m.samples,
            )
        })
        .collect()
}

fn custom_text_rows(list: &[CustomTemplateMetric]) -> Vec<TextMetric<'_>> {
    list.iter()
        .map(|m| {
            text_metric(
                &m.name,
                m.count,
                m.per_10k,
                m.max_per_10k,
                &m.note,
                m.warn,
                &m.samples,
            )
        })
        .collect()
}

fn learned_text_rows(list: &[LearnedFilterMetric]) -> Vec<TextMetric<'_>> {
    list.iter()
        .map(|m| {
            text_metric(
                &m.name,
                m.count,
                m.per_10k,
                m.max_per_10k,
                &m.note,
                m.warn,
                &m.samples,
            )
        })
        .collect()
}

/// Python `format_text_report` 的完整移植：把完整 analysis 渲染成文本报告。
#[must_use]
pub fn format_text_report(a: &Analysis, sample_limit: usize) -> String {
    let s = &a.summary;
    let mut output: Vec<String> = vec![
        format!("FILE {}", a.source),
        format!("chars={}", s.chars),
        format!("sentences={}", s.sentences),
        format!("paragraphs={}", s.paragraphs),
        format!("avg_sentence_chars={}", py_float_str(s.avg_sentence_chars)),
        format!("short_sentences={}", s.short_sentences),
        format!("very_short_sentences={}", s.very_short_sentences),
        format!(
            "short_sentence_ratio={}",
            py_float_str(s.short_sentence_ratio)
        ),
        format!("quote_ratio={}", py_float_str(s.quote_ratio)),
        format!("warn_sections={}", s.warn_sections),
    ];

    // hard_flags
    output.push("hard_flags:".into());
    output.push(format!("  count={}", a.hard_flags.len()));
    for item in a.hard_flags.iter().take(sample_limit * 8) {
        let mut line = format!("    [{}] {}: count={}", item.section, item.name, item.count);
        if let Some(per_10k) = item.per_10k {
            line.push_str(&format!(", per_10k={}", py_float_str(per_10k)));
        }
        line.push_str(&format!("  # {}", item.note));
        if !item.sample.is_empty() {
            line.push_str(&format!(" | {}", item.sample));
        }
        output.push(line);
    }

    // review_reminders
    output.push("review_reminders:".into());
    output.push(format!("  count={}", a.review_reminders.len()));
    for item in a.review_reminders.iter().take(sample_limit * 4) {
        output.push(format!(
            "    [{}] {} {}  # {}",
            item.priority, item.category, item.title, item.reason
        ));
        output.push(format!("      check: {}", item.check));
        output.push(format!("      action: {}", item.action));
        for evidence in item.evidence.iter().take(sample_limit) {
            output.push(format!("      evidence: {evidence}"));
        }
    }

    // style_fatigue
    output.push("style_fatigue:".into());
    output.push(format!("  count={}", a.style_fatigue.len()));
    for item in &a.style_fatigue {
        output.push(format!(
            "    [{}] {}: count={}  # {}",
            item.status, item.family, item.count, item.risk
        ));
        output.push(format!("      reduce: {}", item.reduce));
        for evidence in item.evidence.iter().take(sample_limit) {
            output.push(format!("      evidence: {evidence}"));
        }
    }

    // fatigue_windows
    output.push("fatigue_windows:".into());
    output.push(format!(
        "  count={}, shown={}",
        a.fatigue_window_count,
        a.fatigue_windows.len()
    ));
    for item in a.fatigue_windows.iter().take(sample_limit) {
        let roles = format_short_roles(&item.roles, ",");
        output.push(format!(
            "    S{}-{} L{}-{} score={} reasons={} roles={}",
            item.start_index,
            item.end_index,
            item.start_line,
            item.end_line,
            item.score,
            item.reasons.join(","),
            roles
        ));
        output.push(format!("      suggestion: {}", item.suggestion));
        let sample = item
            .sample
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ");
        output.push(format!("      sample: {sample}"));
    }

    // ba_operation_contexts
    output.push("ba_operation_contexts:".into());
    output.push(format!(
        "  [{}] types={}",
        if a.ba_operation_contexts.iter().any(|c| c.warn) {
            "WARN"
        } else {
            "OK"
        },
        a.ba_operation_contexts.len()
    ));
    for item in a.ba_operation_contexts.iter().take(sample_limit * 3) {
        output.push(format!(
            "    {} {}: count={} suggestion={}",
            if item.warn { "WARN" } else { "WATCH" },
            item.role,
            item.count,
            item.suggestion
        ));
        for sample in item.samples.iter().take(sample_limit) {
            output.push(format!(
                "      - S{} L{} {}: {}",
                sample.index, sample.line_no, sample.snippet, sample.sentence
            ));
        }
    }

    // 9 个规则指标小节（顺序与 Python 一致）
    output.extend(metric_block_lines(
        "tokens",
        &regex_text_rows(&a.tokens),
        sample_limit,
    ));
    output.extend(metric_block_lines(
        "tracked_terms",
        &tracked_text_rows(&a.tracked_terms),
        sample_limit,
    ));
    output.extend(metric_block_lines(
        "patterns",
        &regex_text_rows(&a.patterns),
        sample_limit,
    ));
    output.extend(metric_block_lines(
        "phrases",
        &regex_text_rows(&a.phrases),
        sample_limit,
    ));
    output.extend(metric_block_lines(
        "modifiers",
        &regex_text_rows(&a.modifiers),
        sample_limit,
    ));
    output.extend(metric_block_lines(
        "punctuation",
        &regex_text_rows(&a.punctuation),
        sample_limit,
    ));
    output.extend(metric_block_lines(
        "punctuation_combos",
        &regex_text_rows(&a.punctuation_combos),
        sample_limit,
    ));
    output.extend(metric_block_lines(
        "custom_templates",
        &custom_text_rows(&a.custom_templates),
        sample_limit,
    ));
    output.extend(metric_block_lines(
        "learned_filters",
        &learned_text_rows(&a.learned_filters),
        sample_limit,
    ));

    // corpus_profile
    let p = &a.corpus_profile;
    output.push("corpus_profile:".into());
    output.push(format!(
        "  [{}] sources={} chars={} draft_chars={}",
        if p.enabled { "OK" } else { "OFF" },
        p.source_count,
        p.chars,
        p.draft_chars
    ));
    if let BaselineJson::Values(baseline) = &p.sentence_length_baseline {
        output.push(format!(
            "  baseline_sentence_chars: p10={}, p25={}, median={}, avg={}, short_ratio={}",
            baseline.p10_chars,
            baseline.p25_chars,
            baseline.median_chars,
            py_float_str(baseline.avg_chars),
            py_float_str(baseline.short_ratio)
        ));
    }
    for item in p.learned_sentence_leads.iter().take(sample_limit) {
        output.push(format!(
            "    learned_lead {}: count={}, corpus_per_10k={}",
            item.phrase,
            item.count,
            py_float_str(item.corpus_per_10k)
        ));
    }
    for item in p.learned_aa_bb_shapes.iter().take(sample_limit) {
        output.push(format!(
            "    learned_aa_bb {}: count={}",
            item.name, item.count
        ));
    }

    // tracked_term_categories
    output.push("tracked_term_categories:".into());
    output.push(format!(
        "  [{}] active_categories={}",
        if a.tracked_term_categories.iter().any(|c| c.warn) {
            "WARN"
        } else {
            "OK"
        },
        a.tracked_term_categories.len()
    ));
    for item in a.tracked_term_categories.iter().take(sample_limit * 4) {
        output.push(format!(
            "    {}: count={}, active_terms={}, warn_terms={}",
            item.category, item.count, item.active_terms, item.warn_terms
        ));
        for term in item.top_terms.iter().take(3) {
            output.push(format!(
                "      - {}: count={}, per_10k={}, warn={}",
                term.term,
                term.count,
                py_float_str(term.per_10k),
                if term.warn { "Y" } else { "N" }
            ));
        }
    }

    // tracked_term_windows
    output.push("tracked_term_windows:".into());
    output.push(format!(
        "  count={}, shown={}",
        a.tracked_term_window_count,
        a.tracked_term_windows.len()
    ));
    for item in a.tracked_term_windows.iter().take(sample_limit) {
        let terms = format_tracked_term_counts(&item.terms, ",");
        output.push(format!(
            "    S{}-{} L{}-{} score={} reasons={} terms={}",
            item.start_index,
            item.end_index,
            item.start_line,
            item.end_line,
            item.score,
            item.reasons.join(","),
            terms
        ));
        output.push(format!("      suggestion: {}", item.suggestion));
        let sample = item
            .sample
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ");
        output.push(format!("      sample: {sample}"));
    }

    // 短语计数小节（标题 + `[WARN|OK] key=N` + `短语: 计数` 行）
    let phrase_block =
        |output: &mut Vec<String>, title: &str, key: &str, items: &[PhraseCount], limit: usize| {
            output.push(format!("{title}:"));
            output.push(format!(
                "  [{}] {key}={}",
                if items.is_empty() { "OK" } else { "WARN" },
                items.len()
            ));
            for item in items.iter().take(limit) {
                output.push(format!("    {}: {}", item.phrase, item.count));
            }
        };
    phrase_block(
        &mut output,
        "sentence_starts",
        "repeated_sentence_leads",
        &a.sentence_starts,
        sample_limit * 3,
    );
    phrase_block(
        &mut output,
        "subject_leads",
        "repeated_subject_leads",
        &a.subject_leads,
        sample_limit * 4,
    );
    phrase_block(
        &mut output,
        "paragraph_leads",
        "repeated_paragraph_leads",
        &a.paragraph_leads,
        sample_limit * 4,
    );
    phrase_block(
        &mut output,
        "sentence_patterns",
        "repeated_sentence_skeletons",
        &a.sentence_patterns,
        sample_limit * 4,
    );
    phrase_block(
        &mut output,
        "judgement_endings",
        "repeated_judgement_endings",
        &a.judgement_endings,
        sample_limit * 4,
    );

    // judgement_contexts
    output.push("judgement_contexts:".into());
    output.push(format!(
        "  [{}] contexts={}",
        if a.judgement_contexts.iter().any(|c| c.warn) {
            "WARN"
        } else {
            "OK"
        },
        a.judgement_contexts.len()
    ));
    for item in &a.judgement_contexts {
        let terms = if item.top_terms.is_empty() {
            "无".to_string()
        } else {
            item.top_terms
                .iter()
                .map(|t| format!("{}:{}", t.term, t.count))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let status = if item.warn {
            "WARN"
        } else if item.watch {
            "WATCH"
        } else {
            "OK"
        };
        output.push(format!(
            "    {status} {}: count={} terms={}",
            item.label, item.count, terms
        ));
        for sample in item.samples.iter().take(sample_limit) {
            output.push(format!(
                "      S{} L{} {}: {}",
                sample.index,
                sample.line_no,
                sample.terms.join(","),
                sample.text
            ));
        }
    }

    phrase_block(
        &mut output,
        "clause_prefixes",
        "repeated_clause_prefixes",
        &a.clause_prefixes,
        sample_limit * 4,
    );
    phrase_block(
        &mut output,
        "parallel_clauses",
        "repeated_parallel_clauses",
        &a.parallel_clauses,
        sample_limit * 4,
    );

    // aa_bb_patterns
    output.push("aa_bb_patterns:".into());
    output.push(format!(
        "  [{}] aa_bb_patterns={}",
        if a.aa_bb_patterns.iter().any(|p| p.warn) {
            "WARN"
        } else {
            "OK"
        },
        a.aa_bb_patterns.len()
    ));
    for item in a.aa_bb_patterns.iter().take(sample_limit * 4) {
        output.push(format!(
            "    {} {} {}: count={}  # {}",
            if item.warn { "WARN" } else { "OK" },
            item.pattern_type,
            item.name,
            item.count,
            item.note
        ));
        for sample in item.samples.iter().take(sample_limit) {
            output.push(format!("      - {sample}"));
        }
    }

    // sentence_lengths
    let sl = &a.sentence_lengths;
    output.push("sentence_lengths:".into());
    output.push(format!(
        "  [{}] count={}, min={}, p10={}, p25={}, median={}, avg={}, max={}",
        if sl.warn { "WARN" } else { "OK" },
        sl.count,
        sl.min_chars,
        sl.p10_chars,
        sl.p25_chars,
        sl.median_chars,
        py_float_str(sl.avg_chars),
        sl.max_chars
    ));
    output.push(format!(
        "    short_count={}, very_short_count={}, short_ratio={}, short_runs={}",
        sl.short_count,
        sl.very_short_count,
        py_float_str(sl.short_ratio),
        sl.short_runs.len()
    ));
    for item in sl.short_sentences.iter().take(sample_limit * 4) {
        output.push(format!(
            "    S{} L{} chars={}: {}",
            item.index, item.line_no, item.chars, item.text
        ));
    }
    for item in sl.short_runs.iter().take(sample_limit) {
        let roles = format_short_roles(&item.roles, ",");
        output.push(format!(
            "    run S{}-{} L{}-{} avg={}: {}",
            item.start_index,
            item.end_index,
            item.start_line,
            item.end_line,
            py_float_str(item.avg_chars),
            item.sample.join(" | ")
        ));
        if !roles.is_empty() {
            output.push(format!("      roles: {roles}"));
        }
        if !item.suggestion.is_empty() {
            output.push(format!("      suggestion: {}", item.suggestion));
        }
    }

    // terms / short_phrases
    output.push("terms:".into());
    output.push(format!(
        "  [{}] repeated_terms={}",
        if a.terms.is_empty() { "OK" } else { "WARN" },
        a.terms.len()
    ));
    for item in a.terms.iter().take(sample_limit * 5) {
        output.push(format!("    {}: {}", item.term, item.count));
    }
    output.push("short_phrases:".into());
    output.push(format!(
        "  [{}] repeated_short_phrases={}",
        if a.short_phrases.is_empty() {
            "OK"
        } else {
            "WARN"
        },
        a.short_phrases.len()
    ));
    for item in a.short_phrases.iter().take(sample_limit * 5) {
        output.push(format!("    {}: {}", item.term, item.count));
    }

    // dialogue
    let d = &a.dialogue;
    output.push("dialogue:".into());
    let runs = &d.consecutive_quote_paragraph_runs;
    output.push(format!(
        "  [{}] consecutive_quote_paragraph_runs={}",
        if runs.is_empty() { "OK" } else { "WARN" },
        runs.len()
    ));
    for item in runs.iter().take(sample_limit) {
        output.push(format!(
            "    paragraph {}-{}: {}",
            item.start_paragraph,
            item.end_paragraph,
            item.sample.join(" | ")
        ));
    }
    let short_runs = &d.short_quote_runs;
    output.push(format!(
        "  [{}] short_quote_runs={}",
        if short_runs.is_empty() { "OK" } else { "WARN" },
        short_runs.len()
    ));
    for item in short_runs.iter().take(sample_limit) {
        output.push(format!(
            "    paragraph {}-{}: avg_len={} | {}",
            item.start_paragraph,
            item.end_paragraph,
            py_float_str(item.avg_len),
            item.sample.join(" | ")
        ));
    }
    let question_runs = &d.question_ping_pong;
    output.push(format!(
        "  [{}] question_ping_pong={}",
        if question_runs.is_empty() {
            "OK"
        } else {
            "WARN"
        },
        question_runs.len()
    ));
    for item in question_runs.iter().take(sample_limit) {
        output.push(format!(
            "    paragraph {}-{}: {}",
            item.start_paragraph,
            item.end_paragraph,
            item.sample.join(" | ")
        ));
    }
    let ping_pong_runs = &d.quote_ping_pong;
    output.push(format!(
        "  [{}] quote_ping_pong={}",
        if ping_pong_runs.is_empty() {
            "OK"
        } else {
            "WARN"
        },
        ping_pong_runs.len()
    ));
    for item in ping_pong_runs.iter().take(sample_limit) {
        output.push(format!(
            "    paragraph {}-{}: avg_len={} | {}",
            item.start_paragraph,
            item.end_paragraph,
            py_float_str(item.avg_len),
            item.sample.join(" | ")
        ));
    }
    let axis_gaps = &d.dialogue_axis_gaps;
    output.push(format!(
        "  [{}] dialogue_axis_gaps={}",
        if axis_gaps.is_empty() { "OK" } else { "WARN" },
        axis_gaps.len()
    ));
    for item in axis_gaps.iter().take(sample_limit) {
        output.push(format!(
            "    S{}-{} L{}-{} score={} reasons={}",
            item.start_index,
            item.end_index,
            item.start_line,
            item.end_line,
            item.score,
            item.reasons.join(",")
        ));
        output.push(format!("      suggestion: {}", item.suggestion));
        let sample = item
            .sample
            .iter()
            .take(4)
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ");
        output.push(format!("      sample: {sample}"));
    }
    let turns = &d.alternating_speaker_runs;
    output.push(format!(
        "  [{}] alternating_speaker_runs={}",
        if turns.is_empty() { "OK" } else { "WARN" },
        turns.len()
    ));
    for item in turns.iter().take(sample_limit) {
        output.push(format!(
            "    paragraph {}: {}",
            item.paragraph, item.pattern
        ));
    }
    output.push(format!(
        "  [INFO] quote_paragraph_ratio={}",
        py_float_str(d.quote_paragraph_ratio)
    ));
    output.push(format!(
        "  [INFO] dense_quote_run_max={}",
        d.dense_quote_run_max
    ));
    output.push(format!(
        "  [INFO] dense_quote_run_count={}",
        d.dense_quote_run_count
    ));

    // dominant_punctuation
    output.push("dominant_punctuation:".into());
    output.push(format!(
        "  [{}] active_marks={}",
        if a.dominant_punctuation.is_empty() {
            "OK"
        } else {
            "WARN"
        },
        a.dominant_punctuation.len()
    ));
    for item in a.dominant_punctuation.iter().take(sample_limit * 4) {
        output.push(format!(
            "    {}: count={}, per_10k={}",
            item.mark,
            item.count,
            py_float_str(item.per_10k)
        ));
    }

    // modifier_pressure（只展示 total > 0 的行）
    output.push("modifier_pressure:".into());
    let active: Vec<&ModifierPressure> = a
        .modifier_pressure
        .iter()
        .filter(|item| item.total > 0)
        .collect();
    output.push(format!(
        "  [{}] active_groups={}",
        if active.iter().any(|item| item.warn) {
            "WARN"
        } else {
            "OK"
        },
        active.len()
    ));
    for item in active.iter().take(sample_limit) {
        output.push(format!(
            "    {}: total={}, dense_sentences={}, warn={}",
            item.label,
            item.total,
            item.dense_sentences,
            if item.warn { "Y" } else { "N" }
        ));
    }

    // ending
    output.push("ending:".into());
    output.push(format!(
        "  [{}] tail_template_check",
        if a.ending.warn { "WARN" } else { "OK" }
    ));
    let image_summary = if a.ending.image_terms.is_empty() {
        "无".to_string()
    } else {
        a.ending
            .image_terms
            .iter()
            .map(|t| format!("{}:{}", t.term, t.count))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let flow_summary = if a.ending.flow_terms.is_empty() {
        "无".to_string()
    } else {
        a.ending
            .flow_terms
            .iter()
            .map(|t| format!("{}:{}", t.term, t.count))
            .collect::<Vec<_>>()
            .join(", ")
    };
    output.push(format!("    image_terms={image_summary}"));
    output.push(format!("    flow_terms={flow_summary}"));

    // template_candidates
    output.push("template_candidates:".into());
    output.push(format!("  count={}", a.template_candidates.len()));
    for item in a.template_candidates.iter().take(sample_limit * 6) {
        let mut line = format!(
            "    [{}] {} x{}  # {}",
            item.candidate_type, item.name, item.count, item.note
        );
        if !item.sample.is_empty() {
            line.push_str(&format!(" | {}", item.sample));
        }
        output.push(line);
    }

    output.join("\n")
}

/// Python `Path(str(source)).stem`：markdown 报告标题与多文件输出命名。
fn path_stem(source: &str) -> String {
    Path::new(source)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// 报告分发（对齐 Python `_render_report`）：markdown 标题取 source 的 stem
/// （空时回退 source），text 走 sample_limit；JSON 由调用方直接序列化。
#[must_use]
pub fn render_report(a: &Analysis, format: ReportFormat, sample_limit: usize) -> String {
    debug_assert!(
        format != ReportFormat::Json,
        "JSON 分支由调用方 serde 序列化，不走渲染"
    );
    if format == ReportFormat::Markdown {
        let title = path_stem(&a.source);
        return format_markdown_report(a, if title.is_empty() { None } else { Some(&title) });
    }
    format_text_report(a, sample_limit)
}
// ---------------------------------------------------------------------------
// Markdown 报告（对齐 Python `format_markdown_report` / `_markdown_table_cell`）

/// Markdown 表格单元格净化：换行 → 空格，`|` → `\|`（对齐 `_markdown_table_cell`）。
fn markdown_table_cell(value: &str) -> String {
    value.replace('\n', " ").replace('|', "\\|")
}

/// `add_metric_section` 的一行指标（各指标类型字段形状一致）。
struct MdMetric<'a> {
    name: &'a str,
    count: usize,
    per_10k: f64,
    max_per_10k: f64,
    warn: bool,
    samples: &'a [Hit],
}

/// 渲染一条规则指标小节（对齐 Python `add_metric_section`）。
fn metric_section_lines(header: &str, metrics: &[MdMetric<'_>]) -> Vec<String> {
    let mut lines = vec![format!("## {header}")];
    if metrics.is_empty() {
        lines.push("- 无".into());
        lines.push(String::new());
        return lines;
    }
    for metric in metrics {
        let status = if metric.warn { "WARN" } else { "OK" };
        lines.push(format!(
            "- `{status}` `{}` count=`{}` per_10k=`{}` max=`{}`",
            metric.name,
            metric.count,
            py_float_str(metric.per_10k),
            py_float_str(metric.max_per_10k)
        ));
        if let Some(sample) = metric.samples.first() {
            lines.push(format!("  样例：`L{}` {}", sample.line_no, sample.snippet));
        }
    }
    lines.push(String::new());
    lines
}

/// 规则指标行 → `MdMetric` 行（`RegexMetric` 与 markdown 小节同形状）。
fn regex_metric_rows(items: &[crate::rules::RegexMetric]) -> Vec<MdMetric<'_>> {
    items
        .iter()
        .map(|m| MdMetric {
            name: &m.name,
            count: m.count,
            per_10k: m.per_10k,
            max_per_10k: m.max_per_10k,
            warn: m.warn,
            samples: &m.samples,
        })
        .collect()
}

/// 对齐 Python `format_markdown_report`：把完整 analysis 渲染成 Markdown 报告。
/// `title` 为 None 时回退 `analysis.source`（对齐 `title or analysis["source"]`）。
#[must_use]
pub fn format_markdown_report(a: &Analysis, title: Option<&str>) -> String {
    let s = &a.summary;
    let title = title.unwrap_or(&a.source);
    let ending_image_md = a
        .ending
        .image_terms
        .iter()
        .map(|t| format!("{} x{}", t.term, t.count))
        .collect::<Vec<_>>()
        .join(", ");
    let ending_flow_md = a
        .ending
        .flow_terms
        .iter()
        .map(|t| format!("{} x{}", t.term, t.count))
        .collect::<Vec<_>>()
        .join(", ");
    let mut lines: Vec<String> = vec![format!("# {title}"), String::new()];

    // ## 概览
    lines.push("## 概览".into());
    lines.push(format!("- 来源：`{}`", a.source));
    lines.push(format!("- 字数：`{}`", s.chars));
    lines.push(format!("- 句子数：`{}`", s.sentences));
    lines.push(format!("- 段落数：`{}`", s.paragraphs));
    lines.push(format!(
        "- 句均字数：`{}`",
        py_float_str(s.avg_sentence_chars)
    ));
    lines.push(format!("- 短句数：`{}`", s.short_sentences));
    lines.push(format!("- 极短句数：`{}`", s.very_short_sentences));
    lines.push(format!(
        "- 短句占比：`{}`",
        py_float_str(s.short_sentence_ratio)
    ));
    lines.push(format!("- 引号占比：`{}`", py_float_str(s.quote_ratio)));
    lines.push(format!("- 警告分区数：`{}`", s.warn_sections));
    lines.push(format!(
        "- 总体状态：`{}`",
        if a.warned { "WARN" } else { "OK" }
    ));
    lines.push(String::new());

    // ## 审查提醒
    lines.push("## 审查提醒".into());
    if !a.review_reminders.is_empty() {
        for item in a.review_reminders.iter().take(8) {
            let mut line = format!(
                "- `{}` `{}` {}：{} 检查：{} 动作：{}",
                item.priority, item.category, item.title, item.reason, item.check, item.action
            );
            if !item.evidence.is_empty() {
                let evidence = item
                    .evidence
                    .iter()
                    .map(|v| markdown_table_cell(v))
                    .collect::<Vec<_>>()
                    .join("；");
                line.push_str(&format!(" 证据：{evidence}"));
            }
            lines.push(line);
        }
    } else {
        lines.push("- 无".into());
    }
    lines.push(String::new());

    // ## 句式疲劳雷达
    lines.push("## 句式疲劳雷达".into());
    if !a.style_fatigue.is_empty() {
        lines.push("| 状态 | 句式家族 | 数量 | 风险 | 减少方式 | 证据 |".into());
        lines.push("|---|---|---:|---|---|---|".into());
        for item in &a.style_fatigue {
            let joined = item
                .evidence
                .iter()
                .map(|v| markdown_table_cell(v))
                .collect::<Vec<_>>()
                .join("；");
            let evidence = if joined.is_empty() {
                "无".into()
            } else {
                joined
            };
            lines.push(format!(
                "| `{}` | {} | `{}` | {} | {} | {evidence} |",
                item.status,
                markdown_table_cell(&item.family),
                item.count,
                markdown_table_cell(&item.risk),
                markdown_table_cell(&item.reduce)
            ));
        }
    } else {
        lines.push("- 无".into());
    }
    lines.push(String::new());

    // ## 局部疲劳窗口
    lines.push("## 局部疲劳窗口".into());
    if !a.fatigue_windows.is_empty() {
        let shown = a.fatigue_windows.len().min(8);
        lines.push(format!(
            "- 命中总数：`{}`；展示：`{}`",
            a.fatigue_window_count, shown
        ));
        for item in a.fatigue_windows.iter().take(shown) {
            let roles = format_short_roles(&item.roles, "，");
            lines.push(format!(
                "- `S{}-{}` `L{}-{}` score=`{}`：{}",
                item.start_index,
                item.end_index,
                item.start_line,
                item.end_line,
                item.score,
                markdown_table_cell(&item.reasons.join("、"))
            ));
            if !roles.is_empty() {
                lines.push(format!("  类型：{}", markdown_table_cell(&roles)));
            }
            if !item.suggestion.is_empty() {
                lines.push(format!("  建议：{}", markdown_table_cell(&item.suggestion)));
            }
            let sample = item
                .sample
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ");
            lines.push(format!("  样例：{}", markdown_table_cell(&sample)));
        }
    } else {
        lines.push("- 无".into());
    }
    lines.push(String::new());

    // ## 把字操作分类
    lines.push("## 把字操作分类".into());
    if !a.ba_operation_contexts.is_empty() {
        for item in &a.ba_operation_contexts {
            lines.push(format!(
                "- `{}` `{}` count=`{}`：{}",
                if item.warn { "WARN" } else { "WATCH" },
                item.role,
                item.count,
                markdown_table_cell(&item.suggestion)
            ));
            for sample in item.samples.iter().take(5) {
                lines.push(format!(
                    "  - `S{}` `L{}` `{}`：{}",
                    sample.index,
                    sample.line_no,
                    sample.snippet,
                    markdown_table_cell(&sample.sentence)
                ));
            }
        }
    } else {
        lines.push("- 无".into());
    }
    lines.push(String::new());

    // ## 优先修项
    lines.push("## 优先修项".into());
    if !a.hard_flags.is_empty() {
        for item in a.hard_flags.iter().take(15) {
            let mut line = format!(
                "- `{}` `{}` x{}：{}",
                item.section, item.name, item.count, item.note
            );
            if let Some(per_10k) = item.per_10k {
                line.push_str(&format!("；per_10k=`{}`", py_float_str(per_10k)));
            }
            if !item.sample.is_empty() {
                line.push_str(&format!("；样例：{}", item.sample));
            }
            lines.push(line);
        }
    } else {
        lines.push("- 无".into());
    }
    lines.push(String::new());

    lines.extend(metric_section_lines(
        "高频词",
        &regex_metric_rows(&a.tokens),
    ));
    let tracked_rows: Vec<MdMetric> = a
        .tracked_terms
        .iter()
        .map(|m| MdMetric {
            name: &m.name,
            count: m.count,
            per_10k: m.per_10k,
            max_per_10k: m.max_per_10k,
            warn: m.warn,
            samples: &m.samples,
        })
        .collect();
    lines.extend(metric_section_lines("跟踪词", &tracked_rows));
    lines.extend(metric_section_lines(
        "模板句",
        &regex_metric_rows(&a.patterns),
    ));
    lines.extend(metric_section_lines(
        "短触发词",
        &regex_metric_rows(&a.phrases),
    ));
    lines.extend(metric_section_lines(
        "黏糊词与判断副词",
        &regex_metric_rows(&a.modifiers),
    ));
    lines.extend(metric_section_lines(
        "标点",
        &regex_metric_rows(&a.punctuation),
    ));
    lines.extend(metric_section_lines(
        "组合标点",
        &regex_metric_rows(&a.punctuation_combos),
    ));
    let custom_rows: Vec<MdMetric> = a
        .custom_templates
        .iter()
        .map(|m| MdMetric {
            name: &m.name,
            count: m.count,
            per_10k: m.per_10k,
            max_per_10k: m.max_per_10k,
            warn: m.warn,
            samples: &m.samples,
        })
        .collect();
    lines.extend(metric_section_lines("模板库命中", &custom_rows));
    let learned_rows: Vec<MdMetric> = a
        .learned_filters
        .iter()
        .map(|m| MdMetric {
            name: &m.name,
            count: m.count,
            per_10k: m.per_10k,
            max_per_10k: m.max_per_10k,
            warn: m.warn,
            samples: &m.samples,
        })
        .collect();
    lines.extend(metric_section_lines("语料学习筛选", &learned_rows));

    // ## 语料学习基线
    lines.push("## 语料学习基线".into());
    let p = &a.corpus_profile;
    if p.enabled {
        lines.push(format!(
            "- 学习来源：`{}` 个文件，语料字数=`{}`，草稿字数=`{}`",
            p.source_count, p.chars, p.draft_chars
        ));
        if let BaselineJson::Values(baseline) = &p.sentence_length_baseline {
            lines.push(format!(
                "- 草稿句长基线：p10=`{}` p25=`{}` median=`{}` avg=`{}` short_ratio=`{}`",
                baseline.p10_chars,
                baseline.p25_chars,
                baseline.median_chars,
                py_float_str(baseline.avg_chars),
                py_float_str(baseline.short_ratio)
            ));
        }
        if !p.learned_sentence_leads.is_empty() {
            let leads = p
                .learned_sentence_leads
                .iter()
                .take(8)
                .map(|i| format!("{} x{}", i.phrase, i.count))
                .collect::<Vec<_>>()
                .join("，");
            lines.push(format!("- 学到的句首高频：{leads}"));
        }
        if !p.learned_aa_bb_shapes.is_empty() {
            let shapes = p
                .learned_aa_bb_shapes
                .iter()
                .take(8)
                .map(|i| format!("{} x{}", i.name, i.count))
                .collect::<Vec<_>>()
                .join("，");
            lines.push(format!("- 学到的 AA/BB 风险：{shapes}"));
        }
    } else {
        lines.push("- 未启用".into());
    }
    lines.push(String::new());

    // ## 跟踪词分类
    lines.push("## 跟踪词分类".into());
    if !a.tracked_term_categories.is_empty() {
        for item in &a.tracked_term_categories {
            lines.push(format!(
                "- `{}` `{}` count=`{}` active_terms=`{}` warn_terms=`{}`",
                if item.warn { "WARN" } else { "OK" },
                item.category,
                item.count,
                item.active_terms,
                item.warn_terms
            ));
            for term in item.top_terms.iter().take(5) {
                lines.push(format!(
                    "  - `{}` x{} per_10k=`{}` warn=`{}`",
                    term.term,
                    term.count,
                    py_float_str(term.per_10k),
                    if term.warn { "Y" } else { "N" }
                ));
            }
        }
    } else {
        lines.push("- 无".into());
    }
    lines.push(String::new());

    // ## 点名局部密度
    lines.push("## 点名局部密度".into());
    if !a.tracked_term_windows.is_empty() {
        let shown = a.tracked_term_windows.len().min(8);
        lines.push(format!(
            "- 命中总数：`{}`；展示：`{}`",
            a.tracked_term_window_count, shown
        ));
        for item in a.tracked_term_windows.iter().take(shown) {
            lines.push(format!(
                "- `S{}-{}` `L{}-{}` score=`{}`：{}",
                item.start_index,
                item.end_index,
                item.start_line,
                item.end_line,
                item.score,
                markdown_table_cell(&item.reasons.join("、"))
            ));
            let terms = format_tracked_term_counts(&item.terms, "，");
            if !terms.is_empty() {
                lines.push(format!("  词项：{}", markdown_table_cell(&terms)));
            }
            if !item.suggestion.is_empty() {
                lines.push(format!("  建议：{}", markdown_table_cell(&item.suggestion)));
            }
            let sample = item
                .sample
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ");
            lines.push(format!("  样例：{}", markdown_table_cell(&sample)));
        }
    } else {
        lines.push("- 无".into());
    }
    lines.push(String::new());

    // ## 高频词片段 / 结构短语（各取前 15）
    lines.push("## 高频词片段".into());
    if a.terms.is_empty() {
        lines.push("- 无".into());
    } else {
        for item in a.terms.iter().take(15) {
            lines.push(format!("- `{}` x{}", item.term, item.count));
        }
    }
    lines.push(String::new());

    lines.push("## 结构短语".into());
    if a.short_phrases.is_empty() {
        lines.push("- 无".into());
    } else {
        for item in a.short_phrases.iter().take(15) {
            lines.push(format!("- `{}` x{}", item.term, item.count));
        }
    }
    lines.push(String::new());

    // ## 句式骨架 / 判断句尾（全量）
    lines.push("## 句式骨架".into());
    if a.sentence_patterns.is_empty() {
        lines.push("- 无".into());
    } else {
        for item in &a.sentence_patterns {
            lines.push(format!("- `{}` x{}", item.phrase, item.count));
        }
    }
    lines.push(String::new());

    lines.push("## 判断句尾".into());
    if a.judgement_endings.is_empty() {
        lines.push("- 无".into());
    } else {
        for item in &a.judgement_endings {
            lines.push(format!("- `{}` x{}", item.phrase, item.count));
        }
    }
    lines.push(String::new());

    // ## 判断句上下文
    lines.push("## 判断句上下文".into());
    if a.judgement_contexts.is_empty() {
        lines.push("- 无".into());
    } else {
        for item in &a.judgement_contexts {
            let status = if item.warn {
                "WARN"
            } else if item.watch {
                "WATCH"
            } else {
                "OK"
            };
            let terms = if item.top_terms.is_empty() {
                "无".to_string()
            } else {
                item.top_terms
                    .iter()
                    .map(|t| format!("{} x{}", t.term, t.count))
                    .collect::<Vec<_>>()
                    .join("，")
            };
            lines.push(format!(
                "- `{status}` `{}` count=`{}` terms={terms}",
                item.label, item.count
            ));
            for sample in item.samples.iter().take(5) {
                lines.push(format!(
                    "  - `S{}` `L{}` `{}`：{}",
                    sample.index,
                    sample.line_no,
                    sample.terms.join(","),
                    sample.text
                ));
            }
        }
    }
    lines.push(String::new());

    // ## 句首重复 / 主语起手 / 段首起手（全量）
    for (header, items) in [
        ("句首重复", &a.sentence_starts),
        ("主语起手", &a.subject_leads),
        ("段首起手", &a.paragraph_leads),
    ] {
        lines.push(format!("## {header}"));
        if items.is_empty() {
            lines.push("- 无".into());
        } else {
            for item in items {
                lines.push(format!("- `{}` x{}", item.phrase, item.count));
            }
        }
        lines.push(String::new());
    }

    // ## 分句骨架 / 并列分句（各取前 15）
    for (header, items) in [
        ("分句骨架", &a.clause_prefixes),
        ("并列分句", &a.parallel_clauses),
    ] {
        lines.push(format!("## {header}"));
        if items.is_empty() {
            lines.push("- 无".into());
        } else {
            for item in items.iter().take(15) {
                lines.push(format!("- `{}` x{}", item.phrase, item.count));
            }
        }
        lines.push(String::new());
    }

    // ## AA/BB 式短节奏（取前 15）
    lines.push("## AA/BB 式短节奏".into());
    if a.aa_bb_patterns.is_empty() {
        lines.push("- 无".into());
    } else {
        for item in a.aa_bb_patterns.iter().take(15) {
            lines.push(format!(
                "- `{}` `{}` `{}` x{}：{}",
                if item.warn { "WARN" } else { "OK" },
                item.pattern_type,
                item.name,
                item.count,
                item.note
            ));
            if let Some(first) = item.samples.first() {
                lines.push(format!("  样例：{first}"));
            }
        }
    }
    lines.push(String::new());

    // ## 逐句字数
    lines.push("## 逐句字数".into());
    let sl = &a.sentence_lengths;
    lines.push(format!(
        "- 状态：`{}` count=`{}` min=`{}` p10=`{}` p25=`{}` median=`{}` avg=`{}` max=`{}`",
        if sl.warn { "WARN" } else { "OK" },
        sl.count,
        sl.min_chars,
        sl.p10_chars,
        sl.p25_chars,
        sl.median_chars,
        py_float_str(sl.avg_chars),
        sl.max_chars
    ));
    lines.push(format!(
        "- 短句：`{}`；极短句：`{}`；短句占比：`{}`；短句连发：`{}`",
        sl.short_count,
        sl.very_short_count,
        py_float_str(sl.short_ratio),
        sl.short_runs.len()
    ));
    if !sl.short_sentences.is_empty() {
        for item in sl.short_sentences.iter().take(12) {
            lines.push(format!(
                "- `S{}` `L{}` `{}字`：{}",
                item.index, item.line_no, item.chars, item.text
            ));
        }
    }
    if !sl.short_runs.is_empty() {
        lines.push("- 短句连发样例：".into());
        for item in sl.short_runs.iter().take(5) {
            let roles = format_short_roles(&item.roles, "，");
            lines.push(format!(
                "- `S{}-{}` `L{}-{}` avg=`{}`：{}",
                item.start_index,
                item.end_index,
                item.start_line,
                item.end_line,
                py_float_str(item.avg_chars),
                item.sample.join(" | ")
            ));
            if !roles.is_empty() {
                lines.push(format!("  类型：{roles}"));
            }
            if !item.suggestion.is_empty() {
                lines.push(format!("  建议：{}", item.suggestion));
            }
        }
    }
    if !a.source.contains(" | ") {
        lines.push(String::new());
        lines.push("### 每句字数明细".into());
        for item in &sl.sentences {
            lines.push(format!(
                "- `S{}` `L{}` `{}字`：{}",
                item.index, item.line_no, item.chars, item.text
            ));
        }
    }
    lines.push(String::new());

    // ## 对话
    lines.push("## 对话".into());
    let d = &a.dialogue;
    lines.push(format!(
        "- 连续短对白块：`{}`",
        d.consecutive_quote_paragraph_runs.len()
    ));
    for item in d.consecutive_quote_paragraph_runs.iter().take(5) {
        lines.push(format!(
            "- 段落 `{}-{}`: {}",
            item.start_paragraph,
            item.end_paragraph,
            item.sample.join(" | ")
        ));
    }
    lines.push(format!("- 短句对白块：`{}`", d.short_quote_runs.len()));
    for item in d.short_quote_runs.iter().take(5) {
        lines.push(format!(
            "- 段落 `{}-{}` 平均句长=`{}`: {}",
            item.start_paragraph,
            item.end_paragraph,
            py_float_str(item.avg_len),
            item.sample.join(" | ")
        ));
    }
    lines.push(format!("- 问答互顶块：`{}`", d.question_ping_pong.len()));
    for item in d.question_ping_pong.iter().take(5) {
        lines.push(format!(
            "- 段落 `{}-{}`: {}",
            item.start_paragraph,
            item.end_paragraph,
            item.sample.join(" | ")
        ));
    }
    lines.push(format!("- 白话乒乓块：`{}`", d.quote_ping_pong.len()));
    for item in d.quote_ping_pong.iter().take(5) {
        lines.push(format!(
            "- 段落 `{}-{}` 平均句长=`{}`: {}",
            item.start_paragraph,
            item.end_paragraph,
            py_float_str(item.avg_len),
            item.sample.join(" | ")
        ));
    }
    lines.push(format!("- 对白转轴缺口：`{}`", d.dialogue_axis_gaps.len()));
    for item in d.dialogue_axis_gaps.iter().take(5) {
        lines.push(format!(
            "- `S{}-{}` `L{}-{}` score=`{}`：{}",
            item.start_index,
            item.end_index,
            item.start_line,
            item.end_line,
            item.score,
            markdown_table_cell(&item.reasons.join("、"))
        ));
        lines.push(format!("  建议：{}", markdown_table_cell(&item.suggestion)));
        let sample = item
            .sample
            .iter()
            .take(4)
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ");
        lines.push(format!("  样例：{}", markdown_table_cell(&sample)));
    }
    lines.push(format!(
        "- A/B 乒乓：`{}`",
        d.alternating_speaker_runs.len()
    ));
    for item in d.alternating_speaker_runs.iter().take(5) {
        lines.push(format!("- 段落 `{}`: `{}`", item.paragraph, item.pattern));
    }
    lines.push(format!(
        "- 对话段占比：`{}`",
        py_float_str(d.quote_paragraph_ratio)
    ));
    lines.push(format!("- 最长连续对白块：`{}`", d.dense_quote_run_max));
    lines.push(format!("- 超长对白块数：`{}`", d.dense_quote_run_count));
    lines.push(String::new());

    // ## 活跃标点（取前 10）
    lines.push("## 活跃标点".into());
    if a.dominant_punctuation.is_empty() {
        lines.push("- 无".into());
    } else {
        for item in a.dominant_punctuation.iter().take(10) {
            lines.push(format!(
                "- `{}` x{} per_10k=`{}`",
                item.mark,
                item.count,
                py_float_str(item.per_10k)
            ));
        }
    }
    lines.push(String::new());

    // ## 形容词 / 动词压力（只展示 total > 0 的行）
    lines.push("## 形容词 / 动词压力".into());
    let active: Vec<&ModifierPressure> = a
        .modifier_pressure
        .iter()
        .filter(|item| item.total > 0)
        .collect();
    if active.is_empty() {
        lines.push("- 无".into());
    } else {
        for item in active {
            lines.push(format!(
                "- `{}` `{}` total=`{}` dense_sentences=`{}`",
                if item.warn { "WARN" } else { "OK" },
                item.label,
                item.total,
                item.dense_sentences
            ));
        }
    }
    lines.push(String::new());

    // ## 章末检查
    lines.push("## 章末检查".into());
    lines.push(format!(
        "- 状态：`{}`",
        if a.ending.warn { "WARN" } else { "OK" }
    ));
    if !a.ending.image_terms.is_empty() {
        lines.push(format!("- 章末意象词：`{ending_image_md}`"));
    }
    if !a.ending.flow_terms.is_empty() {
        lines.push(format!("- 章末流程词：`{ending_flow_md}`"));
    }
    let tail = prefix_chars(&a.ending.tail_excerpt, 100);
    lines.push(format!(
        "- 章末摘录：{}",
        if tail.is_empty() {
            "无"
        } else {
            tail.as_str()
        }
    ));
    lines.push(String::new());

    // ## 模版候选（取前 20）
    lines.push("## 模版候选".into());
    if a.template_candidates.is_empty() {
        lines.push("- 无".into());
    } else {
        for item in a.template_candidates.iter().take(20) {
            let mut line = format!(
                "- `{}` `{}` x{}：{}",
                item.candidate_type, item.name, item.count, item.note
            );
            if !item.sample.is_empty() {
                line.push_str(&format!("；样例：{}", item.sample));
            }
            lines.push(line);
        }
    }
    lines.push(String::new());

    lines.join("\n")
}

/// `run` 的 CLI 参数（对齐 Python `main` 的 argparse 项）。
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// 位置参数：草稿文件或目录。
    pub positional: Vec<PathBuf>,
    /// `-i/--input`：输入文件或目录（可重复）。
    pub inputs: Vec<PathBuf>,
    /// 每条规则最多记录的样本行数（Python 默认 3）。
    pub sample_limit: usize,
    /// `--fail-on-warn`：有警告时退出码 1。
    pub fail_on_warn: bool,
    /// `--format`：报告格式（json/text/markdown 均对齐 Python）。
    pub format: ReportFormat,
    /// `-o/--output`：输出文件。
    pub output: Option<PathBuf>,
    /// `--learn-from`：语料路径（缺省时自动定位同小说语料）。
    pub learn_from: Option<Vec<PathBuf>>,
    /// `--no-corpus-learning`：禁用语料学习。
    pub no_corpus_learning: bool,
}

/// 对齐 Python `main`：多输入收集、语料学习开关、报告写出与退出码。
pub fn run(opts: &RunOptions) -> Result<i32> {
    let raw_inputs = resolve_inputs(&opts.positional, &opts.inputs)?;
    let files = iter_target_files(&raw_inputs);
    if files.is_empty() {
        eprintln!("No target files found.");
        return Ok(2);
    }
    let rules = crate::config::load_rules(&crate::config::default_rules_path())?;
    let ctx = DraftContext::new(rules)?;
    let template_bank = build_template_bank(ctx.draft_rules());
    let corpus_profile: Option<CorpusProfile> = if opts.no_corpus_learning {
        None
    } else {
        let corpus_paths = match opts.learn_from.as_ref() {
            Some(paths) => paths.clone(),
            None => ctx.corpus_paths_for_targets(&files),
        };
        build_corpus_profile(&ctx, &corpus_paths)?
    };
    let term_bank = ctx.draft_rules().tracked_terms.clone();
    let mut any_warn = false;
    let mut reports: Vec<Analysis> = Vec::new();
    for path in &files {
        let analysis = analyze_path(
            &ctx,
            path,
            &template_bank,
            &term_bank,
            corpus_profile.as_ref(),
            opts.sample_limit,
        )?;
        any_warn |= analysis.warned;
        reports.push(analysis);
    }
    if opts.format == ReportFormat::Json {
        let json = serde_json::to_string_pretty(&reports)?;
        match &opts.output {
            Some(path) => write_json_line(path, &json)?,
            None => println!("{json}"),
        }
    } else {
        let suffix = if opts.format == ReportFormat::Markdown {
            ".md"
        } else {
            ".txt"
        };
        match &opts.output {
            None => {
                for report in &reports {
                    println!("{}", render_report(report, opts.format, opts.sample_limit));
                    println!();
                }
            }
            Some(out_path) => {
                if reports.len() == 1 {
                    let rendered = render_report(&reports[0], opts.format, opts.sample_limit);
                    write_text(out_path, &format!("{rendered}\n"))?;
                } else {
                    fs::create_dir_all(out_path)
                        .with_context(|| format!("无法创建输出目录 {}", out_path.display()))?;
                    for report in &reports {
                        let stem = path_stem(&report.source);
                        let file = out_path.join(format!("{stem}{suffix}"));
                        let rendered = render_report(report, opts.format, opts.sample_limit);
                        write_text(&file, &format!("{rendered}\n"))?;
                    }
                }
            }
        }
    }
    if opts.fail_on_warn && any_warn {
        return Ok(1);
    }
    Ok(0)
}
