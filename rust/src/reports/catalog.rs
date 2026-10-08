//! `reports/catalog.py` 移植：工作区级模板/词项候选目录（`reports-catalog` 子命令）。
//!
//! 输入为 novel 目录（读已有 `draft-stats/*/template-backlog/CANDIDATES.json`）
//! 或草稿章节文件（现跑分析并按 story 写 backlog 两件套），聚合后生成
//! `draft-stats/template-catalog/SUMMARY.md` 与 `CATALOG.json`（字节对齐）。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use glob::glob;
use serde_json::{json, Map, Value};

use crate::audit::draft::{analyze_path, build_corpus_profile, Analysis, DraftContext};
use crate::config::{self, RegexRule, TemplateRule, TrackedTerm};
use crate::input::{write_json, write_text};
use crate::reports::backlog;
use crate::rules::build_template_bank;
use crate::stats::draft::{collect_chapter_files, Ctr};

/// `reports-catalog` 子命令参数（对齐 Python `parse_args`：仅位置 `paths` nargs+）。
#[derive(Debug, Clone)]
pub struct CatalogOptions {
    /// novel 目录、draft 目录或草稿章节文件。
    pub paths: Vec<PathBuf>,
}

/// `summary_path_for(novel_dir)`：`novel_dir/draft-stats/template-catalog/SUMMARY.md`。
fn summary_path_for(novel_dir: &Path) -> PathBuf {
    novel_dir
        .join("draft-stats")
        .join("template-catalog")
        .join("SUMMARY.md")
}

/// `json_path_for(novel_dir)`：`novel_dir/draft-stats/template-catalog/CATALOG.json`。
fn json_path_for(novel_dir: &Path) -> PathBuf {
    novel_dir
        .join("draft-stats")
        .join("template-catalog")
        .join("CATALOG.json")
}

/// `novel_dir_for_story_dir(story_dir)`：最近的名为 `drafts` 的祖先之父目录。
fn novel_dir_for_story_dir(story_dir: &Path) -> Option<PathBuf> {
    for parent in story_dir.ancestors().skip(1) {
        if parent.file_name().is_some_and(|name| name == "drafts") {
            return parent.parent().map(PathBuf::from);
        }
    }
    None
}

/// 对齐 Python `build_story_payloads`：逐章分析（sample_limit=6）→ 按 story 写
/// backlog 两件套 → 收集 CANDIDATES 载荷。novel 目录无法解析时报 Err（调用方
/// 打印 `Failed to resolve novel directory from draft paths.` 并退 1）。
fn build_story_payloads(
    files: &[PathBuf],
    ctx: &DraftContext,
    template_bank: &[TemplateRule],
    term_bank: &[TrackedTerm],
) -> Result<(Option<PathBuf>, Vec<Value>)> {
    let corpus_profile = build_corpus_profile(ctx, &ctx.corpus_paths_for_targets(files))?;

    let mut novel_dir: Option<PathBuf> = None;
    let mut analyses: Vec<(PathBuf, Analysis)> = Vec::new();
    for draft_path in files {
        if novel_dir.is_none() {
            if let Some(story_dir) = draft_path.parent() {
                novel_dir = novel_dir_for_story_dir(story_dir);
            }
        }
        let analysis = analyze_path(
            ctx,
            draft_path,
            template_bank,
            term_bank,
            corpus_profile.as_ref(),
            6,
        )
        .with_context(|| format!("无法分析章节 {}", draft_path.display()))?;
        analyses.push((draft_path.clone(), analysis));
    }

    // novel_dir 无法解析时返回 None（由 resolve_payloads 打印固定文案并退 1）。

    // 按 story 目录分组（首现序），再按 story 目录字典序处理（对齐 Python sorted）。
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
    if novel_dir.is_none() {
        // 对齐 Python：`SystemExit("Failed to resolve novel directory from draft paths.")` 发生在
        // 写任何 backlog 文件之前。
        return Ok((None, Vec::new()));
    }

    let mut payloads: Vec<Value> = Vec::new();
    for (story_dir, indexes) in &groups {
        let items: Vec<(PathBuf, &Analysis)> = indexes
            .iter()
            .map(|&i| (analyses[i].0.clone(), &analyses[i].1))
            .collect();
        let (markdown, payload) = backlog::build_story_backlog(story_dir, &items)?;
        let summary = backlog::backlog_path_for(&items[0].0)?;
        let candidates = backlog::candidates_path_for(&items[0].0)?;
        write_text(&summary, &markdown)?;
        write_json(&candidates, &payload)?;
        payloads.push(payload);
    }
    Ok((novel_dir, payloads))
}

