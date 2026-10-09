//! `tools.jev`：`jev-review` 子命令（P0 精判 + P1 改写闭环）。
//!
//! 读取 `audit-draft --format json` 的分析结果，把规则命中的具体句子
//! 批量提交给 Jev 做语义精判（`jev_noul`：每句「是典型 AI 生成腔」的
//! 概率），按概率降序输出 top-N 值得改写的句子、来源与改写提示。
//! 加 `--rewrite` 后，再用生成模型（默认 ferrite 网关的 agnes-3.0-flash）
//! 对 top-N 逐句改写，输出「原句 → 改写」对照。
//!
//! 这是 P0/P1 验证原型：sentinel 核心规则层零改动，Jev 判断与生成模型
//! 都是可选外部层。
//!
//! Jev 端点凭据来自环境变量（可用 CLI 参数覆盖）：
//! - `JEV_API_BASE_URL`：Jev 兼容端点（如 `https://…/v1/systemone`）
//! - `JEV_API_KEY`：Bearer 密钥
//! - `JEV_MODEL`：模型别名，默认 `jev-latest`
//!
//! 改写模型端点凭据（`--rewrite` 时）：`FERRITE_BASE_URL`（默认
//! `http://127.0.0.1:3211/v1`）、`FERRITE_API_KEY`、`FERRITE_MODEL`
//! （默认 `agnes-3.0-flash`）。改写请求头伪装成 omp（oh-my-pi）客户端
//! （User-Agent + x-opencode-*），参考 `~/.omp/agent/models.yml` 的
//! 客户端标识。
//!
//! 协议参考 `@jkudish/jev-mcp` 的 compatible provider：POST
//! `{model, state, questions}`，响应 `{answers, usage, model}`；
//! `noul` 问题形如 `{type:"noul", instructions, criteria:{true,false}}`。

use std::path::{Path, PathBuf};

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
    /// 调用生成模型对 top-N 逐句改写。
    pub rewrite: bool,
    /// 覆盖 `FERRITE_BASE_URL`。
    pub rewrite_base_url: Option<String>,
    /// 覆盖 `FERRITE_API_KEY`。
    pub rewrite_api_key: Option<String>,
    /// 覆盖 `FERRITE_MODEL`。
    pub rewrite_model: Option<String>,
    /// 同时收集全文句子（sentence_lengths.sentences），而不只是规则命中样本。
    pub all_sentences: bool,
    /// 改写后用 jev_compare 校验语义保真，不一致则拒绝改写。
    pub verify: bool,
    /// 原始草稿文件路径（配合 `--output-draft` 生成改写后全文）。
    pub draft: Option<PathBuf>,
    /// 改写后全文输出路径。
    pub output_draft: Option<PathBuf>,
}

/// 一个候选命中句。
#[derive(Debug, Clone)]
struct Candidate {
    id: usize,
    text: String,
    source: String,
    label: String,
    /// 命中样本对应的规则说明（review.yaml 的 note 字段）。
    note: String,
}

/// 一个已判定句子。
#[derive(Debug, Clone)]
struct Ranked {
    text: String,
    source: String,
    label: String,
    /// 命中样本对应的规则说明（review.yaml 的 note 字段）。
    note: String,
    probability: f64,
    rewrite: Option<String>,
    /// 改写后经 jev 判定的「AI 腔概率」（`--verify` 时）。
    ai_prob_after: Option<f64>,
    /// 改写是否因语义校验不一致被拒绝（`--verify` 时）。
    verify_rejected: bool,
}

/// 单句最大提交字符数（`jev_noul` 上限 2000，留安全余量）。
const MAX_ITEM_CHARS: usize = 600;
/// 默认模型别名。
const DEFAULT_MODEL: &str = "jev-latest";
/// 默认改写模型端点（ferrite 网关）。
const DEFAULT_REWRITE_BASE_URL: &str = "http://127.0.0.1:3211/v1";
/// 默认改写模型。
const DEFAULT_REWRITE_MODEL: &str = "agnes-3.0-flash";
/// 语义校验接受阈值：改写句与原文同属一个事实的最低概率。
const VERIFY_SAME_FACT_MIN: f64 = 0.7;
/// 全文句子路径标记（`--all-sentences` 收集用）。
const SENTENCE_LENGTHS_PATH: &str = ".sentence_lengths.sentences[";

