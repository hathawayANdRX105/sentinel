//! Sentinel 小说审查工具（Rust 重写）——CLI 入口。
//!
//! 子命令：`rules`（配置校验统计）、`audit-draft`（草稿全量分析；
//! `--format json` 与 Python `src/audit/draft.py` 对齐，text/markdown 渲染未移植）、
//! `audit-plan` / `audit-concept`（大纲与概念卡审查）。

use std::path::PathBuf;
use std::process;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use sentinel::{audit, config};

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
        /// 报告输出格式（json 完整对齐；text/markdown 渲染未移植，会显式报错）
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
