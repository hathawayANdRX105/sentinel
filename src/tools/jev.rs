//! `tools.jev`：`jev-review` 子命令（P0 原型）。
//!
//! 读取 `audit-draft --format json` 的分析结果，把规则命中的具体句子
//! 批量提交给 Jev 做语义精判（`jev_noul`：每句「是典型 AI 生成腔」的
//! 概率），按概率降序输出 top-N 值得改写的句子、来源与改写提示。
//!
//! 这是 P0 验证原型：sentinel 核心规则层零改动，Jev 判断作为可选外部层。
//! 端点凭据来自环境变量（可用 CLI 参数覆盖）：
//! - `JEV_API_BASE_URL`：Jev 兼容端点（如 `https://…/v1/systemone`）
//! - `JEV_API_KEY`：Bearer 密钥
//! - `JEV_MODEL`：模型别名，默认 `jev-latest`
//!
//! 协议参考 `@jkudish/jev-mcp` 的 compatible provider：POST
//! `{model, state, questions}`，响应 `{answers, usage, model}`；
//! `noul` 问题形如 `{type:"noul", instructions, criteria:{true,false}}`。

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

/// `jev-review` 的 CLI 参数。
#[derive(Debug, Clone)]
pub struct JevReviewOptions {
    /// `audit-draft --format json` 输出的 analysis JSON 文件。
    pub analysis: PathBuf,
    /// 输出前 N 句（默认 10）。
    pub top: usize,
    /// 最多提交给 Jev 的候选句数（`jev_noul` 单批上限 64）。
    pub limit: usize,
    /// 覆盖 `JEV_API_BASE_URL`。
    pub base_url: Option<String>,
    /// 覆盖 `JEV_API_KEY`。
    pub api_key: Option<String>,
    /// 覆盖 `JEV_MODEL`。
    pub model: Option<String>,
    /// 报告写出路径（默认 stdout）。
    pub output: Option<PathBuf>,
    /// 以 JSON 输出结果。
    pub json: bool,
}

/// 一个候选命中句。
#[derive(Debug, Clone)]
struct Candidate {
    id: usize,
    text: String,
    source: String,
    label: String,
}

/// 一个已判定句子。
#[derive(Debug, Clone)]
struct Ranked {
    text: String,
    source: String,
    label: String,
    probability: f64,
}

/// 单句最大提交字符数（`jev_noul` 上限 2000，留安全余量）。
const MAX_ITEM_CHARS: usize = 600;
/// 默认模型别名。
const DEFAULT_MODEL: &str = "jev-latest";

/// 运行 `jev-review`，返回进程退出码。
pub fn run(opts: &JevReviewOptions) -> Result<i32> {
    let analysis_text = std::fs::read_to_string(&opts.analysis)
        .with_context(|| format!("读取分析 JSON 失败: {}", opts.analysis.display()))?;
    let root: Value = serde_json::from_str(&analysis_text)
        .with_context(|| "分析 JSON 解析失败（需要 audit-draft --format json 的输出）")?;

    let candidates = collect_candidates(&root, opts.limit);
    if candidates.is_empty() {
        eprintln!("未找到任何含 `text` 字段的命中样本。");
        return Ok(0);
    }

    let base_url = opts
        .base_url
        .clone()
        .or_else(|| std::env::var("JEV_API_BASE_URL").ok())
        .context("缺少 Jev 端点：设置 JEV_API_BASE_URL 或传 --base-url")?;
    let api_key = opts
        .api_key
        .clone()
        .or_else(|| std::env::var("JEV_API_KEY").ok())
        .context("缺少 Jev 密钥：设置 JEV_API_KEY 或传 --api-key")?;
    let model = opts
        .model
        .clone()
        .or_else(|| std::env::var("JEV_MODEL").ok())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());

    let (ranked, answered_model) = ask_jev_noul(&base_url, &api_key, &model, &candidates)?;
    let report = if opts.json {
        render_json(&ranked, &answered_model, opts.top)?
    } else {
        render_text(&ranked, &answered_model, opts.top)?
    };

    match &opts.output {
        Some(path) => std::fs::write(path, report)
            .with_context(|| format!("写出报告失败: {}", path.display()))?,
        None => print!("{report}"),
    }
    Ok(0)
}

/// 递归收集 JSON 中规则/分析**命中样本**的文本：只收集路径落在
/// `samples[...]` / `sample[...]` 下的 `text` 字段或字符串元素，
/// 排除 `sentence_lengths.sentences` 等全文句子，使候选聚焦于
/// sentinel 已标记的句子。保留来源路径与同层 `label`/`name`/`category`
/// 作为诊断标签。
fn collect_candidates(root: &Value, limit: usize) -> Vec<Candidate> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    collect_text(root, "$".to_string(), &mut seen, &mut out);
    out.truncate(limit);
    out
}

fn collect_text(
    value: &Value,
    path: String,
    seen: &mut std::collections::HashSet<String>,
    out: &mut Vec<Candidate>,
) {
    match value {
        Value::Object(map) => {
            // 当前对象是一个命中样本：取 text 字段（仅样本路径）。
            if path.contains(".samples[") {
                if let Some(text) = map.get("text").and_then(Value::as_str) {
                    push_candidate(text, &path, map, seen, out);
                }
            }
            for (k, v) in map {
                collect_text(v, format!("{path}.{k}"), seen, out);
            }
        }
        Value::Array(arr) => {
            if is_string_sample_element(&path) {
                // 字符串数组样本（如 fatigue_windows.sample、dialogue.sample）。
                for s in arr.iter().filter_map(Value::as_str) {
                    push_candidate(s, &path, &Map::new(), seen, out);
                }
            } else {
                for (i, v) in arr.iter().enumerate() {
                    collect_text(v, format!("{path}[{i}]"), seen, out);
                }
            }
        }
        _ => {}
    }
}

