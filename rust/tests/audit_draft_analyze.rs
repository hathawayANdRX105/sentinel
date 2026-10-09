//! `tests/test_rules_config.py::test_analyze_text_hits_yaml_template_rule` 的
//! 单元级行为断言移植：`analyze_text` 携带 YAML 模板库/词库（默认
//! `configs/rules/review.yaml`）时，输入文本必须在内置 `patterns` 节
//! （label `不是A而是B`，即 YAML `template_rules` 中与内置规则重名被过滤的
//! 那条模板对应的内置对照句规则）上产生命中。

use sentinel::audit::draft::{analyze_text, DraftContext};
use sentinel::config::{default_rules_path, load_rules};
use sentinel::rules::build_template_bank;

#[test]
fn analyze_text_hits_negation_contrast_pattern() {
    let rules = load_rules(&default_rules_path()).expect("默认规则文件应可加载");
    let ctx = DraftContext::new(rules.clone()).expect("DraftContext 应可装配");
    let template_bank = build_template_bank(ctx.draft_rules());
    let term_bank = ctx.draft_rules().tracked_terms.clone();

    let text = "这不是冲动，而是判断。\n那不是巧合，而是设计。\n他不是退让，而是换位。\n";
    let analysis = analyze_text(&ctx, text, "fixture", &template_bank, &term_bank, None, 3)
        .expect("analyze_text 不应报错");

    let hits = analysis
        .patterns
        .iter()
        .filter(|m| m.name == "不是A而是B")
        .collect::<Vec<_>>();
    assert!(!hits.is_empty(), "expected YAML pattern 不是A而是B to hit");
    assert!(hits[0].count >= 1, "命中数应 >= 1，实际 {}", hits[0].count);
}
