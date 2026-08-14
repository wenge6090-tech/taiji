---
description: 画 BCP 蓝图（总装图 + 部件图，Mermaid），先读 Blueprint.md
argument-hint: "[变更描述，可省略]"
---
# BCP 蓝图任务

目标：$@
（若为空，以当前会话上下文中的目标/最近讨论为准）

执行步骤：
1. read `Blueprint.md`，确认现有**设计哲学 + Mermaid 架构图 + 数据流**，再动笔。实现细节（接口/类型）以**代码为准**，经 `AGENTS.md` 路径索引定位，不重复画实现。
2. 输出系统总装图（flowchart）：本次变更影响的模块边界、调用/依赖关系、数据流方向、关键路径标注。
3. 输出核心部件图（classDiagram）：涉及的关键 struct/enum/trait 与字段类型。
4. 附一段说明：模块职责划分、循环依赖风险、数据流断点、本次变更涉及 `Blueprint.md` 的哪些章节。

约束：只画蓝图（Mermaid），不写实现代码；蓝图与 `Blueprint.md` 冲突时以该文件为准（实现命名以代码为准）。用户确认后，下一步走 `/plan` 生成实现计划。
