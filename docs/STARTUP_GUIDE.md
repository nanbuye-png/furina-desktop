# Furina Desktop v0.1.2 启动指南

本文只描述独立 Desktop 项目的配置、构建与启动。历史 Furina Agent CLI 不参与 Desktop 运行。

## 1. 前置要求

| 依赖 | 要求 | 用途 |
| --- | --- | --- |
| Windows | 10 / 11 | 当前主要运行平台 |
| Rust | stable | Tauri 后端与 Rust Core |
| Node.js | 18+ | React/Vite 前端构建 |
| Python | 3.11+ | `python/furina_tools` sidecar |

## 2. 进入 Desktop 根目录

```powershell
Set-Location D:\project\furina\Furina_Desktop
```

运行时会自动寻找同时包含 `python/furina_tools` 和 `persona` 的 Desktop 根目录。正常启动不需要设置历史根目录环境变量。

## 3. 配置凭据

首次使用时创建本地密钥文件：

```powershell
Copy-Item .furina\secrets.env.example .furina\secrets.env
```

按实际使用的提供方填写：

```dotenv
FURINA_API_KEY=
FISH_AUDIO_API_KEY=
QWEN_API_KEY=
ZHIPU_API_KEY=
FURINA_VOICE_REFERENCE_ID=
```

`.furina/secrets.env` 已被 Git 忽略，不得提交或复制到文档中。

## 4. 构建前端

```powershell
Set-Location desktop\ui
npm install
npm run build
Set-Location ..\..
```

构建产物位于 `desktop/ui/dist`，由 Tauri 的 `frontendDist` 配置加载。

## 5. 启动 Desktop

```powershell
cargo run -p furina-desktop
```

首次构建会下载 Rust crates。启动后验证文本对话、ASR、TTS、审批弹窗，以及 doctor 返回的 root 与 workspace 均位于 `Furina_Desktop`。

## 6. 独立运行数据

Desktop 只在自身 `.furina` 目录内维护本地状态：

| 路径 | 用途 |
| --- | --- |
| `.furina/config.yaml` | Desktop 配置 |
| `.furina/secrets.env` | 本地 API 凭据 |
| `.furina/memory/` | Desktop 人格、情绪、关系和记忆 |
| `.furina/voice/` | TTS 音频缓存 |
| `.furina/web_cache/` | 网页缓存 |
| `.furina/avatar/Furina.vrm` | 可选 3D Avatar |

这些运行数据不与历史 CLI 目录共享。

## 7. Avatar

需要 3D Avatar 时，将正式 VRM 导出文件复制为 `.furina/avatar/Furina.vrm`。正式资产来源为同级 `Furina_3D/exports_vrm` 项目，Avatar 文件不提交到 Desktop Git 仓库。

## 8. 常见问题

- **找不到 Desktop 根目录：** 从 `Furina_Desktop` 根目录或其子目录启动，并保留 `python/furina_tools` 与 `persona`。
- **缺少密钥：** 确认 `.furina/secrets.env` 已创建，变量名与配置中的 `api_key_env` 一致。
- **前端为空：** 在 `desktop/ui` 重新运行 `npm install` 和 `npm run build`。
- **单实例锁：** 关闭已有 Desktop 窗口后再启动；锁文件位于 `.furina/memory/instance.lock`。
- **网络错误：** 检查系统代理、API endpoint 和对应 API key。

## 9. 验证命令

```powershell
Set-Location desktop\ui
npm run build
Set-Location ..\..
cargo metadata --no-deps --format-version 1
cargo test --workspace
python -m unittest discover -s python/furina_tools/tests -v
```

Cargo workspace 和所有本地 path dependency 必须位于 `Furina_Desktop` 内。
