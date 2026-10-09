# 蛙蛙写作（商业，无开源）

- 站点：https://wawawriter.com
- 文章：https://wawawriter.com/notes/article/6aa7a14c55e21370c5fef631
- 状态：商业产品，无开源仓库，仅作产品形态参照

## 产品能力（与 sentinel 目标最接近的中文产品）

1. **22 类 AI 特征检测** —— 比 sentinel 当前的 regex/template/tracked_terms 更细的分类体系
2. **人味评分 0-100** —— 单数值总览（sentinel 的 `jev-review --verify` 已实现近似：基于改写后 AI 腔概率的评分）
3. **P0/P1/P2 分级逐条改写** —— 与 sentinel review_reminders 的 P1/P2 分级同构；它把告警直接对应到「逐条改写」
4. **改写前后对比** —— jev-review 的「原句 → 改写」对照已实现
5. **反向红线复检** —— 改写后再跑检测，防「越改越糟」；对应 issue #4 的 sentinel 复检环节（尚未自动化）

## 可借鉴

- 22 类特征分类可作为 `jev_classify` 诊断标签的 catalog 参照
- 「反向红线复检」应做成自动步骤：改写 → sentinel 复检 → 若 AI 腔未降级则继续迭代