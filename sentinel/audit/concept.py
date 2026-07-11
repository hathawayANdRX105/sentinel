#!/usr/bin/env python3
"""Audit concept card files for missing fields and category drift."""

from __future__ import annotations

import argparse
import re
from dataclasses import dataclass
from pathlib import Path


HEADING_RE = re.compile(r"^(#{1,6})\s+(.+?)\s*$")
FIELD_RE = re.compile(r"^\s*-\s*([^：:]+)[：:]\s*(.*)$")

COMMON_FIELDS = ("状态", "所属分类", "卡片 ID", "别名 / 英文名", "关联卡片")

CATEGORY_RULES = {
    "characters": {
        "allowed_categories": {"角色", "characters"},
        "required_fields": ("首次生效", "年龄", "当前位置", "时间线状态"),
        "required_headings": ("身份", "外形", "性格", "能力", "关系", "视角与信息边界", "写作约束"),
        "required_heading_fields": {
            "身份": ("身份", "阵营", "社会位置"),
            "外形": ("外形标签", "场面印象"),
            "性格": ("性格关键词", "行为习惯", "对话习惯"),
            "能力": ("能力", "触发条件", "限制"),
            "关系": ("当前关系状态",),
            "视角与信息边界": ("当前能认知什么", "当前不能直接知道什么", "称谓规则"),
        },
        "id_prefix": "CHR-",
    },
    "units": {
        "allowed_categories": {"单位", "units"},
        "required_fields": ("当前状态", "当前所在", "时间线状态"),
        "required_headings": ("定位", "能力", "行为与表现", "写作约束"),
        "required_heading_fields": {
            "定位": ("类型",),
            "能力": ("能力", "触发条件"),
            "行为与表现": ("外观识别点", "平时表现", "危机表现"),
        },
        "id_prefix": "UNT-",
    },
    "technology": {
        "allowed_categories": {"科技", "technology"},
        "required_fields": ("技术等级", "当前适用区域", "时间线状态"),
        "required_headings": ("定位", "核心设定", "社会影响", "写作约束"),
        "required_heading_fields": {
            "定位": ("类型", "谁在掌握", "谁在使用"),
            "核心设定": ("功能", "依赖条件", "限制 / 风险"),
            "社会影响": ("高层用法", "底层用法", "价格 / 门槛"),
        },
        "id_prefix": "TEC-",
    },
    "economy": {
        "allowed_categories": {"经济", "economy"},
        "required_fields": ("适用地点", "时间线状态"),
        "required_headings": ("定位", "运行方式", "叙事作用", "写作约束"),
        "required_heading_fields": {
            "定位": ("类型", "核心规则", "谁受益", "谁吃亏"),
            "运行方式": ("关键凭证 / 货币", "触发条件", "典型场景"),
            "叙事作用": ("能压出什么冲突", "容易显影在哪些场面"),
        },
        "id_prefix": "ECO-",
    },
    "organizations": {
        "allowed_categories": {"组织", "organizations"},
        "required_fields": ("主要地点", "时间线状态"),
        "required_headings": ("定位", "内部状态", "叙事作用", "写作约束"),
        "required_heading_fields": {
            "定位": ("性质", "目标", "对外关系"),
            "内部状态": ("当前问题", "当前优势", "典型做事方式"),
            "叙事作用": ("能推动什么冲突",),
        },
        "id_prefix": "ORG-",
    },
    "items": {
        "allowed_categories": {"物品", "items"},
        "required_fields": ("当前持有者", "当前位置", "数量状态"),
        "required_headings": ("属性", "叙事用途", "写作约束"),
        "required_heading_fields": {
            "属性": ("类型", "功能", "触发条件", "限制"),
            "叙事用途": ("当前作用", "潜在伏笔"),
        },
        "id_prefix": "ITM-",
    },
    "locations": {
        "allowed_categories": {"地点", "locations"},
        "required_fields": ("所在区域", "时间线状态"),
        "required_headings": ("定位", "场面特征", "设定", "写作约束"),
        "required_heading_fields": {
            "定位": ("功能", "所属势力", "常驻人群"),
            "场面特征": ("视觉特征", "声音特征", "气味 / 触感"),
            "设定": ("特殊规则", "触发条件", "风险"),
        },
        "id_prefix": "LOC-",
    },
    "weather": {
        "allowed_categories": {"天气", "weather"},
        "required_fields": ("主要地点", "时间线状态"),
        "required_headings": ("定位", "影响", "叙事作用", "写作约束"),
        "required_heading_fields": {
            "定位": ("类型", "持续条件", "触发来源"),
            "影响": ("对人", "对设备 / 建筑", "对战斗 / 交通"),
            "叙事作用": ("适合用来烘托什么", "不能替代什么真实剧情"),
        },
        "id_prefix": "WTH-",
    },
    "events": {
        "allowed_categories": {"事件", "events"},
        "required_fields": ("发生地点", "时间线位置", "当前状态"),
        "required_headings": ("触发", "过程", "解决与遗留", "写作约束"),
        "required_heading_fields": {
            "触发": ("触发条件", "直接原因", "深层原因"),
            "过程": ("关键参与方", "表层结果", "隐性后果"),
            "解决与遗留": ("当前解决方法", "未解决问题"),
        },
        "id_prefix": "EVT-",
    },
    "meta": {
        "allowed_categories": {"补充", "meta"},
        "required_fields": ("适用范围",),
        "required_headings": (),
        "required_heading_fields": {},
        "id_prefix": "META-",
    },
}