/// 路径是否落在单数字段 `sample` 的字符串数组元素内
/// （如 `…fatigue_windows[0].sample[0]`），排除 `samples[0].terms`
/// 等元数据数组。
fn is_string_sample_element(path: &str) -> bool {
    let Some(pos) = path.rfind(".sample[") else {
        return false;
    };
    let rest = &path[pos + ".sample[".len()..];
    rest.ends_with(']')
        && !rest.is_empty()
        && rest[..rest.len() - 1].chars().all(|c| c.is_ascii_digit())
}

/// 去重并写入一个候选样本。
fn push_candidate(
    text: &str,
    path: &str,
    map: &Map<String, Value>,
    seen: &mut std::collections::HashSet<String>,
    out: &mut Vec<Candidate>,
) {
    let text = text.trim();
    if !text.is_empty() && seen.insert(text.to_string()) {
        let label = ["label", "name", "category"]
            .iter()
            .find_map(|k| map.get(*k).and_then(Value::as_str))
            .unwrap_or("")
            .to_string();
        let truncated = truncate_chars(text, MAX_ITEM_CHARS);
        out.push(Candidate {
            id: out.len(),
            text: truncated,
            source: path.to_string(),
            label,
        });
    }
}

/// 按字符数截断（避免单条超限）。
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

/// 调用 Jev 兼容端点，批量 `noul` 判定，按概率降序返回。
fn ask_jev_noul(
    base_url: &str,
    api_key: &str,
    model: &str,
    candidates: &[Candidate],
) -> Result<(Vec<Ranked>, String)> {
    let mut questions = Map::new();
    for c in candidates {
        let id = format!("p_{}", c.id);
        let instructions = format!(
            "proposition `{id}`: 这句小说文本是典型的 AI 生成腔（枯燥、模板化、解释腔、流水账、操作日志等）：\"{}\"",
            c.text
        );
        questions.insert(
            id,
            json!({
                "type": "noul",
                "instructions": instructions,
                "criteria": {
                    "true": "是典型的 AI 生成腔，枯燥乏味",
                    "false": "不是 AI 生成腔，自然生动"
                }
            }),
        );
    }

    let propositions: Vec<Value> = candidates
        .iter()
        .map(|c| json!({"id": format!("p_{}", c.id), "text": c.text}))
        .collect();
    let body = json!({
        "model": model,
        "state": { "propositions": propositions },
        "questions": questions,
    });

    let response = ureq::post(base_url)
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Content-Type", "application/json")
        .set("Accept", "application/json")
        .send_string(&body.to_string());

    let text = match response {
        Ok(resp) => resp.into_string().context("读取 Jev 响应体失败")?,
        Err(ureq::Error::Status(code, resp)) => {
            let body_text = resp.into_string().unwrap_or_default();
            bail!("Jev 端点返回 HTTP {code}: {body_text}");
        }
        Err(e) => bail!("Jev 端点请求失败: {e}"),
    };

    let parsed: Value = serde_json::from_str(&text).with_context(|| "Jev 响应不是有效 JSON")?;
    let answers = parsed
        .get("answers")
        .and_then(Value::as_object)
        .context("Jev 响应缺少 answers 对象")?;
    let answered_model = parsed
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(model)
        .to_string();

    let mut ranked = Vec::new();
    for c in candidates {
        let id = format!("p_{}", c.id);
        let noul = answers
            .get(&id)
            .and_then(|a| a.get("noul"))
            .and_then(Value::as_f64)
            .with_context(|| format!("Jev 响应缺少问题 {id} 的 noul 判定"))?;
        ranked.push(Ranked {
            text: c.text.clone(),
            source: c.source.clone(),
            label: c.label.clone(),
            probability: noul,
        });
    }
    ranked.sort_by(|a, b| {
        b.probability
            .partial_cmp(&a.probability)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok((ranked, answered_model))
}

/// 渲染文本报告。
fn render_text(ranked: &[Ranked], model: &str, top: usize) -> Result<String> {
    let mut s = String::new();
    s.push_str("# jev-review：AI 腔句子精判（P0）\n\n");
    s.push_str(&format!("模型: {model} | 判定句数: {}\n\n", ranked.len()));
    s.push_str(&format!("## Top {}\n\n", top.min(ranked.len())));
    for (i, r) in ranked.iter().take(top).enumerate() {
        s.push_str(&format!(
            "{}. [AI 腔概率 {:.2}] {}\n   句子：{}\n   来源：{}\n   改写提示：把「{}」中总结式、模板化的表述改为具体可见的动作、声音、物件或人物误读，避免解释腔替读者下结论；保留原有人名、数字、引语与事实。\n\n",
            i + 1,
            r.probability,
            if r.label.is_empty() { "未标注" } else { &r.label },
            r.text,
            r.source,
            r.text
        ));
    }
    Ok(s)
}

/// 渲染 JSON 结果。
fn render_json(ranked: &[Ranked], model: &str, top: usize) -> Result<String> {
    let top_items: Vec<Value> = ranked
        .iter()
        .take(top)
        .enumerate()
        .map(|(i, r)| {
            json!({
                "rank": i + 1,
                "ai_probability": r.probability,
                "text": r.text,
                "source": r.source,
                "label": r.label,
            })
        })
        .collect();
    let out = json!({
        "tool": "jev-review",
        "model": model,
        "judged": ranked.len(),
        "top": top_items,
    });
    Ok(serde_json::to_string_pretty(&out)?)
}
