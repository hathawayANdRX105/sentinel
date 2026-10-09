//! `reports-scorecard` 子命令：
//! 草稿章评审记分卡（scorecards/*.md + 每 story 目录 SUMMARY.md 镜像树）。
//!
//! - 章节分析复用 `audit::draft::analyze_path`（语料学习缺省开启）；
//! - 一致性快照来自 `consistency::build_story_conflict_snapshot`；
//! - 对齐信号来自 `reports::alignment`；
//! - 中文文案固定；浮点输出走 `audit::draft::float_repr`（浮点展示语义）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::audit::draft::{
    analyze_path, build_corpus_profile, float_repr, Analysis, Counter, DraftContext, HardFlag,
    ReviewReminder,
};
use crate::audit::plan::PlanEngine;
use crate::config::{self, EndingLabels};
use crate::consistency::{build_story_conflict_snapshot, BacklogItem, StoryConflictSnapshot};
use crate::input::write_text;
use crate::reports::alignment;
use crate::rules::{build_template_bank, round2};
use crate::stats::draft::{
    chapter_sort_key, collect_chapter_files, ending_flow_text, infer_ending_label, stats_path_for,
    summarize_runs,
};

/// `reports-scorecard` 子命令参数（位置 `paths` nargs+、
/// `--sample-limit` 缺省 6）。
#[derive(Debug, Clone)]
pub struct ScorecardOptions {
    /// 位置参数：草稿章文件或目录。
    pub paths: Vec<PathBuf>,
    /// 每条规则最多记录的样本行数（默认 6）。
    pub sample_limit: usize,
}

/// `lib.paths.novel_dir_for_draft`：最近的父目录名为 `drafts` 时返回其父目录。
#[must_use]
pub fn novel_dir_for_draft(draft_path: &Path) -> Option<PathBuf> {
    for parent in draft_path.ancestors().skip(1) {
        if parent.file_name().is_some_and(|name| name == "drafts") {
            return parent
                .parent()
                .map(Path::to_path_buf)
                .or_else(|| Some(parent.to_path_buf()));
        }
    }
    None
}

/// 章末标签展示名（对齐 `stats.draft.ending_label_display` 的查表语义）。
fn ending_display(labels: &EndingLabels, label: &str) -> String {
    labels
        .display
        .get(label)
        .cloned()
        .unwrap_or_else(|| label.to_string())
}

/// `scorecard_path_for`：`stats_path_for(draft).parent / scorecards / {stem}.md`。
pub fn scorecard_path_for(draft_path: &Path) -> Result<PathBuf> {
    let stats = stats_path_for(draft_path, None)?;
    let stem = draft_path
        .file_stem()
        .and_then(|s| s.to_str())
        .with_context(|| format!("章节文件名必须为合法 UTF-8: {}", draft_path.display()))?;
    Ok(stats
        .parent()
        .context("stats 路径无父目录")?
        .join("scorecards")
        .join(format!("{stem}.md")))
}

fn clamp_score(value: i32) -> i32 {
    value.clamp(1, 5)
}

fn axis_score(base: i32, penalties: &[i32], bonuses: &[i32]) -> i32 {
    let total = base - penalties.iter().sum::<i32>() + bonuses.iter().sum::<i32>();
    clamp_score(total)
}

/// 加分候选（`{name, reason}`）。
#[derive(Debug, Clone)]
pub struct BonusCandidate {
    pub name: String,
    pub reason: String,
}

/// `build_bonus_candidates`（风格侧，截前 4）。
pub fn build_bonus_candidates(analysis: &Analysis) -> Vec<BonusCandidate> {
    let mut candidates: Vec<BonusCandidate> = Vec::new();
    let summary = &analysis.summary;
    let dialogue = &analysis.dialogue;

    if summary.warn_sections <= 6 && analysis.style_fatigue.len() <= 3 {
        candidates.push(BonusCandidate {
            name: "整体收敛较稳".to_string(),
            reason: "硬警告分区不多，句式疲劳家族数量也较少，说明这一章的文体控制相对稳定。"
                .to_string(),
        });
    }
    if (0.12..=0.45).contains(&summary.quote_ratio) && dialogue.dialogue_axis_gaps.is_empty() {
        candidates.push(BonusCandidate {
            name: "对白与叙述配比自然".to_string(),
            reason: "对白比例在可读区间内，且没有明显对白转轴缺口，说明场面没有塌成纯互答录音。"
                .to_string(),
        });
    }
    if (0.18..=0.35).contains(&summary.short_sentence_ratio) && !analysis.sentence_lengths.warn {
        candidates.push(BonusCandidate {
            name: "短句节奏可保留".to_string(),
            reason: "短句比例存在但没有连发失控，更像节奏设计而不是内容写薄。".to_string(),
        });
    }
    if !analysis.ending.warn && !analysis.ending.tail_excerpt.is_empty() {
        candidates.push(BonusCandidate {
            name: "章末收束未模板化".to_string(),
            reason: "章末没有落入现有意象/流程词模板，说明结尾功能有继续发展的空间。".to_string(),
        });
    }
    if !analysis.aa_bb_patterns.is_empty() && !analysis.aa_bb_patterns.iter().any(|p| p.warn) {
        candidates.push(BonusCandidate {
            name: "局部排比可视作风格点缀".to_string(),
            reason: "检测到少量 AA/BB 或短分句节奏，但还没形成模板疲劳，可以先按风格候选保留。"
                .to_string(),
        });
    }
    let scene_map = &analysis.scene_map;
    if !scene_map.warn && scene_map.switch_count >= 2 {
        candidates.push(BonusCandidate {
            name: "场面功能有切换".to_string(),
            reason: "粗分块没有被单一对白或说明吃满，说明这一章至少在尝试做功能接力。".to_string(),
        });
    }
    let dialogue_emotions = &analysis.dialogue_emotions;
    if dialogue_emotions.dialogue_sentences >= 4
        && !dialogue_emotions.flatness_warn
        && !dialogue_emotions.volatility_warn
        && dialogue_emotions.shift_count >= 1
    {
        candidates.push(BonusCandidate {
            name: "对白情绪有起伏".to_string(),
            reason: "对白情绪不是单一平推，也没有明显横跳，更像在做关系推进而不是纯互顶。"
                .to_string(),
        });
    }
    let battle_profile = &analysis.battle_profile;
    if battle_profile.sequence_count >= 1 && battle_profile.result_ratio >= 0.35 {
        candidates.push(BonusCandidate {
            name: "动作段有后果反馈".to_string(),
            reason: "冲突段不只累计动作动词，也带出了结果、伤害或位移反馈，可以视作紧凑度候选。"
                .to_string(),
        });
    }
    candidates.truncate(4);
    candidates
}