@dataclass
class Warning:
    line_no: int
    kind: str
    message: str
    snippet: str = ""


def normalize_heading(title: str) -> str:
    return title.replace("：", "").replace(":", "").strip()


def parse_fields(lines: list[str]) -> dict[str, tuple[int, str]]:
    fields: dict[str, tuple[int, str]] = {}
    for line_no, line in enumerate(lines, start=1):
        match = FIELD_RE.match(line)
        if match:
            fields[match.group(1).strip()] = (line_no, match.group(2).strip())
    return fields


def parse_headings(lines: list[str]) -> list[tuple[int, str, int]]:
    headings: list[tuple[int, str, int]] = []
    for line_no, line in enumerate(lines, start=1):
        match = HEADING_RE.match(line)
        if match:
            headings.append((line_no, normalize_heading(match.group(2)), len(match.group(1))))
    return headings


def collect_section_lines(lines: list[str], headings: list[tuple[int, str, int]]) -> dict[str, list[str]]:
    sections: dict[str, list[str]] = {}
    boundaries = headings + [(len(lines) + 1, "__END__", 0)]
    for idx, (line_no, title, _level) in enumerate(headings):
        next_line_no = boundaries[idx + 1][0]
        sections[title] = lines[line_no:next_line_no - 1]
    return sections


def parse_section_fields(section_lines: list[str]) -> dict[str, str]:
    values: dict[str, str] = {}
    current: str | None = None
    for line in section_lines:
        match = FIELD_RE.match(line)
        if match:
            current = match.group(1).strip()
            values[current] = match.group(2).strip()
            continue
        if current and line.strip().startswith("- ") and not values[current]:
            values[current] = line.strip()[2:].strip()
    return values


def is_empty_value(value: str) -> bool:
    stripped = value.strip()
    if not stripped:
        return True
    if stripped in {"-", "待补充", "TBD"}:
        return True
    if "已确认 / 待确认" in stripped:
        return True
    return False


