# Furina 3D Avatar 长期路线图

> 制定日期：2026-08-07 · 配套：[FURINA_PROJECT_FREEZE_REPORT.md](FURINA_PROJECT_FREEZE_REPORT.md)、
> [FURINA_AVATAR_ASSET_SPEC_V1.md](FURINA_AVATAR_ASSET_SPEC_V1.md)、
> [FURINA_DESKTOP_UI_SPEC_V1.md](FURINA_DESKTOP_UI_SPEC_V1.md)
>
> 决策：**放弃 Live2D 过渡方案**，直接规划 3D Avatar 体系。本轮只做规划，不实现、不加依赖、不做模型。

---

## 1. 总体架构

```text
Furina Soul Engine
        ↓
Avatar State Protocol（avatar_expression / avatar_intent，已冻结）
        ↓
Avatar Provider Interface（实现无关）
        ↓
┌──────────────────────────────────────────────┐
│ Placeholder Provider（现状）                  │
│ VRM Provider（Three.js + @pixiv/three-vrm）   │
│ Unity Runtime Provider（未来评估）            │
│ Future Engine Provider（GPU/引擎扩展）        │
└──────────────────────────────────────────────┘
```

Avatar 是纯表现层，**不影响** Agent Core / Memory / Persona / Relationship。

## 2. Avatar Provider 长期说明

**Avatar Provider 不是具体渲染技术**。它只是一个实现无关的适配层，消费统一的
`avatar_state`（表情/意图/口型/视线），输出给具体渲染器。未来更换"身体"（VRM 换模型、
切到 Unity、接新引擎）只替换 Provider，**不触碰** Soul Engine / Persona / Memory /
Relationship / Agent。

```text
Avatar State（协议）
  ↓
Avatar Provider Interface
  ↓
Placeholder / VRM(Three.js) / Unity / Future Engine
```

## 3. 技术路线选择

**放弃 Live2D**，采用 **Blender + VRM + Three.js** 主路线：

| 环节 | 选择 | 理由 |
| --- | --- | --- |
| 建模 | Blender | 免费开源、生态成熟、VRM 插件完善 |
| 格式 | VRM（`.vrm`） | 统一 humanoid 骨骼/表情/视线/动画标准，跨引擎可移植 |
| Web 渲染 | Three.js + `@pixiv/three-vrm` | 与 Tauri WebView / React 同进程，最轻量 |
| 未来扩展 | Unity / 游戏引擎 | 高级动画、全身动作、AI 驱动表情（Phase 6 评估） |

## 4. VRM 技术方案

模型格式：`.vrm`。Avatar 参数映射（与 spec `avatar_expression/avatar_intent` 一致）：

| 协议输入 | VRM 输出 |
| --- | --- |
| `emotion`（6 心情 × 强度） | VRM Expression（BlendShape） |
| `intent`（confident_idle/teasing/comforting/thinking/listening/refused） | Motion（动作组） |
| `speaking` + `audio_level` | LipSync（ParamMouthOpenY） |
| 鼠标位置 | LookAt（Eye Tracking） |
| 待机 | Idle（眨眼/呼吸） |

未接入前，Placeholder Provider（剪影占位）兜底，协议不变。

## 5. VRM Avatar Performance Budget（第一版建议）

> 目的：Furina Desktop 是桌面 AI Agent，不是大型游戏。以下为**建议规范**，不绑定具体模型，
> 待 VRM 接入后按实测校准。

### 兼容目标

- 目标设备：RTX 4060 级别桌面显卡
- 帧率：60fps（Avatar 舞台内）
- 启动：模型加载到首次渲染 < 2s
- 内存：Avatar 相关占用 < 300MB

### 模型复杂度

| 项 | 建议 |
| --- | --- |
| Polygon | 30k – 80k triangles |
| Mesh 数量 | ≤ 15 |
| Material 数量 | ≤ 6 |
| Draw Call | ≤ 30 |

### 纹理规范

| 项 | 建议 |
| --- | --- |
| 纹理数量 | ≤ 6 张 |
| 主体 | 2048×2048 |
| 面部 | 1024×1024 |
| 配件 | 1024×1024 |
| 压缩 | WebP 或等效有损压缩，避免 RGBA 大贴图滥用 |