/// 对齐 Python `load_story_payloads_from_stats`：按排序读全部
/// `draft-stats/arc*/story*/template-backlog/CANDIDATES.json`（仅 dict 有效）。
fn load_story_payloads_from_stats(novel_dir: &Path) -> Vec<Value> {
    let pattern = format!(
        "{}/draft-stats/arc*/story*/template-backlog/CANDIDATES.json",
        novel_dir.display()
    );
    let mut paths: Vec<PathBuf> = glob(&pattern)
        .map(|paths| paths.filter_map(Result::ok).collect::<Vec<_>>())
        .unwrap_or_default();
    paths.sort();
    let mut payloads: Vec<Value> = Vec::new();
    for path in paths {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => continue,
        };
        let payload: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if payload.is_object() {
            payloads.push(payload);
        }
    }
    payloads
}

/// 对齐 Python `load_bank_names(key)`：`name` → 模板库名集合；`term` → 词库名集合。
fn load_bank_names(draft: &crate::config::DraftConfig, key: &str) -> HashSet<String> {
    if key == "name" {
        draft
            .template_rules
            .iter()
            .filter(|rule| !rule.name.is_empty())
            .map(|rule| rule.name.clone())
            .collect()
    } else {
        draft
            .tracked_terms
            .iter()
            .filter(|term| !term.term.is_empty())
            .map(|term| term.term.clone())
            .collect()
    }
}

/// 对齐 Python `load_hardcoded_template_names`（六组规则的 label+name 并集）。
fn load_hardcoded_template_names(draft: &crate::config::DraftConfig) -> HashSet<String> {
    let groups: Vec<Vec<&RegexRule>> = vec![
        draft.regex_rules.patterns.iter().collect(),
        draft.regex_rules.phrases.iter().collect(),
        draft.regex_rules.tokens.iter().collect(),
        draft.regex_rules.punctuation.iter().collect(),
        draft.regex_rules.punctuation_combos.iter().collect(),
        draft.regex_rules.modifiers.iter().collect(),
    ];
    let mut names: HashSet<String> = HashSet::new();
    for group in &groups {
        for rule in group {
            if let Some(label) = &rule.label {
                if !label.is_empty() {
                    names.insert(label.clone());
                }
            }
            if !rule.name.is_empty() {
                names.insert(rule.name.clone());
            }
        }
    }
    names
}

/// 对齐 Python `load_hardcoded_term_names`（三组规则的 name 并集）。
fn load_hardcoded_term_names(draft: &crate::config::DraftConfig) -> HashSet<String> {
    let groups: Vec<Vec<&RegexRule>> = vec![
        draft.regex_rules.tokens.iter().collect(),
        draft.regex_rules.phrases.iter().collect(),
        draft.regex_rules.modifiers.iter().collect(),
    ];
    let mut names: HashSet<String> = HashSet::new();
    for group in &groups {
        for rule in group {
            if !rule.name.is_empty() {
                names.insert(rule.name.clone());
            }
        }
    }
    names
}

/// 对齐 Python `bucket_family`。
fn bucket_family(bucket: &str) -> String {
    if matches!(
        bucket,
        "custom_template" | "patterns" | "phrases" | "tokens"
    ) {
        "template_family".to_string()
    } else if matches!(
        bucket,
        "punctuation" | "sentence_length" | "fatigue_window" | "ba_operation_context"
    ) {
        "narrative_signal".to_string()
    } else if matches!(bucket, "learned_filter" | "tracked_term") {
        "term_family".to_string()
    } else {
        bucket.to_string()
    }
}

/// 中间行（聚合候选；对齐 Python 行 dict 键序，`_story_set` 不序列化）。
#[derive(Debug, Default)]
struct CandidateRow {
    bucket: String,
    name: String,
    count: i64,
    story_count: i64,
    stories: Vec<String>,
    chapter_total: i64,
    sample: String,
    suggested_target: String,
    reasons: Vec<String>,
    story_set: HashSet<String>,
}

impl CandidateRow {
    fn to_value(&self) -> Value {
        json!({
            "bucket": self.bucket,
            "name": self.name,
            "count": self.count,
            "story_count": self.story_count,
            "stories": self.stories,
            "chapter_total": self.chapter_total,
            "sample": self.sample,
            "suggested_target": self.suggested_target,
            "reasons": self.reasons,
        })
    }
}

