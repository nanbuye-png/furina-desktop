# Furina Desktop v0.1.2 RC1 验收手册

本手册用于 Windows Sandbox 中的 RC1 人工验收。当前阶段不创建 Tag 或 GitHub Release。

## 1. 宿主机准备

在仓库根目录执行：

~~~powershell
.\scripts\rc\Test-RcArtifacts.ps1
.\scripts\rc\New-RcSandboxKit.ps1
~~~

生成目录为 `dist\rc1-sandbox-kit`，测试结果写入 `dist\rc1-results`；两者都位于已被 Git 忽略的 `dist` 下。

依次启动：

1. `dist\rc1-sandbox-kit\Furina-RC1-Portable.wsb`
2. 完成便携测试后关闭 Sandbox，销毁其中的真实 API Key。
3. `dist\rc1-sandbox-kit\Furina-RC1-Installed.wsb`

若 `.wsb` 无法启动，请先在“启用或关闭 Windows 功能”中启用 Windows Sandbox，并重启 Windows。

## 2. 数据安全规则

- 真实 API Key 只能输入应用设置界面，不得写入结果目录、截图、CSV 或 Markdown。
- 合法视觉 VRM 由测试者临时复制进 Sandbox，不得放入 RC 工具包或结果目录。
- `migration-only-synthetic.vrm` 只用于迁移和解析检查，不用于头发、眨眼、嘴型或表情验收。
- 每完成一种运行模式后关闭 Sandbox，确保密钥和私人 Avatar 被销毁。

## 3. 便携版验收

- 确认环境记录中没有可用的 Python、Node.js、Rust 或 Cargo。
- 确认无需上述开发工具即可启动。
- 在诊断中确认运行模式为 `portable`。
- 确认数据根位于应用目录的 `data\.furina`。
- 检查 sidecar、persona、默认配置和 `portable.flag`。
- 临时将 sidecar 改名，重启应用验证降级；恢复文件名后验证工具恢复。

## 4. 安装版验收

- 运行安装器并记录 SmartScreen 未知发布者提示。
- 完成安装后从快捷方式启动。
- 在诊断中确认运行模式为 `installed`，并记录实际 data root。
- 完成配置、对话和 Avatar 导入后卸载，确认 data root 仍存在。
- 在同一 Sandbox 会话重新安装，确认配置、memory、关系和 Avatar 仍可读取。

## 5. 设置与真实服务

- 完成迁移、LLM、ASR/TTS、Avatar、诊断五步设置。
- 密钥输入和状态只能显示掩码。
- 分别点击 LLM、ASR、TTS 的“测试连接”，每项只调用一次。
- 保存后立即验证文本对话、录音识别和语音播放，不重启应用。
- 修改一项非敏感配置并热重载，确认 Soul、memory、关系和 UI 对话保持。
- 在 Agent 工作、等待审批和录音时验证热重载被阻止。
- 输入错误 provider 配置触发失败，确认旧服务仍可继续使用。

## 6. Avatar 人工验收

- 正面、侧面、背面和头部近距离检查 `髮+`，不得出现黑色覆盖。
- 检查其他头发、皮肤、眼睛和服装没有材质变化。
- 待机观察至少 2 分钟，确认眨眼不会卡住。
- 播放短句、长句和连续 TTS，确认嘴型跟随且结束后闭合。
- 播放过程中打断，确认声音与嘴型同时停止。
- 导入合法视觉 VRM 后重启，确认 Avatar、眨眼和嘴型保持。
- 使用 `invalid-avatar.vrm` 和 `wrong-extension.txt` 验证拒绝与旧模型保留。

## 7. Sidecar 与迁移

- 工作区测试文件：`C:\FurinaRc\<Mode>\workspace\README.txt`。
- 请求读取该文件，先拒绝审批，再批准一次只读调用。
- 旧 Desktop 候选：`C:\project\legacy-desktop`。
- CLI 干扰目录：`C:\project\legacy-cli`，不得出现在候选列表。
- 迁移后确认仅复制 config、dummy secrets、memory 和 synthetic VRM。
- 确认 instance.lock、voice、web cache、日志和 target 未复制，源目录保持不变。
- 再次迁移应被 migration record 阻止；目标已有用户数据时应停止迁移。

## 8. 两小时稳定性测试

选择已完成真实服务配置的运行模式，执行：

~~~powershell
powershell -ExecutionPolicy Bypass -File C:\FurinaRc\Portable\Start-Soak.ps1
~~~

脚本每 30 秒记录 CPU、内存、句柄、线程和窗口响应状态，并每 15 分钟提示执行文本对话、ASR、TTS、TTS 打断和 Avatar 检查。每 30 分钟增加一次只读 sidecar 调用；第 30、90 分钟执行热重载；第 60 分钟执行 Avatar 重载。

完成后检查：

- `soak-samples.csv`
- `soak-summary.json`
- 无崩溃、白屏、冻结、无限加载或音频设备永久占用。
- 30 分钟预热后没有明显持续单向增长且无法回落的内存趋势。

## 9. 记录与门禁

分别在 `C:\FurinaRcResults\Portable` 与 `C:\FurinaRcResults\Installed` 中填写：

- `rc-checklist.csv`：`Status` 使用 `PASS`、`FAIL`、`BLOCKED` 或 `SKIP`。
- `rc-issues.csv`：严重度使用 `P0`、`P1` 或 `P2`；未关闭的 P2 必须填写 `Disposition`。

执行最终门禁：

~~~powershell
powershell -ExecutionPolicy Bypass -File C:\FurinaRcKit\scripts\Test-RcGate.ps1 -ResultDirectory C:\FurinaRcResults\Portable,C:\FurinaRcResults\Installed -RequireSoak
~~~

RC1 通过条件：所有必测项为 PASS、没有未关闭 P0/P1、P2 均有处理决定、2 小时监控通过、安全扫描通过。

## 10. 问题报告要求

记录运行模式、Windows 版本、硬件、复现步骤、预期结果、实际结果和脱敏证据。不得附加完整 `secrets.env`、API Key、私人对话、memory 或未授权 VRM。
