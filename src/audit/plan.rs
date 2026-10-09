//! 大纲（arc/story/chapter）plan 文件审查，移植自 Python `src/audit/plan.py`。
//!
//! 与 Python 语义逐项对齐：
//! - 标题解析、小节收集、标签块解析（同名的 dict 覆盖语义用「按位置替换」模拟）；
//! - `round`/排序/`min` 平手等坑按契约处理（本项目 plan 节无 float 指标，
//!   仅排序与词数统计）；
//! - 正则统一 `fancy_regex`；`ignore_case` 规则在 pattern 前缀 `(?i)`；
//! - `str.count`（非重叠）→ `str::matches` 计数；`len(str)` 码点 → `chars().count()`；
//! - 报告渲染（markdown）与 Python `format_report` 逐字节对齐。

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result};
use fancy_regex::Regex;
use serde::Serialize;

use crate::config::{self, PlanConfig, PlanRegexRule};
use crate::input;

/// 大纲审查警告，对应 Python `audit/plan.py` 的 `Warning` dataclass。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Warning {
    pub line_no: u32,
    pub kind: String,
    pub message: String,
    pub snippet: String,
}

/// 构造一条警告（顺序与 Python dataclass 字段一致）。
fn warn(line_no: u32, kind: &str, message: &str, snippet: &str) -> Warning {
    Warning {
        line_no,
        kind: kind.to_string(),
        message: message.to_string(),
        snippet: snippet.to_string(),
    }
}

/// 报告输出格式（对应 `--format` 参数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Json,
    Markdown,
}

// 模块级正则（对应 plan.py 常量；HEADING 用 `^` 锚定 + fancy find 等价 Python match）。
static HEADING_RE: LazyLock<Regex> =
    LazyLock::new(|| fancy_regex::Regex::new(r"^(#{1,6})\s+(.+?)\s*$").unwrap());
static LABEL_RE: LazyLock<Regex> =
    LazyLock::new(|| fancy_regex::Regex::new(r"^\s*-\s*([^：:]+)[：:]\s*(.*)$").unwrap());
static LIST_RE: LazyLock<Regex> =
    LazyLock::new(|| fancy_regex::Regex::new(r"^\s*(?:-\s+|\d+\.\s+|\*\s+)(.+)$").unwrap());
static SCENE_TITLE_RE: LazyLock<Regex> =
    LazyLock::new(|| fancy_regex::Regex::new(r"^Scene\s+\d+").unwrap());

fn re_search(re: &Regex, text: &str) -> bool {
    matches!(re.find(text), Ok(Some(_)))
}

fn re_find_all(re: &Regex, text: &str) -> usize {
    re.find_iter(text).filter(|m| m.is_ok()).count()
}

/// Python `len(str)`：码点数。
fn cpl(s: &str) -> u32 {
    s.chars().count() as u32
}

/// Python 字符串 `[:n]` 切片：取前 n 个码点。
fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn contains_any(text: &str, terms: &[String]) -> bool {
    terms.iter().any(|term| text.contains(term))
}

/// 标题结构：(line_no, 规范化标题, 级别)。
pub type Heading = (u32, String, u32);

/// 小节行：(line_no, 原始行)。
pub type BodyLine = (u32, String);

/// 小节容器：键序=首次插入序、值=最后一次赋值（模拟 Python dict 覆盖）。
pub struct Sections {
    names: Vec<String>,
    bodies: Vec<Vec<BodyLine>>,
}

impl Sections {
    fn insert(&mut self, name: &str, body: Vec<BodyLine>) {
        match self.names.iter().position(|n| n == name) {
            Some(i) => self.bodies[i] = body,
            None => {
                self.names.push(name.to_string());
                self.bodies.push(body);
            }
        }
    }

    /// `sections.get(name, [])` 语义：不存在时返回空。
    fn body(&self, name: &str) -> &[BodyLine] {
        self.names
            .iter()
            .position(|n| n == name)
            .map_or(&[], |i| &self.bodies[i])
    }

    /// `find_section`：按候选顺序取第一个存在的小节名。
    fn find<'a>(&self, choices: &[&'a str]) -> Option<&'a str> {
        choices
            .iter()
            .copied()
            .find(|choice| self.names.iter().any(|n| n == *choice))
    }

    fn iter(&self) -> impl Iterator<Item = (&str, &[BodyLine])> {
        self.names
            .iter()
            .enumerate()
            .map(|(i, name)| (name.as_str(), &self.bodies[i][..]))
    }
}

/// 标签块容器：`find_label_blocks` 的 dict 语义（键序=首现序，同键追加）。
struct LabelBlocks {
    labels: Vec<String>,
    blocks: Vec<Vec<BodyLine>>,
}

impl LabelBlocks {
    fn get(&self, name: &str) -> &[BodyLine] {
        self.labels
            .iter()
            .position(|l| l == name)
            .map_or(&[], |i| &self.blocks[i])
    }