/// 对齐 Python `aggregate_candidates(payloads, field, count_key="count")`。
fn aggregate_candidates(payloads: &[Value], field: &str) -> Vec<Value> {
    let mut rows: Vec<CandidateRow> = Vec::new();
    let mut index: HashMap<(String, String), usize> = HashMap::new();

    for payload in payloads {
        let story = payload
            .get("story")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let chapters = payload.get("chapters").and_then(Value::as_i64).unwrap_or(0);
        let items = match payload.get(field).and_then(Value::as_array) {
            Some(items) => items,
            None => continue,
        };
        for item in items {
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let bucket = item
                .get("bucket")
                .and_then(Value::as_str)
                .unwrap_or(field)
                .to_string();
            let key = (bucket.clone(), name.clone());
            let row = match index.get(&key) {
                Some(&i) => &mut rows[i],
                None => {
                    let i = rows.len();
                    index.insert(key.clone(), i);
                    rows.push(CandidateRow {
                        bucket: bucket.clone(),
                        name: name.clone(),
                        count: 0,
                        story_count: 0,
                        stories: Vec::new(),
                        chapter_total: 0,
                        sample: item
                            .get("sample")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        suggested_target: item
                            .get("suggested_target")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        reasons: Vec::new(),
                        story_set: HashSet::new(),
                    });
                    &mut rows[i]
                }
            };
            row.count += item.get("count").and_then(Value::as_i64).unwrap_or(0);
            if row.story_set.insert(story.clone()) {
                row.chapter_total += chapters;
            }
            let reason = item
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if !reason.is_empty() && !row.reasons.contains(&reason) {
                row.reasons.push(reason);
            }
            if row.sample.is_empty() {
                let sample = item
                    .get("sample")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if !sample.is_empty() {
                    row.sample = sample;
                }
            }
        }
    }

    let mut rows: Vec<Value> = rows
        .into_iter()
        .map(|mut row| {
            row.story_count = row.story_set.len() as i64;
            let mut stories: Vec<String> = row.story_set.iter().cloned().collect();
            stories.sort();
            row.stories = stories;
            row.to_value()
        })
        .collect();
    sort_by_counts(&mut rows);
    rows
}

/// Python `sorted(key=lambda item: (-story_count, -count, name))` 的稳定排序
/// （无键时退回首现序）。
fn sort_by_counts(rows: &mut [Value]) {
    rows.sort_by(|a, b| {
        b.get("story_count")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .cmp(&a.get("story_count").and_then(Value::as_i64).unwrap_or(0))
            .then(
                b.get("count")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    .cmp(&a.get("count").and_then(Value::as_i64).unwrap_or(0)),
            )
            .then_with(|| {
                a.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .cmp(b.get("name").and_then(Value::as_str).unwrap_or(""))
            })
    });
}

/// 中间行（家族聚合；对齐 Python 行 dict 键序）。
#[derive(Debug, Default)]
struct FamilyRow {
    name: String,
    count: i64,
    story_count: i64,
    stories: Vec<String>,
    chapter_total: i64,
    sample: String,
    suggested_target: String,
    buckets: Vec<String>,
    bucket_families: Vec<String>,
    reasons: Vec<String>,
    story_set: HashSet<String>,
    bucket_set: HashSet<String>,
    family_set: HashSet<String>,
}

impl FamilyRow {
    fn to_value(&self) -> Value {
        json!({
            "name": self.name,
            "count": self.count,
            "story_count": self.story_count,
            "chapter_total": self.chapter_total,
            "sample": self.sample,
            "suggested_target": self.suggested_target,
            "reasons": self.reasons,
            "stories": self.stories,
            "buckets": self.buckets,
            "bucket_families": self.bucket_families,
        })
    }
}

