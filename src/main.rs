//! Sentinel 小说审查工具（Rust 重写）——CLI 入口。
//!
//! 子命令：`rules`（配置校验统计）、`audit-draft`（草稿全量分析；
//! `--format json` 与 Python `src/audit/draft.py` 对齐，text/markdown 渲染字节级移植）、
//! `audit-plan` / `audit-concept`（大纲与概念卡审查）、
//! `stats-draft`（章节/滚动窗口镜像统计树）与 `stats-plan` / `stats-concept`
//! （镜像 markdown 统计树，与 Python `src/stats/*` 对齐）、
//! `consistency`（SQLite/FTS5 一致性索引与 13 个子命令，对应 Python `consistency` 模块）。

use std::path::PathBuf;
use std::process;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use sentinel::{audit, config, consistency, reports, stats, study, tools};

/// 小说大纲/草稿审查与统计工具（Rust 重写）。
#[derive(Parser)]
#[command(name = "sentinel", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// `--format` 报告格式取值（audit-plan）。
#[derive(Debug, Clone, Copy, ValueEnum)]
enum ReportFormat {
    /// 文本（markdown 报告，stdout 或 .txt 文件）
    Text,
    /// JSON 报告
    Json,
    /// Markdown 报告（.md 文件）
    Markdown,
}