    fn has(&self, name: &str) -> bool {
        self.labels.iter().any(|l| l == name)
    }
}

/// 已编译的 10 条 plan 正则（对应 plan.py 模块级 `_compiled_regex` 结果）。
struct PlanRegs {
    style_leak: Regex,
    abstract_lookpoint: Regex,
    answer_leak: Regex,
    scene_leak: Regex,
    prose_leak: Regex,
    judgement: Regex,
    hookish: Regex,
    foreshadow_id: Regex,
    rhythm_style_leak: Regex,
    story_prose_leak: Regex,
}

/// 大纲审查引擎：持有 plan 节配置 + 编译后的正则（对应 plan.py 模块级常量）。
pub struct PlanEngine {
    cfg: PlanConfig,
    regs: PlanRegs,
}

fn compile_rule(rules: &HashMap<String, PlanRegexRule>, name: &str) -> Result<Regex> {
    let rule = rules
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("Missing review rules section: plan.regex.{name}"))?;
    let pattern = if rule.ignore_case {
        format!("(?i){}", rule.pattern)
    } else {
        rule.pattern.clone()
    };
    Regex::new(&pattern).with_context(|| format!("plan 正则 {name} 编译失败: {pattern}"))
}

impl PlanEngine {
    /// 由 `plan` 节配置构建引擎；缺节/正则非法时返回错误。
    pub fn new(cfg: &PlanConfig) -> Result<Self> {
        let regs = PlanRegs {
            style_leak: compile_rule(&cfg.regex, "style_leak")?,
            abstract_lookpoint: compile_rule(&cfg.regex, "abstract_lookpoint")?,
            answer_leak: compile_rule(&cfg.regex, "answer_leak")?,
            scene_leak: compile_rule(&cfg.regex, "scene_leak")?,
            prose_leak: compile_rule(&cfg.regex, "prose_leak")?,
            judgement: compile_rule(&cfg.regex, "judgement")?,
            hookish: compile_rule(&cfg.regex, "hookish")?,
            foreshadow_id: compile_rule(&cfg.regex, "foreshadow_id")?,
            rhythm_style_leak: compile_rule(&cfg.regex, "rhythm_style_leak")?,
            story_prose_leak: compile_rule(&cfg.regex, "story_prose_leak")?,
        };
        Ok(Self {
            cfg: cfg.clone(),
            regs,
        })
    }

    /// `CHAPTER_FUNCTION_RULES`（`plan.function_rules.chapter`）。
    pub fn chapter_function_rules(&self) -> &HashMap<String, Vec<String>> {
        &self.cfg.function_rules.chapter
    }

    /// `ENDING_FUNCTION_RULES`（`plan.function_rules.ending`）。
    pub fn ending_function_rules(&self) -> &HashMap<String, Vec<String>> {
        &self.cfg.function_rules.ending
    }

    /// `CHAPTER_ENDING_GROUP`（`plan.chapter_ending_group`）。
    pub fn chapter_ending_group(&self) -> &[String] {
        &self.cfg.chapter_ending_group
    }
}

fn normalize_heading(title: &str) -> String {
    title.replace("：", "").replace(":", "").trim().to_string()
}

/// `parse_headings`：逐行匹配 markdown 标题，产出 (line_no, 标题, 级别)。
pub fn parse_headings(lines: &[&str]) -> Vec<Heading> {
    let mut headings: Vec<Heading> = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if let Ok(Some(caps)) = HEADING_RE.captures(line) {
            let level = caps[1].len() as u32;
            headings.push((idx as u32 + 1, normalize_heading(&caps[2]), level));
        }
    }
    headings
}

/// `collect_section_lines`：小节体为标题行之后到下一标题行之前（不含）的行。
pub fn collect_section_lines(lines: &[&str], headings: &[Heading]) -> Sections {
    let total = lines.len() as u32 + 1;
    let mut sections = Sections {
        names: Vec::new(),
        bodies: Vec::new(),
    };
    for (i, heading) in headings.iter().enumerate() {
        let next_line_no = headings.get(i + 1).map_or(total, |h| h.0);
        let body: Vec<BodyLine> = ((heading.0 + 1)..next_line_no)
            .map(|n| (n, lines[(n - 1) as usize].to_string()))
            .collect();
        sections.insert(&heading.1, body);
    }
    sections
}

/// `find_section`：按候选顺序取第一个存在的小节名；都不存在时返回 `(None, 空切片)`。
pub fn find_section<'a>(
    sections: &'a Sections,
    choices: &[&'a str],
) -> (Option<&'a str>, &'a [BodyLine]) {
    match sections.find(choices) {
        Some(name) => (Some(name), sections.body(name)),
        None => (None, &[]),
    }
}

