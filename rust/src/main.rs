//! Sentinel 小说审查工具（Rust 重写）——CLI 入口。
//!
//! 当前阶段提供 `rules`（配置校验统计）与 `audit-draft`（规则指标扫描，
//! 阶段 1a）子命令；完整审查/统计随后续阶段逐步接入。

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use sentinel::{config, rules};

/// 小说大纲/草稿审查与统计工具（Rust 重写）。
#[derive(Parser)]
#[command(name = "sentinel", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 加载并校验 review.yaml，输出各节统计
    Rules {
        /// 规则文件路径（默认 configs/rules/review.yaml）
        #[arg(long, value_name = "PATH")]
        rules: Option<PathBuf>,
    },
    /// 对单篇草稿运行规则指标扫描，输出 JSON（阶段 1a：规则节）
    AuditDraft {
        /// 草稿 Markdown 文件
        #[arg(long, value_name = "PATH")]
        input: PathBuf,
        /// 输出文件（默认 stdout）
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
        /// 规则文件路径（默认 configs/rules/review.yaml）
        #[arg(long, value_name = "PATH")]
        rules: Option<PathBuf>,
        /// 每条规则最多记录的样本行数
        #[arg(long, default_value_t = 3)]
        sample_limit: usize,
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
        Command::AuditDraft {
            input,
            output,
            rules: rules_path,
            sample_limit,
        } => {
            let rules_path = rules_path.unwrap_or_else(config::default_rules_path);
            let cfg = config::load_rules(&rules_path)?;
            let text = fs::read_to_string(&input)
                .with_context(|| format!("无法读取草稿文件 {}", input.display()))?;
            let source = input.display().to_string();
            let report = rules::audit_draft_report(&cfg.draft, &text, &source, sample_limit)?;
            let json = serde_json::to_string_pretty(&report)?;
            match output {
                Some(path) => fs::write(path, format!("{json}\n"))?,
                None => println!("{json}"),
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
