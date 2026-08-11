# Furina Avatar Asset Specification v1.0

> 制定日期：2026-08-07 · 配套：[FURINA_3D_AVATAR_ROADMAP.md](FURINA_3D_AVATAR_ROADMAP.md)、
> [FURINA_DESKTOP_UI_SPEC_V1.md](FURINA_DESKTOP_UI_SPEC_V1.md)
> 用途：作为未来 Blender 制作 VRM Avatar 的**交付标准**。本轮不制作模型。

---

## 1. 文档目的

本规范定义 Furina Avatar 从制作到接入 Runtime 所需的资产标准，包括模型文件格式、
角色设计资料、VRM 能力要求、制作流程、Runtime 验收标准与版权约束。目标是让
未来交付的 VRM 资产**一次达标**，无需返工即可被 Three.js / Unity 等 Provider 消费。

## 2. 模型文件规范

最终交付：**`Furina.vrm`**

必须包含：

- **VRM humanoid 骨骼**：符合 VRM 标准 humanoid 定义（head / neck / spine / hips /
  arms / legs / fingers 等）
- **表情系统**：BlendShape（VRM Expression）可被 Runtime 驱动
- **视线系统**：LookAt（眼球旋转）可被鼠标/注视点驱动
- **基础动画支持**：包含 idle / talk / wave / thinking 动作（或独立 motion 资源）

模型资产需满足 `FURINA_3D_AVATAR_ROADMAP.md` §5 性能预算：30k–80k tris、Mesh ≤15、
Material ≤6、纹理 ≤6 张（主体 2048²、面部/配件 1024²）。

## 3. 角色设计资料要求

建模前必须准备：

### 参考资料

- 正面参考图
- 侧面参考图
- 背面参考图
- 三视图（含尺寸比例标注）

### 设计资料

- 发型（正面/侧面/背面结构）
- 服装（款式、褶皱走向、层次）
- 饰品（位置、材质）
- 配色方案（主色/辅色/点缀色）
- 材质风格（布纹/金属/半透明/发光等）

## 4. VRM 能力要求

### Expression（最低）

`Neutral` / `Happy` / `Sad` / `Angry` / `Surprised` / `Thinking`

### Lip Sync

- 支持 mouth open（BlendShape）
- 支持 audio driven lip sync（按 `audio_level` 0–1 驱动开合）

### Look At

- 支持 eye tracking（眼球朝注视点旋转）

### Motion（最低）

`Idle` / `Talking` / `Wave` / `Thinking`

## 5. Blender 制作流程

从设计到 VRM 的完整流程（每一步的目的）：

```text
角色设计 → 三视图 → 建模 → 拓扑优化 → UV → 材质 → 骨骼绑定 → BlendShape → 动作制作 → VRM 导出
```

| 步骤 | 目的 |
| --- | --- |
| 角色设计 | 确定形象、气质、配色，作为建模依据 |
| 三视图 | 提供正/侧/背比例基准，避免建模走形 |
| 建模 | 建立角色基础几何体 |
| 拓扑优化 | 动画友好布线（面部/关节环形线），避免表情/变形破面 |
| UV | 为贴图分配坐标，保证纹理不变形 |
| 材质 | 建立 PBR 质感（皮肤/布料/金属/发光） |
| 骨骼绑定 | 按 VRM humanoid 标准绑定并蒙皮 |
| BlendShape | 制作 6 个最低表情的变形 |
| 动作制作 | 制作 idle / talk / wave / thinking 动作 |
| VRM 导出 | 使用 VRM Addon 导出 `Furina.vrm` 并校验 |

## 6. Runtime 验收标准

模型进入 Furina Desktop 前必须验证：

| 项 | 标准 |
| --- | --- |
| VRM 成功加载 | 无报错、模型可见 |
| 表情正常 | 6 个 Expression 均可触发 |
| 视线正常 | LookAt 跟随注视点 |
| 口型正常 | audio_level 驱动 mouth 开合 |
| 动作正常 | idle / talk / wave / thinking 可播放 |
| 性能符合预算 | 加载 <2s、60fps、内存 <300MB（RTX 4060 级别） |

## 7. 版权规范

- Furina 属于 miHoYo / HoYoverse 相关 IP；项目定位为**个人学习 / 同人二创 / 非商业**。
- 官方资源（模型、贴图、立绘、动作）**不得进入仓库**。
- 模型文件**不得提交 git**（VRM 只存本地）。
- `.furina/avatar/` 保持 gitignore。
- 模型须为同人自制或获得授权；如使用第三方素材，遵守其许可协议。

---

本规范与路线图、桌面 UI Spec 共同构成 Avatar 协议与资产标准的唯一依据。