/// `section_text`：小节各行按原样以 \n 连接。
pub fn section_text(section: &[BodyLine]) -> String {
    section
        .iter()
        .map(|line| line.1.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// `find_label_blocks`：标签行开启当前标签；其后 `- ` 子项归入该标签。
fn find_label_blocks(section: &[BodyLine]) -> LabelBlocks {
    let mut labels: Vec<String> = Vec::new();
    let mut blocks: Vec<Vec<BodyLine>> = Vec::new();
    let mut current: Option<usize> = None;
    for &(line_no, ref line) in section {
        if let Ok(Some(caps)) = LABEL_RE.captures(line) {
            let label = caps[1].trim().to_string();
            let content = caps[2].trim().to_string();
            let idx = match labels.iter().position(|l| l == &label) {
                Some(i) => i,
                None => {
                    labels.push(label.clone());
                    blocks.push(Vec::new());
                    labels.len() - 1
                }
            };
            if !content.is_empty() {
                blocks[idx].push((line_no, content));
            }
            current = Some(idx);
            continue;
        }
        if let Some(i) = current {
            let stripped = line.trim();
            if let Some(rest) = stripped.strip_prefix("- ") {
                blocks[i].push((line_no, rest.trim().to_string()));
            }
        }
    }
    LabelBlocks { labels, blocks }
}

/// `bullet_lines`：列表项（- / 1. / *）内容（strip 后取捕获组再 strip）。
pub fn bullet_lines(section: &[BodyLine]) -> Vec<BodyLine> {
    section
        .iter()
        .filter_map(|&(line_no, ref line)| {
            LIST_RE
                .captures(line.trim())
                .ok()
                .flatten()
                .map(|caps| (line_no, caps[1].trim().to_string()))
        })
        .collect()
}

/// `detect_function_label`：按 (-count, label) 排序取首；全 0 时返回默认 "unclear"。
pub fn detect_function_label(text: &str, rules: &HashMap<String, Vec<String>>) -> String {
    let mut hits: Vec<(String, usize)> = Vec::new();
    for (label, terms) in rules {
        let count = terms.iter().map(|term| text.matches(term).count()).sum();
        if count > 0 {
            hits.push((label.clone(), count));
        }
    }
    if hits.is_empty() {
        return "unclear".to_string();
    }
    hits.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    hits.into_iter().next().map(|h| h.0).unwrap()
}

/// `dominant_generic_terms`：通用推进词在条目中的出现次数，按 (-count, term) 排序。
fn dominant_generic_terms(engine: &PlanEngine, items: &[BodyLine]) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for item in items {
        for term in &engine.cfg.generic_progress_terms {
            if item.1.contains(term) {
                match counts.iter_mut().find(|(t, _)| t == term) {
                    Some(entry) => entry.1 += 1,
                    None => counts.push((term.clone(), 1)),
                }
            }
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
}

fn heading_present(headings: &[Heading], group: &[String]) -> bool {
    headings
        .iter()
        .any(|(_, title, _)| group.iter().any(|c| c == title))
}

/// `detect_plan_type`：路径名含类型串优先；其次按特征标题判断。
fn detect_plan_type(engine: &PlanEngine, path: &Path, text: &str) -> String {
    let joined = path.display().to_string();
    for plan_type in ["arc-plan", "story-plan", "chapter-plan"] {
        if joined.contains(plan_type) {
            return plan_type.to_string();
        }
    }
    let sections = &engine.cfg.sections;
    let chapter_marker = format!("## {}", sections.chapter_function);
    let events_marker = format!("## {}", sections.story_events);
    let loads_marker = format!("## {}", sections.story_loads);
    if text.contains(&chapter_marker) || text.contains("Scene 1") {
        "chapter-plan".to_string()
    } else if text.contains(&events_marker) && text.contains(&loads_marker) {
        "story-plan".to_string()
    } else {
        "arc-plan".to_string()
    }
}

fn audit_required(engine: &PlanEngine, plan_type: &str, headings: &[Heading]) -> Vec<Warning> {
    let mut warnings: Vec<Warning> = Vec::new();
    if let Some(groups) = engine.cfg.required_headings.get(plan_type) {
        for group in groups {
            if !heading_present(headings, group) {
                warnings.push(warn(
                    0,
                    "missing_heading",
                    &format!("缺少必备标题：{}", group[0]),
                    "",
                ));
            }
        }
    }
    warnings
}

fn audit_arc_plan(engine: &PlanEngine, sections: &Sections) -> Vec<Warning> {
    let mut warnings: Vec<Warning> = Vec::new();
    let story_layout = sections.body(&engine.cfg.sections.story_layout);
    let story_bullets = bullet_lines(story_layout);
    let story_table_rows = story_layout
        .iter()
        .filter(|line| line.1.trim().starts_with('|'))
        .count();
    if story_bullets.len() + story_table_rows
        < engine.cfg.thresholds.story_layout_min_items as usize
    {
        warnings.push(warn(
            0,
            "thin_story_layout",
            "Story 排布条目过少，Arc 推进骨架偏薄",
            "",
        ));
    }
    for &(line_no, ref line) in &story_bullets {
        if re_search(&engine.regs.scene_leak, line) {
            warnings.push(warn(
                line_no,
                "layer_drift",
                "Arc 层不应写 Scene/章节拆分",
                line,
            ));
        }
    }

    let foil_table = sections.body(&engine.cfg.sections.foreshadow_table);
    let foil_text = section_text(foil_table);
    if re_find_all(&engine.regs.foreshadow_id, &foil_text)
        < engine.cfg.thresholds.foreshadow_min_ids as usize
    {
        warnings.push(warn(
            0,
            "foreshadow_sparse",
            "伏笔表里的编号偏少，建议统一编号并补齐",
            "",
        ));
    }
    warnings
}

fn audit_story_plan(engine: &PlanEngine, sections: &Sections) -> Vec<Warning> {
    let mut warnings: Vec<Warning> = Vec::new();

    let events = bullet_lines(sections.body(&engine.cfg.sections.story_events));
    if events.len() < engine.cfg.thresholds.story_events_min_items as usize {
        warnings.push(warn(
            0,
            "thin_events",
            "核心事件少于 3 条，推进骨架偏薄",
            "",
        ));
    }
    let generic_events = dominant_generic_terms(engine, &events);
    if !generic_events.is_empty()
        && generic_events[0].1 >= 2usize.max(events.len().saturating_sub(1))
    {
        warnings.push(warn(
            0,
            "event_monotony",
            "核心事件过度依赖同一类推进动词，后续章节容易全部写成‘继续调查/继续解释’",
            &format!("{} x{}", generic_events[0].0, generic_events[0].1),
        ));
    }

    let load_lines = bullet_lines(sections.body(&engine.cfg.sections.story_loads));
    if load_lines.is_empty() {
        warnings.push(warn(0, "missing_loads", "粗章节负载没有明确章序或分段", ""));
    }
    for &(line_no, ref line) in &load_lines {
        if re_search(&engine.regs.scene_leak, line) {
            warnings.push(warn(
                line_no,
                "scene_leak",
                "Story 层不应直接拆 Scene",
                line,
            ));
        }
        let colon_count = line.chars().filter(|c| matches!(*c, '：' | ':')).count();
        if colon_count > 1 {
            warnings.push(warn(
                line_no,
                "over_detailed_load",
                "粗章节负载像在偷写章节施工图",
                line,
            ));
        }
    }

    let role_lines = bullet_lines(sections.body(&engine.cfg.sections.story_roles));
    if role_lines.len() < 2 {
        warnings.push(warn(
            0,
            "thin_roles",
            "主要角色与功能过少，角色承载面不清",
            "",
        ));
    } else if !role_lines
        .iter()
        .any(|item| contains_any(&item.1, &engine.cfg.role_function_terms))
    {
        warnings.push(warn(
            0,
            "thin_role_functions",
            "主要角色与功能更像点名名单，没有明确写出谁在推动、阻拦、见证或施压",
            "",
        ));
    }

    let story_text = sections
        .iter()
        .filter(|(name, _)| {
            engine
                .cfg
                .sections
                .story_environment
                .iter()
                .any(|e| e == *name)
        })
        .map(|(_, body)| section_text(body))
        .collect::<Vec<_>>()
        .join("\n");
    if contains_any(&story_text, &engine.cfg.environment_pressure_terms)
        && !contains_any(&story_text, &engine.cfg.environment_witness_terms)
    {
        warnings.push(warn(
            0,
            "missing_environment_witness",
            "Story 在讨论制度/阶级/城市压力，但没有明确安排配角、小事故、手续或环境证词来承接",
            "",
        ));
    }
    warnings
}

fn audit_lookpoint(engine: &PlanEngine, line_no: u32, items: &[BodyLine]) -> Vec<Warning> {
    let values: Vec<&str> = items
        .iter()
        .map(|item| item.1.as_str())
        .filter(|text| !text.is_empty())
        .collect();
    let mut warnings: Vec<Warning> = Vec::new();
    if values.is_empty() {
        warnings.push(warn(
            line_no,
            "thin_lookpoint",
            "看点为空，只有字段名没有戏",
            "看点",
        ));
        return warnings;
    }
    let joined = values.join(" ");
    let weak_hits: Vec<&String> = engine
        .cfg
        .lookpoint_weak_terms
        .iter()
        .filter(|term| joined.contains(*term))
        .collect();
    let strong_hits: Vec<&String> = engine
        .cfg
        .lookpoint_strong_terms
        .iter()
        .filter(|term| joined.contains(*term))
        .collect();
    if !weak_hits.is_empty() && strong_hits.is_empty() {
        warnings.push(warn(
            line_no,
            "abstract_lookpoint",
            "看点更像气氛词或人物态度，缺少可拍出来的戏",
            &joined,
        ));
    }
    if values.len() == 1 && cpl(values[0]) <= engine.cfg.thresholds.lookpoint_short_max_chars {
        warnings.push(warn(
            line_no,
            "thin_lookpoint",
            "看点过短，像标签不是内容",
            values[0],
        ));
    }
    warnings
}

fn audit_chapter_plan(
    engine: &PlanEngine,
    sections: &Sections,
    headings: &[Heading],
) -> Vec<Warning> {
    let mut warnings: Vec<Warning> = Vec::new();
    let scene_titles: Vec<(u32, &String)> = headings
        .iter()
        .filter(|(_, title, _)| re_search(&SCENE_TITLE_RE, title))
        .map(|(line_no, title, _)| (*line_no, title))
        .collect();

    let mut chapter_function_label = "unclear".to_string();
    match sections.find(&[engine.cfg.sections.chapter_function.as_str()]) {
        None => {
            warnings.push(warn(
                0,
                "missing_chapter_function",
                "缺少本章功能，施工图不知道这一章到底主推进什么",
                "",
            ));
        }
        Some(name) => {
            let section = sections.body(name);
            let function_items = bullet_lines(section);
            let joined: String = function_items
                .iter()
                .map(|item| item.1.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let function_text = if joined.is_empty() {
                section_text(section)
            } else {
                joined
            };
            if cpl(function_text.trim()) < 8 {
                warnings.push(warn(
                    0,
                    "thin_chapter_function",
                    "本章功能过短，像标签不是施工指令",
                    function_text.trim(),
                ));
            }
            chapter_function_label =
                detect_function_label(&function_text, &engine.cfg.function_rules.chapter);
            if chapter_function_label == "unclear" {
                warnings.push(warn(
                    0,
                    "unclear_chapter_function",
                    "本章功能没有落到冲突/调查/关系/手续/转场等主功能，后续容易一章承担过多杂事",
                    &take_chars(function_text.trim(), 120),
                ));
            }
        }
    }

    if scene_titles.is_empty() {
        warnings.push(warn(0, "missing_scene", "章节规划没有 Scene 拆分", ""));
        return warnings;
    }

    let mut scene_function_counts: BTreeMap<String, usize> = BTreeMap::new();
    for &(scene_line_no, scene_title) in &scene_titles {
        let body = sections.body(scene_title);
        let labels = find_label_blocks(body);
        let body_text = section_text(body);
        for group in &engine.cfg.scene_field_groups {
            if !group.iter().any(|choice| labels.has(choice)) {
                warnings.push(warn(
                    scene_line_no,
                    "scene_field",
                    &format!("{scene_title} 缺少字段：{}", group[0]),
                    scene_title,
                ));
            }
        }
        if cpl(&body_text) > engine.cfg.thresholds.scene_body_max_chars {
            warnings.push(warn(
                scene_line_no,
                "scene_overwrite",
                &format!("{scene_title} 内容过长，疑似写成半正文"),
                scene_title,
            ));
        }
        let prose_marks = body_text
            .chars()
            .filter(|c| matches!(*c, '。' | '！' | '？'))
            .count();
        if prose_marks as u32 >= engine.cfg.thresholds.scene_prose_mark_min {
            warnings.push(warn(
                scene_line_no,
                "scene_prose_density",
                &format!("{scene_title} 句号/感叹/问号过多，像半正文"),
                scene_title,
            ));
        }
        if re_search(&engine.regs.prose_leak, &body_text)
            && prose_marks as u32 >= engine.cfg.thresholds.scene_dialogue_prose_mark_min
        {
            warnings.push(warn(
                scene_line_no,
                "scene_dialogue_leak",
                &format!("{scene_title} 混入成句对白或正文化句子"),
                scene_title,
            ));
        }

        let lookpoint_items = labels.get(&engine.cfg.sections.lookpoint);
        warnings.extend(audit_lookpoint(engine, scene_line_no, lookpoint_items));

        if labels.has(&engine.cfg.sections.rhythm) {
            let joined_rhythm: String = labels
                .get(&engine.cfg.sections.rhythm)
                .iter()
                .map(|item| item.1.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if re_search(&engine.regs.rhythm_style_leak, &joined_rhythm) {
                warnings.push(warn(
                    scene_line_no,
                    "rhythm_style_leak",
                    "节奏字段混入写法要求",
                    &joined_rhythm,
                ));
            }
        }

        if labels.has(&engine.cfg.sections.state_change) {
            let joined_change: String = labels
                .get(&engine.cfg.sections.state_change)
                .iter()
                .map(|item| item.1.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if re_search(&engine.regs.judgement, &joined_change)
                && cpl(&joined_change) <= engine.cfg.thresholds.thin_change_max_chars
            {
                warnings.push(warn(
                    scene_line_no,
                    "thin_change",
                    "状态变化太抽象，缺少局势或关系上的具体变化",
                    &joined_change,
                ));
            }
        }

        let scene_function_text: String = engine
            .cfg
            .sections
            .scene_function_fields
            .iter()
            .flat_map(|field| labels.get(field).iter().map(|item| item.1.as_str()))
            .collect::<Vec<_>>()
            .join(" ");
        let scene_function =
            detect_function_label(&scene_function_text, &engine.cfg.function_rules.scene);
        *scene_function_counts
            .entry(scene_function.clone())
            .or_insert(0) += 1;
    }

    let mut generic_scene_terms: BTreeMap<String, Vec<&String>> = BTreeMap::new();
    let mut environment_scene_hits = 0usize;
    let mut witness_scene_hits = 0usize;
    for &(_, scene_title) in &scene_titles {
        let body = sections.body(scene_title);
        let labels = find_label_blocks(body);
        let joined_scene: String = engine
            .cfg
            .sections
            .scene_function_fields
            .iter()
            .flat_map(|field| labels.get(field).iter().map(|item| item.1.as_str()))
            .collect::<Vec<_>>()
            .join(" ");
        for term in &engine.cfg.generic_progress_terms {
            if joined_scene.contains(term) {
                generic_scene_terms
                    .entry(term.clone())
                    .or_default()
                    .push(scene_title);
            }
        }
        if contains_any(&joined_scene, &engine.cfg.environment_pressure_terms) {
            environment_scene_hits += 1;
        }
        if contains_any(&joined_scene, &engine.cfg.environment_witness_terms) {
            witness_scene_hits += 1;
        }
    }
    // Python `sorted(generic_scene_terms.items())`：按词排序；BTreeMap 迭代序一致。
    for (term, titles) in &generic_scene_terms {
        if titles.len() >= 2 {
            let snippet = titles
                .iter()
                .take(4)
                .map(|title| title.as_str())
                .collect::<Vec<_>>()
                .join(" / ");
            warnings.push(warn(
                0,
                "repeated_scene_function",
                &format!("多个 Scene 都在围绕“{term}”推进，施工图可能过于单调"),
                &snippet,
            ));
            break;
        }
    }

    if scene_titles.len() as u32 >= engine.cfg.thresholds.scene_monotony_min_scenes {
        let effective: BTreeMap<&String, &usize> = scene_function_counts
            .iter()
            .filter(|(name, _)| **name != "unclear")
            .collect();
        if let Some((dominant_scene_function, dominant_scene_count)) = effective
            .iter()
            .min_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)))
        {
            let missing = scene_titles
                .len()
                .saturating_sub(engine.cfg.thresholds.scene_monotony_max_missing as usize);
            if **dominant_scene_count >= 2.max(missing) {
                warnings.push(warn(
                    0,
                    "scene_function_monotony",
                    &format!("多个 Scene 都在承担 `{dominant_scene_function}` 功能，章节施工图的功能切换偏少"),
                    &format!("{dominant_scene_function} x{dominant_scene_count}/{}", scene_titles.len()),
                ));
            }
        }
    }

    if environment_scene_hits >= 1 && witness_scene_hits == 0 {
        warnings.push(warn(
            0,
            "missing_environment_witness",
            "本章想承载城市/制度压力，但 Scene 里看不见配角、手续、交易、窗口或环境证词",
            "",
        ));
    }

    let ending_choices: Vec<&str> = engine
        .cfg
        .chapter_ending_group
        .iter()
        .map(|s| s.as_str())
        .collect();
    match sections.find(&ending_choices) {
        None => {
            warnings.push(warn(0, "missing_ending", "缺少章节收尾或章节钩子", ""));
        }
        Some(ending_name) => {
            let section = sections.body(ending_name);
            let ending_items = bullet_lines(section);
            if ending_items.is_empty() {
                warnings.push(warn(
                    0,
                    "thin_ending",
                    &format!("{ending_name} 为空"),
                    ending_name,
                ));
            }
            for &(line_no, ref line) in &ending_items {
                if re_search(&engine.regs.hookish, line) {
                    warnings.push(warn(
                        line_no,
                        "hook_meta",
                        "收尾字段里写了元话术，不是具体收尾动作",
                        line,
                    ));
                }
                if engine
                    .cfg
                    .ending_weak_terms
                    .iter()
                    .any(|term| line.contains(term))
                {
                    warnings.push(warn(
                        line_no,
                        "template_ending",
                        "收尾疑似落回主角总结或城市意象模板",
                        line,
                    ));
                }
            }
            let ending_text: String = ending_items
                .iter()
                .map(|item| item.1.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let ending_function_label =
                detect_function_label(&ending_text, &engine.cfg.function_rules.ending);
            if !ending_text.is_empty() && ending_function_label == "unclear" {
                warnings.push(warn(
                    0,
                    "unclear_ending_function",
                    "章节收尾没有落到新信息/外部威胁/关系转向/手续压力等功能类型，钩子作用不清",
                    &take_chars(&ending_text, 120),
                ));
            }
            if chapter_function_label != "unclear"
                && ending_function_label == chapter_function_label
                && scene_titles.len() >= 3
            {
                warnings.push(warn(
                    0,
                    "flat_chapter_curve",
                    "本章功能和收尾功能落在同一类型，可能从头到尾都在做同一种事，缺少章末转向",
                    &format!("chapter={chapter_function_label} ending={ending_function_label}"),
                ));
            }
        }
    }
    warnings
}

fn audit_content(engine: &PlanEngine, plan_type: &str, sections: &Sections) -> Vec<Warning> {
    let mut warnings: Vec<Warning> = Vec::new();
    for (section_title, body) in sections.iter() {
        for &(line_no, ref line) in body {
            let stripped = line.trim();
            if stripped.is_empty() {
                continue;
            }
            if re_search(&engine.regs.style_leak, stripped) {
                warnings.push(warn(
                    line_no,
                    "style_leak",
                    "规划字段里混入写法建议或读者效果",
                    stripped,
                ));
            }
            if re_search(&engine.regs.answer_leak, stripped) {
                warnings.push(warn(
                    line_no,
                    "answer_leak",
                    "规划阶段疑似提前把答案写穿",
                    stripped,
                ));
            }
            if plan_type == "arc-plan" && re_search(&engine.regs.scene_leak, stripped) {
                warnings.push(warn(
                    line_no,
                    "layer_drift",
                    "Arc 层混入 Scene/章节粒度内容",
                    stripped,
                ));
            }
            if plan_type == "story-plan" && re_search(&engine.regs.story_prose_leak, stripped) {
                warnings.push(warn(
                    line_no,
                    "layer_drift",
                    "Story 层不应讨论完整正文写法",
                    stripped,
                ));
            }
            if section_title.starts_with("Scene ")
                && re_search(&engine.regs.abstract_lookpoint, stripped)
            {
                warnings.push(warn(
                    line_no,
                    "abstract_lookpoint",
                    "Scene 字段疑似滑回抽象气氛描述",
                    stripped,
                ));
            }
            if re_search(&engine.regs.judgement, stripped)
                && stripped.starts_with('-')
                && cpl(stripped) <= 18
            {
                warnings.push(warn(
                    line_no,
                    "thin_judgement",
                    "字段内容像判断句，不像推进点",
                    stripped,
                ));
            }
        }
    }
    warnings
}

/// `audit_file`：读文件 → 类型检测 → 标题/小节解析 → 各类警告。
pub fn audit_file(engine: &PlanEngine, path: &Path) -> Result<(String, Vec<Warning>)> {
    let text =
        fs::read_to_string(path).with_context(|| format!("无法读取大纲文件 {}", path.display()))?;
    let lines: Vec<&str> = text.lines().collect();
    let plan_type = detect_plan_type(engine, path, &text);
    let headings = parse_headings(&lines);
    let sections = collect_section_lines(&lines, &headings);

    let mut warnings: Vec<Warning> = Vec::new();
    warnings.extend(audit_required(engine, &plan_type, &headings));
    warnings.extend(audit_content(engine, &plan_type, &sections));
    match plan_type.as_str() {
        "arc-plan" => warnings.extend(audit_arc_plan(engine, &sections)),
        "story-plan" => warnings.extend(audit_story_plan(engine, &sections)),
        "chapter-plan" => warnings.extend(audit_chapter_plan(engine, &sections, &headings)),
        _ => {}
    }
    Ok((plan_type, warnings))
}

/// `format_report`：markdown 报告，与 Python 逐字节一致（`path.name` 为最后一级路径）。
pub fn format_report(path: &Path, plan_type: &str, warnings: &[Warning]) -> String {
    let status = if warnings.is_empty() { "OK" } else { "WARN" };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let mut lines = vec![
        format!("# {name}"),
        String::new(),
        format!("- type: `{plan_type}`"),
        format!("- status: `{status}`"),
        format!("- warnings: `{}`", warnings.len()),
        String::new(),
    ];
    if warnings.is_empty() {
        lines.push("无警告。".to_string());
        return lines.join("\n");
    }
    lines.push("## Warnings".to_string());
    for warning in warnings {
        let location = if warning.line_no > 0 {
            format!("L{}", warning.line_no)
        } else {
            "global".to_string()
        };
        lines.push(format!(
            "- `{location}` `{}` {}",
            warning.kind, warning.message
        ));
        if !warning.snippet.is_empty() {
            lines.push(format!("  - `{}`", warning.snippet));
        }
    }
    lines.join("\n")
}

/// JSON 报告单篇结构（字段名与 Python `_json_report` 一致，`type` 为 Python 保留键名）。
#[derive(Debug, Clone, PartialEq, Serialize)]
struct JsonReport {
    source: String,
    #[serde(rename = "type")]
    plan_type: String,
    status: String,
    warnings: Vec<Warning>,
}

/// `iter_targets`：目录递归找 `*.md`（跳过 readme.md，按路径排序）；文件直接入列。
pub fn iter_targets(raw_paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut targets: Vec<PathBuf> = Vec::new();
    for raw in raw_paths {
        if raw.is_dir() {
            let mut found: Vec<PathBuf> = Vec::new();
            walk_md_dir(raw, &mut found);
            found.sort();
            for path in found {
                let is_readme = path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().to_lowercase() == "readme.md");
                if !is_readme {
                    targets.push(path);
                }
            }
        } else if raw.is_file() {
            let is_readme = raw
                .file_name()
                .is_some_and(|n| n.to_string_lossy().to_lowercase() == "readme.md");
            if !is_readme {
                targets.push(raw.clone());
            }
        }
    }
    targets
}

fn walk_md_dir(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if meta.is_dir() {
            subdirs.push(path);
        } else if meta.is_file() && entry.file_name().to_string_lossy().ends_with(".md") {
            out.push(path);
        }
    }
    subdirs.sort();
    for sub in subdirs {
        walk_md_dir(&sub, out);
    }
}

fn json_reports(reports: &[(PathBuf, String, Vec<Warning>)]) -> Result<String> {
    let payload: Vec<JsonReport> = reports
        .iter()
        .map(|(path, plan_type, warnings)| JsonReport {
            source: path.display().to_string(),
            plan_type: plan_type.clone(),
            status: if warnings.is_empty() { "OK" } else { "WARN" }.to_string(),
            warnings: warnings.clone(),
        })
        .collect();
    Ok(serde_json::to_string_pretty(&payload)?)
}

fn write_reports(
    reports: &[(PathBuf, String, Vec<Warning>)],
    format: OutputFormat,
    output: Option<&Path>,
) -> Result<()> {
    match output {
        None => match format {
            OutputFormat::Json => {
                let json = json_reports(reports)?;
                println!("{json}");
            }
            OutputFormat::Text | OutputFormat::Markdown => {
                for (path, plan_type, warnings) in reports {
                    println!("{}", format_report(path, plan_type, warnings));
                    println!();
                }
            }
        },
        Some(out_path) => match format {
            OutputFormat::Json => {
                let json = json_reports(reports)?;
                input::write_json_line(out_path, &json)?;
            }
            OutputFormat::Text | OutputFormat::Markdown => {
                let suffix = if format == OutputFormat::Markdown {
                    ".md"
                } else {
                    ".txt"
                };
                if reports.len() == 1 {
                    let (path, plan_type, warnings) = &reports[0];
                    let report = format_report(path, plan_type, warnings);
                    input::write_text(out_path, &format!("{report}\n"))?;
                } else {
                    fs::create_dir_all(out_path)
                        .with_context(|| format!("无法创建目录 {}", out_path.display()))?;
                    for (path, plan_type, warnings) in reports {
                        let stem = path
                            .file_stem()
                            .map(|s| s.to_string_lossy())
                            .unwrap_or_default();
                        let report = format_report(path, plan_type, warnings);
                        input::write_text(
                            &out_path.join(format!("{stem}{suffix}")),
                            &format!("{report}\n"),
                        )?;
                    }
                }
            }
        },
    }
    Ok(())
}

/// `main` 全流程：解析输入 → 加载配置 → 逐文件审计 → 写报告。返回退出码。
pub fn run(
    paths: &[PathBuf],
    inputs: &[PathBuf],
    format: OutputFormat,
    fail_on_warn: bool,
    output: Option<&Path>,
) -> Result<i32> {
    let raw = input::resolve_inputs(paths, inputs)?;
    let targets = iter_targets(&raw);
    if targets.is_empty() {
        anyhow::bail!("No plan files found.");
    }
    let cfg = config::load_rules(&config::default_rules_path())?;
    let engine = PlanEngine::new(&cfg.plan)?;

    let mut reports: Vec<(PathBuf, String, Vec<Warning>)> = Vec::new();
    let mut warned = false;
    for path in &targets {
        let (plan_type, warnings) = audit_file(&engine, path)?;
        if !warnings.is_empty() {
            warned = true;
        }
        reports.push((path.clone(), plan_type, warnings));
    }
    write_reports(&reports, format, output)?;
    Ok(if fail_on_warn && warned { 1 } else { 0 })
}