/// `build_consistency_bonus_candidates`（一致性侧，截前 2）。
fn build_consistency_bonus_candidates(
    snapshot: Option<&StoryConflictSnapshot>,
) -> Vec<BonusCandidate> {
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };
    if !snapshot.available {
        return Vec::new();
    }
    let mut candidates: Vec<BonusCandidate> = Vec::new();
    if snapshot.decision_counter.get("designed_keep") >= 1 {
        candidates.push(BonusCandidate {
            name: "一致性例外已沉淀".to_string(),
            reason: "同一 story 已有候选被人工判为设计性保留，说明工具开始学会区分“可保留的变化”和“真漂移”。".to_string(),
        });
    }
    if snapshot.decision_counter.get("false_positive") >= 1 && snapshot.pending_rows.is_empty() {
        candidates.push(BonusCandidate {
            name: "一致性复核收敛".to_string(),
            reason: "这一条 story 的一致性候选已有复核反馈，且当前没有遗留待判项，说明复审闭环在起作用。".to_string(),
        });
    }
    candidates.truncate(2);
    candidates
}

/// 评审轴（`{name, score, reason}`）。
#[derive(Debug, Clone)]
pub struct Axis {
    pub name: String,
    pub score: i32,
    pub reason: String,
}

/// `build_axes`：8 条固定轴（名称/说明固定）。
pub fn build_axes(
    analysis: &Analysis,
    consistency_snapshot: Option<&StoryConflictSnapshot>,
    alignment_snapshot: Option<&alignment::Alignment>,
    story_trend_snapshot: Option<&TrendSnapshot>,
) -> Vec<Axis> {
    let summary = &analysis.summary;
    let dialogue = &analysis.dialogue;
    let fatigue_warn_count = analysis
        .style_fatigue
        .iter()
        .filter(|item| item.status == "WARN")
        .count();
    let reminder_p1_count = analysis
        .review_reminders
        .iter()
        .filter(|item| item.priority == "P1")
        .count();
    let reminder_p2_count = analysis
        .review_reminders
        .iter()
        .filter(|item| item.priority == "P2")
        .count();
    let dialogue_gap_count = dialogue.dialogue_axis_gaps.len();
    let ping_pong_count = dialogue.quote_ping_pong.len() + dialogue.question_ping_pong.len();
    let scene_map = &analysis.scene_map;
    let dialogue_emotions = &analysis.dialogue_emotions;
    let character_voice = &analysis.character_voice;
    let tone_profile = &analysis.tone_profile;
    let battle_profile = &analysis.battle_profile;
    let viewpoint_profile = &analysis.viewpoint_profile;
    let mut consistency_pending = 0usize;
    let mut consistency_confirmed = 0usize;
    let mut consistency_false_positive = 0usize;
    let mut alignment_mismatch_count = 0usize;
    if let Some(consistency_snapshot) = consistency_snapshot {
        if consistency_snapshot.available {
            consistency_pending = consistency_snapshot.pending_rows.len();
            consistency_confirmed = consistency_snapshot.decision_counter.get("confirmed");
            consistency_false_positive =
                consistency_snapshot.decision_counter.get("false_positive");
        }
    }
    if let Some(alignment_snapshot) = alignment_snapshot {
        if alignment_snapshot.available {
            alignment_mismatch_count = alignment_snapshot.mismatch_count.unwrap_or(0);
        }
    }
    let convergence_kinds: Vec<String> = story_trend_snapshot
        .map(|t| t.convergence_kinds.clone())
        .unwrap_or_default();

    let mut axes: Vec<Axis> = Vec::new();

    let repetition_score = axis_score(
        5,
        &[
            ((analysis.hard_flags.len() / 6).min(3)) as i32,
            fatigue_warn_count.min(2) as i32,
            i32::from(analysis.tracked_term_window_count >= 3),
            i32::from(analysis.sentence_patterns.len() >= 4),
        ],
        &[i32::from(summary.warn_sections <= 4)],
    );
    axes.push(Axis {
        name: "重复控制".to_string(),
        score: repetition_score,
        reason: "综合硬警告、句式疲劳、局部点名密度与句首骨架重复。".to_string(),
    });

    let sentence_score = axis_score(
        5,
        &[
            i32::from(analysis.sentence_lengths.warn),
            i32::from(analysis.clause_prefixes.len() >= 4),
            i32::from(analysis.parallel_clauses.len() >= 3),
            i32::from(analysis.aa_bb_patterns.iter().any(|p| p.warn)),
        ],
        &[i32::from(
            (0.18..=0.35).contains(&summary.short_sentence_ratio),
        )],
    );
    axes.push(Axis {
        name: "句式弹性".to_string(),
        score: sentence_score,
        reason: "观察短句、并列分句、AA/BB 节奏和分句前缀，判断是节奏还是手癖。".to_string(),
    });

    let dialogue_score = axis_score(
        5,
        &[
            dialogue_gap_count.min(2) as i32,
            i32::from(ping_pong_count >= 2),
            i32::from(dialogue.dense_quote_run_count >= 2),
            i32::from(summary.quote_ratio > 0.55),
            i32::from(dialogue_emotions.flatness_warn),
            i32::from(dialogue_emotions.volatility_warn),
            i32::from(character_voice.warn),
            i32::from(convergence_kinds.iter().any(|k| k == "ending_emotion")),
        ],
        &[
            i32::from((0.12..=0.45).contains(&summary.quote_ratio) && dialogue_gap_count == 0),
            i32::from(dialogue_emotions.shift_count >= 1 && !dialogue_emotions.volatility_warn),
            i32::from(!character_voice.warn && character_voice.speaker_count >= 2),
        ],
    );
    axes.push(Axis {
        name: "对白情感与转轴".to_string(),
        score: dialogue_score,
        reason: "看对白是否有动作、环境、第三方或设备转轴，而不是长时间互顶。".to_string(),
    });

    let tone_score = axis_score(
        4,
        &[
            i32::from(analysis.ending.warn),
            i32::from(analysis.modifier_pressure.iter().any(|m| m.warn)),
            i32::from(analysis.fatigue_windows.len() >= 4),
            i32::from(tone_profile.warn),
            i32::from(scene_map.warn),
            i32::from(convergence_kinds.iter().any(|k| k == "ending_tone")),
        ],
        &[
            i32::from(!analysis.ending.warn),
            i32::from(tone_profile.stable_ratio >= 0.35 && tone_profile.dominant_tone != "none"),
        ],
    );
    axes.push(Axis {
        name: "场景色调稳定".to_string(),
        score: tone_score,
        reason: "暂时用章末模板、修饰压力和局部疲劳窗口做代理指标，后续再接更细的色调分类。"
            .to_string(),
    });

    let tension_score = axis_score(
        4,
        &[
            i32::from(summary.short_sentence_ratio > 0.42),
            i32::from(ping_pong_count >= 2),
            i32::from(dialogue_gap_count >= 2),
            i32::from(battle_profile.warn),
        ],
        &[
            i32::from(summary.avg_sentence_chars >= 14.0 && summary.avg_sentence_chars <= 28.0),
            i32::from(battle_profile.sequence_count >= 1 && battle_profile.result_ratio >= 0.35),
        ],
    );
    axes.push(Axis {
        name: "张力与紧凑度".to_string(),
        score: tension_score,
        reason: "用句长、对白互顶和转轴缺口粗看战斗/冲突段是否只是快而不紧。".to_string(),
    });

    let viewpoint_score = axis_score(
        4,
        &[
            i32::from(analysis.judgement_contexts.len() >= 3),
            i32::from(analysis.learned_filters.len() >= 4),
            i32::from(reminder_p1_count >= 3),
            i32::from(viewpoint_profile.warn),
        ],
        &[
            i32::from(reminder_p1_count == 0),
            i32::from(!viewpoint_profile.warn && !viewpoint_profile.dominant_anchor.is_empty()),
        ],
    );
    axes.push(Axis {
        name: "视角与判断稳定".to_string(),
        score: viewpoint_score,
        reason: "当前主要用判断句上下文、语料偏移和高优先提醒做代理，先拦旁白抢跑与说明过重。"
            .to_string(),
    });

    let consistency_score = axis_score(
        4,
        &[
            i32::from(analysis.tracked_term_window_count >= 4),
            i32::from(analysis.learned_filters.len() >= 5),
            i32::from(summary.warn_sections >= 12),
            i32::from(consistency_pending >= 1),
            i32::from(consistency_confirmed >= 2),
            i32::from(alignment_mismatch_count >= 2),
        ],
        &[
            i32::from(analysis.corpus_profile.enabled),
            i32::from(consistency_false_positive >= 1 && consistency_pending == 0),
            i32::from(
                alignment_mismatch_count == 0
                    && matches!(
                        alignment_snapshot,
                        Some(alignment) if alignment.available
                    ),
            ),
        ],
    );
    axes.push(Axis {
        name: "一致性准备度".to_string(),
        score: consistency_score,
        reason: "看当前章与同书语料的偏离程度，以及当前 story 的一致性候选是否已被复核、确认或仍待处理。".to_string(),
    });

    let structure_score = axis_score(
        4,
        &[
            i32::from(reminder_p1_count >= 2),
            i32::from(analysis.fatigue_windows.len() >= 5),
            i32::from(summary.warn_sections >= 14),
            i32::from(scene_map.warn),
            i32::from(alignment_mismatch_count >= 1),
        ],
        &[
            i32::from(reminder_p1_count == 0 && reminder_p2_count <= 2),
            i32::from(scene_map.switch_count >= 2 && !scene_map.warn),
            i32::from(
                alignment_mismatch_count == 0
                    && matches!(
                        alignment_snapshot,
                        Some(alignment) if alignment.available
                    ),
            ),
        ],
    );
    axes.push(Axis {
        name: "结构完成度".to_string(),
        score: structure_score,
        reason: "暂用高优先提醒、局部高压窗口和总体告警量做代理，后续再接 Scene/章末功能分析。"
            .to_string(),
    });
    axes
}