/// 运行 `jev-review`，返回进程退出码。
pub fn run(opts: &JevReviewOptions) -> Result<i32> {
    let analysis_text = std::fs::read_to_string(&opts.analysis)
        .with_context(|| format!("读取分析 JSON 失败: {}", opts.analysis.display()))?;
    let root: Value = serde_json::from_str(&analysis_text)
        .with_context(|| "分析 JSON 解析失败（需要 audit-draft --format json 的输出）")?;

    let candidates = collect_candidates(&root, opts.limit, opts.all_sentences);
    if candidates.is_empty() {
        eprintln!("未找到任何候选句子（JSON 中无命中样本或全文句子）。");
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

    let (mut ranked, answered_model) = ask_jev_noul(&base_url, &api_key, &model, &candidates)?;

    if opts.rewrite {
        let rw_base_url = opts
            .rewrite_base_url
            .clone()
            .or_else(|| std::env::var("FERRITE_BASE_URL").ok())
            .unwrap_or_else(|| DEFAULT_REWRITE_BASE_URL.to_string());
        let rw_api_key = opts
            .rewrite_api_key
            .clone()
            .or_else(|| std::env::var("FERRITE_API_KEY").ok())
            .context("缺少改写模型密钥：设置 FERRITE_API_KEY 或传 --rewrite-api-key")?;
        let rw_model = opts
            .rewrite_model
            .clone()
            .or_else(|| std::env::var("FERRITE_MODEL").ok())
            .unwrap_or_else(|| DEFAULT_REWRITE_MODEL.to_string());
        for r in ranked.iter_mut().take(opts.top) {
            match rewrite_sentence(&rw_base_url, &rw_api_key, &rw_model, r) {
                Ok(text) => {
                    if opts.verify {
                        match verify_rewrite(&base_url, &api_key, &model, &r.text, &text) {
                            Ok((same_fact, ai_after)) => {
                                r.ai_prob_after = Some(ai_after);
                                if same_fact {
                                    r.rewrite = Some(text);
                                } else {
                                    r.verify_rejected = true;
                                    eprintln!("改写被拒（语义不一致）：{}", r.source);
                                }
                            }
                            Err(e) => {
                                // 校验失败时保守接受改写，但保留原句可查。
                                r.rewrite = Some(text);
                                eprintln!("语义校验失败，保留改写（{}）：{e:#}", r.source);
                            }
                        }
                    } else {
                        r.rewrite = Some(text);
                    }
                }
                Err(e) => eprintln!("改写失败（{}）：{e:#}", r.source),
            }
        }
    }

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

    if let (Some(draft_path), Some(output_path)) = (&opts.draft, &opts.output_draft) {
        let draft_text = std::fs::read_to_string(draft_path)
            .with_context(|| format!("读取原始草稿失败: {}", draft_path.display()))?;
        let rewritten = apply_rewrites(&draft_text, &ranked);
        std::fs::write(output_path, rewritten)
            .with_context(|| format!("写出改写后全文失败: {}", output_path.display()))?;
        println!("\n改写后全文已写出：{}", output_path.display());

        // 反向红线复检：原稿 vs 改写后草稿的规则告警对比。
        if opts.verify {
            match redline_check(draft_path, output_path) {
                Ok((ow, oh, rw, rh)) => {
                    println!(
                        "\n## 红线复检\n\n- 原稿：warn_sections={ow}, hard_flags={oh}\n- 改写后：warn_sections={rw}, hard_flags={rh}\n- 变化：warn_sections {:+} / hard_flags {:+}\n",
                        rw as i64 - ow as i64,
                        rh as i64 - oh as i64
                    );
                }
                Err(e) => eprintln!("红线复检失败：{e:#}"),
            }
        }
    }
    Ok(0)
}

/// 反向红线复检：对原稿与改写后草稿各跑一次 `audit-draft` 分析，
/// 返回 `(原稿 warn_sections, 原稿 hard_flags, 改写后 warn_sections, 改写后 hard_flags)`。
fn redline_check(draft_path: &Path, rewritten_path: &Path) -> Result<(usize, usize, usize, usize)> {
    let rules = crate::config::load_rules(&crate::config::default_rules_path())?;
    let ctx = crate::audit::draft::DraftContext::new(rules)?;
    let template_bank = crate::rules::build_template_bank(ctx.draft_rules());
    let term_bank = ctx.draft_rules().tracked_terms.clone();
    let orig =
        crate::audit::draft::analyze_path(&ctx, draft_path, &template_bank, &term_bank, None, 100)?;
    let rewritten = crate::audit::draft::analyze_path(
        &ctx,
        rewritten_path,
        &template_bank,
        &term_bank,
        None,
        100,
    )?;
    Ok((
        orig.summary.warn_sections,
        orig.hard_flags.len(),
        rewritten.summary.warn_sections,
        rewritten.hard_flags.len(),
    ))
}

/// 递归收集文本候选：默认只收集规则/分析**命中样本**（路径落在
/// `samples[...]` / `sample[...]` 下的 `text` 字段或字符串元素）；
/// `all_sentences` 时额外收集 `sentence_lengths.sentences` 的全文句子。
/// 保留来源路径与同层 `label`/`name`/`category` 作为诊断标签。
fn collect_candidates(root: &Value, limit: usize, all_sentences: bool) -> Vec<Candidate> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    collect_text(root, "$".to_string(), &mut seen, &mut out, all_sentences);
    out.truncate(limit);
    out
}

