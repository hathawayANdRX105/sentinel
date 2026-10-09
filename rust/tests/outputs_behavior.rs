//! `tests/test_outputs.py` 四项行为断言的 Rust pub API 移植。
//!
//! Python 侧依赖外置 `novel-novel2` 数据（缺失时 SkipTest）；Rust 侧等价保证
//! 由 `tests/fixtures/stats-draft/novel-a` 镜像树承担（该树的 golden 基线已记录
//! `repeated=`意象压轴 x2`` 章末连发与 ch03/ch04 的 `ending_tone` 合流，见
//! `tests/fixtures/expected/reports-scorecard/default` 与 `reports-learning/default`）。

use std::path::{Path, PathBuf};

use sentinel::audit::draft::{analyze_path, build_corpus_profile, Analysis, DraftContext};
use sentinel::audit::plan::PlanEngine;
use sentinel::config::{default_rules_path, load_rules, EndingLabels};
use sentinel::consistency::{build_story_conflict_snapshot_from_path, StoryConflictSnapshot};
use sentinel::input::write_text;
use sentinel::reports::alignment::Alignment;
use sentinel::reports::kit::collect_review_assignments;
use sentinel::reports::learning;
use sentinel::reports::scorecard::{
    build_axes, build_story_summary, build_story_trend_snapshots, decide_gate,
};
use sentinel::rules::build_template_bank;
use sentinel::stats::draft::{collect_chapter_files, stats_path_for};

fn story_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stats-draft/novel-a/drafts")
}

/// 装配 story 全章：章文件（chapter 序）× 分析（sample_limit=4，语料学习开启）
/// × 一致性快照 + 标签/引擎，对齐 Python `ReviewOutputTests.setUpClass`。
struct Fixture {
    analyses: Vec<(PathBuf, Analysis)>,
    snapshots: Vec<(PathBuf, StoryConflictSnapshot)>,
    labels: EndingLabels,
    engine: PlanEngine,
}

fn fixture() -> Fixture {
    let rules = load_rules(&default_rules_path()).expect("默认规则文件应可加载");
    let engine = PlanEngine::new(&rules.plan).expect("PlanEngine 应可装配");
    let ctx = DraftContext::new(rules.clone()).expect("DraftContext 应可装配");
    let bank = build_template_bank(ctx.draft_rules());
    let terms = ctx.draft_rules().tracked_terms.clone();
    let labels = ctx.draft_rules().ending_labels.clone();

    let files = collect_chapter_files(&[story_dir()]).expect("fixture story 应含章节文件");
    assert!(!files.is_empty());
    let corpus = build_corpus_profile(&ctx, &ctx.corpus_paths_for_targets(&files))
        .expect("语料画像构建不应报错");
    let analyses = files
        .iter()
        .map(|p| {
            (
                p.clone(),
                analyze_path(&ctx, p, &bank, &terms, corpus.as_ref(), 4)
                    .unwrap_or_else(|e| panic!("analyze_path({}): {}", p.display(), e)),
            )
        })
        .collect::<Vec<_>>();
    let snapshots = analyses
        .iter()
        .map(|(p, _)| {
            (
                p.clone(),
                build_story_conflict_snapshot_from_path(p, 200)
                    .unwrap_or_else(|e| panic!("一致性快照({}): {}", p.display(), e)),
            )
        })
        .collect::<Vec<_>>();
    Fixture {
        analyses,
        snapshots,
        labels,
        engine,
    }
}

fn analysis_refs(f: &Fixture) -> Vec<(PathBuf, &Analysis)> {
    f.analyses.iter().map(|(p, a)| (p.clone(), a)).collect()
}

fn snapshot_refs(f: &Fixture) -> Vec<(PathBuf, &StoryConflictSnapshot)> {
    f.snapshots.iter().map(|(p, s)| (p.clone(), s)).collect()
}

/// Python `test_reviewlib_paths_and_io_helpers`：`stats_path_for` 的
/// `drafts/` → `draft-stats/` 替换 + 写文件自动建父目录。
#[test]
fn paths_and_io_helpers() {
    let draft_path = PathBuf::from("novel1/drafts/arc1/story3/ch01.md");
    let stats_path =
        stats_path_for(&draft_path, None).expect("drafts/ 下路径应可计算 stats 镜像路径");
    assert!(
        stats_path.ends_with("novel1/draft-stats/arc1/story3/ch01.md"),
        "stats 镜像路径不符: {}",
        stats_path.display()
    );

    let tmp = tempfile::tempdir().expect("临时目录");
    let target = tmp.path().join("nested/sample.txt");
    write_text(&target, "ok").expect("write_text 应自动建父目录");
    assert_eq!(
        std::fs::read_to_string(&target).expect("已写文件应可读"),
        "ok"
    );
}