/// `decide_gate`：门禁（gate / priority / recommendation）。
pub fn decide_gate(analysis: &Analysis, axes: &[Axis]) -> (String, String, String) {
    let p1_count = analysis
        .review_reminders
        .iter()
        .filter(|item| item.priority == "P1")
        .count();
    let avg_score =
        axes.iter().map(|axis| axis.score as f64).sum::<f64>() / axes.len().max(1) as f64;
    let hard_count = analysis.hard_flags.len();
    let warn_sections = analysis.summary.warn_sections;

    if p1_count >= 4 || warn_sections >= 16 || avg_score < 2.4 || hard_count >= 22 {
        return (
            "FAIL".to_string(),
            "P0".to_string(),
            "targeted_rewrite".to_string(),
        );
    }
    if p1_count >= 2 || warn_sections >= 10 || avg_score < 3.4 || hard_count >= 12 {
        return (
            "WATCH".to_string(),
            "P1".to_string(),
            "light_revise".to_string(),
        );
    }
    ("PASS".to_string(), "P2".to_string(), "retain".to_string())
}

/// 同值连续段（`collect_repeated_value_runs` 行）。
#[derive(Debug, Clone)]
struct RepeatedRun {
    value: String,
    paths: Vec<PathBuf>,
}

/// `collect_repeated_value_runs`：按 `chapter_sort_key` 排序后收 ≥ min_run 的同值段。
fn collect_repeated_value_runs(rows: &[(PathBuf, &str)], min_run: usize) -> Vec<RepeatedRun> {
    let mut ordered: Vec<(PathBuf, &str)> = rows.to_vec();
    ordered.sort_by_key(|a| chapter_sort_key(&a.0));
    let mut runs: Vec<RepeatedRun> = Vec::new();
    let mut current_value: Option<&str> = None;
    let mut current_paths: Vec<PathBuf> = Vec::new();
    let flush = |runs: &mut Vec<RepeatedRun>,
                 current_value: &mut Option<&str>,
                 current_paths: &mut Vec<PathBuf>| {
        if let Some(value) = *current_value {
            if current_paths.len() >= min_run {
                runs.push(RepeatedRun {
                    value: value.to_string(),
                    paths: current_paths.clone(),
                });
            }
        }
        *current_value = None;
        current_paths.clear();
    };
    for (draft_path, value) in &ordered {
        let value = *value;
        if Some(value) == current_value {
            current_paths.push(draft_path.clone());
            continue;
        }
        flush(&mut runs, &mut current_value, &mut current_paths);
        current_value = Some(value);
        current_paths.push(draft_path.clone());
    }
    flush(&mut runs, &mut current_value, &mut current_paths);
    runs
}

