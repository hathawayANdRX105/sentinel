//! 规则配置加载。
//!
//! 把 `configs/rules/review.yaml` 解析为强类型结构（`load` / `mapping_at` /
//! `list_at`）：缺失关键节或 YAML 非法时返回错误。
//! 未知字段被 serde 忽略（宽容解析）。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// 默认规则文件路径：`<repo root>/configs/rules/review.yaml`。
pub fn default_rules_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("configs/rules/review.yaml")
}

/// 读取并解析规则文件。
pub fn load_rules(path: &Path) -> Result<ReviewRules> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("读取规则文件失败: {}", path.display()))?;
    let rules: ReviewRules = serde_yaml::from_str(&content)
        .with_context(|| format!("解析规则 YAML 失败: {}", path.display()))?;
    Ok(rules)
}

/// 规则根结构，对应 review.yaml 顶层 `draft` / `plan`。
#[derive(Debug, Clone, Deserialize)]
pub struct ReviewRules {
    pub draft: DraftConfig,
    pub plan: PlanConfig,
    #[serde(default)]
    pub study: Option<StudyConfig>,
}

/// 草稿规则，对应 `draft` 节。
#[derive(Debug, Clone, Deserialize)]
pub struct DraftConfig {
    pub regex_rules: RegexRules,
    pub template_rules: Vec<TemplateRule>,
    pub inactive_template_candidates: Vec<TemplateRule>,
    pub tracked_terms: Vec<TrackedTerm>,
    pub thresholds: DraftThresholds,
    pub learned_term_window: LearnedTermWindow,
    pub markdown_noise_line: MarkdownNoiseLine,
    pub connective_sentence_patterns: Vec<ConnectivePattern>,
    pub speaker: SpeakerConfig,
    pub lexicon: Lexicon,
    pub ending_labels: EndingLabels,
}

/// 正则规则分组，对应 `draft.regex_rules`。
#[derive(Debug, Clone, Deserialize)]
pub struct RegexRules {
    pub tokens: Vec<RegexRule>,
    pub patterns: Vec<RegexRule>,
    pub phrases: Vec<RegexRule>,
    pub modifiers: Vec<RegexRule>,
    pub punctuation: Vec<RegexRule>,
    pub punctuation_combos: Vec<RegexRule>,
}

/// 单条正则规则（高频词/句式/标点等）。
#[derive(Debug, Clone, Deserialize)]
pub struct RegexRule {
    pub name: String,
    pub pattern: String,
    /// 每万字上限。
    pub max_per_10k: f64,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    /// 语域标签（可逗号组合多个）：`colloquial`（口语）/ `literary`（文学、
    /// 含散文）/ `classical`（文白古典）/ `lightnovel`（日轻翻译腔）/
    /// `webnovel`（网文）/ `common`（各人类语域通用）/ `neutral`（无语域倾向）。
    /// 来自真语料矩阵校验：这些规则的命中在对应语域是人类正常用法，
    /// 而不是 AI 腔证据（如标点「：」「！」在网文/古典/文学里都高频，
    /// 而 42 章 AI 草稿零命中）。报告携带此标签，让评审按作品实际语域
    /// 判读，而不是一律当 AI 味。
    #[serde(default)]
    pub register: Option<String>,
}

