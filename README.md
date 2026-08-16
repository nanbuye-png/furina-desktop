# Furina Desktop

> Windows 桌面版 Personal AI Lifeform：对话、语音、Soul 状态、长期记忆、工具审批与 VRM Avatar。

[![Tauri 2](https://img.shields.io/badge/Desktop-Tauri%202-24c8db)](https://v2.tauri.app/) [![React](https://img.shields.io/badge/UI-React%2018-61dafb)](https://react.dev/) [![Rust](https://img.shields.io/badge/Core-Rust-ce412b)](https://www.rust-lang.org/)

Furina Desktop 是独立的 Desktop 主线，不读取或共享历史 `furina-agent` CLI 的本地数据。当前版本为 `v0.1.4` 开发版，重点是稳定 Runtime、可恢复长任务、受控工具权限、Avatar 口型与 Motion。

## 功能概览

- React + Vite + Tauri 2 桌面界面
- 流式文本对话、ASR、TTS 和可打断播放
- Soul 情绪、关系、记忆与状态面板
- 工具审批、长任务检查点、自动续跑和任务恢复
- `self_status` 等自身只读检查工具、Agent 经验库和改进提案
- Three.js + VRM Avatar：TTS 音频包络驱动口型、命名 Motion API
- Windows NSIS 安装包和便携包由人工审核后发布

## 本地运行（Windows）

### 环境要求

- Windows 10/11
- Rust stable（建议通过 `rustup` 安装）
- Node.js 18+ 和 npm
- Python 3.11+
- WebView2 Runtime

### 1. 克隆项目

```powershell
git clone https://github.com/nanbuye-png/furina-desktop.git
Set-Location furina-desktop
```

### 2. 创建本地密钥文件

```powershell
Copy-Item .furina\secrets.env.example .furina\secrets.env
```

编辑 `.furina/secrets.env`，至少填写当前 LLM 使用的密钥：

```dotenv
FURINA_API_KEY=your-llm-key
```

按需填写 `QWEN_API_KEY`（ASR）、`FISH_AUDIO_API_KEY`（TTS）、`ZHIPU_API_KEY`（视觉）或搜索服务密钥。**不要提交 `.furina/secrets.env`。**

### 3. 安装前端依赖并构建

```powershell
Set-Location desktop\ui
npm install
npm run build
Set-Location ..\..
```

Tauri 会加载 `desktop/ui/dist`；首次启动和修改前端后都需要重新执行 `npm run build`。

### 4. 启动 Desktop

```powershell
cargo run -p furina-desktop
```

首次启动按设置向导填写模型配置；需要 Avatar 时，将合法的 VRM 文件放到：

```text
.furina/avatar/Furina.vrm
```

不放 Avatar 也可以运行文本和语音功能。运行数据、密钥、记忆、音频缓存和 Avatar 均保存在本地 `.furina/`，不会随 Git 提交。

## 开发与验证

前端测试：

```powershell
Set-Location desktop\ui
npm run test
```

Rust workspace 测试：

```powershell
Set-Location ..\..
cargo test --workspace
```

Python sidecar 测试：

```powershell
python -m unittest discover -s python/furina_tools/tests -v
```

只运行前端开发服务器：

```powershell
Set-Location desktop\ui
npm run dev
```

完整启动、配置和故障排查见 [`docs/STARTUP_GUIDE.md`](docs/STARTUP_GUIDE.md)。

## 项目结构

```text
crates/                  Rust Core、协议、Soul
desktop/src-tauri/        Tauri 后端与打包配置
desktop/ui/               React + Vite 前端
python/furina_tools/     Python 工具 sidecar
persona/                 人格与系统提示词
tests/                   Golden fixtures 与验收场景
docs/                    启动、架构、语音和 Avatar 文档
.furina/                 本地配置、密钥、记忆、缓存和 Avatar（Git 忽略）
```

## 隐私与安全

- `.furina/secrets.env`、记忆、Avatar、音频和运行缓存不进入仓库。
- 普通工具受 workspace、权限审批和写入审批约束。
- 自身检查只读、脱敏，并使用源码白名单和路径逃逸检查。
- Agent 经验只保存截断摘要、结果指纹和必要证据，不保存密钥、完整对话或完整命令输出。
- 自身源码、配置和提案不会在无人审批时写入；不自动 Git commit、push、发布或更新自身。
- 当前产品范围固定为 Windows；自动更新/代码签名以及 macOS/Linux 系统级验证已放弃。

## 路线图

| 状态 | 项目 |
| --- | --- |
| ✅ | Desktop 与历史 CLI、Git、运行数据分离 |
| ✅ | React + Vite + Tauri 2 主界面 |
| ✅ | 文本、ASR、TTS、Soul、记忆、工具审批与 Agent Runtime |
| ✅ | 长任务检查点、恢复、自检、经验学习和受控改进提案 |
| ✅ | Three.js + VRM、音频级口型同步与命名 Motion |
| ✅ | UI bundle 拆分：Avatar/设置按需加载、React/Avatar vendor 分包 |
| ✅ | Windows 安装包、便携包与人工发布流程 |
| 🚫 | 自动更新与代码签名 |
| 🚫 | macOS/Linux 系统级验证与跨平台发布 |

## 相关文档

- [`docs/STARTUP_GUIDE.md`](docs/STARTUP_GUIDE.md)：完整启动与故障排查
- [`docs/README.md`](docs/README.md)：文档索引与项目说明
- [`docs/FURINA_3D_AVATAR_ROADMAP.md`](docs/FURINA_3D_AVATAR_ROADMAP.md)：Avatar 路线与性能预算
- [历史 Furina Agent CLI](https://github.com/nanbuye-png/furina-agent)：已冻结，仅供历史参考

## 免责声明

本项目为个人研究和开发项目。使用者应自行确认模型、API、音色、VRM Avatar、字体及其他素材的来源与使用方式合法合规，并自行承担相关服务费用和使用责任。