/// 对齐 Python `aggregate_candidate_families(candidates)`。
fn aggregate_candidate_families(candidates: &[Value]) -> Vec<Value> {
    let mut rows: Vec<FamilyRow> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();

    for item in candidates {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let row = match index.get(&name) {
            Some(&i) => &mut rows[i],
            None => {
                let i = rows.len();
                index.insert(name.clone(), i);
                rows.push(FamilyRow {
                    name: name.clone(),
                    count: 0,
                    story_count: 0,
                    stories: Vec::new(),
                    chapter_total: 0,
                    sample: item
                        .get("sample")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    suggested_target: item
                        .get("suggested_target")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    buckets: Vec::new(),
                    bucket_families: Vec::new(),
                    reasons: Vec::new(),
                    story_set: HashSet::new(),
                    bucket_set: HashSet::new(),
                    family_set: HashSet::new(),
                });
                &mut rows[i]
            }
        };
        row.count += item.get("count").and_then(Value::as_i64).unwrap_or(0);
        if let Some(stories) = item.get("stories").and_then(Value::as_array) {
            for story in stories {
                row.story_set
                    .insert(story.as_str().unwrap_or_default().to_string());
            }
        }
        row.chapter_total = row.chapter_total.max(
            item.get("chapter_total")
                .and_then(Value::as_i64)
                .unwrap_or(0),
        );
        let bucket = item.get("bucket").and_then(Value::as_str).unwrap_or("");
        row.bucket_set.insert(bucket.to_string());
        row.family_set.insert(bucket_family(bucket));
        if let Some(reasons) = item.get("reasons").and_then(Value::as_array) {
            for reason in reasons {
                let text = reason.as_str().unwrap_or_default().trim().to_string();
                if !text.is_empty() && !row.reasons.contains(&text) {
                    row.reasons.push(text);
                }
            }
        }
        if row.sample.is_empty() {
            let sample = item
                .get("sample")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if !sample.is_empty() {
                row.sample = sample;
            }
        }
    }

    let mut rows: Vec<Value> = rows
        .into_iter()
        .map(|mut row| {
            row.story_count = row.story_set.len() as i64;
            let mut stories: Vec<String> = row.story_set.iter().cloned().collect();
            stories.sort();
            row.stories = stories;
            let mut buckets: Vec<String> = row.bucket_set.iter().cloned().collect();
            buckets.sort();
            row.buckets = buckets;
            let mut families: Vec<String> = row.family_set.iter().cloned().collect();
            families.sort();
            row.bucket_families = families;
            row.to_value()
        })
        .collect();
    sort_by_counts(&mut rows);
    rows
}

/// 对齐 Python `aggregate_deposition_targets(payloads)`（`Counter.most_common` 全量）。
fn aggregate_deposition_targets(payloads: &[Value]) -> Vec<Value> {
    let mut counter: Ctr = Ctr::default();
    for payload in payloads {
        let items = match payload.get("deposition_targets").and_then(Value::as_array) {
            Some(items) => items,
            None => continue,
        };
        for item in items {
            let target = item
                .get("target")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let count = item.get("count").and_then(Value::as_i64).unwrap_or(0);
            counter.add(&target, count as usize);
        }
    }
    counter
        .most_common_all()
        .into_iter()
        .map(|(target, count)| json!({"target": target, "count": count}))
        .collect()
}

/// 对齐 Python `build_learning_anchors`（排序后截前 16）。
fn build_learning_anchors(
    template_families: &[Value],
    term_candidates: &[Value],
    keep_candidates: &[Value],
) -> Vec<Value> {
    let mut anchors: Vec<Value> = Vec::new();
    for item in template_families {
        let story_count = item.get("story_count").and_then(Value::as_i64).unwrap_or(0);
        let count = item.get("count").and_then(Value::as_i64).unwrap_or(0);
        if story_count >= 2 && count >= 10 {
            let buckets = item
                .get("buckets")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .map(|b| b.as_str().unwrap_or_default().to_string())
                        .collect::<Vec<_>>()
                        .join("/")
                })
                .unwrap_or_default();
            anchors.push(json!({
                "kind": "template",
                "name": item.get("name").and_then(Value::as_str).unwrap_or(""),
                "bucket": buckets,
                "count": count,
                "story_count": story_count,
                "suggested_target": item.get("suggested_target").and_then(Value::as_str).unwrap_or(""),
                "reason": "跨 Story 反复出现，值得判断它是硬模板、结构模式，还是应改抽取定义。",
            }));
        }
    }
    for item in term_candidates {
        let story_count = item.get("story_count").and_then(Value::as_i64).unwrap_or(0);
        let count = item.get("count").and_then(Value::as_i64).unwrap_or(0);
        if story_count >= 2 && count >= 8 {
            anchors.push(json!({
                "kind": "term",
                "name": item.get("name").and_then(Value::as_str).unwrap_or(""),
                "bucket": item.get("bucket").and_then(Value::as_str).unwrap_or(""),
                "count": count,
                "story_count": story_count,
                "suggested_target": item.get("suggested_target").and_then(Value::as_str).unwrap_or(""),
                "reason": "跨 Story 稳定偏高，适合正式进词库或改成更细的 phrase 规则。",
            }));
        }
    }
    for item in keep_candidates {
        let story_count = item.get("story_count").and_then(Value::as_i64).unwrap_or(0);
        if story_count >= 2 {
            anchors.push(json!({
                "kind": "keep",
                "name": item.get("name").and_then(Value::as_str).unwrap_or(""),
                "bucket": item.get("bucket").and_then(Value::as_str).unwrap_or(""),
                "count": item.get("count").and_then(Value::as_i64).unwrap_or(0),
                "story_count": story_count,
                "suggested_target": item.get("suggested_target").and_then(Value::as_str).unwrap_or(""),
                "reason": "多条 Story 都在把它当加分或保留候选，说明审查不该一刀切地误杀这类文笔设计。",
            }));
        }
    }
    sort_by_counts(&mut anchors);
    anchors.truncate(16);
    anchors
}