### 表情规范（最低支持）

`neutral` / `happy` / `sad` / `angry` / `surprised` / `thinking`

### 动作规范（最低支持）

`idle` / `talk` / `wave` / `thinking`

## 6. Blender 制作流程（10 步）

| # | 步骤 | 目的 | 工具 |
| --- | --- | --- | --- |
| 1 | 角色设计 | 确定形象/气质/配色 | 概念图/参考图 |
| 2 | 三视图准备 | 建模比例基准 | 绘图/修图工具 |
| 3 | 建模 | 建立基础几何体 | Blender |
| 4 | 拓扑优化 | 动画友好布线 | Blender（Retopo） |
| 5 | UV | 贴图坐标 | Blender UV Editor |
| 6 | 材质 | PBR 质感 | Blender Shader |
| 7 | 骨骼绑定 | humanoid 蒙皮 | Blender Armature |
| 8 | BlendShape | 表情变形 | Blender Shape Keys |
| 9 | 动作制作 | idle/talk/wave/thinking | Blender Action Editor |
| 10 | VRM 导出 | 统一标准资产 | Blender VRM Addon |

详细交付标准见 **FURINA_AVATAR_ASSET_SPEC_V1.md**。

## 7. 素材准备清单（用户侧）

### 角色设计

- 正面 / 侧面 / 背面参考图、三视图
- 服装设计资料、发型资料、饰品资料
- 配色方案、材质风格（半透明/发光/布纹等）

### 模型制作环境

- Blender（推荐 3.6+）
- Blender VRM Addon
- 贴图工具（Blender 内置 / Substance / 在线工具）
- 骨骼方案（VRM humanoid 标准）

### Avatar 表现清单

- 表情列表：neutral / happy / sad / angry / surprised / thinking
- 动作列表：idle / talk / wave / thinking
- 待机动作、说话动作、互动动作（后续扩展）

## 8. 开发阶段规划

### Phase 1：Avatar Protocol 冻结（本次已完成）

- 目标：`avatar_expression/avatar_intent` 冻结、实现无关
- 输入：现有 spec + 冻结报告
- 输出：协议冻结文档
- 验收：无任何渲染技术绑定

### Phase 2：Placeholder Avatar（现状已具备）

- 目标：占位剪影 + 情绪光效 + 口型点
- 输入：soul_state / avatar_expression
- 输出：可用的占位渲染
- 验收：心情/口型在占位上可见

### Phase 3：VRM Avatar 接入（已完成）

- 目标：Three.js + `@pixiv/three-vrm` 加载 `Furina.vrm`
- 输入：VRM 模型（资产规范达标）+ 性能预算
- 输出：Avatar 舞台显示 3D 形象
- 验收：本地 VRM 可加载，资产缺失时可回退 Placeholder；性能预算继续实测优化

### Phase 4：语音口型（部分完成）

- 当前：已按 speaking 状态驱动基础口型；目标升级为 `audio_level` → 口型跟随
- 输入：TTS 音频电平 + VRM BlendShape
- 输出：说话时口型自然开合
- 验收：口型与音频同步无明显延迟

### Phase 5：表情与动作系统（部分完成）

- 当前：Expression 权重映射已接入；目标继续完成 `emotion → Expression`、`intent → Motion`
- 输入：avatar_expression / avatar_intent
- 输出：心情表情切换、意图动作触发
- 验收：6 表情 × 4 动作可触发且与协议一致

### Phase 6：Unity 高级 Runtime 评估

- 目标：评估 Unity 独立 Runtime（高级动画/全身动作/AI 表情）
- 输入：桌面版现状 + 3D 需求
- 输出：可行性评估与架构方案
- 验收：形成决策，不承诺落地

## 9. 版权规范

- Furina 形象属 miHoYo / HoYoverse 相关 IP；本项目为个人学习/同人二创/非商业。
- 官方资源（模型/贴图/立绘）**不得进入仓库**。
- VRM 模型必须为同人自制或获得授权；模型文件**不提交 git**。
- `.furina/avatar/` 保持 gitignore，素材只存本地。

## 10. Non-Goals（长期）

暂不实现：Live2D、VR、全身实时动作捕捉、Unreal MetaHuman。
