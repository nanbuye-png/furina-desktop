# V3-2 语音方案（fish.audio TTS，已实现）

> 2026-08-05 更新：用户提供 fish.audio API key 与自购角色音色包后，选定
> **fish.audio TTS** 为 v1 方案并已实现（`crates/furina-core/src/voice.rs`）。
> 本文档同时记录实现要点与遗留风险。

## 最终方案（已落地）

| 项 | 值 |
| --- | --- |
| 端点 | `POST https://api.fish.audio/v1/tts` |
| 鉴权 | `Authorization: Bearer <FISH_AUDIO_API_KEY>` |
| 模型 | 请求 **header** `model: s2.1-pro-free`（免费）；s2.1-pro / s2-pro / s1 需额度 |
| 请求 | `{"text": "...", "format": "mp3"}`（可选 `reference_id` 指定音色包） |
| 输出 | 音频字节 → 写入 `<root>/.furina/voice/furina_voice_<ms>.mp3`（已 gitignore） |
| 播放 | Windows `cmd /c start "" path`（macOS/Linux 回退 open/xdg-open） |
| 开关 | Desktop 顶部语音开关；`voice.auto_play: true` 可作为默认值 |
| 情绪 | 句首文本标记（S2 方括号语法）：开心/得意 `[happy]`、委屈/难过 `[sad]`、恼火 `[angry]`、淡定不标记 |

## 关键实现要点

- 情绪只影响语气：`VoiceClient::synthesize(text, emotion)` 在句首拼情绪标记，
  文本内容原样透传，不改写任何技术事实。
- `model` 以请求 **header** 传入（body 里放无效，会回落到付费模型返回 402）。
- 默认免费方案：`s2.1-pro-free` + 平台默认音色（`reference_id` 留空），无需充值。
- 自购音色 ID 不进公开仓库：`reference_id` 解析顺序为
  `FURINA_VOICE_REFERENCE_ID`（secrets.env，推荐）→ `config.yaml voice.reference_id`；
  仓库模板留空；**公开音色包（如芙宁娜音色包）在免费模型 `s2.1-pro-free` 下
  直接填 modelID 即可使用，无需充值**。
- 合成前清理文本：跳过 ``` 代码块、去掉 markdown 行首符号、按 `max_text_len`
  （默认 1000 字符）截断，防止长回复被 API 拒绝或朗读出代码。
- **流式同步（2026-08-05 实现）**：LLM SSE 增量 → `Event::MessageDelta` →
  `StreamText`（跳过分析前缀 + 句子切分）→ 逐句显示 + `VoiceQueue` 流水线
  （合成端边收边合成，播放端用 Windows MCI `play wait` 同步顺序播放）。
  文本打字机与语音跟读基本同步；失败静默降级为纯文本（不打断对话）。
- 播放器：弃用 `cmd /c start`（异步无法排队），改用 winmm.dll `mciSendStringW`
  `play ... wait` 同步阻塞播放 mp3（系统自带、零依赖）。
- 音频文件不进入转录、不进入日志、不进入记忆抽取。

## 实测结论（2026-08-05，免费模型已跑通）

- **免费模型实测通过**：header `model: s2.1-pro-free` + 无 reference_id →
  `200` 返回 34KB mp3（平台默认音色，免费）。
- **默认音色随机漂移**：同文本连发 5 次（无 reference_id），每次文件大小与哈希都
  不同——免费模型的默认音色每次合成会变（Fish Speech 默认音色无克隆时即随机）。
  要音色稳定必须指定 `reference_id`。
- **模型放 body 无效**：body 加 `model` 字段会被忽略，回落到付费模型 →
  `402 Insufficient API credit`。这是之前 402 的真正原因之一。
- **公开音色包 + 免费模型实测 200**：`s2.1-pro-free` + 芙宁娜音色包
  `reference_id` → `200`，音色固定。更正早期结论：芙宁娜音色包是**公开音色包**，
  免费模型直接可用、无需充值；早期 402 实为 ASR 接口（`/v1/asr`）按量计费 /
  `model` 参数位置错误所致，与音色包无关。

## 遗留与后续

- ASR（听懂用户说话）：**2026-08-06 已落地**（见下文「语音输入 ASR」），qwen 免费
  额度方案；本地 whisper 留作未来隐私优先选项。
- 3D Avatar 口型同步（V3-3）：需要音频驱动，与 TTS 输出端解耦，桌面版再做
  （路线见 FURINA_3D_AVATAR_ROADMAP.md）。
- 多音色/语速控制：fish.audio 支持更多参数（如 `chunk_length`、参考音频），
  需要时按官方文档扩展；情绪标签完整列表见
  docs.fish.audio/developer-guide/core-features/emotions（额度可用后核对）。
- 版权：仓库不含语音数据/音色包；音色 ID 仅存本地，不随仓库分发。

---

## 2026-08-06 更新：ASR 语音输入落地 + 语音/文本一致性 + 桌面版

### 1. ASR（听懂用户说话）已实现——qwen3.5-omni-flash 免费额度

桌面版按住说话（PTT）的语音输入走 **qwen3.5-omni-flash**（阿里百炼 DashScope，
免费额度），替代 fish.audio `/v1/asr`（按量计费，无额度 402）：

```yaml
asr:
  enabled: true
  provider: qwen        # qwen（默认，免费）| fish（按量计费）
