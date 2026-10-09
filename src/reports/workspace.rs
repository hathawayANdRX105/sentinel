//! `reports.workspace`：concept / plan / draft / consistency 四节 → 单份
//! AUDIT.md 工作区看板。
//!
//! - 各节复用既有模块（stats、audit、reports、consistency）的既有逻辑；
//!   本文件只做编排与看板渲染，渲染输出逐字节稳定。
//! - draft 节的浮点均值走 `rules::round2`（banker's）+ `audit::draft::float_repr`
//!   （浮点展示语义），与 scorecard 的 avg 渲染一致。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result};
use fancy_regex::Regex;
use serde_json::Value;

use crate::audit::draft::{float_repr, Analysis, DraftContext};
use crate::audit::plan as plan_audit;
use crate::audit::plan::PlanEngine;
use crate::config::{self, EndingLabels};
use crate::consistency::{
    self, AlignmentRow, ConflictRow, GoalTensionRow, PairRow, RelationshipTensionRow, TensionRow,
};
use crate::input;
use crate::reports::alignment;
use crate::reports::backlog;
use crate::reports::catalog;
use crate::reports::kit;
use crate::reports::scorecard;
use crate::rules::{build_template_bank, round2};
use crate::stats::draft::{
    collect_chapter_files, ending_display, ending_flow_text, infer_ending_label, summarize_runs,
    AnalysisEnv,
};
use crate::stats::{concept as concept_stats, plan as plan_stats, Ctr};
use rusqlite::Connection;

/// 计划目录名。
const PLAN_DIR_NAMES: &[&str] = &["arc-plan", "story-plan", "chapter-plan"];

/// 漂移样本上限。
const DRIFT_SAMPLE_LIMIT: usize = 6;

/// 章节 ID 正则（忽略大小写的 `ch<数字>`）。
static CHAPTER_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("(?i)ch(\\d+)").expect("CHAPTER_ID_RE 应可编译"));

/// `reports-workspace` 子命令参数。
#[derive(Debug, Clone)]
pub struct WorkspaceOptions {
    /// novel 目录（例如 novel1）。
    pub novel_dir: PathBuf,
    /// 每条规则最多记录的样本行数（缺省 3）。
    pub sample_limit: usize,
    /// 滚动章节窗口大小（缺省 [2, 3]；显式 `--window-sizes` 不带值时为空列表）。
    pub window_sizes: Vec<usize>,
}

/// 四节收集 + 看板渲染 + 写 AUDIT.md，stdout 仅打印 AUDIT.md 路径。
pub fn run(opts: &WorkspaceOptions) -> Result<(i32, Vec<PathBuf>)> {
    let novel_dir = &opts.novel_dir;
    if !novel_dir.exists() {
        eprintln!("Novel directory not found: {}", novel_dir.display());
        return Ok((1, Vec::new()));
    }
    let rules = config::load_rules(&config::default_rules_path())?;
    let engine = PlanEngine::new(&rules.plan)?;
    let ctx = DraftContext::new(rules)?;
    let concept = collect_concept_section(novel_dir)?;
    let plans = collect_plan_section(novel_dir, &engine)?;
    let drafts = collect_draft_section(
        novel_dir,
        opts.sample_limit,
        &opts.window_sizes,
        &ctx,
        &engine,
    )?;
    let consistency_section = collect_consistency_section(novel_dir)?;
    let dashboard = build_dashboard(novel_dir, &concept, &plans, &drafts, &consistency_section);
    let out_path = novel_dir.join("AUDIT.md");
    input::write_text(&out_path, &dashboard)?;
    Ok((0, vec![out_path]))
}

// ---------------------------------------------------------------------------
// 小型共享工具（模块级）
// ---------------------------------------------------------------------------

/// `name xN` 空格连排摘要，空为「无」。
fn summarize_counter(counter: &Ctr, limit: usize) -> String {
    let joined = counter
        .most_common(limit)
        .iter()
        .map(|(name, count)| format!("{name} x{count}"))
        .collect::<Vec<_>>()
        .join(" ");
    if joined.is_empty() {
        "无".to_string()
    } else {
        joined
    }
}

/// 连续标签摘要（workspace 本地版，
/// 与 `stats.draft.summarize_runs` 不同：不带展示名映射、带 ch 区间、排除弱标签）。
fn summarize_runs_local(labels_in_order: &[String]) -> Vec<String> {
    if labels_in_order.is_empty() {
        return Vec::new();
    }
    let mut runs: Vec<String> = Vec::new();
    let mut current = &labels_in_order[0];
    let mut start = 1usize;
    let mut length = 1usize;
    for (offset, label) in labels_in_order[1..].iter().enumerate() {
        let index = offset + 2;
        if label == current {
            length += 1;
            continue;
        }
        if !matches!(current.as_str(), "unclear" | "missing" | "none" | "neutral") && length >= 3 {
            runs.push(format!(
                "{current} x{length} (ch{start:02}-ch{end:02})",
                end = index - 1
            ));
        }
        current = label;
        start = index;
        length = 1;
    }
    if !matches!(current.as_str(), "unclear" | "missing" | "none" | "neutral") && length >= 3 {
        runs.push(format!(
            "{current} x{length} (ch{start:02}-ch{end:02})",
            end = start + length - 1
        ));
    }
    runs.truncate(4);
    runs
}

/// 结局信号/基调/情绪三流汇聚点摘要行。
fn summarize_story_convergences(
    ending_signal_flow: &[String],
    tone_flow: &[String],
    emotion_flow: &[String],
    labels: &EndingLabels,
) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    for idx in 0..ending_signal_flow.len().saturating_sub(1) {
        if ending_signal_flow[idx] != ending_signal_flow[idx + 1] {
            continue;
        }
        if tone_flow[idx] == tone_flow[idx + 1] && tone_flow[idx] != "none" {
            items.push(format!(
                "{}+tone:{} x2",
                ending_display(labels, &ending_signal_flow[idx]),
                tone_flow[idx]
            ));
        }
        if emotion_flow[idx] == emotion_flow[idx + 1] && emotion_flow[idx] != "neutral" {
            items.push(format!(
                "{}+emotion:{} x2",
                ending_display(labels, &ending_signal_flow[idx]),
                emotion_flow[idx]
            ));
        }
    }
    let mut seen: Vec<String> = Vec::new();
    for item in items {
        if !seen.contains(&item) {
            seen.push(item);
        }
    }
    seen.truncate(4);
    seen
}

