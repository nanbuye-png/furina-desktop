# Furina Desktop

Furina Desktop v0.1.1 是后续唯一开发主线。历史 Furina Agent CLI 已冻结在独立的 GitHub Tag/Release 中，不参与 Desktop 的本地构建或运行。

## 组件

- `desktop/ui`：React + Vite 前端
- `desktop/src-tauri`：Tauri 桌面壳与 IPC
- `crates/furina-core`：Agent、LLM、语音和工具运行时
- `crates/furina-proto`：事件与消息协议
- `crates/furina-soul`：人格、情绪、关系和 Desktop 本地记忆
- `python/furina_tools`：Python 工具 sidecar
- `persona`：人格配置与系统提示词

## 启动

```powershell
Set-Location D:\project\furina\Furina_Desktop
Copy-Item .furina\secrets.env.example .furina\secrets.env
Set-Location desktop\ui
npm install
npm run build
Set-Location ..\..
cargo run -p furina-desktop
```

完整配置和故障排查见 `docs/STARTUP_GUIDE.md`。

## 本地数据边界

Desktop 只使用 `Furina_Desktop/.furina` 下的配置、记忆、语音缓存和 Avatar。密钥、运行数据、构建产物和依赖目录均由 `.gitignore` 排除。