/// Python `test_scorecard_and_learning_summaries_include_ending_trends`：
/// story 级 SUMMARY（scorecard 与 learning 两份）渲染章末趋势节。
#[test]
fn scorecard_and_learning_summaries_include_ending_trends() {
    let f = fixture();
    let story = story_dir();
    let items = analysis_refs(&f);
    let snaps = snapshot_refs(&f);

    let score_summary = build_story_summary(&story, &items, &f.snapshots, &f.labels, &f.engine)
        .expect("scorecard story SUMMARY 不应报错");
    assert!(
        score_summary.contains("## Ending Trend Signals"),
        "scorecard SUMMARY 缺章末趋势节"
    );
    assert!(
        score_summary.contains("flow=`"),
        "scorecard SUMMARY 缺 flow=` 信号"
    );

    let learn_summary = learning::build_story_summary(&f.engine, &f.labels, &story, &items, &snaps)
        .expect("learning story SUMMARY 不应报错");
    assert!(
        learn_summary.contains("## Ending Trend Signals"),
        "learning SUMMARY 缺章末趋势节"
    );
    assert!(
        learn_summary.contains("repeated=`"),
        "learning SUMMARY 缺 repeated=` 信号"
    );
}

/// Python `test_story_trend_convergence_can_raise_scorecard_risk`：
/// 章末-色调合流章节的趋势快照应含 `ending_tone`，且喂给 `build_axes` 后
/// 门禁秩不降（`gate_rank[converged] >= gate_rank[base]`）。
#[test]
fn story_trend_convergence_can_raise_scorecard_risk() {
    let f = fixture();
    let items = analysis_refs(&f);

    let trend_snapshots = build_story_trend_snapshots(&items, &f.labels);
    let converging = trend_snapshots
        .iter()
        .filter(|(_, s)| s.convergence_kinds.iter().any(|k| k == "ending_tone"))
        .map(|(p, _)| p.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        converging.len(),
        2,
        "fixture 树 golden 基线记录 ch03/ch04 双章 ending_tone 合流"
    );
    let converging_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/stats-draft/novel-a/drafts/ch04-旧闸.md");
    let trend = trend_snapshots
        .iter()
        .find(|(p, _)| p == &converging_path)
        .expect("合流章节应出现于趋势快照");
    assert!(
        trend.1.convergence_kinds.iter().any(|k| k == "ending_tone"),
        "ch04 趋势快照应含 ending_tone 合流"
    );

    let idx = f
        .analyses
        .iter()
        .position(|(p, _)| p == &converging_path)
        .expect("合流章节应在分析集内");
    let chapter_snapshot = &f.snapshots[idx].1;
    let alignment = Alignment::unavailable();

    let base_axes = build_axes(
        &f.analyses[idx].1,
        Some(chapter_snapshot),
        Some(&alignment),
        None,
    );
    let (base_gate, _, _) = decide_gate(&f.analyses[idx].1, &base_axes);
    let converged_axes = build_axes(
        &f.analyses[idx].1,
        Some(chapter_snapshot),
        Some(&alignment),
        Some(&trend.1),
    );
    let (converged_gate, _, _) = decide_gate(&f.analyses[idx].1, &converged_axes);

    let gate_rank = |gate: &str| match gate {
        "PASS" => 0,
        "WATCH" => 1,
        "FAIL" => 2,
        other => panic!("未知门禁 {other}"),
    };
    assert!(
        gate_rank(&converged_gate) >= gate_rank(&base_gate),
        "合流只应抬升不应降低门禁风险（base={base_gate} converged={converged_gate}）"
    );
}

/// Python `test_review_kit_assigns_repeated_ending_trend_review`：
/// 同类章末连发应派出一条 `source=ending_trends` 的评审任务。
#[test]
fn review_kit_assigns_repeated_ending_trend_review() {
    let f = fixture();
    let items = analysis_refs(&f);

    let assignments =
        collect_review_assignments(&story_dir(), &items, &f.snapshots, &f.labels, &f.engine)
            .expect("评审派单不应报错");
    assert!(
        assignments.iter().any(|a| a.source == "ending_trends"),
        "expected review kit to emit an ending_trends assignment"
    );
}