/// `story_dir.relative_to(drafts_dir).as_posix()`：自身相对为 `.`；越界时退回原样。
fn rel_posix(path: &Path, base: &Path) -> String {
    match path.strip_prefix(base) {
        Ok(p) if p.as_os_str().is_empty() => ".".to_string(),
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

/// 由路径解析章节序与章名：`(order, chapter_name)`。
fn chapter_order_from_path(path_text: &str, novel_dir: &Path) -> (i64, String) {
    let path = Path::new(path_text);
    let (_doc_type, _arc, _story, chapter) = consistency::classify_document(path, novel_dir);
    let chapter_name = chapter.unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    if let Ok(Some(caps)) = CHAPTER_ID_RE.captures(&chapter_name) {
        let n = caps
            .get(1)
            .and_then(|m| m.as_str().parse::<i64>().ok())
            .unwrap_or(9999);
        return (n, chapter_name);
    }
    (9999, chapter_name)
}

// ---------------------------------------------------------------------------
// consistency 节派生行（模块级 builder）
// ---------------------------------------------------------------------------

/// story 轨迹摘要行（`build_story_trajectory_summary` 输出）。
#[derive(Debug, Clone)]
pub struct TrajectorySummaryRow {
    pub story: String,
    pub state_summary: String,
    pub goal_summary: String,
    pub relationship_summary: String,
    pub samples: Vec<String>,
}

/// story 轨迹明细条目（`build_story_trajectory_details` 输出）。
#[derive(Debug, Clone)]
pub struct TrajectoryDetailItem {
    pub title: String,
    pub kind: &'static str,
    pub summary: String,
    pub timeline: Vec<String>,
}

/// story 轨迹明细行（`build_story_trajectory_details` 输出）。
#[derive(Debug, Clone)]
pub struct TrajectoryDetailRow {
    pub story: String,
    pub items: Vec<TrajectoryDetailItem>,
}

/// 关系对轨迹条目 / 行。
#[derive(Debug, Clone)]
pub struct PairTrajectoryItem {
    pub pair: String,
    pub summary: String,
    pub timeline: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PairTrajectoryRow {
    pub story: String,
    pub items: Vec<PairTrajectoryItem>,
}

/// 叙事轨迹行（drafts × consistency 合流）。
#[derive(Debug, Clone)]
pub struct NarrativeRow {
    pub story: String,
    pub summary: String,
    pub details: Vec<String>,
}

/// fact 证据按章聚合的 timeline 行。
fn build_fact_timeline(
    conn: &Connection,
    novel_dir: &Path,
    story: &str,
    title: &str,
    fact_types: &[&str],
) -> Result<Vec<String>> {
    let types: Vec<String> = fact_types.iter().map(|s| s.to_string()).collect();
    let rows = consistency::query_story_tension_evidence(conn, story, title, 8 * 4, Some(&types))?;
    let mut grouped: BTreeMap<(i64, String), Vec<String>> = BTreeMap::new();
    for row in rows {
        let order = chapter_order_from_path(&row.path, novel_dir);
        grouped
            .entry(order)
            .or_default()
            .push(format!("{}:{}", row.fact_type, row.cue));
    }
    let mut timeline: Vec<String> = Vec::new();
    for ((_order, chapter_name), events) in &grouped {
        let mut deduped: Vec<&String> = Vec::new();
        for event in events {
            if !deduped.contains(&event) {
                deduped.push(event);
            }
        }
        timeline.push(format!(
            "{chapter_name} {}",
            deduped
                .into_iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join("/")
        ));
        if timeline.len() >= 8 {
            break;
        }
    }
    Ok(timeline)
}

/// story 轨迹摘要（直接吃 typed 行）。
fn build_story_trajectory_summary(
    tension: &[TensionRow],
    goal: &[GoalTensionRow],
    relationship: &[RelationshipTensionRow],
) -> Vec<TrajectorySummaryRow> {
    #[derive(Default)]
    struct Bucket {
        state: Ctr,
        goal: Ctr,
        relationship: Ctr,
        samples: Vec<String>,
    }
    let mut by_story: BTreeMap<String, Bucket> = BTreeMap::new();
    for item in tension {
        let entry = by_story.entry(item.story.clone()).or_default();
        entry.state.add(&item.title, 1);
        if entry.samples.len() < 6 {
            let mut parts: Vec<String> = Vec::new();
            let inj_n = item.injury_negative.as_deref().unwrap_or("");
            let inj_s = item.injury_stable.as_deref().unwrap_or("");
            let eq_d = item.equipment_damaged.as_deref().unwrap_or("");
            let eq_a = item.equipment_active.as_deref().unwrap_or("");
            if !inj_n.is_empty() && !inj_s.is_empty() {
                parts.push(format!("injury={inj_n}->{inj_s}"));
            }
            if !eq_d.is_empty() && !eq_a.is_empty() {
                parts.push(format!("equipment={eq_d}->{eq_a}"));
            }
            entry
                .samples
                .push(format!("state `{}` {}", item.title, parts.join(" ; ")));
        }
    }
    for item in goal {
        let entry = by_story.entry(item.story.clone()).or_default();
        entry.goal.add(&item.title, 1);
        if entry.samples.len() < 6 {
            let mut parts: Vec<String> = Vec::new();
            let g = item.goal_assigned.as_deref().unwrap_or("");
            let c = item.goal_changed.as_deref().unwrap_or("");
            let done = item.goal_completed.as_deref().unwrap_or("");
            if !g.is_empty() {
                parts.push(format!("assigned={g}"));
            }
            if !c.is_empty() {
                parts.push(format!("changed={c}"));
            }
            if !done.is_empty() {
                parts.push(format!("completed={done}"));
            }
            entry
                .samples
                .push(format!("goal `{}` {}", item.title, parts.join(" ; ")));
        }
    }
    for item in relationship {
        let entry = by_story.entry(item.story.clone()).or_default();
        entry.relationship.add(&item.title, 1);
        if entry.samples.len() < 6 {
            entry.samples.push(format!(
                "relationship `{}` close={} ; distant={}",
                item.title,
                item.relationship_close.as_deref().unwrap_or(""),
                item.relationship_distant.as_deref().unwrap_or("")
            ));
        }
    }
    by_story
        .into_iter()
        .map(|(story, entry)| TrajectorySummaryRow {
            state_summary: summarize_counter(&entry.state, 3),
            goal_summary: summarize_counter(&entry.goal, 3),
            relationship_summary: summarize_counter(&entry.relationship, 3),
            story,
            samples: entry.samples.into_iter().take(4).collect(),
        })
        .collect()
}

/// story 轨迹明细。
fn build_story_trajectory_details(
    conn: &Connection,
    novel_dir: &Path,
    tension: &[TensionRow],
    goal: &[GoalTensionRow],
    relationship: &[RelationshipTensionRow],
) -> Result<Vec<TrajectoryDetailRow>> {
    let mut by_story: BTreeMap<String, Vec<TrajectoryDetailItem>> = BTreeMap::new();
    for item in tension {
        let inj_n = item.injury_negative.as_deref().unwrap_or("");
        let inj_s = item.injury_stable.as_deref().unwrap_or("");
        let eq_d = item.equipment_damaged.as_deref().unwrap_or("");
        let eq_a = item.equipment_active.as_deref().unwrap_or("");
        let fact_types: &[&str] = if !inj_n.is_empty() && !inj_s.is_empty() {
            &["injury_negative", "injury_stable"]
        } else if !eq_d.is_empty() && !eq_a.is_empty() {
            &["equipment_damaged", "equipment_active"]
        } else {
            continue;
        };
        let summary = if fact_types[0] == "injury_negative" {
            format!("injury={inj_n}->{inj_s}")
        } else {
            format!("equipment={eq_d}->{eq_a}")
        };
        let timeline = build_fact_timeline(conn, novel_dir, &item.story, &item.title, fact_types)?;
        by_story
            .entry(item.story.clone())
            .or_default()
            .push(TrajectoryDetailItem {
                title: item.title.clone(),
                kind: "state",
                summary,
                timeline,
            });
    }
    for item in goal {
        let g = item.goal_assigned.as_deref().unwrap_or("");
        let c = item.goal_changed.as_deref().unwrap_or("");
        let done = item.goal_completed.as_deref().unwrap_or("");
        let parts = [
            if g.is_empty() {
                None
            } else {
                Some(format!("assigned={g}"))
            },
            if c.is_empty() {
                None
            } else {
                Some(format!("changed={c}"))
            },
            if done.is_empty() {
                None
            } else {
                Some(format!("completed={done}"))
            },
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        let timeline = build_fact_timeline(
            conn,
            novel_dir,
            &item.story,
            &item.title,
            &["goal_assigned", "goal_changed", "goal_completed"],
        )?;
        by_story
            .entry(item.story.clone())
            .or_default()
            .push(TrajectoryDetailItem {
                title: item.title.clone(),
                kind: "goal",
                summary: parts.join(" ; "),
                timeline,
            });
    }
    for item in relationship {
        let summary = format!(
            "close={} ; distant={}",
            item.relationship_close.as_deref().unwrap_or(""),
            item.relationship_distant.as_deref().unwrap_or("")
        );
        let timeline = build_fact_timeline(
            conn,
            novel_dir,
            &item.story,
            &item.title,
            &["relationship_close", "relationship_distant"],
        )?;
        by_story
            .entry(item.story.clone())
            .or_default()
            .push(TrajectoryDetailItem {
                title: item.title.clone(),
                kind: "relationship",
                summary,
                timeline,
            });
    }
    Ok(by_story
        .into_iter()
        .map(|(story, items)| TrajectoryDetailRow {
            story,
            items: items.into_iter().take(8).collect(),
        })
        .collect())
}

/// 叙事轨迹行（drafts + consistency 派生）。
fn build_narrative_trajectory_rows(
    drafts: &DraftSection,
    consistency: &ConsistencySection,
) -> Vec<NarrativeRow> {
    let draft_rows: BTreeMap<String, &DraftStory> = drafts
        .stories
        .iter()
        .map(|s| {
            (
                s.story
                    .split_once('/')
                    .map(|(_, rest)| rest.to_string())
                    .unwrap_or_else(|| s.story.clone()),
                s,
            )
        })
        .collect();
    let consistency_rows: BTreeMap<String, &TrajectorySummaryRow> = consistency
        .story_trajectories
        .iter()
        .map(|r| (r.story.clone(), r))
        .collect();
    let detail_rows: BTreeMap<String, &TrajectoryDetailRow> = consistency
        .story_trajectory_details
        .iter()
        .map(|r| (r.story.clone(), r))
        .collect();

    let mut story_keys: BTreeSet<&String> = BTreeSet::new();
    story_keys.extend(draft_rows.keys());
    story_keys.extend(consistency_rows.keys());
    story_keys.extend(detail_rows.keys());

    story_keys
        .into_iter()
        .map(|story| {
            let draft = draft_rows.get(story).copied();
            let consistency_summary = consistency_rows.get(story).copied();
            let consistency_detail = detail_rows.get(story).copied();
            let mut trajectory_parts: Vec<String> = Vec::new();
            if let Some(d) = draft {
                if !d.speaker_summary.is_empty() && d.speaker_summary != "无" {
                    trajectory_parts.push(format!("speakers={}", d.speaker_summary));
                }
                if !d.tone_runs.is_empty() {
                    trajectory_parts.push(format!(
                        "tone_runs={}",
                        d.tone_runs
                            .iter()
                            .take(2)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" | ")
                    ));
                }
                if !d.emotion_runs.is_empty() {
                    trajectory_parts.push(format!(
                        "emotion_runs={}",
                        d.emotion_runs
                            .iter()
                            .take(2)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" | ")
                    ));
                }
                if !d.chapter_runs.is_empty() {
                    trajectory_parts.push(format!(
                        "chapter_runs={}",
                        d.chapter_runs
                            .iter()
                            .take(2)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" | ")
                    ));
                }
                if !d.voice_drifts.is_empty() {
                    trajectory_parts.push(format!(
                        "voice={}",
                        d.voice_drifts
                            .iter()
                            .take(2)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" | ")
                    ));
                }
            }
            if let Some(c) = consistency_summary {
                if !c.goal_summary.is_empty() && c.goal_summary != "无" {
                    trajectory_parts.push(format!("goal={}", c.goal_summary));
                }
                if !c.relationship_summary.is_empty() && c.relationship_summary != "无" {
                    trajectory_parts.push(format!("relationship={}", c.relationship_summary));
                }
                if !c.state_summary.is_empty() && c.state_summary != "无" {
                    trajectory_parts.push(format!("state={}", c.state_summary));
                }
            }
            let detail_samples: Vec<String> = consistency_detail
                .map(|d| {
                    d.items
                        .iter()
                        .take(3)
                        .map(|item| {
                            format!(
                                "{}:{} {}",
                                item.kind,
                                item.title,
                                item.timeline
                                    .iter()
                                    .take(2)
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join(" -> ")
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            NarrativeRow {
                story: story.to_string(),
                summary: {
                    let joined = trajectory_parts.join(" ; ");
                    if joined.is_empty() {
                        "无".to_string()
                    } else {
                        joined
                    }
                },
                details: detail_samples,
            }
        })
        .collect()
}

/// 关系对轨迹（每 story 上限 4 条）。
fn build_relationship_pair_trajectories(
    novel_dir: &Path,
    pair_rows: &[PairRow],
    limit_per_story: usize,
) -> Vec<PairTrajectoryRow> {
    let mut grouped: BTreeMap<String, BTreeMap<String, Vec<&PairRow>>> = BTreeMap::new();
    for row in pair_rows {
        let pair = format!("{}~{}", row.left_title, row.right_title);
        grouped
            .entry(row.story.clone())
            .or_default()
            .entry(pair)
            .or_default()
            .push(row);
    }

    let mut results: Vec<PairTrajectoryRow> = Vec::new();
    for (story, pairs) in &grouped {
        let mut items: Vec<PairTrajectoryItem> = Vec::new();
        for (pair, rows) in pairs {
            let mut ordered_rows: Vec<&PairRow> = rows.to_vec();
            ordered_rows.sort_by_key(|row| chapter_order_from_path(&row.path, novel_dir));
            let mut timeline: Vec<String> = Vec::new();
            let mut close_count = 0usize;
            let mut distant_count = 0usize;
            for row in &ordered_rows {
                let chapter_name = chapter_order_from_path(&row.path, novel_dir).1;
                let mut parts: Vec<String> = Vec::new();
                if !row.close_cues.is_empty() {
                    close_count += 1;
                    parts.push(format!("close={}", row.close_cues));
                }
                if !row.distant_cues.is_empty() {
                    distant_count += 1;
                    parts.push(format!("distant={}", row.distant_cues));
                }
                if !parts.is_empty() {
                    timeline.push(format!("{chapter_name} {}", parts.join(" ; ")));
                }
            }
            if timeline.is_empty() {
                continue;
            }
            items.push(PairTrajectoryItem {
                pair: pair.clone(),
                summary: format!("close_hits={close_count} distant_hits={distant_count}"),
                timeline: timeline.into_iter().take(4).collect(),
            });
        }
        if !items.is_empty() {
            results.push(PairTrajectoryRow {
                story: story.clone(),
                items: items.into_iter().take(limit_per_story).collect(),
            });
        }
    }
    results
}

/// 模板沉淀目标推断。
fn infer_template_deposition_target(candidate_type: &str, candidate_name: &str) -> &'static str {
    if matches!(
        candidate_type,
        "dialogue" | "dialogue_axis_gap" | "dialogue_emotion"
    ) {
        return "skills/review-guide.md";
    }
    if matches!(
        candidate_type,
        "tracked_term" | "tracked_term_window" | "learned_filter"
    ) {
        return "configs/rules/review.yaml#draft.tracked_terms";
    }
    if matches!(
        candidate_type,
        "scene_map" | "battle_profile" | "viewpoint_profile"
    ) {
        return "configs/rules/review.yaml#draft.template_rules";
    }
    if matches!(
        candidate_type,
        "sentence_pattern" | "short_phrase" | "aa_bb_pattern" | "custom_template"
    ) {
        return "configs/rules/review.yaml#draft.template_rules";
    }
    if candidate_type == "ending" {
        return "novel1/rules/draft.md";
    }
    if matches!(
        candidate_name,
        "场面功能失衡" | "动作链缺结果" | "视角锚点漂移"
    ) {
        return "configs/rules/review.yaml#draft.template_rules";
    }
    "configs/rules/review.yaml#draft.template_rules"
}

// ---------------------------------------------------------------------------
// 各节数据结构与收集（`collect_*_section`）
// ---------------------------------------------------------------------------

/// concept 节 `categories` 条目。
#[derive(Debug, Clone)]
pub struct ConceptCategory {
    pub category: String,
    pub cards: usize,
    pub warnings: usize,
    pub top_file: String,
    pub top_count: usize,
    pub top_kinds: String,
}

/// concept 节 dict。
#[derive(Debug, Clone)]
pub struct ConceptSection {
    pub exists: bool,
    pub files: usize,
    pub warnings: usize,
    pub summary_paths: Vec<PathBuf>,
    pub categories: Vec<ConceptCategory>,
}

/// plan 节 `plan_types` 条目。
#[derive(Debug, Clone)]
pub struct PlanTypeInfo {
    pub plan_type: String,
    pub files: usize,
    pub warnings: usize,
    pub top_file: String,
    pub top_count: usize,
    pub top_kinds: String,
}

/// plan 节 `story_trends` 条目。
#[derive(Debug, Clone)]
pub struct StoryTrend {
    pub story: String,
    pub chapter_flow: String,
    pub ending_flow: String,
    pub chapter_runs: Vec<String>,
    pub ending_runs: Vec<String>,
}

/// plan 节 dict。
#[derive(Debug, Clone)]
pub struct PlanSection {
    pub exists: bool,
    pub files: usize,
    pub warnings: usize,
    pub summary_paths: Vec<PathBuf>,
    pub plan_types: Vec<PlanTypeInfo>,
    pub chapter_function_distribution: String,
    pub ending_function_distribution: String,
    pub story_trends: Vec<StoryTrend>,
}

/// draft 节 `stories` 条目。
#[derive(Debug, Clone)]
pub struct DraftStory {
    pub story: String,
    pub chapters: usize,
    pub warnings: usize,
    pub top_file: String,
    pub top_count: usize,
    pub top_templates: String,
    pub top_fatigue: String,
    pub gate_summary: String,
    pub recommendation_summary: String,
    pub avg_axis: String,
    pub scene_summary: String,
    pub tone_summary: String,
    pub emotion_summary: String,
    pub speaker_summary: String,
    pub template_summary: String,
    pub alignment_summary: String,
    pub ending_signal_summary: String,
    pub alignment_mismatches: Vec<String>,
    pub voice_drifts: Vec<String>,
    pub chapter_runs: Vec<String>,
    pub ending_runs: Vec<String>,
    pub ending_signal_flow: String,
    pub ending_signal_runs: Vec<String>,
    pub trend_convergences: Vec<String>,
    pub tone_runs: Vec<String>,
    pub emotion_runs: Vec<String>,
    pub scorecard_summary_path: PathBuf,
    pub review_kit_summary_path: PathBuf,
    pub template_backlog_summary_path: PathBuf,
}

/// draft 节 `alignment_mismatches` 条目。
#[derive(Debug, Clone)]
pub struct DraftDriftSample {
    pub draft: String,
    pub chapter: String,
    pub ending: String,
    pub score: usize,
}

/// draft 节 dict。
#[derive(Debug, Clone)]
pub struct DraftSection {
    pub exists: bool,
    pub files: usize,
    pub warnings: usize,
    pub stories: Vec<DraftStory>,
    pub workspace_templates: String,
    pub template_targets: String,
    pub template_research_path: PathBuf,
    pub template_catalog_summary_path: PathBuf,
    pub template_catalog_json_path: PathBuf,
    pub template_learning_anchors: Vec<Value>,
    pub template_writeback_queue: Vec<Value>,
    pub alignment_summary: String,
    pub alignment_mismatches: Vec<DraftDriftSample>,
}

/// consistency 节 dict。
#[derive(Debug, Clone)]
pub struct ConsistencySection {
    pub db_path: PathBuf,
    pub feedback_path: PathBuf,
    pub feedback_entries: usize,
    pub feedback_decisions: Vec<(String, usize)>,
    pub feedback_facets: Vec<(String, usize)>,
    pub feedback_backlog: Vec<consistency::BacklogItem>,
    pub feedback_pending: usize,
    pub feedback_pending_samples: Vec<ConflictRow>,
    pub story_alignment: Vec<AlignmentRow>,
    pub story_tension: Vec<TensionRow>,
    pub story_goal_tension: Vec<GoalTensionRow>,
    pub story_relationship_tension: Vec<RelationshipTensionRow>,
    pub story_conflicts: Vec<ConflictRow>,
    pub story_trajectories: Vec<TrajectorySummaryRow>,
    pub story_trajectory_details: Vec<TrajectoryDetailRow>,
    pub relationship_pair_trajectories: Vec<PairTrajectoryRow>,
}

/// 收集 concept 节。
fn collect_concept_section(novel_dir: &Path) -> Result<ConceptSection> {
    let cards_dir = novel_dir.join("concept").join("cards");
    if !cards_dir.exists() {
        return Ok(ConceptSection {
            exists: false,
            files: 0,
            warnings: 0,
            summary_paths: Vec::new(),
            categories: Vec::new(),
        });
    }
    let files = concept_stats::collect_targets(std::slice::from_ref(&cards_dir), false);
    let reports = concept_stats::build_single_reports(&files)?;
    let summary_paths = concept_stats::build_directory_summaries(&reports)?;
    let mut grouped: BTreeMap<String, Vec<concept_stats::Report>> = BTreeMap::new();
    for report in &reports {
        let key = report
            .0
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        grouped.entry(key).or_default().push(report.clone());
    }
    let mut categories: Vec<ConceptCategory> = Vec::new();
    for (category, items) in &grouped {
        let mut ordered = items.clone();
        ordered.sort_by(|a, b| {
            b.1.len().cmp(&a.1.len()).then_with(|| {
                let an =
                    a.0.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                let bn =
                    b.0.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                an.cmp(&bn)
            })
        });
        let total_warnings = items.iter().map(|(_, warnings)| warnings.len()).sum();
        let (top_file, top_warnings) = ordered[0].clone();
        let mut kind_counter = Ctr::default();
        for w in &top_warnings {
            kind_counter.add(&w.kind, 1);
        }
        let joined = kind_counter
            .most_common(3)
            .iter()
            .map(|(kind, count)| format!("{kind} x{count}"))
            .collect::<Vec<_>>()
            .join(", ");
        categories.push(ConceptCategory {
            category: category.clone(),
            cards: items.len(),
            warnings: total_warnings,
            top_file: top_file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            top_count: top_warnings.len(),
            top_kinds: if joined.is_empty() {
                "无".into()
            } else {
                joined
            },
        });
    }
    Ok(ConceptSection {
        exists: true,
        files: files.len(),
        warnings: reports.iter().map(|(_, w)| w.len()).sum(),
        summary_paths,
        categories,
    })
}

/// 收集 plan 节。
fn collect_plan_section(novel_dir: &Path, engine: &PlanEngine) -> Result<PlanSection> {
    let plan_dirs: Vec<PathBuf> = PLAN_DIR_NAMES
        .iter()
        .map(|name| novel_dir.join(name))
        .filter(|path| path.exists())
        .collect();
    if plan_dirs.is_empty() {
        return Ok(PlanSection {
            exists: false,
            files: 0,
            warnings: 0,
            summary_paths: Vec::new(),
            plan_types: Vec::new(),
            chapter_function_distribution: String::new(),
            ending_function_distribution: String::new(),
            story_trends: Vec::new(),
        });
    }
    let files = plan_stats::collect_targets(&plan_dirs);
    let reports = plan_stats::build_single_reports(engine, &files, None, None)?;
    let summary_paths = plan_stats::build_directory_summaries(engine, &reports, None)?;

    let mut grouped: BTreeMap<String, Vec<plan_stats::Report>> = BTreeMap::new();
    let mut chapter_function_counter = Ctr::default();
    let mut ending_function_counter = Ctr::default();
    let mut chapter_plan_by_story: BTreeMap<String, Vec<(PathBuf, String, String)>> =
        BTreeMap::new();
    for (path, plan_type, warnings) in &reports {
        grouped.entry(plan_type.clone()).or_default().push((
            path.clone(),
            plan_type.clone(),
            warnings.clone(),
        ));
        if plan_type == "chapter-plan" {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("无法读取大纲文件 {}", path.display()))?;
            let lines: Vec<&str> = text.lines().collect();
            let headings = plan_audit::parse_headings(&lines);
            let sections = plan_audit::collect_section_lines(&lines, &headings);
            let (_name, chapter_function_section) =
                plan_audit::find_section(&sections, &["本章功能"]);
            let bullets: Vec<String> = plan_audit::bullet_lines(chapter_function_section)
                .iter()
                .map(|line| line.1.clone())
                .collect();
            let chapter_function_text = {
                let joined = bullets.join(" ");
                if joined.is_empty() {
                    plan_audit::section_text(chapter_function_section)
                } else {
                    joined
                }
            };
            let chapter_function = plan_audit::detect_function_label(
                &chapter_function_text,
                engine.chapter_function_rules(),
            );
            chapter_function_counter.add(&chapter_function, 1);

            let ending_choices: Vec<&str> = engine
                .chapter_ending_group()
                .iter()
                .map(|s| s.as_str())
                .collect();
            let (_ending_name, ending_section) =
                plan_audit::find_section(&sections, &ending_choices);
            let ending_bullets: Vec<String> = plan_audit::bullet_lines(ending_section)
                .iter()
                .map(|line| line.1.clone())
                .collect();
            let ending_text = {
                let joined = ending_bullets.join(" ");
                if joined.is_empty() {
                    plan_audit::section_text(ending_section)
                } else {
                    joined
                }
            };
            let ending_function =
                plan_audit::detect_function_label(&ending_text, engine.ending_function_rules());
            ending_function_counter.add(&ending_function, 1);

            let (_doc_type, _arc, story, _chapter) =
                consistency::classify_document(path, novel_dir);
            let story_key = story
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "story1".to_string());
            chapter_plan_by_story.entry(story_key).or_default().push((
                path.clone(),
                chapter_function,
                ending_function,
            ));
        }
    }

    let mut plan_types: Vec<PlanTypeInfo> = Vec::new();
    for (plan_type, items) in &grouped {
        let mut ordered = items.clone();
        ordered.sort_by(|a, b| {
            b.2.len().cmp(&a.2.len()).then_with(|| {
                let an =
                    a.0.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                let bn =
                    b.0.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                an.cmp(&bn)
            })
        });
        let total_warnings = items.iter().map(|(_, _, w)| w.len()).sum();
        let (top_file, _top_type, top_warnings) = &ordered[0];
        let mut kind_counter = Ctr::default();
        for w in top_warnings {
            kind_counter.add(&w.kind, 1);
        }
        let joined = kind_counter
            .most_common(3)
            .iter()
            .map(|(kind, count)| format!("{kind} x{count}"))
            .collect::<Vec<_>>()
            .join(", ");
        plan_types.push(PlanTypeInfo {
            plan_type: plan_type.clone(),
            files: items.len(),
            warnings: total_warnings,
            top_file: top_file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            top_count: top_warnings.len(),
            top_kinds: if joined.is_empty() {
                "无".into()
            } else {
                joined
            },
        });
    }

    let mut story_trends: Vec<StoryTrend> = Vec::new();
    for (story, rows) in &chapter_plan_by_story {
        let mut ordered_rows = rows.clone();
        ordered_rows.sort_by_key(|item| {
            item.0
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        let chapter_labels: Vec<String> = ordered_rows.iter().map(|item| item.1.clone()).collect();
        let ending_labels: Vec<String> = ordered_rows.iter().map(|item| item.2.clone()).collect();
        story_trends.push(StoryTrend {
            story: story.clone(),
            chapter_flow: {
                let joined = chapter_labels
                    .iter()
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" -> ");
                if chapter_labels.is_empty() {
                    "无".into()
                } else {
                    joined
                }
            },
            ending_flow: {
                let joined = ending_labels
                    .iter()
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" -> ");
                if ending_labels.is_empty() {
                    "无".into()
                } else {
                    joined
                }
            },
            chapter_runs: summarize_runs_local(&chapter_labels),
            ending_runs: summarize_runs_local(&ending_labels),
        });
    }

    Ok(PlanSection {
        exists: true,
        files: files.len(),
        warnings: reports.iter().map(|(_, _, w)| w.len()).sum(),
        summary_paths,
        plan_types,
        chapter_function_distribution: summarize_counter(&chapter_function_counter, 6),
        ending_function_distribution: summarize_counter(&ending_function_counter, 6),
        story_trends,
    })
}

/// 收集 draft 节（复用 draft-stats / scorecard / catalog /
/// alignment / backlog / kit 模块，本函数只做等价编排与派生字段计算）。
fn collect_draft_section(
    novel_dir: &Path,
    sample_limit: usize,
    window_sizes: &[usize],
    ctx: &DraftContext,
    engine: &PlanEngine,
) -> Result<DraftSection> {
    let labels = &ctx.draft_rules().ending_labels;
    let drafts_dir = novel_dir.join("drafts");
    if !drafts_dir.exists() {
        return Ok(DraftSection {
            exists: false,
            files: 0,
            warnings: 0,
            stories: Vec::new(),
            workspace_templates: String::new(),
            template_targets: String::new(),
            template_research_path: novel_dir.join("draft-stats").join("TEMPLATE_RESEARCH.md"),
            template_catalog_summary_path: catalog::summary_path_for(novel_dir),
            template_catalog_json_path: catalog::json_path_for(novel_dir),
            template_learning_anchors: Vec::new(),
            template_writeback_queue: Vec::new(),
            alignment_summary: String::new(),
            alignment_mismatches: Vec::new(),
        });
    }
    let files = collect_chapter_files(std::slice::from_ref(&drafts_dir))?;
    if files.is_empty() {
        return Ok(DraftSection {
            exists: false,
            files: 0,
            warnings: 0,
            stories: Vec::new(),
            workspace_templates: String::new(),
            template_targets: String::new(),
            template_research_path: novel_dir.join("draft-stats").join("TEMPLATE_RESEARCH.md"),
            template_catalog_summary_path: catalog::summary_path_for(novel_dir),
            template_catalog_json_path: catalog::json_path_for(novel_dir),
            template_learning_anchors: Vec::new(),
            template_writeback_queue: Vec::new(),
            alignment_summary: String::new(),
            alignment_mismatches: Vec::new(),
        });
    }

    let corpus_paths = ctx.corpus_paths_for_targets(&files);
    let corpus_profile = crate::audit::draft::build_corpus_profile(ctx, &corpus_paths)?;
    let template_bank = build_template_bank(ctx.draft_rules());
    let env = AnalysisEnv {
        ctx,
        template_bank: &template_bank,
        term_bank: &ctx.draft_rules().tracked_terms,
        corpus_profile: corpus_profile.as_ref(),
        sample_limit,
    };
    let analyses = env.analyze_chapters(&files)?;
    {
        use crate::stats::draft as draft_stats;
        draft_stats::build_single_reports(&env, &files, None, Some(&analyses), None)?;
        draft_stats::build_group_reports(
            &env,
            &files,
            window_sizes,
            None,
            Some(&analyses),
            labels,
        )?;
    }

    // grouped：story_dir -> 章分析（按父目录分组）。
    let mut grouped: BTreeMap<PathBuf, Vec<&(PathBuf, Analysis)>> = BTreeMap::new();
    for pair in &analyses {
        let parent = pair
            .0
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        grouped.entry(parent).or_default().push(pair);
    }

    let mut workspace_template_counter = Ctr::default();
    let mut deposition_counter = Ctr::default();
    let mut alignment_counter = Ctr::default();
    let mut alignment_mismatches: Vec<DraftDriftSample> = Vec::new();
    for (path, analysis) in &analyses {
        let alignment_result =
            alignment::build_plan_draft_alignment(engine, path, Some(novel_dir), analysis)?;
        if alignment_result.available {
            alignment_counter.add(
                &format!(
                    "chapter::{}->{}",
                    alignment_result
                        .plan_chapter_function
                        .as_deref()
                        .unwrap_or(""),
                    alignment_result
                        .draft_chapter_function
                        .as_deref()
                        .unwrap_or("")
                ),
                1,
            );
            alignment_counter.add(
                &format!(
                    "ending::{}->{}",
                    alignment_result
                        .plan_ending_function
                        .as_deref()
                        .unwrap_or(""),
                    alignment_result
                        .draft_ending_function
                        .as_deref()
                        .unwrap_or("")
                ),
                1,
            );
            if alignment_result.mismatch_count.unwrap_or(0) >= 1
                && alignment_mismatches.len() < DRIFT_SAMPLE_LIMIT * 4
            {
                alignment_mismatches.push(DraftDriftSample {
                    draft: rel_posix(path, &drafts_dir),
                    chapter: format!(
                        "{}->{}",
                        alignment_result
                            .plan_chapter_function
                            .as_deref()
                            .unwrap_or(""),
                        alignment_result
                            .draft_chapter_function
                            .as_deref()
                            .unwrap_or("")
                    ),
                    ending: format!(
                        "{}->{}",
                        alignment_result
                            .plan_ending_function
                            .as_deref()
                            .unwrap_or(""),
                        alignment_result
                            .draft_ending_function
                            .as_deref()
                            .unwrap_or("")
                    ),
                    score: alignment_result.mismatch_count.unwrap_or(0),
                });
            }
        }
        let mut seen: Vec<String> = Vec::new();
        for candidate in analysis.template_candidates.iter().take(20) {
            let key = format!("{}::{}", candidate.candidate_type, candidate.name);
            if seen.contains(&key) {
                continue;
            }
            seen.push(key.clone());
            workspace_template_counter.add(&key, 1);
            deposition_counter.add(
                infer_template_deposition_target(&candidate.candidate_type, &candidate.name),
                1,
            );
        }
    }

    let mut stories: Vec<DraftStory> = Vec::new();
    for (story_dir, items) in &grouped {
        let mut ordered = items.clone();
        ordered.sort_by(|a, b| {
            b.1.summary
                .warn_sections
                .cmp(&a.1.summary.warn_sections)
                .then_with(|| b.1.summary.chars.cmp(&a.1.summary.chars))
                .then_with(|| {
                    let an =
                        a.0.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                    let bn =
                        b.0.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                    an.cmp(&bn)
                })
        });
        let (top_path, top_analysis) = ordered[0];
        let top_templates = top_analysis
            .template_candidates
            .iter()
            .take(3)
            .map(|c| format!("{} x{}", c.name, c.count))
            .collect::<Vec<_>>()
            .join(", ");
        let top_fatigue = top_analysis
            .style_fatigue
            .iter()
            .filter(|i| i.status == "WARN")
            .map(|i| format!("{} x{}", i.family, i.count))
            .collect::<Vec<_>>()
            .join(", ");

        let mut gates = Ctr::default();
        let mut recommendations = Ctr::default();
        let mut axis_totals = Ctr::default();
        let mut scene_role_counter = Ctr::default();
        let mut tone_counter = Ctr::default();
        let mut emotion_counter = Ctr::default();
        let mut speaker_counter = Ctr::default();
        let mut template_counter = Ctr::default();
        let mut story_alignment_counter = Ctr::default();
        let mut story_alignment_mismatches: Vec<String> = Vec::new();
        let mut voice_drifts: Vec<String> = Vec::new();
        let mut chapter_flow: Vec<String> = Vec::new();
        let mut ending_flow: Vec<String> = Vec::new();
        let mut ending_signal_flow: Vec<String> = Vec::new();
        let mut tone_flow: Vec<String> = Vec::new();
        let mut emotion_flow: Vec<String> = Vec::new();
        let mut ending_signal_counter = Ctr::default();

        for (path, analysis) in items {
            let axes = scorecard::build_axes(analysis, None, None, None);
            let (gate, _priority, recommendation) = scorecard::decide_gate(analysis, &axes);
            gates.add(&gate, 1);
            recommendations.add(&recommendation, 1);
            for axis in &axes {
                axis_totals.add(&axis.name, axis.score as usize);
            }
            scene_role_counter.add(&analysis.scene_map.dominant_role, 1);

            let dominant_tone = &analysis.tone_profile.dominant_tone;
            if !dominant_tone.is_empty() && dominant_tone != "none" {
                tone_counter.add(dominant_tone, 1);
            }
            tone_flow.push(if dominant_tone.is_empty() {
                "none".to_string()
            } else {
                dominant_tone.clone()
            });

            let dominant_emotion = &analysis.dialogue_emotions.dominant_emotion;
            if !dominant_emotion.is_empty() && dominant_emotion != "neutral" {
                emotion_counter.add(dominant_emotion, 1);
            }

            for speaker in analysis.character_voice.speakers.iter().take(4) {
                speaker_counter.add(&speaker.speaker, speaker.lines);
            }
            let cv = &analysis.character_voice;
            if cv.warn {
                voice_drifts.extend(cv.homogenized_pairs.iter().take(2).cloned());
            }

            emotion_flow.push(if dominant_emotion.is_empty() {
                "neutral".to_string()
            } else {
                dominant_emotion.clone()
            });
            let ending_signal = infer_ending_label(analysis, labels);
            ending_signal_counter.add(&ending_signal, 1);
            ending_signal_flow.push(ending_signal);

            let alignment_result =
                alignment::build_plan_draft_alignment(engine, path, Some(novel_dir), analysis)?;
            if alignment_result.available {
                story_alignment_counter.add(
                    &format!(
                        "{}->{}",
                        alignment_result
                            .plan_chapter_function
                            .as_deref()
                            .unwrap_or(""),
                        alignment_result
                            .draft_chapter_function
                            .as_deref()
                            .unwrap_or("")
                    ),
                    1,
                );
                chapter_flow.push(
                    alignment_result
                        .draft_chapter_function
                        .as_deref()
                        .unwrap_or("")
                        .to_string(),
                );
                ending_flow.push(
                    alignment_result
                        .draft_ending_function
                        .as_deref()
                        .unwrap_or("")
                        .to_string(),
                );
                if alignment_result.mismatch_count.unwrap_or(0) >= 1
                    && story_alignment_mismatches.len() < 4
                {
                    story_alignment_mismatches.push(format!(
                        "{} chapter={}->{} ending={}->{}",
                        path.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        alignment_result
                            .plan_chapter_function
                            .as_deref()
                            .unwrap_or(""),
                        alignment_result
                            .draft_chapter_function
                            .as_deref()
                            .unwrap_or(""),
                        alignment_result
                            .plan_ending_function
                            .as_deref()
                            .unwrap_or(""),
                        alignment_result
                            .draft_ending_function
                            .as_deref()
                            .unwrap_or("")
                    ));
                }
            }

            let mut seen_templates: Vec<String> = Vec::new();
            for candidate in analysis.template_candidates.iter().take(20) {
                let key = format!("{}::{}", candidate.candidate_type, candidate.name);
                if seen_templates.contains(&key) {
                    continue;
                }
                seen_templates.push(key.clone());
                template_counter.add(&key, 1);
            }

            let scorecard_path = scorecard::scorecard_path_for(path)?;
            let report =
                scorecard::build_scorecard_report(engine, labels, path, analysis, None, None)?;
            input::write_text(&scorecard_path, &report)?;
        }

        let first_path = &items[0].0;
        let scorecard_summary_path = scorecard::scorecard_path_for(first_path)?
            .parent()
            .with_context(|| "scorecard path has no parent")?
            .join("SUMMARY.md");
        let pairs: Vec<(PathBuf, &Analysis)> = items.iter().map(|(p, a)| (p.clone(), a)).collect();
        let summary_md = scorecard::build_story_summary(story_dir, &pairs, &[], labels, engine)?;
        input::write_text(&scorecard_summary_path, &summary_md)?;

        let review_kit_summary_path = kit::review_kit_path_for(first_path)?;
        let template_backlog_summary_path = backlog::backlog_path_for(first_path)?;

        // avg_axis：按轴名排序 `{name} {round(total/n,2)}`（banker's + 浮点展示）。
        let avg_axis = {
            let mut entries = axis_totals.items();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let n = items.len().max(1);
            entries
                .iter()
                .map(|(name, total)| {
                    format!("{name} {}", float_repr(round2(*total as f64 / n as f64)))
                })
                .collect::<Vec<_>>()
                .join(", ")
        };

        // ending_signal_summary：按展示名计数（计数 + 首现序）。
        // —— 同显示名多 raw 时 dict 赋值「同位更新值、末次 raw 胜」，再 most_common(4) 空格连排。
        let ending_signal_summary = {
            let mut entries: Vec<(String, usize)> = Vec::new();
            for (name, count) in ending_signal_counter.items() {
                let display = ending_display(labels, &name);
                if let Some(slot) = entries.iter_mut().find(|(d, _)| *d == display) {
                    slot.1 = count;
                } else {
                    entries.push((display, count));
                }
            }
            let mut ordered = entries.clone();
            ordered.sort_by_key(|b| std::cmp::Reverse(b.1));
            let joined = ordered
                .into_iter()
                .take(4)
                .map(|(name, count)| format!("{name} x{count}"))
                .collect::<Vec<_>>()
                .join(" ");
            if joined.is_empty() {
                "无".to_string()
            } else {
                joined
            }
        };

        stories.push(DraftStory {
            story: rel_posix(story_dir, &drafts_dir),
            chapters: items.len(),
            warnings: items.iter().map(|(_, a)| a.summary.warn_sections).sum(),
            top_file: top_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            top_count: top_analysis.summary.warn_sections,
            top_templates: if top_templates.is_empty() {
                "无".into()
            } else {
                top_templates
            },
            top_fatigue: if top_fatigue.is_empty() {
                "无".into()
            } else {
                top_fatigue
            },
            gate_summary: ctr_sorted_join(&gates),
            recommendation_summary: ctr_sorted_join(&recommendations),
            avg_axis,
            scene_summary: summarize_counter(&scene_role_counter, 3),
            tone_summary: summarize_counter(&tone_counter, 3),
            emotion_summary: summarize_counter(&emotion_counter, 3),
            speaker_summary: summarize_counter(&speaker_counter, 4),
            template_summary: summarize_counter(&template_counter, 4),
            alignment_summary: summarize_counter(&story_alignment_counter, 3),
            voice_drifts: voice_drifts.into_iter().take(4).collect(),
            ending_signal_summary,
            alignment_mismatches: story_alignment_mismatches,
            chapter_runs: summarize_runs_local(&chapter_flow),
            ending_runs: summarize_runs_local(&ending_flow),
            ending_signal_flow: ending_flow_text(&ending_signal_flow, 8, labels),
            ending_signal_runs: summarize_runs(&ending_signal_flow, 2, 4, labels),
            trend_convergences: summarize_story_convergences(
                &ending_signal_flow,
                &tone_flow,
                &emotion_flow,
                labels,
            ),
            tone_runs: summarize_runs_local(&tone_flow),
            emotion_runs: summarize_runs_local(&emotion_flow),
            scorecard_summary_path,
            review_kit_summary_path,
            template_backlog_summary_path,
        });
    }

    // story payloads → TEMPLATE_RESEARCH.md + CATALOG。
    let mut story_payloads: Vec<Value> = Vec::new();
    for (story_dir, items) in &grouped {
        let pairs: Vec<(PathBuf, &Analysis)> = items.iter().map(|(p, a)| (p.clone(), a)).collect();
        let (_md, payload) = backlog::build_story_backlog(story_dir, &pairs)?;
        story_payloads.push(payload);
    }
    let template_research_path = novel_dir.join("draft-stats").join("TEMPLATE_RESEARCH.md");
    input::write_text(
        &template_research_path,
        &build_template_research_report(
            novel_dir,
            &grouped,
            &workspace_template_counter,
            &deposition_counter,
        ),
    )?;

    let catalog_value = catalog::build_catalog_payload(ctx, novel_dir, &story_payloads);
    let mut catalog_arrays: Vec<Vec<Value>> = Vec::new();
    for key in [
        "template_candidates",
        "template_families",
        "term_candidates",
        "keep_candidates",
        "deposition_targets",
        "learning_anchors",
        "writeback_queue",
    ] {
        catalog_arrays.push(
            catalog_value
                .get(key)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
    }
    let catalog_sections = catalog::CatalogSections {
        template_candidates: &catalog_arrays[0],
        template_families: &catalog_arrays[1],
        term_candidates: &catalog_arrays[2],
        keep_candidates: &catalog_arrays[3],
        deposition_targets: &catalog_arrays[4],
        learning_anchors: &catalog_arrays[5],
        writeback_queue: &catalog_arrays[6],
    };
    let catalog_markdown =
        catalog::build_catalog_markdown(novel_dir, story_payloads.len(), &catalog_sections);
    let summary_path = catalog::summary_path_for(novel_dir);
    let json_path = catalog::json_path_for(novel_dir);
    input::write_text(&summary_path, &catalog_markdown)?;
    input::write_json(&json_path, &catalog_value)?;

    Ok(DraftSection {
        exists: true,
        files: files.len(),
        warnings: analyses.iter().map(|(_, a)| a.summary.warn_sections).sum(),
        stories,
        workspace_templates: summarize_counter(&workspace_template_counter, 6),
        template_targets: summarize_counter(&deposition_counter, 4),
        template_research_path,
        template_catalog_summary_path: summary_path,
        template_catalog_json_path: json_path,
        template_learning_anchors: catalog_arrays[5].iter().take(4).cloned().collect(),
        template_writeback_queue: catalog_arrays[6].iter().take(5).cloned().collect(),
        alignment_summary: summarize_counter(&alignment_counter, 6),
        alignment_mismatches: alignment_mismatches
            .into_iter()
            .take(DRIFT_SAMPLE_LIMIT)
            .collect(),
    })
}

/// `sorted(counter.items())` → `{name} x{count}` 空格连排（`or "无"`）。
fn ctr_sorted_join(counter: &Ctr) -> String {
    let mut entries = counter.items();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let joined = entries
        .iter()
        .map(|(name, count)| format!("{name} x{count}"))
        .collect::<Vec<_>>()
        .join(" ");
    if joined.is_empty() {
        "无".to_string()
    } else {
        joined
    }
}

/// 写 `draft-stats/TEMPLATE_RESEARCH.md`。
fn build_template_research_report(
    novel_dir: &Path,
    grouped: &BTreeMap<PathBuf, Vec<&(PathBuf, Analysis)>>,
    workspace_counter: &Ctr,
    deposition_counter: &Ctr,
) -> String {
    let mut lines: Vec<String> = vec!["# Template Research".to_string(), String::new()];
    lines.push(format!(
        "- novel: `{}`",
        novel_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    lines.push(String::new());

    lines.push("## Workspace Candidates".to_string());
    if !workspace_counter.is_empty() {
        for (name, count) in workspace_counter.most_common(20) {
            lines.push(format!("- `{name}` x{count}"));
        }
    } else {
        lines.push("- 无".to_string());
    }
    lines.push(String::new());

    lines.push("## Deposition Targets".to_string());
    if !deposition_counter.is_empty() {
        for (name, count) in deposition_counter.most_common_all() {
            lines.push(format!("- `{name}` x{count}"));
        }
    } else {
        lines.push("- 无".to_string());
    }
    lines.push(String::new());

    lines.push("## By Story".to_string());
    for (story_dir, items) in grouped {
        let mut template_counter = Ctr::default();
        for (_path, analysis) in items {
            let mut seen: Vec<String> = Vec::new();
            for candidate in analysis.template_candidates.iter().take(20) {
                let key = format!("{}::{}", candidate.candidate_type, candidate.name);
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key.clone());
                template_counter.add(&key, 1);
            }
        }
        lines.push(format!("- `{}`", story_dir.display()));
        if !template_counter.is_empty() {
            for (name, count) in template_counter.most_common(8) {
                lines.push(format!("  - `{name}` x{count}"));
            }
        } else {
            lines.push("  - 无".to_string());
        }
    }
    lines.push(String::new());
    lines.join("\n") + "\n"
}

/// 收集 consistency 节。
fn collect_consistency_section(novel_dir: &Path) -> Result<ConsistencySection> {
    let db_path = novel_dir
        .join("research")
        .join("consistency")
        .join("consistency.sqlite3");
    let feedback_path = novel_dir
        .join("research")
        .join("consistency")
        .join("review-feedback.jsonl");
    consistency::build_index(novel_dir, &db_path)?;
    let conn = consistency::open_db(&db_path)?;
    let alignment_rows = consistency::query_story_alignment_rows(&conn, 200)?;
    let tension_rows = consistency::query_story_tension_rows(&conn, 200)?;
    let goal_rows = consistency::query_story_goal_tension_rows(&conn, 200)?;
    let relationship_rows = consistency::query_story_relationship_tension_rows(&conn, 200)?;
    let pair_rows = consistency::query_story_relationship_pair_rows(&conn, 800)?;
    let conflict_rows = consistency::collect_conflict_rows(&conn, 200, Some(&feedback_path))?;
    let feedback_summary = consistency::summarize_feedback(&conn, &feedback_path, 200, None)?;
    let story_trajectories =
        build_story_trajectory_summary(&tension_rows, &goal_rows, &relationship_rows);
    let story_trajectory_details = build_story_trajectory_details(
        &conn,
        novel_dir,
        &tension_rows,
        &goal_rows,
        &relationship_rows,
    )?;
    let relationship_pair_trajectories =
        build_relationship_pair_trajectories(novel_dir, &pair_rows, 4);
    drop(conn);

    Ok(ConsistencySection {
        db_path,
        feedback_path,
        feedback_entries: feedback_summary.entries.len(),
        feedback_decisions: feedback_summary.decision_counter.most_common_all(),
        feedback_facets: feedback_summary.facet_counter.most_common(6),
        feedback_backlog: feedback_summary.backlog,
        feedback_pending: feedback_summary.unresolved.len(),
        feedback_pending_samples: feedback_summary.unresolved.into_iter().take(6).collect(),
        story_alignment: alignment_rows,
        story_tension: tension_rows,
        story_goal_tension: goal_rows,
        story_relationship_tension: relationship_rows,
        story_conflicts: conflict_rows,
        story_trajectories,
        story_trajectory_details,
        relationship_pair_trajectories,
    })
}

/// 标量渲染（catalog JSON 值 → str）：int 十进制 / str 原样 / None → "None"。
fn json_scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "None".to_string(),
        other => other.to_string(),
    }
}

/// 渲染单份 AUDIT.md 看板（输出逐字节稳定）。
fn build_dashboard(
    novel_dir: &Path,
    concept: &ConceptSection,
    plans: &PlanSection,
    drafts: &DraftSection,
    consistency: &ConsistencySection,
) -> String {
    let narrative_trajectories = build_narrative_trajectory_rows(drafts, consistency);
    let novel_name = novel_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut lines: Vec<String> = vec!["# AUDIT DASHBOARD".to_string(), String::new()];
    lines.push(format!("- novel: `{novel_name}`"));
    lines.push(format!(
        "- concept cards: `{}`",
        if concept.exists { concept.files } else { 0 }
    ));
    lines.push(format!(
        "- plan files: `{}`",
        if plans.exists { plans.files } else { 0 }
    ));
    lines.push(format!(
        "- draft chapters: `{}`",
        if drafts.exists { drafts.files } else { 0 }
    ));
    lines.push(String::new());

    // ## Concept
    lines.push("## Concept".to_string());
    if !concept.exists {
        lines.push("- 无概念卡目录".to_string());
    } else {
        lines.push(format!("- total warnings: `{}`", concept.warnings));
        for item in &concept.categories {
            lines.push(format!(
                "- `{}` cards=`{}` warnings=`{}` top=`{}` (`{}` | {})",
                item.category,
                item.cards,
                item.warnings,
                item.top_file,
                item.top_count,
                item.top_kinds
            ));
        }
        lines.push("- summary files:".to_string());
        for path in &concept.summary_paths {
            lines.push(format!("  - `{}`", path.display()));
        }
    }
    lines.push(String::new());

    // ## Plans
    lines.push("## Plans".to_string());
    if !plans.exists {
        lines.push("- 无大纲目录".to_string());
    } else {
        lines.push(format!("- total warnings: `{}`", plans.warnings));
        lines.push(format!(
            "- chapter functions: {}",
            plans.chapter_function_distribution
        ));
        lines.push(format!(
            "- ending functions: {}",
            plans.ending_function_distribution
        ));
        if !plans.story_trends.is_empty() {
            lines.push("- story function trends:".to_string());
            for item in plans.story_trends.iter().take(8) {
                lines.push(format!(
                    "  - `{}` chapter_flow=`{}` ending_flow=`{}`",
                    item.story, item.chapter_flow, item.ending_flow
                ));
                if !item.chapter_runs.is_empty() || !item.ending_runs.is_empty() {
                    let mut run_parts: Vec<String> = Vec::new();
                    if !item.chapter_runs.is_empty() {
                        run_parts.push(format!("chapter_runs={}", item.chapter_runs.join(" | ")));
                    }
                    if !item.ending_runs.is_empty() {
                        run_parts.push(format!("ending_runs={}", item.ending_runs.join(" | ")));
                    }
                    lines.push(format!("    trend=`{}`", run_parts.join("; ")));
                }
            }
        }
        for item in &plans.plan_types {
            lines.push(format!(
                "- `{}` files=`{}` warnings=`{}` top=`{}` (`{}` | {})",
                item.plan_type,
                item.files,
                item.warnings,
                item.top_file,
                item.top_count,
                item.top_kinds
            ));
        }
        lines.push("- summary files:".to_string());
        for path in &plans.summary_paths {
            lines.push(format!("  - `{}`", path.display()));
        }
    }
    lines.push(String::new());

    // ## Drafts
    lines.push("## Drafts".to_string());
    if !drafts.exists {
        lines.push("- 无草稿目录".to_string());
    } else {
        lines.push(format!("- total warn sections: `{}`", drafts.warnings));
        lines.push(format!(
            "- workspace templates: {}",
            drafts.workspace_templates
        ));
        lines.push(format!("- deposition targets: {}", drafts.template_targets));
        lines.push(format!(
            "- plan-draft alignment: {}",
            drafts.alignment_summary
        ));
        for item in &drafts.stories {
            lines.push(format!(
                "- `{}` chapters=`{}` warn_sections=`{}` top=`{}` (`{}` | {}) fatigue=`{}`",
                item.story,
                item.chapters,
                item.warnings,
                item.top_file,
                item.top_count,
                item.top_templates,
                item.top_fatigue
            ));
            lines.push(format!(
                "  gate=`{}` recommendation=`{}` avg_axes=`{}`",
                item.gate_summary, item.recommendation_summary, item.avg_axis
            ));
            lines.push(format!(
                "  narrative=`scene:{} tone:{} emotion:{} speakers:{}` templates=`{}` alignment=`{}` ending_signals=`{}`",
                item.scene_summary, item.tone_summary, item.emotion_summary, item.speaker_summary,
                item.template_summary, item.alignment_summary, item.ending_signal_summary
            ));
            if !item.alignment_mismatches.is_empty() {
                lines.push(format!(
                    "  drift=`{}`",
                    item.alignment_mismatches
                        .iter()
                        .take(2)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" | ")
                ));
            }
            if !item.voice_drifts.is_empty() {
                lines.push(format!(
                    "  voice=`{}`",
                    item.voice_drifts
                        .iter()
                        .take(2)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" | ")
                ));
            }
            let has_trend = !item.chapter_runs.is_empty()
                || !item.ending_runs.is_empty()
                || !item.ending_signal_runs.is_empty()
                || !item.tone_runs.is_empty()
                || !item.emotion_runs.is_empty()
                || !item.trend_convergences.is_empty();
            if has_trend {
                let mut trend_parts: Vec<String> = Vec::new();
                if !item.chapter_runs.is_empty() {
                    trend_parts.push(format!("chapter_runs={}", item.chapter_runs.join(" | ")));
                }
                if !item.ending_runs.is_empty() {
                    trend_parts.push(format!("ending_runs={}", item.ending_runs.join(" | ")));
                }
                if !item.ending_signal_runs.is_empty() {
                    trend_parts.push(format!(
                        "ending_signal_runs={}",
                        item.ending_signal_runs.join(" | ")
                    ));
                }
                if !item.trend_convergences.is_empty() {
                    trend_parts.push(format!(
                        "convergence={}",
                        item.trend_convergences.join(" | ")
                    ));
                }
                if !item.tone_runs.is_empty() {
                    trend_parts.push(format!("tone_runs={}", item.tone_runs.join(" | ")));
                }
                if !item.emotion_runs.is_empty() {
                    trend_parts.push(format!("emotion_runs={}", item.emotion_runs.join(" | ")));
                }
                lines.push(format!("  trend=`{}`", trend_parts.join("; ")));
                lines.push(format!(
                    "  ending_signal_flow=`{}`",
                    item.ending_signal_flow
                ));
            }
        }
        lines.push("- mirror stats:".to_string());
        lines.push(format!("  - `{}`", novel_dir.join("draft-stats").display()));
        lines.push("- scorecard summaries:".to_string());
        for item in &drafts.stories {
            lines.push(format!("  - `{}`", item.scorecard_summary_path.display()));
        }
        lines.push("- review kits:".to_string());
        for item in &drafts.stories {
            lines.push(format!("  - `{}`", item.review_kit_summary_path.display()));
        }
        lines.push("- template backlogs:".to_string());
        for item in &drafts.stories {
            lines.push(format!(
                "  - `{}`",
                item.template_backlog_summary_path.display()
            ));
        }
        lines.push("- template research:".to_string());
        lines.push(format!("  - `{}`", drafts.template_research_path.display()));
        lines.push("- template catalog:".to_string());
        lines.push(format!(
            "  - `{}`",
            drafts.template_catalog_summary_path.display()
        ));
        lines.push(format!(
            "  - `{}`",
            drafts.template_catalog_json_path.display()
        ));
        if !drafts.template_learning_anchors.is_empty() {
            lines.push("- learning anchors:".to_string());
            for item in &drafts.template_learning_anchors {
                let kind = json_scalar(item.get("kind").unwrap_or(&Value::Null));
                let bucket_name = format!(
                    "{}::{}",
                    json_scalar(item.get("bucket").unwrap_or(&Value::Null)),
                    json_scalar(item.get("name").unwrap_or(&Value::Null))
                );
                let story_count = json_scalar(item.get("story_count").unwrap_or(&Value::Null));
                let count = json_scalar(item.get("count").unwrap_or(&Value::Null));
                lines.push(format!(
                    "  - `{kind}` `{bucket_name}` stories=`{story_count}` total=`{count}`"
                ));
            }
        }
        if !drafts.template_writeback_queue.is_empty() {
            lines.push("- writeback queue:".to_string());
            for item in &drafts.template_writeback_queue {
                lines.push(format!(
                    "  - `{}` `{}` stories=`{}` total=`{}` -> `{}`",
                    json_scalar(item.get("kind").unwrap_or(&Value::Null)),
                    json_scalar(item.get("name").unwrap_or(&Value::Null)),
                    json_scalar(item.get("stories").unwrap_or(&Value::Null)),
                    json_scalar(item.get("count").unwrap_or(&Value::Null)),
                    json_scalar(item.get("target").unwrap_or(&Value::Null))
                ));
            }
        }
        if !drafts.alignment_mismatches.is_empty() {
            lines.push("- plan-draft drift samples:".to_string());
            for item in &drafts.alignment_mismatches {
                lines.push(format!(
                    "  - `{}` chapter=`{}` ending=`{}`",
                    item.draft, item.chapter, item.ending
                ));
            }
        }
    }
    lines.push(String::new());

    // ## Consistency
    lines.push("## Consistency".to_string());
    lines.push(format!("- index: `{}`", consistency.db_path.display()));
    lines.push(format!(
        "- feedback log: `{}` entries=`{}`",
        consistency.feedback_path.display(),
        consistency.feedback_entries
    ));
    if !consistency.feedback_decisions.is_empty() {
        lines.push(format!(
            "- feedback decisions: {}",
            consistency
                .feedback_decisions
                .iter()
                .map(|(name, count)| format!("`{name}` x{count}"))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    } else {
        lines.push("- feedback decisions: 无".to_string());
    }
    if !consistency.feedback_facets.is_empty() {
        lines.push(format!(
            "- feedback facets: {}",
            consistency
                .feedback_facets
                .iter()
                .map(|(name, count)| format!("`{name}` x{count}"))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    } else {
        lines.push("- feedback facets: 无".to_string());
    }
    lines.push(format!(
        "- pending review: `{}`",
        consistency.feedback_pending
    ));
    if !consistency.feedback_backlog.is_empty() {
        lines.push("- feedback backlog:".to_string());
        for item in consistency.feedback_backlog.iter().take(4) {
            lines.push(format!("  - `{}` {}", item.target, item.reason));
        }
    } else {
        lines.push("- feedback backlog: 无".to_string());
    }
    if !consistency.feedback_pending_samples.is_empty() {
        lines.push("- pending samples:".to_string());
        for row in &consistency.feedback_pending_samples {
            lines.push(format!(
                "  - `{}` `{}` `{}` confidence=`{}`",
                row.story, row.category, row.title, row.confidence
            ));
        }
    } else {
        lines.push("- pending samples: 无".to_string());
    }
    if !consistency.story_alignment.is_empty() {
        lines.push("- story alignment:".to_string());
        for item in consistency.story_alignment.iter().take(12) {
            let mut line = format!(
                "  - `{}` plan_entities=`{}` draft_entities=`{}`",
                item.story, item.plan_entities, item.draft_entities
            );
            if let Some(only) = &item.plan_only_entities {
                if !only.is_empty() {
                    line.push_str(&format!(" plan_only=`{only}`"));
                }
            }
            if let Some(only) = &item.draft_only_entities {
                if !only.is_empty() {
                    line.push_str(&format!(" draft_only=`{only}`"));
                }
            }
            lines.push(line);
        }
    } else {
        lines.push("- story alignment: 无".to_string());
    }
    if !narrative_trajectories.is_empty() {
        lines.push("- narrative trajectories:".to_string());
        for item in narrative_trajectories.iter().take(8) {
            lines.push(format!("  - `{}` {}", item.story, item.summary));
            if !item.details.is_empty() {
                lines.push(format!(
                    "    detail=`{}`",
                    item.details
                        .iter()
                        .take(2)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" | ")
                ));
            }
        }
    } else {
        lines.push("- narrative trajectories: 无".to_string());
    }
    if !consistency.story_trajectories.is_empty() {
        lines.push("- story trajectories:".to_string());
        for item in consistency.story_trajectories.iter().take(8) {
            lines.push(format!(
                "  - `{}` state=`{}` goal=`{}` relationship=`{}`",
                item.story, item.state_summary, item.goal_summary, item.relationship_summary
            ));
            if !item.samples.is_empty() {
                lines.push(format!(
                    "    sample=`{}`",
                    item.samples
                        .iter()
                        .take(2)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" | ")
                ));
            }
        }
    } else {
        lines.push("- story trajectories: 无".to_string());
    }
    if !consistency.story_trajectory_details.is_empty() {
        lines.push("- trajectory details:".to_string());
        for item in consistency.story_trajectory_details.iter().take(6) {
            lines.push(format!("  - `{}`", item.story));
            for detail in item.items.iter().take(4) {
                let mut line = format!(
                    "    - `{}` `{}` {}",
                    detail.kind, detail.title, detail.summary
                );
                if !detail.timeline.is_empty() {
                    line.push_str(&format!(
                        " timeline=`{}`",
                        detail
                            .timeline
                            .iter()
                            .take(3)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" -> ")
                    ));
                }
                lines.push(line);
            }
        }
    } else {
        lines.push("- trajectory details: 无".to_string());
    }
    if !consistency.relationship_pair_trajectories.is_empty() {
        lines.push("- relationship pair trajectories:".to_string());
        for item in consistency.relationship_pair_trajectories.iter().take(6) {
            lines.push(format!("  - `{}`", item.story));
            for detail in item.items.iter().take(4) {
                let mut line = format!("    - `{}` {}", detail.pair, detail.summary);
                if !detail.timeline.is_empty() {
                    line.push_str(&format!(
                        " timeline=`{}`",
                        detail
                            .timeline
                            .iter()
                            .take(3)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" -> ")
                    ));
                }
                lines.push(line);
            }
        }
    } else {
        lines.push("- relationship pair trajectories: 无".to_string());
    }
    if !consistency.story_tension.is_empty() {
        lines.push("- state tension:".to_string());
        for item in consistency.story_tension.iter().take(12) {
            let mut parts: Vec<String> = Vec::new();
            let inj_n = item.injury_negative.as_deref().unwrap_or("");
            let inj_s = item.injury_stable.as_deref().unwrap_or("");
            if !inj_n.is_empty() && !inj_s.is_empty() {
                parts.push(format!("injury={inj_n} -> {inj_s}"));
            }
            let eq_d = item.equipment_damaged.as_deref().unwrap_or("");
            let eq_a = item.equipment_active.as_deref().unwrap_or("");
            if !eq_d.is_empty() && !eq_a.is_empty() {
                parts.push(format!("equipment={eq_d} -> {eq_a}"));
            }
            lines.push(format!(
                "  - `{}` `{}` ({}) {}",
                item.story,
                item.title,
                item.category,
                parts.join(" ; ")
            ));
        }
    } else {
        lines.push("- state tension: 无".to_string());
    }
    if !consistency.story_goal_tension.is_empty() {
        lines.push("- goal tension:".to_string());
        for item in consistency.story_goal_tension.iter().take(12) {
            let mut parts: Vec<String> = Vec::new();
            let g = item.goal_assigned.as_deref().unwrap_or("");
            let c = item.goal_changed.as_deref().unwrap_or("");
            let done = item.goal_completed.as_deref().unwrap_or("");
            if !g.is_empty() {
                parts.push(format!("assigned={g}"));
            }
            if !c.is_empty() {
                parts.push(format!("changed={c}"));
            }
            if !done.is_empty() {
                parts.push(format!("completed={done}"));
            }
            lines.push(format!(
                "  - `{}` `{}` ({}) {}",
                item.story,
                item.title,
                item.category,
                parts.join(" ; ")
            ));
        }
    } else {
        lines.push("- goal tension: 无".to_string());
    }
    if !consistency.story_relationship_tension.is_empty() {
        lines.push("- relationship tension:".to_string());
        for item in consistency.story_relationship_tension.iter().take(12) {
            lines.push(format!(
                "  - `{}` `{}` ({}) close={} ; distant={}",
                item.story,
                item.title,
                item.category,
                item.relationship_close.as_deref().unwrap_or(""),
                item.relationship_distant.as_deref().unwrap_or("")
            ));
        }
    } else {
        lines.push("- relationship tension: 无".to_string());
    }
    if !consistency.story_conflicts.is_empty() {
        lines.push("- conflict candidates:".to_string());
        for item in consistency.story_conflicts.iter().take(12) {
            lines.push(format!(
                "  - `{}` `{}` `{}` ({}) confidence=`{}` support=`{}` {}",
                item.story,
                item.category,
                item.title,
                item.entity_category,
                item.confidence,
                item.support_note,
                item.summary
            ));
        }
    } else {
        lines.push("- conflict candidates: 无".to_string());
    }
    lines.push(String::new());

    // ## Suggested Order（逐字照抄）
    lines.push("## Suggested Order".to_string());
    lines.push("1. 先修 `draft` 里 `gate=FAIL`、`recommendation=targeted_rewrite` 的章节，再看 `pairs / triples`".to_string());
    lines.push("2. 再修 `chapter-plan` 与 `story-plan` 的字段错位和空字段".to_string());
    lines.push("3. 如果某章评分里 `一致性准备度` 明显偏低，先跑 `sentinel consistency suspects` 再决定是否只是局部误写".to_string());
    lines.push("4. 如果已经锁定某条 Story，要逐条复核一致性候选，直接跑 `sentinel consistency review-queue novel1 --story storyN`".to_string());
    lines.push("5. 做完一轮局部复核后，立刻跑 `sentinel consistency feedback-summary novel1 --story storyN` 看这一条 Story 是否开始收敛".to_string());
    lines.push("6. 最后补 `concept` 缺口，避免下游继续空转".to_string());
    lines.join("\n") + "\n"
}
