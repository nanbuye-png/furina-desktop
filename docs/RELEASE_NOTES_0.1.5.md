# Furina Desktop v0.1.5

## 本次更新

- 修复 TTS 会朗读动作、神态、环境声和内心旁白的问题。
- 新增本地流式朗读文本过滤器，不改变聊天窗口显示的原始回复。
- 过滤括号动作、Emoji、颜文字、代码块和 Markdown 噪音，同时保留普通解释性括号内容。
- 保持长回复按句流式合成与播放，不增加额外模型请求。
- Rust 核心增加防御性文本清理，避免其他 TTS 调用路径绕过过滤。

## Windows 下载

- `Furina-Desktop-0.1.5-x64-Setup.exe`：Windows 安装器。
- `Furina-Desktop-0.1.5-x64-Portable.zip`：免安装便携版。
- `SHA256SUMS.txt`：发布文件完整性校验。