/// 模板规则，同时用于 `template_rules` 与 `inactive_template_candidates`。
#[derive(Debug, Clone, Deserialize)]
pub struct TemplateRule {
    pub name: String,
    pub pattern: String,
    pub note: String,
    pub max_per_10k: f64,
    pub category: String,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// 跟踪词（人名/地名/动作等）。
#[derive(Debug, Clone, Deserialize)]
pub struct TrackedTerm {
    pub category: String,
    pub term: String,
    pub max_per_10k: f64,
    #[serde(default)]
    pub note: Option<String>,
}

/// 草稿阈值，对应 `draft.thresholds`。
#[derive(Debug, Clone, Deserialize)]
pub struct DraftThresholds {
    pub default_corpus_parts: Vec<Vec<String>>,
    pub short_sentence_max_chars: u32,
    pub very_short_sentence_max_chars: u32,
    pub short_sentence_run_max_chars: u32,
    pub short_sentence_run_min: u32,
    pub tracked_term_window_size: u32,
    pub tracked_term_window_min_top: u32,
    pub tracked_term_window_min_total: u32,
    pub tracked_term_window_min_category: u32,
    pub learned_filter_limit: u32,
}

/// 语料学习窗口配置，对应 `draft.learned_term_window`。
#[derive(Debug, Clone, Deserialize)]
pub struct LearnedTermWindow {
    pub categories: Vec<String>,
    pub noise_suffixes: Vec<String>,
    pub noise_prefixes: Vec<String>,
    pub noise_chars: Vec<String>,
}

/// Markdown 噪音行正则，对应 `draft.markdown_noise_line`。
#[derive(Debug, Clone, Deserialize)]
pub struct MarkdownNoiseLine {
    pub pattern: String,
}

/// 连接词句首模式，对应 `draft.connective_sentence_patterns`。
#[derive(Debug, Clone, Deserialize)]
pub struct ConnectivePattern {
    pub label: String,
    pub pattern: String,
}

/// 说话人识别配置，对应 `draft.speaker`。
#[derive(Debug, Clone, Deserialize)]
pub struct SpeakerConfig {
    pub patterns: Vec<String>,
    pub line_patterns: Vec<String>,
    pub suffixes: Vec<String>,
}

/// 词库，对应 `draft.lexicon`。绝大多数是词表，个别是字符串或标签→词表映射。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Lexicon {
    #[serde(default)]
    pub word_stoplist: Vec<String>,
    #[serde(default)]
    pub corpus_stop_terms: Vec<String>,
    #[serde(default)]
    pub structure_chars: String,
    #[serde(default)]
    pub subject_leads: Vec<String>,
    #[serde(default)]
    pub paragraph_leads: Vec<String>,
    #[serde(default)]
    pub allowed_short_corpus_terms: Vec<String>,
    #[serde(default)]
    pub ending_image_terms: Vec<String>,
    #[serde(default)]
    pub ending_flow_terms: Vec<String>,
    #[serde(default)]
    pub judgement_endings: Vec<String>,
    #[serde(default)]
    pub fatigue_window_judgement_terms: Vec<String>,
    #[serde(default)]
    pub fatigue_window_sticky_terms: Vec<String>,
    #[serde(default)]
    pub judgement_context_terms: Vec<String>,
    #[serde(default)]
    pub short_role_info_terms: Vec<String>,
    #[serde(default)]
    pub short_role_emotion_terms: Vec<String>,
    #[serde(default)]
    pub ba_clue_terms: Vec<String>,
    #[serde(default)]
    pub ba_emotion_terms: Vec<String>,
    #[serde(default)]
    pub ba_scene_terms: Vec<String>,
    #[serde(default)]
    pub ba_scene_verbs: Vec<String>,
    #[serde(default)]
    pub ba_tool_terms: Vec<String>,
    #[serde(default)]
    pub dialogue_axis_action_terms: Vec<String>,
    #[serde(default)]
    pub dialogue_axis_env_terms: Vec<String>,
    #[serde(default)]
    pub dialogue_axis_device_terms: Vec<String>,
    #[serde(default)]
    pub dialogue_axis_third_party_terms: Vec<String>,
    #[serde(default)]
    pub adjective_hints: Vec<String>,
    #[serde(default)]
    pub verb_hints: Vec<String>,
    #[serde(default)]
    pub scene_break_leads: Vec<String>,
    #[serde(default)]
    pub paragraph_info_terms: Vec<String>,
    #[serde(default)]
    pub mental_state_terms: Vec<String>,
    #[serde(default, deserialize_with = "ordered_term_rules_deserialize")]
    pub dialogue_emotion_rules: OrderedTermRules,
    #[serde(default)]
    pub character_name_stoplist: Vec<String>,
    #[serde(default, deserialize_with = "ordered_term_rules_deserialize")]
    pub tone_rules: OrderedTermRules,
    #[serde(default)]
    pub battle_action_terms: Vec<String>,
    #[serde(default)]
    pub battle_result_terms: Vec<String>,
    #[serde(default)]
    pub battle_damage_terms: Vec<String>,
    #[serde(default)]
    pub battle_movement_terms: Vec<String>,
}

/// 保留 YAML 插入序的「标签 → 词表」映射。
///
/// `dialogue_emotion_rules` / `tone_rules` 的首匹配语义依赖 YAML 里的书写顺序
/// （规则顺序 = YAML 插入序）。`serde_yaml::Mapping`
/// 保留插入序，据此直接转有序 `Vec` 对；不能用 `HashMap`（丢序），也不能
/// 直接声明 `Vec<(K, V)>`（YAML 值是 mapping，不是 sequence，反序列化失败）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderedTermRules {
    pub rules: Vec<(String, Vec<String>)>,
}

