# refs：类似项目参考资料索引

从网络调研收集的类似项目资料，按「可借鉴实现」分类。每个子目录含 README 或笔记。

## 规则式去 AI 味（与 sentinel 规则层同思路）

| 目录 | 项目 | 亮点 |
|---|---|---|
| `humanizer/` | blader/humanizer（55k★，MIT） | 26 个 AI 写作模式（维基百科 Signs of AI writing）；找 tell → 按清单改写 → 自检；voice matching |
| `llmstrip/` | HugoLopes45/llmstrip（Rust） | 34 条规则（词级 24 + 结构级 10）；同一规则清单既做 CLI 检测又做 LLM 改写 prompt；规则有论文语料依据 |
| `ai-text-humanizer-app/` | DadaNanjesha/AI-Text-Humanizer-App（261★） | spaCy/NLTK 规则式改写，无 LLM |
| `ai_humanizer/` | Firdavs-coder/ai_humanizer（36★） | 本地 Ollama+phi3 改写 + 人性化评分 UI |
| `wawawriter/` | 蛙蛙写作（商业） | 22 类特征检测 + 人味评分 + P0/P1/P2 分级逐条改写 + 反向红线复检 |
| `wikipedia-signs-of-ai-writing/` | Wikipedia: Signs of AI writing | AI 文本模式公共清单源头 |

## 角色扮演 / 设定类 / 文字冒险（issue #5 相关）

| 目录 | 项目 | 亮点 |
|---|---|---|
| `sillytavern/` | SillyTavern（23.6k★） | Character Card v2/v3 设定卡；World Info/Lorebook 关键词触发动态注入 |
| `koboldcpp/` | LostRuins/koboldcpp | 本地 LLM 推理 + 文字冒险生态 |
| `rpg-os/` | croatianrdy2defend-create/RPG-OS | 持久化 LLM RPG：AI 城主、世界/角色文件状态、跨会话恢复 |
| `ai-dungeon/` | AI Dungeon（商业） | 文字冒险鼻祖，规则与叙述分离的教训 |
| `azgaar-fantasy-map/` | Azgaar/Fantasy-Map-Generator | 开源奇幻地图生成（设定类辅助） |

## LLM 判断 / 评估

| 目录 | 项目 | 亮点 |
|---|---|---|
| `prometheus-eval/` | prometheus-eval/prometheus-eval | 开源 LLM-as-judge 评估模型（Prometheus-2） |