/// 对齐 Python `build_writeback_queue`（排序 `(-stories, -count, name)` 后截前 16）。
fn build_writeback_queue(
    template_families: &[Value],
    term_candidates: &[Value],
    keep_candidates: &[Value],
    template_bank_names: &HashSet<String>,
    term_bank_names: &HashSet<String>,
    hardcoded_template_names: &HashSet<String>,
    hardcoded_term_names: &HashSet<String>,
) -> Vec<Value> {
    let mut queue: Vec<Value> = Vec::new();
    for item in template_families {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let in_bank = template_bank_names.contains(&name);
        let in_hardcoded = hardcoded_template_names.contains(&name);
        let story_count = item.get("story_count").and_then(Value::as_i64).unwrap_or(0);
        let count = item.get("count").and_then(Value::as_i64).unwrap_or(0);
        if story_count < 2 {
            continue;
        }
        if in_bank && count >= 12 {
            queue.push(json!({
                "kind": "template_recalibration",
                "name": name,
                "target": "configs/rules/review.yaml#draft.template_rules",
                "stories": story_count,
                "count": count,
                "state": "bank",
                "reason": "模板已在库中，但跨 Story 仍高频命中，应回看 pattern、阈值或说明是否过宽。",
            }));
        } else if in_hardcoded && count >= 12 {
            queue.push(json!({
                "kind": "rule_recalibration",
                "name": name,
                "target": "audit.draft",
                "stories": story_count,
                "count": count,
                "state": "hardcoded",
                "reason": "这条规则已经写在审查脚本里，高频命中更像阈值、分类或说明需要回调，而不是简单再加一条 bank。",
            }));
        } else if !in_bank && count >= 12 {
            queue.push(json!({
                "kind": "template_add",
                "name": name,
                "target": "configs/rules/review.yaml#draft.template_rules",
                "stories": story_count,
                "count": count,
                "state": "new",
                "reason": "同名家族跨 Story 稳定出现，适合进入模板库或至少进入候选审阅清单。",
            }));
        }
    }
    for item in term_candidates {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let in_bank = term_bank_names.contains(&name);
        let in_hardcoded = hardcoded_term_names.contains(&name);
        let story_count = item.get("story_count").and_then(Value::as_i64).unwrap_or(0);
        let count = item.get("count").and_then(Value::as_i64).unwrap_or(0);
        if story_count < 2 {
            continue;
        }
        if in_bank && count >= 12 {
            queue.push(json!({
                "kind": "term_recalibration",
                "name": name,
                "target": "configs/rules/review.yaml#draft.tracked_terms",
                "stories": story_count,
                "count": count,
                "state": "bank",
                "reason": "词项已在库中却仍跨 Story 偏高，应调阈值、说明，或拆成更细 phrase 规则。",
            }));
        } else if in_hardcoded && count >= 8 {
            queue.push(json!({
                "kind": "rule_recalibration",
                "name": name,
                "target": "audit.draft",
                "stories": story_count,
                "count": count,
                "state": "hardcoded",
                "reason": "这条词项已经在审查脚本基础规则里，高频命中说明更适合调阈值或拆分类，而不是重复入库。",
            }));
        } else if !in_bank && count >= 8 {
            queue.push(json!({
                "kind": "term_add",
                "name": name,
                "target": "configs/rules/review.yaml#draft.tracked_terms",
                "stories": story_count,
                "count": count,
                "state": "new",
                "reason": "词项跨 Story 稳定偏高，适合做第一轮真实学习回写。",
            }));
        }
    }
    for item in keep_candidates {
        let story_count = item.get("story_count").and_then(Value::as_i64).unwrap_or(0);
        if story_count >= 3 {
            queue.push(json!({
                "kind": "keep_rule",
                "name": item.get("name").and_then(Value::as_str).unwrap_or(""),
                "target": "skills/review-guide.md",
                "stories": story_count,
                "count": item.get("count").and_then(Value::as_i64).unwrap_or(0),
                "state": "new",
                "reason": "这类候选在多条 Story 都被视作可保留，应补“设计性重复保留”口径。",
            }));
        }
    }
    // 排序键 (-stories, -count, name)：writeback 行没有 story_count 键位，改读 "stories"。
    queue.sort_by(|a, b| {
        b.get("stories")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .cmp(&a.get("stories").and_then(Value::as_i64).unwrap_or(0))
            .then(
                b.get("count")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    .cmp(&a.get("count").and_then(Value::as_i64).unwrap_or(0)),
            )
            .then_with(|| {
                a.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .cmp(b.get("name").and_then(Value::as_str).unwrap_or(""))
            })
    });
    queue.truncate(16);
    queue
}

