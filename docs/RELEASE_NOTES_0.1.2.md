# Furina Desktop v0.1.2

Windows x64 稳定公开测试版。

## 主要更新

- 修复 VRM 材质 `髮+` 的黑色高光问题，不修改用户 VRM 文件。
- 新增首次设置向导、LLM/ASR/TTS 配置和运行时热重载。
- 新增安全 VRM 导入、旧 Desktop 数据检测与确认迁移。
- 安装版使用应用专属 AppData，便携版使用解压目录内的 `data`。
- 内置 PyInstaller Python sidecar；sidecar 不可用时聊天、语音和 Avatar 保持可用。
- 提供 NSIS 安装器和免安装 ZIP。

## 下载

- `Furina-Desktop-0.1.2-x64-Setup.exe`
- `Furina-Desktop-0.1.2-x64-Portable.zip`
- `SHA256SUMS.txt`

本版本暂未进行代码签名，Windows SmartScreen 可能显示“未知发布者”。RC1 当前仅保存在本地 `dist` 目录，请使用 `SHA256SUMS.txt` 校验文件完整性；正式公开测试版通过验收后再上传 Release。

## 数据与资产

- 卸载安装版默认保留 AppData 内的配置、密钥和记忆。
- 迁移仅复制旧 Desktop 数据，不删除源目录，也不读取历史 CLI 数据。
- 候选包与后续 Release 均不包含 Furina VRM 或其他用户模型，Avatar 由用户自行导入合法资产。

## 已知限制

- 仅支持 Windows x64。
- 暂无自动更新、代码签名和 ARM64 构建。
- Avatar 大型动作系统推迟到后续版本。
