# Furina Desktop 文档索引

本目录保存 Furina Desktop 的启动、架构、Soul、语音和 Avatar 文档。历史 Furina Agent CLI 文档保留在 [furina-agent](https://github.com/nanbuye-png/furina-agent) 仓库，本目录只维护 Desktop 主线。

## 推荐阅读顺序

1. [STARTUP_GUIDE.md](STARTUP_GUIDE.md) — 安装、密钥、构建、启动和故障排查。
2. [FURINA_DESKTOP_UI_SPEC_V1.md](FURINA_DESKTOP_UI_SPEC_V1.md) — Desktop 架构与协议边界。
3. [FURINA_PROJECT_FREEZE_REPORT.md](FURINA_PROJECT_FREEZE_REPORT.md) — 稳定接口与允许演进范围。
4. [FURINA_SOUL_ROADMAP.md](FURINA_SOUL_ROADMAP.md) — 人格、情绪、关系、记忆与主动行为路线。
5. [VOICE_RESEARCH.md](VOICE_RESEARCH.md) — ASR/TTS 技术决策与已验证链路。
6. [FURINA_3D_AVATAR_ROADMAP.md](FURINA_3D_AVATAR_ROADMAP.md) — VRM Avatar 长期路线。
7. [FURINA_AVATAR_ASSET_SPEC_V1.md](FURINA_AVATAR_ASSET_SPEC_V1.md) — 模型制作和验收规范。

## 文档状态

| 文档 | 类型 | 状态 |
| --- | --- | --- |
| STARTUP_GUIDE | 操作指南 | 随当前版本维护 |
| FURINA_DESKTOP_UI_SPEC_V1 | 架构规范 | v1 冻结 |
| FURINA_PROJECT_FREEZE_REPORT | 架构边界 | 随基线更新 |
| FURINA_SOUL_ROADMAP | 长期路线 | 持续演进 |
| VOICE_RESEARCH | 技术记录 | 已实现方案 + 后续事项 |
| FURINA_3D_AVATAR_ROADMAP | 长期路线 | VRM 路线 |
| FURINA_AVATAR_ASSET_SPEC_V1 | 资产规范 | v1 冻结 |

## 当前边界

- Desktop 的源码、Git 和运行数据均独立于历史 CLI。
- Desktop 架构由 Rust Core、Soul、Proto、Python sidecar、Tauri 和 React UI 组成。
- `.furina/secrets.env`、memory、voice、web cache 和 Avatar 均为本地数据，不进入 Git。
- 对冻结协议的破坏性修改应同步更新 UI Spec、Freeze Report 和版本号。