#[derive(Subcommand)]
enum Command {
    /// 加载并校验 review.yaml，输出各节统计
    Rules {
        /// 规则文件路径（默认 configs/rules/review.yaml）
        #[arg(long, value_name = "PATH")]
        rules: Option<PathBuf>,
    },
    /// 分析草稿：全量指标、场面/对白/语料学习（`--format json` 与 Python 对齐）
    AuditDraft {
        /// 草稿文件或目录（多文件输出 JSON 数组）
        #[arg(value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// 输入文件或目录（可重复）
        #[arg(short = 'i', long, value_name = "PATH")]
        input: Vec<PathBuf>,
        /// 每条规则最多记录的样本行数
        #[arg(long, default_value_t = 3)]
        sample_limit: usize,
        /// 有警告时以退出码 1 结束
        #[arg(long)]
        fail_on_warn: bool,
        /// 报告输出格式（三种格式均与 Python 字节级对齐）
        #[arg(long, value_enum, default_value_t = ReportFormat::Text)]
        format: ReportFormat,
        /// 输出文件（json）
        #[arg(short = 'o', long, value_name = "PATH")]
        output: Option<PathBuf>,
        /// 语料学习路径（可多个；缺省时自动定位同小说的 concept/cards、plans、drafts）
        #[arg(long, value_name = "PATH", num_args = 0..)]
        learn_from: Option<Vec<PathBuf>>,
        /// 禁用语料学习
        #[arg(long)]
        no_corpus_learning: bool,
    },
    /// 审查大纲（arc/story/chapter）plan 文件：结构漂移与字段误用
    AuditPlan {
        /// Plan 文件或目录
        #[arg(value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// 输入 plan 文件或目录（可重复）
        #[arg(short = 'i', long, value_name = "PATH")]
        input: Vec<PathBuf>,
        /// 报告输出格式
        #[arg(long, value_enum, default_value_t = ReportFormat::Markdown)]
        format: ReportFormat,
        /// 有警告时以退出码 1 结束
        #[arg(long)]
        fail_on_warn: bool,
        /// 输出文件或目录
        #[arg(short = 'o', long, value_name = "PATH")]
        output: Option<PathBuf>,
    },
    /// 审查概念卡：缺失字段与分类漂移
    AuditConcept {
        /// 概念卡文件或目录（必填）
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// 把模板文件纳入审查
        #[arg(long)]
        include_templates: bool,
    },
    /// 计划镜像统计：逐文件 *-stats 报告 + 逐目录 SUMMARY.md（对应 Python `stats.plan`）
    StatsPlan {
        /// Plan 文件或目录
        #[arg(value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// 输入 plan 文件或目录（可重复）
        #[arg(short = 'i', long, value_name = "PATH")]
        input: Vec<PathBuf>,
        /// 单文件报告输出（仅在收集到恰好一个 plan 文件时允许）
        #[arg(short = 'o', long, value_name = "PATH")]
        output: Option<PathBuf>,
        /// 镜像统计根目录；缺省写入小说本地的 *-stats 树
        #[arg(long, value_name = "PATH")]
        output_root: Option<PathBuf>,
    },
    /// 概念卡镜像统计：card-stats 树 + 逐目录 SUMMARY.md（对应 Python `stats.concept`）
    StatsConcept {
        /// 概念卡文件或目录（必填）
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// 把模板文件纳入审查
        #[arg(long)]
        include_templates: bool,
    },
    /// 章节镜像统计：章节报告 + 滚动窗口合并 + 逐目录 SUMMARY（对应 Python `stats.draft`）
    StatsDraft {
        /// 草稿文件或目录
        #[arg(value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// 输入草稿文件或目录（可重复）
        #[arg(short = 'i', long, value_name = "PATH")]
        input: Vec<PathBuf>,
        /// 单文件章节报告输出（仅在收集到恰好一个章节时允许）
        #[arg(short = 'o', long, value_name = "PATH")]
        output: Option<PathBuf>,
        /// 镜像统计根目录；缺省写入小说本地的 draft-stats 树
        #[arg(long, value_name = "PATH")]
        output_root: Option<PathBuf>,
        /// 每条规则最多记录的样本行数
        #[arg(long, default_value_t = 3)]
        sample_limit: usize,
        /// 滚动章节窗口大小（缺省 2 3；显式 `--window-sizes` 不带值时为空列表）
        #[arg(long, value_name = "SIZE", num_args = 0..)]
        window_sizes: Option<Vec<usize>>,
        /// 禁用从既有卡片、计划、草稿学到的筛选器
        #[arg(long)]
        no_corpus_learning: bool,
    },
    /// 草稿章节评审记分卡：scorecards/*.md + 逐 story SUMMARY.md（对应 Python `reports.scorecard`）
    ReportsScorecard {
        /// 草稿章节文件或目录
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// 每条规则最多记录的样本行数
        #[arg(long, default_value_t = 6)]
        sample_limit: usize,
    },
    /// 草稿章节评审学习日志：learning/*.md + 逐 story SUMMARY.md（对应 Python `reports.learning`）
    ReportsLearning {
        /// 草稿章节文件或目录
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// 每条规则最多记录的样本行数
        #[arg(long, default_value_t = 6)]
        sample_limit: usize,
    },
    /// 研究导向章节句子画像：profiles/*.md + 逐 story SUMMARY.md（对应 Python `reports.profiles`）
    ReportsProfiles {
        /// 草稿章节文件或目录
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// 每节最多渲染的条数
        #[arg(long, default_value_t = 8)]
        sample_limit: usize,
        /// 可选镜像根目录；缺省写小说本地的 draft-stats 树
        #[arg(long, value_name = "PATH")]
        output_root: Option<PathBuf>,
    },
    /// 跨 Story 模板/词项候选目录：draft-stats/template-catalog/{SUMMARY.md,CATALOG.json}（对应 Python `reports.catalog`）
    ReportsCatalog {
        /// novel 目录、draft 目录或草稿章节文件
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,
    },
    /// 跨章模板积压：template-backlog/{SUMMARY.md,CANDIDATES.json}（对应 Python `reports.backlog`）
    ReportsBacklog {
        /// 草稿章节文件或目录
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// 每条规则最多记录的样本行数
        #[arg(long, default_value_t = 6)]
        sample_limit: usize,
    },
    /// 故事级评审套件：单章三类报告 + story 级 SUMMARY + review-kit/SUMMARY.md（对应 Python `reports.kit`）
    ReportsKit {
        /// 草稿章节文件或目录
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// 每条规则最多记录的样本行数
        #[arg(long, default_value_t = 6)]
        sample_limit: usize,
    },
    /// 一致性索引：SQLite/FTS5 构建与查询（对应 Python `consistency` 模块）
    Consistency {
        #[command(subcommand)]
        cmd: consistency::ConsistencyCmd,
    },
    /// 整工作区看板：concept/plan/draft/consistency 四节 → 单份 AUDIT.md（对应 Python `reports.workspace`）
    ReportsWorkspace {
        /// novel 目录（例如 novel1）
        #[arg(required = true, value_name = "NOVEL_DIR")]
        novel_dir: PathBuf,
        /// 每条规则最多记录的样本行数
        #[arg(long, default_value_t = 3)]
        sample_limit: usize,
        /// 滚动章节窗口大小（缺省 2 3；显式 `--window-sizes` 不带值时为空列表）
        #[arg(long, value_name = "SIZE", num_args = 0..)]
        window_sizes: Option<Vec<usize>>,
    },
    /// 模板候选回写：dry-run 预览或写回 review.yaml（对应 Python `tools.apply`）
    ToolsApply {
        /// 模板目录 CATALOG.json 路径
        #[arg(required = true, value_name = "CATALOG")]
        catalog: PathBuf,
        /// 只预览，不改文件
        #[arg(long)]
        dry_run: bool,
        /// 执行回写
        #[arg(long)]
        apply: bool,
    },
    /// 对比两份 analysis JSON，输出 Markdown 指标差异表（对应 Python `study.compare`）
    StudyCompare {
        /// 基线 analysis JSON
        #[arg(required = true, value_name = "BASELINE")]
        baseline: PathBuf,
        /// 应用后 analysis JSON
        #[arg(required = true, value_name = "APPLIED")]
        applied: PathBuf,
    },
    /// 视角切分候选检测：逐段密度不变量 + 尾组（对应 Python `study.pov`）
    StudyPov {
        /// 章节 Markdown 文件
        #[arg(required = true, value_name = "CHAPTER")]
        chapter: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Rules { rules: rules_path } => {
            let path = rules_path.unwrap_or_else(config::default_rules_path);
            let cfg = config::load_rules(&path)?;
            print_rule_stats(&cfg);
        }
        Command::AuditPlan {
            paths,
            input,
            format,
            fail_on_warn,
            output,
        } => {
            let plan_format = match format {
                ReportFormat::Text => audit::plan::OutputFormat::Text,
                ReportFormat::Json => audit::plan::OutputFormat::Json,
                ReportFormat::Markdown => audit::plan::OutputFormat::Markdown,
            };
            let rc =
                audit::plan::run(&paths, &input, plan_format, fail_on_warn, output.as_deref())?;
            if rc != 0 {
                process::exit(rc);
            }
        }
        Command::AuditConcept {
            paths,
            include_templates,
        } => {
            let rc = audit::concept::run(&paths, include_templates)?;
            if rc != 0 {
                process::exit(rc);
            }
        }
        Command::AuditDraft {
            paths,
            input,
            sample_limit,
            fail_on_warn,
            format,
            output,
            learn_from,
            no_corpus_learning,
        } => {
            let draft_format = match format {
                ReportFormat::Text => audit::draft::ReportFormat::Text,
                ReportFormat::Json => audit::draft::ReportFormat::Json,
                ReportFormat::Markdown => audit::draft::ReportFormat::Markdown,
            };
            let opts = audit::draft::RunOptions {
                positional: paths,
                inputs: input,
                sample_limit,
                fail_on_warn,
                format: draft_format,
                output,
                learn_from,
                no_corpus_learning,
            };
            let rc = audit::draft::run(&opts)?;
            if rc != 0 {
                process::exit(rc);
            }
        }
        Command::StatsPlan {
            paths,
            input,
            output,
            output_root,
        } => {
            let rc = stats::plan::run(&paths, &input, output.as_deref(), output_root.as_deref())?;
            if rc != 0 {
                process::exit(rc);
            }
        }
        Command::StatsConcept {
            paths,
            include_templates,
        } => {
            let rc = stats::concept::run(&paths, include_templates)?;
            if rc != 0 {
                process::exit(rc);
            }
        }
        Command::StatsDraft {
            paths,
            input,
            output,
            output_root,
            sample_limit,
            window_sizes,
            no_corpus_learning,
        } => {
            let opts = stats::draft::StatsDraftOptions {
                positional: paths,
                inputs: input,
                output,
                output_root,
                sample_limit,
                window_sizes: window_sizes.unwrap_or_else(|| vec![2, 3]),
                no_corpus_learning,
            };
            let rc = stats::draft::run(&opts)?;
            if rc != 0 {
                process::exit(rc);
            }
        }
        Command::ReportsScorecard {
            paths,
            sample_limit,
        } => {
            let opts = reports::scorecard::ScorecardOptions {
                paths,
                sample_limit,
            };
            let (rc, printed) = reports::scorecard::run(&opts)?;
            for path in &printed {
                println!("{}", path.display());
            }
            if rc != 0 {
                process::exit(rc);
            }
        }
        Command::ReportsLearning {
            paths,
            sample_limit,
        } => {
            let opts = reports::learning::LearningOptions {
                paths,
                sample_limit,
            };
            let (rc, printed) = reports::learning::run(&opts)?;
            for path in &printed {
                println!("{}", path.display());
            }
            if rc != 0 {
                process::exit(rc);
            }
        }
        Command::ReportsProfiles {
            paths,
            sample_limit,
            output_root,
        } => {
            let opts = reports::profiles::ProfileOptions {
                paths,
                sample_limit,
                output_root,
            };
            let (rc, printed) = reports::profiles::run(&opts)?;
            for path in &printed {
                println!("{}", path.display());
            }
            if rc != 0 {
                process::exit(rc);
            }
        }
        Command::ReportsCatalog { paths } => {
            let opts = reports::catalog::CatalogOptions { paths };
            let (rc, printed) = reports::catalog::run(&opts)?;
            for path in &printed {
                println!("{}", path.display());
            }
            if rc != 0 {
                process::exit(rc);
            }
        }
        Command::ReportsBacklog {
            paths,
            sample_limit,
        } => {
            let opts = reports::backlog::BacklogOptions {
                paths,
                sample_limit,
            };
            let (rc, printed) = reports::backlog::run(&opts)?;
            for path in &printed {
                println!("{}", path.display());
            }
            if rc != 0 {
                process::exit(rc);
            }
        }
        Command::ReportsKit {
            paths,
            sample_limit,
        } => {
            let opts = reports::kit::KitOptions {
                paths,
                sample_limit,
            };
            let (rc, printed) = reports::kit::run(&opts)?;
            for path in &printed {
                println!("{}", path.display());
            }
            if rc != 0 {
                process::exit(rc);
            }
        }
        Command::Consistency { cmd } => {
            let rc = consistency::run(&cmd)?;
            if rc != 0 {
                process::exit(rc);
            }
        }
        Command::ReportsWorkspace {
            novel_dir,
            sample_limit,
            window_sizes,
        } => {
            let opts = reports::workspace::WorkspaceOptions {
                novel_dir,
                sample_limit,
                window_sizes: window_sizes.unwrap_or_else(|| vec![2, 3]),
            };
            let (rc, printed) = reports::workspace::run(&opts)?;
            for path in &printed {
                println!("{}", path.display());
            }
            if rc != 0 {
                process::exit(rc);
            }
        }
        Command::ToolsApply {
            catalog,
            dry_run,
            apply,
        } => {
            let opts = tools::apply::ApplyOptions {
                catalog,
                dry_run,
                apply,
            };
            let (rc, dry_run_text) = tools::apply::run(&opts)?;
            if let Some(text) = dry_run_text {
                println!("{text}");
            }
            if rc != 0 {
                process::exit(rc);
            }
        }
        Command::StudyCompare { baseline, applied } => {
            match study::compare::run_compare(&baseline, &applied) {
                Ok(table) => println!("{table}"),
                Err(e) => {
                    eprintln!("Error: {e}");
                    process::exit(1);
                }
            }
        }
        Command::StudyPov { chapter } => {
            let cfg = config::load_rules(&config::default_rules_path())?;
            match study::pov::run_pov(&chapter, &cfg) {
                Ok(json) => println!("{json}"),
                Err(e) => {
                    eprintln!("Error: {e}");
                    process::exit(1);
                }
            }
        }
    }
    Ok(())
}