/// story 内跨章合流快照（`convergence_kinds` + `notes`）。
#[derive(Debug, Clone, Default)]
pub struct TrendSnapshot {
    pub convergence_kinds: Vec<String>,
    pub notes: Vec<String>,
}

/// `build_story_trend_snapshots`：章末标签连续段 × 色调/情绪同值段重叠。
#[must_use]
pub fn build_story_trend_snapshots(
    analyses: &[(PathBuf, &Analysis)],
    labels: &EndingLabels,
) -> Vec<(PathBuf, TrendSnapshot)> {
    let mut ordered: Vec<(PathBuf, &Analysis)> = analyses.to_vec();
    ordered.sort_by_key(|a| chapter_sort_key(&a.0));

    let mut ending_runs: Vec<(String, Vec<PathBuf>)> = Vec::new();
    let mut current_label: Option<String> = None;
    let mut current_paths: Vec<PathBuf> = Vec::new();
    for (draft_path, analysis) in &ordered {
        let label = infer_ending_label(analysis, labels);
        if Some(label.as_str()) == current_label.as_deref() {
            current_paths.push(draft_path.clone());
            continue;
        }
        if let Some(label) = current_label.take() {
            if current_paths.len() >= 2 {
                ending_runs.push((label, current_paths.clone()));
            }
        }
        current_label = Some(label);
        current_paths.clear();
        current_paths.push(draft_path.clone());
    }
    if let Some(label) = current_label {
        if current_paths.len() >= 2 {
            ending_runs.push((label, current_paths));
        }
    }

    let tone_runs = collect_repeated_value_runs(
        &ordered
            .iter()
            .map(|(path, analysis)| {
                let dominant = &analysis.tone_profile.dominant_tone;
                (
                    path.clone(),
                    if dominant.is_empty() {
                        "none"
                    } else {
                        dominant
                    },
                )
            })
            .collect::<Vec<_>>(),
        2,
    );
    let emotion_runs = collect_repeated_value_runs(
        &ordered
            .iter()
            .map(|(path, analysis)| {
                let dominant = &analysis.dialogue_emotions.dominant_emotion;
                (
                    path.clone(),
                    if dominant.is_empty() {
                        "neutral"
                    } else {
                        dominant
                    },
                )
            })
            .collect::<Vec<_>>(),
        2,
    );

    fn snapshot_entry<'a>(
        snapshots: &'a mut Vec<(PathBuf, TrendSnapshot)>,
        path: &PathBuf,
    ) -> &'a mut TrendSnapshot {
        if let Some(i) = snapshots.iter().position(|(p, _)| p == path) {
            &mut snapshots[i].1
        } else {
            snapshots.push((path.clone(), TrendSnapshot::default()));
            &mut snapshots.last_mut().expect("刚插入").1
        }
    }
    let mut snapshots: Vec<(PathBuf, TrendSnapshot)> = Vec::new();
    for (ending_label, ending_paths) in &ending_runs {
        let ending_set: HashSet<&PathBuf> = ending_paths.iter().collect();
        let ending_display_name = ending_display(labels, ending_label);
        for tone_run in &tone_runs {
            if tone_run.value == "none" {
                continue;
            }
            let overlap: Vec<&PathBuf> = tone_run
                .paths
                .iter()
                .filter(|p| ending_set.contains(p))
                .collect();
            if overlap.len() < 2 {
                continue;
            }
            for path in overlap {
                let snapshot = snapshot_entry(&mut snapshots, path);
                snapshot.convergence_kinds.push("ending_tone".to_string());
                snapshot
                    .notes
                    .push(format!("{ending_display_name}+tone:{}", tone_run.value));
            }
        }
        for emotion_run in &emotion_runs {
            if emotion_run.value == "neutral" {
                continue;
            }
            let overlap: Vec<&PathBuf> = emotion_run
                .paths
                .iter()
                .filter(|p| ending_set.contains(p))
                .collect();
            if overlap.len() < 2 {
                continue;
            }
            for path in overlap {
                let snapshot = snapshot_entry(&mut snapshots, path);
                snapshot
                    .convergence_kinds
                    .push("ending_emotion".to_string());
                snapshot.notes.push(format!(
                    "{ending_display_name}+emotion:{}",
                    emotion_run.value
                ));
            }
        }
    }
    snapshots
}

