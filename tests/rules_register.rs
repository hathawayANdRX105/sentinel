//! 规则语域（register）契约测试。
//!
//! 来自真语料矩阵校验（/tmp/corpus 五类语料）：文学与轻小说的正常用法在
//! colloquial 规则上大量误报（鲁迅「不」15 次、「却」7 次，AI 集 0）。
//! 修复不是关规则，而是给规则标语域、报告携带，让评审按作品实际语域判读。
//!
//! 锁死：
//! - `RegexRule.register` 缺省 neutral；显式值透传到 `CompiledRule`/`RegexMetric`；
//! - `HardFlag.register` 对规则来源透传、非规则来源为 neutral；
//! - review.yaml 已标注的规则带正确语域；
//! - 真语料上鲁迅命中 literary 规则的标签可见（防回归）。

use std::path::{Path, PathBuf};

use sentinel::audit::draft::{self, ReportFormat, RunOptions};
use sentinel::config::{default_rules_path, load_rules};
use sentinel::rules::build_rule_metrics;

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
}

fn compile(rules_yaml: &str) -> Vec<sentinel::rules::CompiledRule> {
    // 直接构造 RegexRule 走 compile：register 缺省与显式两条路径。
    let rule: sentinel::config::RegexRule = serde_yaml::from_str(&format!(
        "name: 测试\npattern: 不\nmax_per_10k: 100.0\n{rules_yaml}"
    ))
    .expect("RegexRule 应可解析");
    vec![sentinel::rules::CompiledRule::compile(&rule).expect("应可编译")]
}

#[test]
fn register_defaults_to_neutral() {
    let rules = compile("note: 无语域标注");
    assert_eq!(rules[0].register, "neutral", "缺省 register 必须为 neutral");
}

#[test]
fn register_flows_through_metrics() {
    let rules = compile("note: 文学语域\nregister: literary");
    let lines = vec!["他没有错。".to_string()];
    let (metrics, _) = build_rule_metrics(&rules, &lines, 5, false, 10);
    assert_eq!(metrics.len(), 1);
    assert_eq!(metrics[0].register, "literary", "语域必须透传到 metric");
}

#[test]
fn review_yaml_marks_literary_false_positives() {
    let rules = load_rules(&default_rules_path()).expect("规则文件应可加载");
    let token = rules
        .draft
        .regex_rules
        .tokens
        .iter()
        .find(|t| t.name == "不")
        .expect("应有 token「不」");
    assert_eq!(
        token.register.as_deref(),
        Some("literary"),
        "「不」在鲁迅命中 15 次而 AI 集 0，必须标 literary"
    );
    let que = rules
        .draft
        .regex_rules
        .tokens
        .iter()
        .find(|t| t.name == "却")
        .expect("应有 token「却」");
    assert_eq!(que.register.as_deref(), Some("literary"));
}

#[test]
fn review_yaml_marks_colloquial_fillers() {
    let rules = load_rules(&default_rules_path()).expect("规则文件应可加载");
    let ran = rules
        .draft
        .regex_rules
        .tokens
        .iter()
        .find(|t| t.name == "然后")
        .expect("应有 token「然后」");
    assert_eq!(
        ran.register.as_deref(),
        Some("colloquial"),
        "「然后」是口语流水账标记，人写网文/轻小说也用"
    );
}

#[test]
fn unmarked_rules_stay_neutral_in_yaml() {
    let rules = load_rules(&default_rules_path()).expect("规则文件应可加载");
    let unmarked = rules
        .draft
        .regex_rules
        .tokens
        .iter()
        .filter(|t| t.register.is_none())
        .count();
    assert!(
        unmarked > 0,
        "大部分 token 无语域标注（走 neutral 默认），不应全量标"
    );
}

/// 端到端：真语料（若存在）上鲁迅的「不」命中带 literary 标签。
/// 语料在 CI 上不存在时跳过（本地/开发者机上验证）。
#[test]
fn lu_xun_hits_literary_register_end_to_end() {
    let corpus = fixture("../.wt-corpus/literature/lx-guxiang.md");
    let alt = Path::new("/tmp/corpus/literature/lx-guxiang.md").to_path_buf();
    let path = if corpus.exists() { corpus } else { alt };
    if !path.exists() {
        eprintln!("跳过：鲁迅语料不在 {path:?}");
        return;
    }
    let tmp = tempfile::tempdir().expect("临时目录");
    let out = tmp.path().join("r.json");
    let opts = RunOptions {
        positional: Vec::new(),
        inputs: vec![path],
        sample_limit: 3,
        fail_on_warn: false,
        format: ReportFormat::Json,
        output: Some(out.clone()),
        learn_from: None,
        no_corpus_learning: true,
    };
    draft::run(&opts).expect("run 应成功");
    let reports: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).expect("读报告")).expect("json");
    let flags = reports[0]["hard_flags"].as_array().expect("数组");
    let bu = flags
        .iter()
        .find(|f| f["name"] == "不")
        .expect("鲁迅文本应命中「不」");
    assert_eq!(
        bu["register"].as_str(),
        Some("literary"),
        "端到端必须把 literary 标签透传到 hard_flags"
    );
}