/// 打印规则配置各节的条目统计。
fn print_rule_stats(cfg: &config::ReviewRules) {
    let draft = &cfg.draft;
    let regex = &draft.regex_rules;
    println!("规则文件加载成功");
    println!("draft.regex_rules.tokens: {}", regex.tokens.len());
    println!("draft.regex_rules.patterns: {}", regex.patterns.len());
    println!("draft.regex_rules.phrases: {}", regex.phrases.len());
    println!("draft.regex_rules.modifiers: {}", regex.modifiers.len());
    println!("draft.regex_rules.punctuation: {}", regex.punctuation.len());
    println!(
        "draft.regex_rules.punctuation_combos: {}",
        regex.punctuation_combos.len()
    );
    println!("draft.template_rules: {}", draft.template_rules.len());
    println!(
        "draft.inactive_template_candidates: {}",
        draft.inactive_template_candidates.len()
    );
    println!("draft.tracked_terms: {}", draft.tracked_terms.len());
    println!(
        "draft.thresholds.short_sentence_max_chars: {}",
        draft.thresholds.short_sentence_max_chars
    );
    println!(
        "draft.learned_term_window.categories: {}",
        draft.learned_term_window.categories.len()
    );
    println!(
        "draft.lexicon.word_stoplist: {}",
        draft.lexicon.word_stoplist.len()
    );
    println!(
        "draft.ending_labels.display: {}",
        draft.ending_labels.display.len()
    );
    println!(
        "plan.required_headings.chapter-plan: {}",
        cfg.plan
            .required_headings
            .get("chapter-plan")
            .map_or(0, Vec::len)
    );
    println!("plan.regex: {}", cfg.plan.regex.len());
    println!(
        "plan.function_rules.chapter: {}",
        cfg.plan.function_rules.chapter.len()
    );
    println!(
        "plan.sections.chapter_function: {}",
        cfg.plan.sections.chapter_function
    );
}