/// `recommendation_note`（5 条固定文案）。
fn recommendation_note(recommendation: &str) -> &str {
    match recommendation {
        "retain" => "当前章可保留主体结构，优先微调个别硬项，不要为了清零统计把文气磨平。",
        "light_revise" => {
            "优先处理报告里的硬警告和高优先提醒，重点改局部句法和对白转轴，不必整章推倒。"
        }
        "targeted_rewrite" => {
            "这一章已出现多轴失衡，先按窗口和提醒重写重点段，再回查上游施工图是否诱发了这些问题。"
        }
        "structural_rework" => "需要回退到章节结构或 Story 负载层重做。",
        "rollback_to_plan" => "当前章问题主要来自上游规划，应先修大纲再修正文。",
        other => panic!("未知 recommendation: {other}"),
    }
}

/// `build_scorecard_report`：单章记分卡 markdown（逐字渲染）。
pub fn build_scorecard_report(
    engine: &PlanEngine,
    labels: &EndingLabels,
    draft_path: &Path,
    analysis: &Analysis,
    consistency_snapshot: Option<&StoryConflictSnapshot>,
    story_trend_snapshot: Option<&TrendSnapshot>,
) -> Result<String> {
    let novel_dir = novel_dir_for_draft(draft_path);
    let alignment = match &novel_dir {
        Some(dir) => {
            alignment::build_plan_draft_alignment(engine, draft_path, Some(dir), analysis)?
        }
        None => alignment::Alignment::novel_dir_not_resolved(None),
    };
    let axes = build_axes(
        analysis,
        consistency_snapshot,
        Some(&alignment),
        story_trend_snapshot,
    );
    let (mut gate, mut priority, mut recommendation) = decide_gate(analysis, &axes);
    let convergence_kinds = story_trend_snapshot
        .map(|t| t.convergence_kinds.clone())
        .unwrap_or_default();
    if gate == "PASS" && !convergence_kinds.is_empty() {
        (gate, priority, recommendation) = ("WATCH".into(), "P1".into(), "light_revise".into());
    } else if gate == "WATCH" && convergence_kinds.len() >= 2 && analysis.summary.warn_sections >= 8
    {
        (gate, priority, recommendation) = ("FAIL".into(), "P0".into(), "targeted_rewrite".into());
    }
    let mut bonus_candidates = build_bonus_candidates(analysis);
    bonus_candidates.extend(build_consistency_bonus_candidates(consistency_snapshot));
    let p1_items: Vec<&ReviewReminder> = analysis
        .review_reminders
        .iter()
        .filter(|item| item.priority == "P1")
        .take(6)
        .collect();
    let hard_flags_top: Vec<&HardFlag> = analysis.hard_flags.iter().take(10).collect();
    let p1_count = analysis
        .review_reminders
        .iter()
        .filter(|item| item.priority == "P1")
        .count();

    let stem = draft_path
        .file_stem()
        .and_then(|s| s.to_str())
        .with_context(|| format!("章节文件名必须为合法 UTF-8: {}", draft_path.display()))?;
    let scene_map = &analysis.scene_map;
    let dialogue_emotions = &analysis.dialogue_emotions;
    let tone_profile = &analysis.tone_profile;
    let battle_profile = &analysis.battle_profile;
    let viewpoint_profile = &analysis.viewpoint_profile;
    let character_voice = &analysis.character_voice;
    let bool_str = |flag: bool| if flag { "True" } else { "False" };

    let mut lines: Vec<String> = vec![format!("# {stem} Review Scorecard"), String::new()];
    lines.push(format!("- source: `{}`", draft_path.display()));
    lines.push(format!("- gate: `{gate}`"));
    lines.push(format!("- priority: `{priority}`"));
    lines.push(format!("- recommendation: `{recommendation}`"));
    lines.push(format!("- note: {}", recommendation_note(&recommendation)));
    lines.push(String::new());

    lines.push("## Axis Scores".to_string());
    lines.push("| 维度 | 分数 | 说明 |".to_string());
    lines.push("|---|---:|---|".to_string());
    for axis in &axes {
        lines.push(format!(
            "| {} | `{}` | {} |",
            axis.name, axis.score, axis.reason
        ));
    }
    lines.push(String::new());

    lines.push("## Hard Gates".to_string());
    lines.push(format!(
        "- warn_sections=`{}`",
        analysis.summary.warn_sections
    ));
    lines.push(format!("- hard_flags=`{}`", analysis.hard_flags.len()));
    lines.push(format!("- P1 reminders=`{p1_count}`"));
    lines.push(format!(
        "- fatigue_windows=`{}`",
        analysis.fatigue_window_count
    ));
    lines.push(format!(
        "- tracked_term_windows=`{}`",
        analysis.tracked_term_window_count
    ));
    lines.push(String::new());

    lines.push("## Bonus Candidates".to_string());
    if !bonus_candidates.is_empty() {
        for item in &bonus_candidates {
            lines.push(format!("- `{}`：{}", item.name, item.reason));
        }
    } else {
        lines.push(
            "- 暂无明显可直接保留的设计性重复候选；这不代表没有亮点，只代表脚本暂未捕捉到。"
                .to_string(),
        );
    }
    lines.push(String::new());

    lines.push("## Narrative Signals".to_string());
    lines.push(format!(
        "- scene_blocks=`{}` dominant_role=`{}` dominance_ratio=`{}` switches=`{}`",
        scene_map.block_count,
        scene_map.dominant_role,
        float_repr(scene_map.dominance_ratio),
        scene_map.switch_count
    ));
    lines.push(format!(
        "- dialogue_emotion=`{}` ratio=`{}` shifts=`{}`",
        dialogue_emotions.dominant_emotion,
        float_repr(dialogue_emotions.dominant_ratio),
        dialogue_emotions.shift_count
    ));
    lines.push(format!(
        "- character_voice=`{}` speakers=`{}` coverage=`{}` warn=`{}`",
        if character_voice.dominant_speaker.is_empty() {
            "none"
        } else {
            character_voice.dominant_speaker.as_str()
        },
        character_voice.speaker_count,
        float_repr(character_voice.coverage_ratio),
        bool_str(character_voice.warn)
    ));
    lines.push(format!(
        "- tone=`{}` stable_ratio=`{}` tone_switches=`{}`",
        tone_profile.dominant_tone,
        float_repr(tone_profile.stable_ratio),
        tone_profile.switch_count
    ));
    lines.push(format!(
        "- battle_sequences=`{}` result_ratio=`{}`",
        battle_profile.sequence_count,
        float_repr(battle_profile.result_ratio)
    ));
    lines.push(format!(
        "- viewpoint_anchor=`{}` switches=`{}` overlaps=`{}`",
        if viewpoint_profile.dominant_anchor.is_empty() {
            "none"
        } else {
            viewpoint_profile.dominant_anchor.as_str()
        },
        viewpoint_profile.switch_count,
        viewpoint_profile.overlap_count
    ));
    lines.push(format!(
        "- ending_signal=`{}`",
        ending_display(labels, &infer_ending_label(analysis, labels))
    ));
    if !convergence_kinds.is_empty() {
        lines.push(format!(
            "- trend_convergence: {}",
            convergence_kinds
                .iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        if let Some(snapshot) = story_trend_snapshot {
            for note in snapshot.notes.iter().take(3) {
                lines.push(format!("  - `{note}`"));
            }
        }
    }
    lines.push(String::new());

    lines.push("## Consistency Snapshot".to_string());
    match consistency_snapshot {
        Some(snapshot) if snapshot.available => {
            lines.push(format!(
                "- story: `{}`",
                snapshot.story.as_deref().unwrap_or("")
            ));
            lines.push(format!(
                "- feedback_log: `{}`",
                snapshot
                    .feedback_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ));
            lines.push(format!(
                "- confirmed=`{}`",
                snapshot.decision_counter.get("confirmed")
            ));
            lines.push(format!(
                "- false_positive=`{}`",
                snapshot.decision_counter.get("false_positive")
            ));
            lines.push(format!(
                "- designed_keep=`{}`",
                snapshot.decision_counter.get("designed_keep")
            ));
            lines.push(format!(
                "- watch=`{}`",
                snapshot.decision_counter.get("watch")
            ));
            lines.push(format!("- pending=`{}`", snapshot.pending_count));
            lines.push(format!(
                "- review_queue: `{}`",
                snapshot.review_queue_command.as_deref().unwrap_or("")
            ));
            lines.push(format!(
                "- feedback_summary: `{}`",
                snapshot.feedback_summary_command.as_deref().unwrap_or("")
            ));
            if !snapshot.facet_counter.is_empty() {
                lines.push("- facets:".to_string());
                for (name, count) in snapshot.facet_counter.most_common(6) {
                    lines.push(format!("  - `{name}` x{count}"));
                }
            }
            if !snapshot.pending_actions.is_empty() {
                lines.push("- pending actions:".to_string());
                for item in snapshot.pending_actions.iter().take(2) {
                    lines.push(format!(
                        "  - `{}` `{}` confidence=`{}`：{}",
                        item.category, item.title, item.confidence, item.focus
                    ));
                    lines.push(format!("  - command: `{}`", item.command));
                }
            }
        }
        _ => {
            lines.push("- 无一致性反馈快照".to_string());
        }
    }
    lines.push(String::new());

    lines.push("## Plan Alignment Snapshot".to_string());
    if alignment.available {
        lines.push(format!(
            "- chapter_plan: `{}`",
            alignment
                .chapter_plan_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        ));
        lines.push(format!(
            "- chapter_function: plan=`{}` draft=`{}` match=`{}`",
            alignment.plan_chapter_function.as_deref().unwrap_or(""),
            alignment.draft_chapter_function.as_deref().unwrap_or(""),
            bool_str(alignment.chapter_match.unwrap_or(false))
        ));
        lines.push(format!(
            "- ending_function: plan=`{}` draft=`{}` match=`{}`",
            alignment.plan_ending_function.as_deref().unwrap_or(""),
            alignment.draft_ending_function.as_deref().unwrap_or(""),
            bool_str(alignment.ending_match.unwrap_or(false))
        ));
        lines.push(format!(
            "- mismatch_count=`{}`",
            alignment.mismatch_count.unwrap_or(0)
        ));
        lines.push(format!(
            "- alignment_status=`{}`",
            alignment.alignment_status.as_deref().unwrap_or("unknown")
        ));
        lines.push(format!(
            "- recommended_action=`{}`",
            alignment
                .recommended_action
                .as_deref()
                .unwrap_or("manual_review")
        ));
        if let Some(drift_types) = &alignment.drift_types {
            if !drift_types.is_empty() {
                lines.push(format!(
                    "- drift_types: {}",
                    drift_types
                        .iter()
                        .map(|name| format!("`{name}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        if let Some(review_note) = &alignment.review_note {
            if !review_note.is_empty() {
                lines.push(format!("- note: {review_note}"));
            }
        }
    } else {
        lines.push("- 无 plan-draft 对齐快照".to_string());
    }
    lines.push(String::new());

    lines.push("## Priority Fixes".to_string());
    if !p1_items.is_empty() {
        for item in &p1_items {
            lines.push(format!(
                "- `{}` {}：{} 检查：{} 动作：{}",
                item.category, item.title, item.reason, item.check, item.action
            ));
        }
    } else {
        lines.push("- 无 P1 项".to_string());
    }
    lines.push(String::new());

    lines.push("## Top Hard Flags".to_string());
    if !hard_flags_top.is_empty() {
        for item in &hard_flags_top {
            let mut line = format!(
                "- `{}` `{}` x{}：{}",
                item.section, item.name, item.count, item.note
            );
            if !item.sample.is_empty() {
                line.push_str(&format!(" 样例：{}", item.sample));
            }
            lines.push(line);
        }
    } else {
        lines.push("- 无".to_string());
    }
    lines.push(String::new());

    lines.push("## Follow-up".to_string());
    lines.push(
        "- 如果是 `WATCH` 或 `FAIL`，先读对应 `profiles/` 目录里的句式画像，再决定是删词、拆句还是重写段落。".to_string(),
    );
    lines.push(
        "- 如果同一角色、地点、装备或称谓在这章显得摇摆，先跑本章快照里的 `review_queue`，再决定是否补 `feedback-add`。".to_string(),
    );
    lines.push(
        "- 如果 `recommendation` 已接近 `targeted_rewrite`，先回查 `chapter-plan` 和 `story-plan`，不要只在正文层补丁。".to_string(),
    );
    if !convergence_kinds.is_empty() {
        lines.push(
            "- 如果本章处在跨章合流里，优先确认它是不是在重复同一种章末温度，而不是只修单章字词。"
                .to_string(),
        );
    }
    lines.push(String::new());

    Ok(lines.join("\n") + "\n")
}

/// `build_story_summary`：story 级 SUMMARY.md（逐字渲染）。
pub fn build_story_summary(
    story_dir: &Path,
    scorecards: &[(PathBuf, &Analysis)],
    snapshots: &[(PathBuf, StoryConflictSnapshot)],
    labels: &EndingLabels,
    engine: &PlanEngine,
) -> Result<String> {
    let mut ordered: Vec<(PathBuf, &Analysis)> = scorecards.to_vec();
    ordered.sort_by_key(|a| chapter_sort_key(&a.0));
    let story_trend_snapshots = build_story_trend_snapshots(&ordered, labels);
    let mut gate_counter = Counter::default();
    let mut recommendation_counter = Counter::default();
    let mut axis_totals = Counter::default();
    let mut feedback_counter = Counter::default();
    let mut feedback_facets = Counter::default();
    let mut alignment_counter = Counter::default();
    let mut ending_alignment_counter = Counter::default();
    let mut ending_signal_counter = Counter::default();
    let mut ending_signal_flow: Vec<String> = Vec::new();
    let mut story_backlog: Vec<BacklogItem> = Vec::new();

    let mut lines: Vec<String> = vec!["# Review Scorecard Summary".to_string(), String::new()];
    lines.push(format!("- story: `{}`", story_dir.display()));
    lines.push(format!("- chapters: `{}`", ordered.len()));
    lines.push(String::new());

    lines.push("## Chapters".to_string());
    for (path, analysis) in &ordered {
        let snapshot = snapshots.iter().find(|(p, _)| p == path).map(|(_, s)| s);
        let novel_dir = novel_dir_for_draft(path);
        let alignment =
            alignment::build_plan_draft_alignment(engine, path, novel_dir.as_deref(), analysis)?;
        let trend_snapshot = story_trend_snapshots
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, t)| t);
        let axes = build_axes(analysis, snapshot, Some(&alignment), trend_snapshot);
        let (mut gate, mut priority, mut recommendation) = decide_gate(analysis, &axes);
        let convergence_kinds = trend_snapshot
            .map(|t| t.convergence_kinds.clone())
            .unwrap_or_default();
        if gate == "PASS" && !convergence_kinds.is_empty() {
            (gate, priority, recommendation) = ("WATCH".into(), "P1".into(), "light_revise".into());
        } else if gate == "WATCH"
            && convergence_kinds.len() >= 2
            && analysis.summary.warn_sections >= 8
        {
            (gate, priority, recommendation) =
                ("FAIL".into(), "P0".into(), "targeted_rewrite".into());
        }
        gate_counter.add(&gate);
        recommendation_counter.add(&recommendation);
        for axis in &axes {
            axis_totals.add_n(&axis.name, axis.score as usize);
        }
        if let Some(snapshot) = snapshot {
            if snapshot.available {
                for (decision, count) in snapshot.decision_counter.items() {
                    feedback_counter.add_n(&decision, count);
                }
                for (facet, count) in snapshot.facet_counter.items() {
                    feedback_facets.add_n(&facet, count);
                }
                if story_backlog.is_empty() {
                    story_backlog = snapshot.global_feedback_backlog.clone();
                }
            }
        }
        if alignment.available {
            alignment_counter.add_n(
                &format!(
                    "{}->{}",
                    alignment.plan_chapter_function.as_deref().unwrap_or(""),
                    alignment.draft_chapter_function.as_deref().unwrap_or("")
                ),
                1,
            );
            ending_alignment_counter.add_n(
                &format!(
                    "{}->{}",
                    alignment.plan_ending_function.as_deref().unwrap_or(""),
                    alignment.draft_ending_function.as_deref().unwrap_or("")
                ),
                1,
            );
        }
        let ending_signal = infer_ending_label(analysis, labels);
        ending_signal_counter.add(&ending_signal);
        ending_signal_flow.push(ending_signal.clone());
        lines.push(format!(
            "- `{}` gate=`{gate}` priority=`{priority}` recommendation=`{recommendation}` warn_sections=`{}` hard_flags=`{}` ending_signal=`{}`",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(""),
            analysis.summary.warn_sections,
            analysis.hard_flags.len(),
            ending_display(labels, &ending_signal)
        ));
        if let Some(trend_snapshot) = trend_snapshot {
            if !trend_snapshot.notes.is_empty() {
                lines.push(format!(
                    "  - convergence=`{}`",
                    trend_snapshot
                        .notes
                        .iter()
                        .take(2)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" | ")
                ));
            }
        }
    }
    lines.push(String::new());

    lines.push("## Gate Distribution".to_string());
    for (name, count) in gate_counter.entries() {
        lines.push(format!("- `{name}` x{count}"));
    }
    lines.push(String::new());

    lines.push("## Recommendation Distribution".to_string());
    for (name, count) in recommendation_counter.entries() {
        lines.push(format!("- `{name}` x{count}"));
    }
    lines.push(String::new());

    lines.push("## Average Axis Scores".to_string());
    for (name, total) in axis_totals.entries() {
        let avg = round2(*total as f64 / ordered.len().max(1) as f64);
        lines.push(format!("- `{name}` avg=`{}`", float_repr(avg)));
    }
    lines.push(String::new());

    lines.push("## Plan-Draft Alignment".to_string());
    if !alignment_counter.is_empty() {
        for (name, count) in alignment_counter.most_common(8) {
            lines.push(format!("- `chapter {name}` x{count}"));
        }
        for (name, count) in ending_alignment_counter.most_common(8) {
            lines.push(format!("- `ending {name}` x{count}"));
        }
    } else {
        lines.push("- 无".to_string());
    }
    lines.push(String::new());

    lines.push("## Ending Trend Signals".to_string());
    if !ending_signal_counter.is_empty() {
        for (name, count) in ending_signal_counter.most_common(8) {
            lines.push(format!("- `{}` x{count}", ending_display(labels, &name)));
        }
        lines.push(format!(
            "- flow=`{}`",
            ending_flow_text(&ending_signal_flow, 8, labels)
        ));
        let runs = summarize_runs(&ending_signal_flow, 2, 4, labels);
        let repeated = if runs.is_empty() {
            "无".to_string()
        } else {
            runs.join(" | ")
        };
        lines.push(format!("- repeated=`{repeated}`"));
    } else {
        lines.push("- 无".to_string());
    }
    lines.push(String::new());

    lines.push("## Consistency Feedback".to_string());
    if !feedback_counter.is_empty() {
        for decision in ["confirmed", "false_positive", "designed_keep", "watch"] {
            lines.push(format!(
                "- `{decision}` x{}",
                feedback_counter.get(decision)
            ));
        }
        if !feedback_facets.is_empty() {
            lines.push("- facets:".to_string());
            for (name, count) in feedback_facets.most_common(8) {
                lines.push(format!("  - `{name}` x{count}"));
            }
        }
    } else {
        lines.push("- 无".to_string());
    }
    lines.push(String::new());

    lines.push("## Consistency Backlog".to_string());
    if !story_backlog.is_empty() {
        for item in story_backlog.iter().take(6) {
            lines.push(format!("- `{}` {}", item.target, item.reason));
        }
    } else {
        lines.push("- 无".to_string());
    }
    lines.push(String::new());

    Ok(lines.join("\n") + "\n")
}

/// 收集章节 → 分析 → 一致性快照 → 按 story 写
/// `scorecards/*.md` + `SUMMARY.md`。返回（退出码, 应打印路径序列）。
pub fn run(opts: &ScorecardOptions) -> Result<(i32, Vec<PathBuf>)> {
    let files = collect_chapter_files(&opts.paths)?;
    if files.is_empty() {
        eprintln!("No draft chapter files found.");
        return Ok((1, Vec::new()));
    }

    let rules = config::load_rules(&config::default_rules_path())?;
    let plan_engine = PlanEngine::new(&rules.plan)?;
    let ctx = DraftContext::new(rules)?;
    let template_bank = build_template_bank(ctx.draft_rules());
    let corpus_profile = build_corpus_profile(&ctx, &ctx.corpus_paths_for_targets(&files))?;
    let labels = ctx.draft_rules().ending_labels.clone();

    let mut analyses: Vec<(PathBuf, Analysis)> = Vec::new();
    for path in &files {
        let analysis = analyze_path(
            &ctx,
            path,
            &template_bank,
            ctx.draft_rules().tracked_terms.as_slice(),
            corpus_profile.as_ref(),
            opts.sample_limit,
        )
        .with_context(|| format!("无法分析章节 {}", path.display()))?;
        analyses.push((path.clone(), analysis));
    }

    let mut snapshots: Vec<(PathBuf, StoryConflictSnapshot)> = Vec::new();
    for (path, _) in &analyses {
        let snapshot = build_story_conflict_snapshot(path)?;
        snapshots.push((path.clone(), snapshot));
    }

    // 按父目录分组（首现序），再按 story 目录字典序处理。
    let mut groups: Vec<(PathBuf, Vec<usize>)> = Vec::new();
    for (index, (path, _)) in analyses.iter().enumerate() {
        let parent = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        if let Some(group) = groups.iter_mut().find(|g| g.0 == parent) {
            group.1.push(index);
        } else {
            groups.push((parent, vec![index]));
        }
    }
    groups.sort_by(|a, b| a.0.cmp(&b.0));

    let mut printed: Vec<PathBuf> = Vec::new();
    for (story_dir, indexes) in &groups {
        let items: Vec<(PathBuf, &Analysis)> = indexes
            .iter()
            .map(|&i| (analyses[i].0.clone(), &analyses[i].1))
            .collect();
        let story_trend_snapshots = build_story_trend_snapshots(&items, &labels);
        for (draft_path, analysis) in &items {
            let out_path = scorecard_path_for(draft_path)?;
            let report = build_scorecard_report(
                &plan_engine,
                &labels,
                draft_path,
                analysis,
                snapshots
                    .iter()
                    .find(|(p, _)| p == draft_path)
                    .map(|(_, s)| s),
                story_trend_snapshots
                    .iter()
                    .find(|(p, _)| p == draft_path)
                    .map(|(_, t)| t),
            )?;
            write_text(&out_path, &report)?;
            printed.push(out_path);
        }
        let summary_path = scorecard_path_for(&items[0].0)?
            .parent()
            .context("scorecard 路径无父目录")?
            .join("SUMMARY.md");
        let summary = build_story_summary(story_dir, &items, &snapshots, &labels, &plan_engine)?;
        write_text(&summary_path, &summary)?;
        printed.push(summary_path);
    }
    Ok((0, printed))
}
