# Porting Notes

移植过程中的裁决记录：参考实现（worktree `src/`）与 Rust 面之间的差异裁决与证据。

## audit-draft：`--diff` / JSONL / `--learning-source` 未实现（N/A）

历史任务书中"audit-draft CLI 面补全（--diff 变更检测 / JSONL 输出 / --learning-source）"一项
描述过时，Python 参考不存在这些语义，未实现：

- `src/audit/draft.py` `parse_args`（:4646-4668）的全部参数为
  `paths / -i / --sample-limit / --fail-on-warn / --format / -o / --learn-from / --no-corpus-learning`，
  无 `--diff`、无 `--learning-source`。
- 全仓 `grep -rniE '\bdiff' src/ --include='*.py'` 零命中（排除 `differ` 等词形后为空）。
- "JSONL 输出"为旧描述：参考实现输出 JSON 数组（`json.dumps(reports, indent=2)`，
  `-o` 时文件尾带 `\n`），Rust `serde_json::to_string_pretty` + 尾换行写盘已对齐
  （`tests/audit_draft_json.rs` 严格深度相等 + 类型位校验）。

## reports-scorecard：退出与打印归位 CLI 面

Python `reports/scorecard.py main()` 用 `raise SystemExit("No draft chapter files found.")`
（stderr + 退出码 1）并在每步 `print(path)`。Rust 面裁决：
`sentinel::reports::scorecard::run(&ScorecardOptions) -> Result<(i32, Vec<PathBuf>)>`
——库侧不直接 `process::exit`/打印；空输入 `eprintln!` 同文案 + 返回 `(1, 空)`；
`(0, 打印序列)` 由 `main.rs` 逐行 `println!` 后按 rc 退出。CLI 端到端
（stdout 字节 + 退出码 + 空输入 stderr 文案）由 `tests/reports_scorecard.rs`
`scorecard_cli_round` 守护。
## Counter JSON 形状

Python `dict(counter)` 序列化为 JSON **对象**（插入序）。Rust 用新类型
`audit::draft::CountMap`（保序 serialize_map）而非 pair 列表，
对照测试为**零归一化**的严格深度相等（`tests/audit_draft_json.rs`）。