def audit_card(path: Path) -> list[Warning]:
    lines = path.read_text(encoding="utf-8").splitlines()
    fields = parse_fields(lines)
    headings = parse_headings(lines)
    sections = collect_section_lines(lines, headings)
    category = path.parent.name
    rule = CATEGORY_RULES.get(category)
    warnings: list[Warning] = []

    if not rule:
        return [Warning(0, "unknown_category", f"未知卡片分类目录：{category}", str(path))]

    for field_name in COMMON_FIELDS + rule["required_fields"]:
        if field_name not in fields:
            warnings.append(Warning(0, "missing_field", f"缺少字段：{field_name}", ""))
            continue
        line_no, value = fields[field_name]
        if is_empty_value(value):
            warnings.append(Warning(line_no, "empty_field", f"字段为空：{field_name}", field_name))

    if "所属分类" in fields:
        line_no, value = fields["所属分类"]
        if value not in rule["allowed_categories"]:
            warnings.append(Warning(line_no, "category_mismatch", f"所属分类与目录不匹配：{value}", value))

    if "卡片 ID" in fields:
        line_no, value = fields["卡片 ID"]
        if is_empty_value(value):
            pass
        elif not value.startswith(rule["id_prefix"]):
            warnings.append(Warning(line_no, "bad_id_prefix", f"卡片 ID 前缀应为 {rule['id_prefix']}", value))

    heading_names = {title for _, title, _ in headings}
    for heading_name in rule["required_headings"]:
        if heading_name not in heading_names:
            warnings.append(Warning(0, "missing_heading", f"缺少段落：{heading_name}", ""))

    for heading_name, required_fields in rule["required_heading_fields"].items():
        section_values = parse_section_fields(sections.get(heading_name, []))
        for field_name in required_fields:
            value = section_values.get(field_name)
            if value is None:
                warnings.append(Warning(0, "missing_section_field", f"{heading_name} 缺少字段：{field_name}", heading_name))
            elif is_empty_value(value):
                warnings.append(Warning(0, "empty_section_field", f"{heading_name} 字段为空：{field_name}", field_name))

    if "关联卡片" in fields:
        line_no, value = fields["关联卡片"]
        if is_empty_value(value):
            warnings.append(Warning(line_no, "empty_relation", "关联卡片为空，后续文件很难追依赖", "关联卡片"))

    return warnings


def format_report(path: Path, warnings: list[Warning]) -> str:
    status = "WARN" if warnings else "OK"
    lines = [
        f"# {path.name}",
        "",
        f"- status: `{status}`",
        f"- warnings: `{len(warnings)}`",
        "",
    ]
    if not warnings:
        lines.append("无警告。")
        return "\n".join(lines)
    lines.append("## Warnings")
    for warning in warnings:
        location = f"L{warning.line_no}" if warning.line_no else "global"
        lines.append(f"- `{location}` `{warning.kind}` {warning.message}")
        if warning.snippet:
            lines.append(f"  - `{warning.snippet}`")
    return "\n".join(lines)


def iter_targets(raw_paths: list[str], include_templates: bool) -> list[Path]:
    targets: list[Path] = []
    for raw in raw_paths:
        path = Path(raw)
        if path.is_dir():
            for candidate in sorted(path.rglob("*.md")):
                if not include_templates and "_templates" in candidate.parts:
                    continue
                if candidate.name.lower() == "readme.md":
                    continue
                targets.append(candidate)
        elif path.is_file():
            if not include_templates and "_templates" in path.parts:
                continue
            if path.name.lower() == "readme.md":
                continue
            targets.append(path)
    return targets


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Audit concept cards for missing fields and category drift.")
    parser.add_argument("paths", nargs="+", help="Card files or directories to inspect")
    parser.add_argument("--include-templates", action="store_true", help="Include template files in the audit")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    targets = iter_targets(args.paths, args.include_templates)
    if not targets:
        raise SystemExit("No concept cards found.")

    warned = False
    for path in targets:
        warnings = audit_card(path)
        print(format_report(path, warnings))
        print()
        if warnings:
            warned = True
    return 1 if warned else 0


if __name__ == "__main__":
    raise SystemExit(main())