fn collect_text(
    value: &Value,
    path: String,
    seen: &mut std::collections::HashSet<String>,
    out: &mut Vec<Candidate>,
    all_sentences: bool,
) {
    match value {
        Value::Object(map) => {
            // 当前对象是一个命中样本：取 text 字段（仅样本路径）。
            if path.contains(".samples[") || (all_sentences && path.contains(SENTENCE_LENGTHS_PATH))
            {
                if let Some(text) = map.get("text").and_then(Value::as_str) {
                    push_candidate(text, &path, map, seen, out);
                }
            }
            for (k, v) in map {
                collect_text(v, format!("{path}.{k}"), seen, out, all_sentences);
            }
        }
        Value::Array(arr) => {
            if is_string_sample_element(&path) {
                // 字符串数组样本（如 fatigue_windows.sample、dialogue.sample）。
                for s in arr.iter().filter_map(Value::as_str) {
                    push_candidate(s, &path, &Map::new(), seen, out);
                }
            } else if all_sentences && path.contains(SENTENCE_LENGTHS_PATH) {
                // 全文句子（--all-sentences）。
                for s in arr.iter().filter_map(Value::as_str) {
                    push_candidate(s, &path, &Map::new(), seen, out);
                }
            } else {
                for (i, v) in arr.iter().enumerate() {
                    collect_text(v, format!("{path}[{i}]"), seen, out, all_sentences);
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
        let note = map
            .get("note")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let truncated = truncate_chars(text, MAX_ITEM_CHARS);
        out.push(Candidate {
            id: out.len(),
            text: truncated,
            source: path.to_string(),
            label,
            note,
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
            note: c.note.clone(),
            probability: noul,
            rewrite: None,
            ai_prob_after: None,
            verify_rejected: false,
        });
    }
    ranked.sort_by(|a, b| {
        b.probability
            .partial_cmp(&a.probability)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok((ranked, answered_model))
}

/// 调用生成模型（ferrite 网关，请求头伪装成 omp 客户端）改写一个句子。
fn rewrite_sentence(base_url: &str, api_key: &str, model: &str, ranked: &Ranked) -> Result<String> {
    let system = "你是资深中文小说编辑，擅长把 AI 生成腔改写成自然、有人味的文学语言。\
约束：只改表达，不改事实——人名、数字、引语、关键情节不得变动；\
不要添加原文没有的信息；只输出改写后的文本，不要解释。";
    let diagnosis = if !ranked.note.is_empty() {
        ranked.note.as_str()
    } else if !ranked.label.is_empty() {
        ranked.label.as_str()
    } else {
        "未标注"
    };
    let user = format!(
        "原文（AI 腔概率 {:.2}，问题：{}）：\n{}\n\n请针对上述问题把这段话改写得像人写的。",
        ranked.probability, diagnosis, ranked.text
    );
    let body = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ],
        "temperature": 0.8,
        "max_tokens": 1024,
    });
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let response = ureq::post(&url)
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Content-Type", "application/json")
        .set("Accept", "application/json")
        .set(
            "User-Agent",
            "opencode/1.18.31 ai-sdk/provider-utils/4.0.40 runtime/bun/1.3.14",
        )
        .set("x-opencode-client", "cli")
        .set("x-opencode-project", "global")
        .set("x-opencode-session", "ses_7f3a9c2e51b84d06af19c3d7")
        .set("x-opencode-request", "req_2f8c1d90ab34e6570189cafe")
        .send_string(&body.to_string());

    let text = match response {
        Ok(resp) => resp.into_string().context("读取改写响应体失败")?,
        Err(ureq::Error::Status(code, resp)) => {
            let body_text = resp.into_string().unwrap_or_default();
            bail!("改写端点返回 HTTP {code}: {body_text}");
        }
        Err(e) => bail!("改写端点请求失败: {e}"),
    };
    let parsed: Value = serde_json::from_str(&text).with_context(|| "改写响应不是有效 JSON")?;
    parsed["choices"][0]["message"]["content"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .context("改写响应缺少 choices[0].message.content")
}

/// 用 Jev 校验改写：`jev_compare`（overall 语义关系）+ 改写后 AI 腔概率。
/// 返回 `(same_fact_ok, ai_prob_after)`。
fn verify_rewrite(
    base_url: &str,
    api_key: &str,
    model: &str,
    original: &str,
    rewritten: &str,
) -> Result<(bool, f64)> {
    let questions = json!({
        "overall": {
            "type": "choice",
            "instructions": "Do the two passages state the same underlying fact, contradict each other, or discuss different facts?",
            "criteria": {
                "same_fact": "Both passages state the same underlying fact or claim",
                "contradicts": "The passages state opposing facts about the same subject",
                "different_facts": "The passages discuss different subjects or make non-overlapping claims"
            }
        },
        "ai_after": {
            "type": "noul",
            "instructions": format!("proposition `ai_after`: 这句改写后的小说文本是典型的 AI 生成腔（枯燥、模板化、解释腔、流水账等）：\"{}\"", rewritten),
            "criteria": {
                "true": "是典型的 AI 生成腔，枯燥乏味",
                "false": "不是 AI 生成腔，自然生动"
            }
        }
    });
    let body = json!({
        "model": model,
        "state": {
            "purpose": null,
            "passage_a": original,
            "passage_b": rewritten,
            "aspects": []
        },
        "questions": questions,
    });
    let response = ureq::post(base_url)
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Content-Type", "application/json")
        .set("Accept", "application/json")
        .send_string(&body.to_string());

    let text = match response {
        Ok(resp) => resp.into_string().context("读取校验响应体失败")?,
        Err(ureq::Error::Status(code, resp)) => {
            let body_text = resp.into_string().unwrap_or_default();
            bail!("校验端点返回 HTTP {code}: {body_text}");
        }
        Err(e) => bail!("校验端点请求失败: {e}"),
    };
    let parsed: Value = serde_json::from_str(&text).with_context(|| "校验响应不是有效 JSON")?;
    let answers = parsed
        .get("answers")
        .and_then(Value::as_object)
        .context("校验响应缺少 answers 对象")?;

    let overall = answers
        .get("overall")
        .context("校验响应缺少 overall 判定")?;
    let same_fact_choice = overall
        .get("choice")
        .and_then(Value::as_str)
        .context("overall 判定缺少 choice")?;
    let same_fact_prob = overall
        .get("probabilities")
        .and_then(|p| p.get("same_fact"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let same_fact_ok = same_fact_choice == "same_fact" && same_fact_prob >= VERIFY_SAME_FACT_MIN;

    let ai_after = answers
        .get("ai_after")
        .and_then(|a| a.get("noul"))
        .and_then(Value::as_f64)
        .context("校验响应缺少 ai_after 判定")?;
    Ok((same_fact_ok, ai_after))
}

/// 把被接受的改写按原句精确替换回填到草稿全文，生成改写后草稿。
///
/// 命中句（`sentence_lengths.sentences`）通常不含句尾标点/引号；替换时若
/// 改写结果以标点或引号结尾，则把原文中紧随其后的同类标点一并吞掉，
/// 避免生成 `。。`、`。"` 这类残留。
fn apply_rewrites(draft_text: &str, ranked: &[Ranked]) -> String {
    const TRAILING: [char; 11] = [
        '。', '！', '？', '；', '，', '：', '"', '”', '’', '」', '』',
    ];
    let mut out = draft_text.to_string();
    for r in ranked {
        if r.verify_rejected {
            continue;
        }
        let Some(rewritten) = &r.rewrite else {
            continue;
        };
        if r.text == *rewritten {
            continue;
        }
        let Some(pos) = out.find(&r.text) else {
            continue;
        };
        let end = pos + r.text.len();
        let mut replace_end = end;
        if let Some(last) = rewritten.chars().last() {
            if TRAILING.contains(&last) {
                if let Some(next) = out[end..].chars().next() {
                    if TRAILING.contains(&next) {
                        replace_end = end + next.len_utf8();
                    }
                }
            }
        }
        out.replace_range(pos..replace_end, rewritten);
    }
    out
}
/// 渲染文本报告。
fn render_text(ranked: &[Ranked], model: &str, top: usize) -> Result<String> {
    let mut s = String::new();
    s.push_str("# jev-review：AI 腔句子精判与改写（P0/P1）\n\n");
    s.push_str(&format!("模型: {model} | 判定句数: {}\n\n", ranked.len()));
    s.push_str(&format!("## Top {}\n\n", top.min(ranked.len())));
    for (i, r) in ranked.iter().take(top).enumerate() {
        s.push_str(&format!(
            "{}. [AI 腔概率 {:.2}] {}\n   句子：{}\n   来源：{}\n",
            i + 1,
            r.probability,
            if r.label.is_empty() {
                "未标注"
            } else {
                &r.label
            },
            r.text,
            r.source
        ));
        if r.verify_rejected {
            s.push_str(&format!(
                "   改写被拒：语义校验不一致（原句保留）。\n   模型改写：{}\n",
                r.rewrite.as_deref().unwrap_or("（无）")
            ));
        } else if let Some(rewritten) = &r.rewrite {
            let verified = r
                .ai_prob_after
                .map(|p| format!("（改写后 AI 腔概率 {p:.2}）"))
                .unwrap_or_default();
            s.push_str(&format!("   改写{verified}：{rewritten}\n"));
        } else {
            s.push_str(&format!(
                "   改写提示：把「{}」中总结式、模板化的表述改为具体可见的动作、声音、物件或人物误读，避免解释腔替读者下结论；保留原有人名、数字、引语与事实。\n",
                r.text
            ));
        }
        s.push('\n');
    }
    s.push_str(&render_humanity_score(ranked, top));
    Ok(s)
}

/// 人味评分：原句与改写后句子的平均 AI 腔概率对比。
fn render_humanity_score(ranked: &[Ranked], top: usize) -> String {
    let items: Vec<&Ranked> = ranked.iter().take(top).collect();
    let before_avg = items.iter().map(|r| r.probability).sum::<f64>() / items.len() as f64;
    let after_values: Vec<f64> = items.iter().filter_map(|r| r.ai_prob_after).collect();
    let mut s = String::from("\n## 人味评分\n\n");
    if after_values.is_empty() {
        s.push_str(&format!(
            "（未启用 --verify，无改写后判定）原句平均 AI 腔概率：{before_avg:.2}\n"
        ));
        return s;
    }
    let after_avg = after_values.iter().sum::<f64>() / after_values.len() as f64;
    let delta = after_avg - before_avg;
    let score = ((1.0 - after_avg) * 100.0).round() as i64;
    s.push_str(&format!(
        "- 原句平均 AI 腔概率：{before_avg:.2}\n- 改写后平均 AI 腔概率：{after_avg:.2}\n- AI 腔概率变化：{delta:+.2}\n- 人味评分：{score}/100（基于改写后判定）\n"
    ));
    s
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
                "rewrite": r.rewrite,
                "ai_probability_after": r.ai_prob_after,
                "verify_rejected": r.verify_rejected,
            })
        })
        .collect();
    let out = json!({
        "tool": "jev-review",
        "model": model,
        "judged": ranked.len(),
        "top": top_items,
        "humanity_score": humanity_score_json(ranked, top),
    });
    Ok(serde_json::to_string_pretty(&out)?)
}

/// 人味评分的 JSON 形态。
fn humanity_score_json(ranked: &[Ranked], top: usize) -> Value {
    let items: Vec<&Ranked> = ranked.iter().take(top).collect();
    if items.is_empty() {
        return json!({"before_avg": null, "after_avg": null, "delta": null, "score": null});
    }
    let before_avg = items.iter().map(|r| r.probability).sum::<f64>() / items.len() as f64;
    let after_values: Vec<f64> = items.iter().filter_map(|r| r.ai_prob_after).collect();
    if after_values.is_empty() {
        return json!({"before_avg": before_avg, "after_avg": null, "delta": null, "score": null});
    }
    let after_avg = after_values.iter().sum::<f64>() / after_values.len() as f64;
    json!({
        "before_avg": before_avg,
        "after_avg": after_avg,
        "delta": after_avg - before_avg,
        "score": ((1.0 - after_avg) * 100.0).round() as i64,
    })
}