qwen:
  api_key_env: QWEN_API_KEY
  base_url: https://dashscope.aliyuncs.com/compatible-mode/v1
  asr_model: qwen3.5-omni-flash
```

请求格式（OpenAI 兼容 `chat/completions`）：

- content 类型必须是 `{"type": "input_audio", "input_audio": {"data": "data:;base64,<b64>", "format": "wav"}}`
  （不是 `type: audio`，会 400；参考 vibetalking 生产实现核对）；
- 只接受 **wav**（16kHz 单声道 PCM），桌面版前端用 ScriptProcessor 直采 PCM 并编码
  WAV（MediaRecorder 的 webm/opus 无法被识别）；
- 实测：6 秒中文语音转写成功，返回文本（同音字误差属正常 ASR 现象）。

### 2. TTS 双 provider：fish（默认）与 qwen（实验）

`voice.provider`：

- **fish（推荐）**：真实朗读引擎，逐字读文本。`s2.1-pro-free` + 公开音色包
  `reference_id`（如 Furina 音色包）实测返回 200，音色固定；免费模型即可使用。
- **qwen（实验）**：qwen3.5-omni-flash 音频输出（`modalities: ["text","audio"]` +
  `audio.voice` 固定音色 + `stream: true`）。**注意：qwen-omni 是对话模型不是朗读
  引擎**——给它一段文本它会"回话"而不是逐字念，音频内容与输入文本不一致，因此仅作
  实验备用；且 `format: wav/mp3` 实际都返回 24kHz 裸 PCM，代码内会自动补 RIFF/WAVE
  头。情绪标记（`[happy]` 等）是 fish S2 语法，qwen 路径不会前置（会被当正文朗读）。

### 3. 语音与文本不一致的根因与修复

曾出现"语音念的是 LLM 中间思考文本"：LLM 回复含「思考过程 + `---` + 正式回复」两段，
旧流式逻辑在思考文本带句号但未命中关键词时，会把它提前当正文显示并朗读。修复：

- `---` 作为**权威分界线**：之前的文本一律静默缓冲、确认后丢弃，只有之后的句子才
  显示/朗读（Desktop 流式渲染链路）；
- 桌面版 TTS 改为**预合成管线**（合成 worker 边收边合成，播放 worker 顺序播放），
  换行/断句处不再有 1–3s 网络延迟空洞；发送新消息时清空旧队列并作废在途合成。

### 4. 桌面版（Tauri 2）语音链路

```text
按住 🎤 说话 → 浏览器直采 PCM → 16kHz WAV → qwen ASR（免费）→ 文本
    → DeepSeek 生成回复 → fish.audio 音色包 TTS → 可打断播报
```

- 前端按钮/语速/情绪直接复用同一 VoiceClient；`tts_synthesize` 返回
  `{format, data}`，前端按格式播放（wav/mp3）。
- 单实例锁只保护 Desktop 自身的 `.furina/memory`，不与历史 CLI 共享状态。
