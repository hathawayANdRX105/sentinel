# Wikipedia: Signs of AI writing

- 页面：https://en.wikipedia.org/wiki/Wikipedia:Signs_of_AI_writing
- 维护：WikiProject AI Cleanup
- 状态：网络不可达未能下载正文，要点从 blader/humanizer（55k★）的 README 归纳

## 核心价值

维基百科编辑用来识别 AI 生成文本的公开模式清单，是「规则式去 AI 味」的事实标准。
blader/humanizer 基于它整理了 26 个模式，分为 6 类：

| 类别 | 模式示例 |
|---|---|
| A. Staging instead of stating | Not X but Y；单行收尾（"Let that sink in"）；假装深刻的格言；铺垫式开场；无对象反驳 |
| B. Rhythm by rule | 强行三连（triad）；重复句首；破折号万能连接；堆叠限定词；被动语态/缺主语 |
| C. Inflation and borrowed authority | 滥用 AI 高频词（delve/testament/landscape）；夸大重要性；模糊关联；销售腔；借权威 |
| D. Formatting by rule | 装饰性加粗；装饰性标题；弯引号 |
| E. Leftovers from chat/draft | 聊天残渣（"Great question!"）；知识边界声明；标题在首句重复；写文档本身而非主题 |
| F. Writing for wrong reader | 向读者重复已知信息 |

## 对 sentinel 的借鉴

- sentinel 的 `review.yaml` 已是中文版同类清单（解释腔/不是A而是B/把字句/比喻模板等），
  可对照 26 模式补漏（如「三连排比」「无对象反驳」「重复句首」）。
- humanizer 的「找 tell → 按清单改写 → 对照原 claims 自检」三步，正是 issue #4 改写闭环的参照。