/// `build_catalog_markdown` 的分节聚合输入（避免 9 参函数触发 clippy）。
#[derive(Debug, Clone, Copy)]
struct CatalogSections<'a> {
    template_candidates: &'a [Value],
    template_families: &'a [Value],
    term_candidates: &'a [Value],
    keep_candidates: &'a [Value],
    deposition_targets: &'a [Value],
    learning_anchors: &'a [Value],
    writeback_queue: &'a [Value],
}

fn build_catalog_markdown(novel_dir: &Path, stories: usize, sections: &CatalogSections) -> String {
    let CatalogSections {
        template_candidates,
        template_families,
        term_candidates,
        keep_candidates,
        deposition_targets,
        learning_anchors,
        writeback_queue,
    } = *sections;
    let get = |item: &Value, key: &str| -> String {
        item.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let gi = |item: &Value, key: &str| item.get(key).and_then(Value::as_i64).unwrap_or(0);

    let mut lines: Vec<String> = vec!["# Template Candidate Catalog".to_string(), String::new()];
    lines.push(format!(
        "- novel: `{}`",
        novel_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    lines.push(format!("- stories: `{stories}`"));
    lines.push(format!(
        "- source: `{}`",
        novel_dir.join("draft-stats").display()
    ));
    lines.push(String::new());

    lines.push("## Learning Anchors".to_string());
    if !learning_anchors.is_empty() {
        for item in learning_anchors {
            lines.push(format!(
                "- `{kind}` `{bucket}::{name}` stories=`{stories}` total=`{total}` -> `{target}`",
                kind = get(item, "kind"),
                bucket = get(item, "bucket"),
                name = get(item, "name"),
                stories = gi(item, "story_count"),
                total = gi(item, "count"),
                target = get(item, "suggested_target"),
            ));
            lines.push(format!("  说明：{}", get(item, "reason")));
        }
    } else {
        lines.push("- 无".to_string());
    }
    lines.push(String::new());

    lines.push("## Merged Families".to_string());
    if !template_families.is_empty() {
        for item in template_families.iter().take(16) {
            let buckets = item
                .get("buckets")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .map(|b| b.as_str().unwrap_or_default().to_string())
                        .collect::<Vec<_>>()
                        .join(" / ")
                })
                .unwrap_or_default();
            lines.push(format!(
                "- `{name}` stories=`{stories}` total=`{total}` buckets=`{buckets}`",
                name = get(item, "name"),
                stories = gi(item, "story_count"),
                total = gi(item, "count"),
            ));
            let sample = get(item, "sample");
            if !sample.is_empty() {
                lines.push(format!("  样例：{sample}"));
            }
        }
    } else {
        lines.push("- 无".to_string());
    }
    lines.push(String::new());

    lines.push("## Template Candidates".to_string());
    if !template_candidates.is_empty() {
        for item in template_candidates.iter().take(20) {
            lines.push(format!(
                "- `{bucket}::{name}` stories=`{stories}` total=`{total}` target=`{target}`",
                bucket = get(item, "bucket"),
                name = get(item, "name"),
                stories = gi(item, "story_count"),
                total = gi(item, "count"),
                target = get(item, "suggested_target"),
            ));
            let sample = get(item, "sample");
            if !sample.is_empty() {
                lines.push(format!("  样例：{sample}"));
            }
        }
    } else {
        lines.push("- 无".to_string());
    }
    lines.push(String::new());

    lines.push("## Term Candidates".to_string());
    if !term_candidates.is_empty() {
        for item in term_candidates.iter().take(20) {
            lines.push(format!(
                "- `{bucket}::{name}` stories=`{stories}` total=`{total}` target=`{target}`",
                bucket = get(item, "bucket"),
                name = get(item, "name"),
                stories = gi(item, "story_count"),
                total = gi(item, "count"),
                target = get(item, "suggested_target"),
            ));
            let sample = get(item, "sample");
            if !sample.is_empty() {
                lines.push(format!("  样例：{sample}"));
            }
        }
    } else {
        lines.push("- 无".to_string());
    }
    lines.push(String::new());

    lines.push("## Keep Candidates".to_string());
    if !keep_candidates.is_empty() {
        for item in keep_candidates.iter().take(12) {
            lines.push(format!(
                "- `{bucket}::{name}` stories=`{stories}` total=`{total}` target=`{target}`",
                bucket = get(item, "bucket"),
                name = get(item, "name"),
                stories = gi(item, "story_count"),
                total = gi(item, "count"),
                target = get(item, "suggested_target"),
            ));
            let reasons = item
                .get("reasons")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .take(2)
                        .collect::<Vec<_>>()
                        .join(" / ")
                })
                .unwrap_or_default();
            if !reasons.is_empty() {
                lines.push(format!("  说明：{reasons}"));
            }
        }
    } else {
        lines.push("- 无".to_string());
    }
    lines.push(String::new());

    lines.push("## Deposition Targets".to_string());
    if !deposition_targets.is_empty() {
        for item in deposition_targets {
            lines.push(format!(
                "- `{}` x{}",
                get(item, "target"),
                gi(item, "count")
            ));
        }
    } else {
        lines.push("- 无".to_string());
    }
    lines.push(String::new());

    lines.push("## Writeback Queue".to_string());
    if !writeback_queue.is_empty() {
        for item in writeback_queue {
            lines.push(format!(
                "- `{kind}` `{name}` stories=`{stories}` total=`{total}` state=`{state}` -> `{target}`",
                kind = get(item, "kind"),
                name = get(item, "name"),
                stories = gi(item, "stories"),
                total = gi(item, "count"),
                state = get(item, "state"),
                target = get(item, "target"),
            ));
            lines.push(format!("  说明：{}", get(item, "reason")));
        }
    } else {
        lines.push("- 无".to_string());
    }
    lines.push(String::new());

    lines.push("## Next Actions".to_string());
    lines
        .push("1. 先看 `Merged Families` 和 `Writeback Queue`，避免同名异桶重复判断。".to_string());
    lines.push(
        "2. 再核 `Keep Candidates`，把设计性重复和局部节奏从纯负向规则里拆出来。".to_string(),
    );
    lines.push(
        "3. 最后按 `Deposition Targets` 分流到模板库、词库、评审指南或本书规则。".to_string(),
    );
    lines.push(String::new());

    lines.join("\n") + "\n"
}