impl<'de> serde::Deserialize<'de> for OrderedTermRules {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_yaml::Value::deserialize(deserializer)?;
        match value {
            serde_yaml::Value::Mapping(map) => {
                let mut rules = Vec::with_capacity(map.len());
                for (key, val) in map.iter() {
                    let label = key
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| key.as_u64().map(|n| n.to_string()))
                        .or_else(|| key.as_bool().map(|b| b.to_string()))
                        .unwrap_or_default();
                    let terms = match val {
                        serde_yaml::Value::Sequence(items) => items
                            .iter()
                            .filter_map(|item| item.as_str().map(str::to_string))
                            .collect(),
                        _ => Vec::new(),
                    };
                    rules.push((label, terms));
                }
                Ok(Self { rules })
            }
            serde_yaml::Value::Null => Ok(Self { rules: Vec::new() }),
            other => Err(serde::de::Error::custom(format!(
                "期望 mapping 或 null，实际为: {other:?}"
            ))),
        }
    }
}

/// `OrderedTermRules` 的 `serde(deserialize_with)` 入口。
fn ordered_term_rules_deserialize<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<OrderedTermRules, D::Error> {
    OrderedTermRules::deserialize(deserializer)
}

/// 章末收束标签，对应 `draft.ending_labels`。
#[derive(Debug, Clone, Deserialize)]
pub struct EndingLabels {
    pub rules: HashMap<String, Vec<String>>,
    pub display: HashMap<String, String>,
}

/// 大纲规则，对应 `plan` 节。
#[derive(Debug, Clone, Deserialize)]
pub struct PlanConfig {
    pub required_headings: HashMap<String, Vec<Vec<String>>>,
    pub scene_field_groups: Vec<Vec<String>>,
    pub chapter_ending_group: Vec<String>,
    pub sections: PlanSections,
    pub thresholds: PlanThresholds,
    pub regex: HashMap<String, PlanRegexRule>,
    pub lookpoint_weak_terms: Vec<String>,
    pub lookpoint_strong_terms: Vec<String>,
    pub ending_weak_terms: Vec<String>,
    pub environment_pressure_terms: Vec<String>,
    pub environment_witness_terms: Vec<String>,
    pub generic_progress_terms: Vec<String>,
    pub role_function_terms: Vec<String>,
    pub function_rules: FunctionRules,
}

/// 大纲节名配置，对应 `plan.sections`。
#[derive(Debug, Clone, Deserialize)]
pub struct PlanSections {
    pub story_layout: String,
    pub foreshadow_table: String,
    pub story_events: String,
    pub story_loads: String,
    pub story_roles: String,
    pub story_environment: Vec<String>,
    pub chapter_function: String,
    pub lookpoint: String,
    pub rhythm: String,
    pub state_change: String,
    pub scene_function_fields: Vec<String>,
}

/// 大纲阈值，对应 `plan.thresholds`。
#[derive(Debug, Clone, Deserialize)]
pub struct PlanThresholds {
    pub story_layout_min_items: u32,
    pub foreshadow_min_ids: u32,
    pub story_events_min_items: u32,
    pub lookpoint_short_max_chars: u32,
    pub scene_body_max_chars: u32,
    pub scene_prose_mark_min: u32,
    pub scene_dialogue_prose_mark_min: u32,
    pub thin_change_max_chars: u32,
    pub scene_monotony_min_scenes: u32,
    pub scene_monotony_max_missing: u32,
}

/// 单条大纲正则，对应 `plan.regex.<name>`。
#[derive(Debug, Clone, Deserialize)]
pub struct PlanRegexRule {
    pub pattern: String,
    #[serde(default)]
    pub ignore_case: bool,
}

/// 功能规则，对应 `plan.function_rules`。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct FunctionRules {
    #[serde(default)]
    pub chapter: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub ending: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub scene: HashMap<String, Vec<String>>,
}

/// 研究工具词集配置，对应 `study` 节（可选；缺省时各词集为空）。
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct StudyConfig {
    /// 代名词表（pronouns）。
    pub pronouns: Vec<String>,
    /// 人名表（personal_names）。
    pub personal_names: Vec<String>,
    /// 对白归属词表（dialogue_attribution）。
    pub dialogue_attribution: Vec<String>,
}
