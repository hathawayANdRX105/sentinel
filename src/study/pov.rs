//! `study.pov`：中文视角切分候选检测。
//!
//! 行为对齐 Python `src/study/pov.py`（143 行，**py 侧导入损坏**：
//! `ANALYSIS_SCHEMA_VERSION` 在 `audit/draft.py` 中不存在，py 必崩；
//! 本模块按 py 代码意图 + Rust 实际行为移植）：
//!
//! - 段落切分复用 [`crate::text::TextSplitter::split_paragraph_infos`]；
//! - `schema_version` 取 **1**（py 常量虚构，无 py 真值可对齐）；
//! - 密度字段（pronoun / personal_name / dialogue_attribution）恒为 0
//!   （py 侧 ponytail 注释承认 audit 管道未填 pronouns / personal_names）；
//! - 候选分组：相邻段密度不变量变化才切组，尾组始终附加；
//! - `study` 词集缺省时全空（[`crate::config::StudyConfig`] 缺省 `None`）。
//!
//! 输出 JSON 键序对齐 py `build_pov_candidates` 返回 dict。

use anyhow::{Context, Result};
use serde::Serialize;

use crate::config::StudyConfig;
use crate::text::TextSplitter;

/// POV analysis 的 `schema_version` 值。
///
/// Python `pov.py` 引用 `ANALYSIS_SCHEMA_VERSION`（`audit.draft` 常量），
/// 该常量在 py 主仓 `src/audit/draft.py` 中**不存在**（grep 零命中），
/// py 侧 ImportError 必崩。Rust 取意图值 `1`。
pub const SCHEMA_VERSION: i64 = 1;

/// 单个 POV 候选段（对齐 py `_dominant_signal_change` 返回 dict 的键序）。
#[derive(Debug, Clone, Serialize)]
struct PoVObservation {
    candidate: &'static str,
    paragraph_range: String,
    evidence_line_numbers: Vec<u32>,
    pronoun_density: (i64, f64),
    personal_name_density: (i64, f64),
    dialogue_attribution: (i64, f64),
}

/// POV analysis 的 `chapter_summary` 节。
#[derive(Debug, Clone, Serialize)]
struct PovSummary {
    total_paragraphs: u32,
    candidates: Vec<PoVObservation>,
}

/// POV analysis 顶层结构（键序对齐 py `build_pov_candidates` dict）。
#[derive(Debug, Clone, Serialize)]
pub struct PovAnalysis {
    schema_version: i64,
    chapter_summary: PovSummary,
    heuristic: bool,
    confidence: &'static str,
    note: &'static str,
}

/// 对给定文本计算 POV 切分候选（对齐 py `build_pov_candidates`）。
///
/// `splitter` 须由 [`crate::text::TextSplitter::new`] 创建，
/// 使用 `draft.markdown_noise_line.pattern` 与 py 侧一致。
pub fn build_pov_candidates(text: &str, splitter: &TextSplitter) -> PovAnalysis {
    let raw_infos = splitter.split_paragraph_infos(text);
    let total_paragraphs = raw_infos.len() as u32;

    // py ponytail：pronouns / personal_names 永远为空（audit 管道未填），
    // dialogue_attribution 恒为 0 → 密度三字段均为 py 元组 `(count, density)`
    // 的形状 `(0, 0.0)`（count int、density float，JSON 渲染 `[0, 0.0]`）。

    // 候选分组逻辑（对齐 py `_dominant_signal_change`）：
    // 由于所有密度恒为 0，`prev` 始终为 `(0.0, 0.0, 0.0)`，
    // 循环中从不触发切组 → 只有尾组（当有段落时）。
    let mut candidates: Vec<PoVObservation> = Vec::new();

    if total_paragraphs > 0 {
        let evidence: Vec<u32> = raw_infos.iter().map(|pi| pi.line_start as u32).collect();
        candidates.push(PoVObservation {
            candidate: "POV shift candidate",
            paragraph_range: format!("0-{}", total_paragraphs),
            evidence_line_numbers: evidence,
            pronoun_density: (0, 0.0),
            personal_name_density: (0, 0.0),
            dialogue_attribution: (0, 0.0),
        });
    }

    PovAnalysis {
        schema_version: SCHEMA_VERSION,
        chapter_summary: PovSummary {
            total_paragraphs,
            candidates,
        },
        heuristic: true,
        confidence: "low",
        note: "Output requires human confirmation and is not a conclusion.",
    }
}

/// 运行 `study-pov` 子命令，返回格式化 JSON（对齐 py `json.dumps(ensure_ascii=False, indent=2)`）。
pub fn run_pov(
    chapter_path: &std::path::Path,
    config: &crate::config::ReviewRules,
) -> Result<String> {
    let text = std::fs::read_to_string(chapter_path)
        .with_context(|| format!("读取章节文件失败: {}", chapter_path.display()))?;
    let splitter = TextSplitter::new(&config.draft.markdown_noise_line.pattern)?;
    let _study: &StudyConfig = config.study.as_ref().unwrap_or(&StudyConfig::default());
    let analysis = build_pov_candidates(&text, &splitter);
    Ok(serde_json::to_string_pretty(&analysis)?)
}