/// 对齐 Python `build_catalog_payload`。
fn build_catalog_payload(ctx: &DraftContext, novel_dir: &Path, payloads: &[Value]) -> Value {
    let draft = ctx.draft_rules();
    let template_bank_names = load_bank_names(draft, "name");
    let term_bank_names = load_bank_names(draft, "term");
    let hardcoded_template_names = load_hardcoded_template_names(draft);
    let hardcoded_term_names = load_hardcoded_term_names(draft);
    let template_candidates = aggregate_candidates(payloads, "template_bank_candidates");
    let template_families = aggregate_candidate_families(&template_candidates);
    let term_candidates = aggregate_candidates(payloads, "term_bank_candidates");
    let keep_candidates = aggregate_candidates(payloads, "keep_candidates");
    let deposition_targets = aggregate_deposition_targets(payloads);
    let learning_anchors =
        build_learning_anchors(&template_families, &term_candidates, &keep_candidates);
    let writeback_queue = build_writeback_queue(
        &template_families,
        &term_candidates,
        &keep_candidates,
        &template_bank_names,
        &term_bank_names,
        &hardcoded_template_names,
        &hardcoded_term_names,
    );
    let story_paths: Vec<Value> = payloads
        .iter()
        .map(|payload| json!(payload.get("story").and_then(Value::as_str).unwrap_or("")))
        .collect();
    let mut map = Map::new();
    map.insert(
        "novel".to_string(),
        json!(novel_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()),
    );
    map.insert("stories".to_string(), json!(payloads.len()));
    map.insert("story_paths".to_string(), Value::Array(story_paths));
    map.insert(
        "template_candidates".to_string(),
        Value::Array(template_candidates.clone()),
    );
    map.insert(
        "template_families".to_string(),
        Value::Array(template_families),
    );
    map.insert("term_candidates".to_string(), Value::Array(term_candidates));
    map.insert("keep_candidates".to_string(), Value::Array(keep_candidates));
    map.insert(
        "deposition_targets".to_string(),
        Value::Array(deposition_targets),
    );
    map.insert(
        "learning_anchors".to_string(),
        Value::Array(learning_anchors),
    );
    map.insert("writeback_queue".to_string(), Value::Array(writeback_queue));
    Value::Object(map)
}

