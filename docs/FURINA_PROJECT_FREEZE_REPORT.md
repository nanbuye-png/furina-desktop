# Furina Agent 项目冻结报告

> 冻结日期：2026-08-07 · 基线：`main @ ba82316`
> 范围：冻结**协议与架构边界**，不冻结业务迭代。
> 关联文档：[FURINA_DESKTOP_UI_SPEC_V1.md](FURINA_DESKTOP_UI_SPEC_V1.md)、
> [FURINA_3D_AVATAR_ROADMAP.md](FURINA_3D_AVATAR_ROADMAP.md)、
> [FURINA_AVATAR_ASSET_SPEC_V1.md](FURINA_AVATAR_ASSET_SPEC_V1.md)

---

## 1. 当前版本状态

| 项 | 状态 |
| --- | --- |
| Git 基线 | `main @ ba82316` |
| Rust 测试 | 193 项全绿；Python 27 项全绿 |
| 桌面版 | v2（React + Vite）已交付：Avatar 舞台 / 对话工作区 / 灵魂面板 / 运行状态条 |
| 语音 | TTS（fish 免费模型 + 公开音色包）+ ASR（qwen 免费额度） |
| 人格化插话 | LLM 关键节点插话（含表达策略层） |
| 审计修复 | P0 边界安全 / P1 台词数据化 / P2 表达策略 已落地 |

## 2. 已完成模块

- **Soul Engine**：六维连续情绪、心情派生、关系五阶段、三类记忆 + 评分、价值观/行为意图、主动性、动态人格注入（`context_block`）。
- **Persona 系统**：furina.yaml 模板 + `commands:` 台词数据源 + LLM 插话 + 表达策略（theatrical / casual / gentle / serious）。
- **Memory / Relationship**：持久化到 `.furina/memory/`（gitignore），重启不丢；关系为状态非文案。
- **Agent Core**：状态机、权限网关（白名单/危险模式/越界/私有目录拒绝）、上下文压缩、验证闭环、多模型网关。
- **CLI / TUI**：折叠输出、`/展开`、记忆/关系面板、语音控制、联网搜索。
- **Desktop v2**：React 前端、soul_state 完整协议、`get_memories`、插话气泡、审批弹窗。
- **联网搜索 / 语音 / 插话**：Web Intelligence、TTS+ASR、LLM 插话均可用。

## 3. 冻结接口清单（禁止随意修改）

以下接口为**架构边界**，破坏性变更需走版本迁移表或评审（见 §7）：

| 接口 | 冻结内容 |
| --- | --- |
| `furina-proto::Event` | 枚举可**新增变体**（如 Interjection），不得修改既有变体语义 |
| `ChatMessage` / `ToolCall` / `ToolSpec` | 线格式（OpenAI 兼容）稳定 |
| `soul_state` JSON | schema_version `"1.0"`；字段增删走版本迁移表 |
| `avatar_expression` / `avatar_intent` | **实现无关抽象**：不绑定 Cubism / Three.js / Unity / 任何模型 |
| Event Envelope | `{ event_id, event_type, timestamp, payload }` |
| Tauri 命令集 | `chat_send / transcribe / tts_synthesize / approval_respond / get_soul_state / get_memories / doctor / stop_speaking` |
| `PromptContextProvider` | Soul 注入 LLM 的管道接口 |
| Soul 存储格式 | `.furina/memory/*`（emotion/relationship/memory jsonl） |

## 4. 架构状态定义

| 层 | 状态 |
| --- | --- |
| Soul Layer | **Stable** |
| Agent Layer | **Stable** |
| Avatar Protocol | **Frozen** |
| Avatar Provider | **Frozen Interface**（实现未定） |
| Avatar Model | **Asset Preparation Phase**（素材准备中） |
| Rendering Implementation | **Not Started**（占位剪影为现状） |

> 冻结的是**协议与边界**，不是 AI 能力开发。以下业务仍可继续迭代：
> Memory 增强、Voice 优化、Tool 扩展、Agent 能力提升、UI/插话/表达策略演进。

## 5. 演进中（不冻结）

- 语音 provider（fish / qwen 切换）
- 插话与表达策略（词表、规则可扩展）
- 桌面 UI / 布局（信息架构冻结，像素不冻结）
- 多模型网关、联网搜索后端、缓存策略
- 3D Avatar 路线（见路线图，属规划非冻结实现）

## 6. 已知技术债务

| # | 债务 | 说明 |
| --- | --- | --- |
| 1 | 桌面端无 persona 模板层 | 工具中间态用中性状态行（设计取舍，非缺陷） |
| 2 | 跨端渲染统一度 | CLI/TUI/Desktop 渲染实现不同，数据源已统一（`commands:`） |
| 3 | 3D 素材缺失 | 无 VRM 模型/立绘，渲染实现未启动 |
| 4 | 性能预算未实测 | 路线图给出第一版建议值，待 VRM 接入后校准 |

## 7. 后续开发边界

- Avatar 协议不绑定任何渲染实现；更换 Provider（Placeholder → VRM → Unity → Future）不影响
  Soul / Persona / Memory / Relationship / Agent。
- 人格/记忆/权限边界不变：GUI 永不直接修改 Soul State；工具不可读写 `.furina/`、`persona/`。
- 破坏性变更流程：字段升级走 `FURINA_DESKTOP_UI_SPEC_V1.md` §4.4 版本迁移表；新事件走
  Event Envelope；协议级变更需 spec v1.1+ 评审。

---

本报告自冻结之日起作为后续开发的边界依据；业务迭代不受限，协议变更走流程。