/// 对齐 Python `resolve_payloads`。
fn resolve_payloads(
    paths: &[PathBuf],
    ctx: &DraftContext,
    template_bank: &[TemplateRule],
    term_bank: &[TrackedTerm],
) -> Result<Option<(PathBuf, Vec<Value>)>> {
    if paths.len() == 1 {
        let path = &paths[0];
        if path.is_dir() && path.join("draft-stats").exists() {
            let novel_dir = path.clone();
            let payloads = load_story_payloads_from_stats(&novel_dir);
            if !payloads.is_empty() {
                return Ok(Some((novel_dir, payloads)));
            }
        }
    }
    let files = collect_chapter_files(paths)?;
    if files.is_empty() {
        eprintln!("No draft chapter files found.");
        return Ok(None);
    }
    let (novel_dir, payloads) = build_story_payloads(&files, ctx, template_bank, term_bank)?;
    match novel_dir {
        Some(dir) => Ok(Some((dir, payloads))),
        None => {
            eprintln!("Failed to resolve novel directory from draft paths.");
            Ok(None)
        }
    }
}

/// 对齐 Python `main`：解析载荷 → 聚合 → 写 SUMMARY.md + CATALOG.json。
/// 返回（退出码, 应打印路径序列）。
pub fn run(opts: &CatalogOptions) -> Result<(i32, Vec<PathBuf>)> {
    let rules = config::load_rules(&config::default_rules_path())?;
    let ctx = DraftContext::new(rules.clone())?;
    let template_bank = build_template_bank(ctx.draft_rules());
    let term_bank = rules.draft.tracked_terms.clone();

    let (novel_dir, payloads) =
        match resolve_payloads(&opts.paths, &ctx, &template_bank, &term_bank)? {
            Some(pair) => pair,
            None => return Ok((1, Vec::new())),
        };
    if payloads.is_empty() {
        eprintln!("No story candidate payloads found.");
        return Ok((1, Vec::new()));
    }

    let catalog = build_catalog_payload(&ctx, &novel_dir, &payloads);
    let arr = |key: &str| -> Vec<Value> {
        catalog
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let template_candidates = arr("template_candidates");
    let template_families = arr("template_families");
    let term_candidates = arr("term_candidates");
    let keep_candidates = arr("keep_candidates");
    let deposition_targets = arr("deposition_targets");
    let learning_anchors = arr("learning_anchors");
    let writeback_queue = arr("writeback_queue");
    let sections = CatalogSections {
        template_candidates: &template_candidates,
        template_families: &template_families,
        term_candidates: &term_candidates,
        keep_candidates: &keep_candidates,
        deposition_targets: &deposition_targets,
        learning_anchors: &learning_anchors,
        writeback_queue: &writeback_queue,
    };
    let markdown = build_catalog_markdown(&novel_dir, payloads.len(), &sections);
    let summary_path = summary_path_for(&novel_dir);
    let catalog_path = json_path_for(&novel_dir);
    write_text(&summary_path, &markdown)?;
    write_json(&catalog_path, &catalog)?;
    Ok((0, vec![summary_path, catalog_path]))
}
