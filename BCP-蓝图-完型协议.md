# taiji 架构蓝图 — 泛化-压缩认知循环系统（Rust / Rig）

> 蓝图-完型协议 V42（同构统一版）。

> **V42 变更（泛化-压缩循环·同构统一 + 阴阳对偶目录结构 + 易经体语言统一）**：§1 设计哲学全面重写——taiji 的核心动态不再是「周易执行引擎 + 连山压缩期消费者 + 归藏文件系统」三模块，而是**周易（泛化执行）→ 连山（非线性流形压缩）→ 归藏（符号固化）三位一体的同构循环**。归藏资产树与周易递归任务树是同构的——资产树的 fork/merge/prune/backprop 是任务树的 decompose/converge/FAIL/child→parent 在符号层的压缩投影。异层同构从「同深度结构相同」扩展为「三个尺度（单节点 / 任务树 / 资产演化）重复同一阴阳生克模式」。§6 归藏重定性为「冻结的执行经验」并引入**阴阳对偶目录结构**（`yang/` 阳轨生成资产 + `yin/` 阴轨验证资产 + `models/` 跨阴阳后验 + `manifold/` 流型拓扑 + `skills/` 标准化技能）。**BCP→Skills 压缩不是离线编译——通过周易执行将 manifold 压缩为 skills，反作用单节点执行效率**。**本次为蓝图定稿，代码尚未迁移；旧目录结构（prompts/verifications/models/skills）仍为当前代码实际使用。**
>
> **易经体语言统一（V42）：全文采用周易 (Zhouyi) / 连山 (Lianshan) / 归藏 (Guizang) 作为核心命名。神经科学名词 TPN / DMN 降级为工程实现的曾用名（见下方「术语对照」）。代码标识符（`TpnCycle`、`dmn_consumer.rs` 等）暂不改动——蓝图哲学先行，代码逐步跟上。**
>
> **当前状态（2026-08）**：代码 277 pass / 0 failed / 9 ignored，V41 及之前全部落地。**本次仅为 BCP 重写，现有代码不动。**
>
> **架构定论（不可推翻，V42 增补第⑦条）**：① 概率系统不能验证概率系统——收敛验证符号化（L0/L1 机械失败 > LLM 任何裁决，§6.6）；② 归藏不是 RAG 知识库——是压缩固化后的可复用符号系统，拒绝向量库/图库/推理器/分布式/并行写/随机采样（§6.0）；③ 激励问题不需要 ground truth——断言证据链（断言 vs 执行轨迹一致性）机械可判定（§8.22）；④ 最小 MVP 开发范式：每步可独立验收（§8.23）；⑤ 权重微调是模型厂家的事——taiji 不设计微调通道；⑥ 一个模型 + 它的约束系统 = 一个领域学习单元——分区独立演化，契约难度随模型能力自适应（§6.1）；**⑦ 归藏资产树与 周易递归任务树异层同构——fork=decompose、merge=converge、prune=FAIL 终止、backprop=子→父统计上浮（§1.1/§6.0）**。
>
> **版本历史**（全文见文末附录 A）：
> - **V42**：BCP 同构统一——§1 泛化-压缩循环定稿 + §6 归藏重定性（周易·连山·归藏三位一体）
> - **V41**：归藏根目录净化——根 client 不再创建资产层目录
> - **V40**：ChatAgent 提示词简单化——移除归藏摘要注入
> - **V39**：种子复制命令 `taiji seed`
> - **V38**：归藏瘦身——移除 index.yaml + truths 资产层
> - **V37**：模型-领域学习单元 + 多级路由定稿
> - **V36**：归藏按模型分区 + 分区路由
> - **V35-V28**：检索/演化数学化 → 契约化 → 收敛树 → 分封制 → 上下文预算 → 交接
>
> **本文件 = 唯一事实。** 实施约束与避坑规则见 [`AGENTS.md`](./AGENTS.md)（给 AI 自检）。
>
> **文档导航**：术语对照 / §1 设计哲学 / §2 系统概览 / 一、周易执行层（§3-§5、§7-§9 + 工程基建）/ 二、归藏与连山（§6）/ 附录 A。§ 编号全局唯一。

---

## 术语对照（Terminology）

本文档统一采用**易经体系命名**，以准确描述泛化-压缩-固化的动态循环关系。神经科学名词降级为工程实现的曾用名，仅出现在代码标识符引用中。

| 易经名称 | 英文 | 定义 | 工程实现（曾用名） |
|------|------|------|------|
| **周易** | Zhouyi | 泛化执行——概率采样、任务拆解与并行探索。万物流变，每一次任务执行 = 一次蒙特卡洛 rollout。 | TPN（Task Processing Network，任务处理网络） |
| **连山** | Lianshan | 非线性流形发现与压缩——如山峦连绵不绝的隐藏规律。从高维执行迹中发现低维结构，贝叶斯后验 + UCB + MCTS 四算子。纯符号层，零 LLM 调用。 | DMN（Default Mode Network，默认模式网络） |
| **归藏** | Guizang | 符号固化——万物归藏其中。压缩后的可复用符号系统（yang/yin 阴阳对偶 + manifold 流型拓扑 + skills 标准化程序 + models 贝叶斯后验）。冻结的执行经验。 | Guizang（保留同名）、理络 Liluo（旧名） |
| **阳 / 阴** | Yang / Yin | 生成与验证的对偶——阳生（概率采样/执行）、阴克（符号验证/裁决）。贯穿三个尺度的同一股扭矩。 | FittingAgent / CausalAgent |
| **元** | Meta | 权重调节与路由决策——在阴阳之间协调，决策模式（编排/执行）与模型选择。 | MetaAgent |

> **代码命名约定**：本文档中，代码标识符（如 `TpnCycle`、`dmn_consumer.rs`、`LiluoClient`）保持原样不动——蓝图哲学先行，代码逐步跟上。工程实现名与易经名的对应见上表。
>
> **阅读约定**：全文「周易」= TPN、「连山」= DMN、「归藏」= Guizang、「阳 Agent」= FittingAgent、「阴 Agent」= CausalAgent、「元 Agent」= MetaAgent。

---

---

---

---

## 1. 设计哲学

### 1.1 异层同构 (Isomorphic Recursion — Three Scales)

taiji 的全部动力学由一个模式在不同尺度上的重复构成：

```
阳（生成/发散/执行）→ 阴（验证/收敛/裁决）→ 元（调节/更新/路由）→ 再阳生...
```

这个模式同时运行在**三个尺度**上，尺度之间通过压缩关系链接：

| 尺度 | 阳（生成/发散） | 阴（验证/收敛） | 元（调节/更新） | 周易-连山-归藏 |
|------|:---:|:---:|:---:|------|
| **Scale 1：单任务节点** | FittingAgent 概率采样/执行 | CausalAgent 因果验证/裁决 | MetaAgent 权重更新/路由决策 | 周易（变） |
| **Scale 2：任务树拆解** | 父 decompose → 子 spawn 并行执行 | Converge 聚合子结果 / 子失败汇报 | BACK_TO_TPN 再路由 / 父再指导(rerun_of) | 周易（变） |
| **Scale 3：资产演化** | 资产 fork（开新变体假设） | 资产 merge（收敛近邻）/ 资产 prune（淘汰低效） | backprop（四维统计回传 α/β 更新 + UCB 排序更新） | 连山→归藏（藏） |

**三个尺度的同构映射（V42 定论）：**

| 周易任务树操作 | 连山压缩映射 | 归藏资产树操作 | 同构语义 |
|---|---|---|---|
| 父 decompose → 子 spawn | **压缩器提取可复用模式** | **fork** 开变体 | 生成新假设分叉 |
| Converge 聚合子结果 | **统计聚合（加权合并）** | **merge** 合并近邻 | 收敛：成功模式归一 |
| 子 FAIL / 路由终止 | **低回报 + 高变异 → 淘汰** | **prune** 剪枝 | 终止：低效路径消亡 |
| 子→父 统计上浮 | **四维 stats + 贝叶斯后验** | **backprop** 回传 | 经验向上累积 |
| BACK_TO_TPN 重路由 | **UCB 探索项激活新候选** | **检索排序更新** | 不陷入局部最优 |

**结构同构 = 代码事实（已实现，非设计目标）**：周易任务节点在任意 depth 保持相同的三相分工 / 权限配置 / 上下文预算——递归终止仅由 depth guard 保证。资产树同样：任意 variant_of 深度的资产遵守相同的字段契约 / 演化算子 / 统计回传管道。**不为不同深度写不同控制流——无论在任务空间还是资产空间。**

**阴阳配对随尺度不变**：单节点内阳 Agent（Orchestration/Execution 模式）与阴 Agent（Converge/Verify 模式）由 MetaAgent 决策；任务树内父阳拆解与阴 Converge 配对；资产树内 fork（阳发散）与 merge/prune（阴收敛）配对。三个尺度上的阴阳对偶是同构的——生成与验证、发散与收敛、探索与利用，同一股扭矩在不同尺度上的表达。

### 1.2 三相互补 (Tri-Phase Complementarity)

| Agent | 相位 | 易经 | 职责 | 权限面 |
|-------|------|------|------|--------|
| **MetaAgent** | 权重更新·元 | 无极生太极 | 遍历归藏图谱提取推理路径，注入认知偏置 | **认知权 + 收集权**：注册只读收集工具（read / search / webfetch，可联网核实），受 SafetyHook 约束；LLM 多轮收集任务上下文、父层 deliverables、归藏资产与网络信息后更新权重，**按递归层数规则 + 任务难易程度决策阴阳配对模式并编排配对提示词**；归藏只读 |
| **FittingAgent** | 概率拟合·阳 | 阳 | 沿路径发散探索，LLM 做微观概率采样，可递归拆解 | **执行权**：注册 5 个 L1 Skills + causal_verify（全节点）+ recursive_decompose（**仅编排模式节点**），受 SafetyHook + TraceHook 约束（全节点唯一持有变更世界工具的相位） |
| **CausalAgent** | 因果验证·阴 | 阴 | 将结果收敛回符号约束，验证宏观因果性 | **裁判权 + 收集权**：注册只读验证工具（read / webfetch，逐文件核验 + 联网核实），受 SafetyHook 约束；LLM 核验 deliverables 与外部事实后裁决路由（PASS / BACK_TO_TPN / BACK_TO_META）。**编排节点用收敛模板（converge），执行节点用验证模板（verify）** |

周易循环 = 阳生（概率采样）→ 阴克（验证驳回）→ 元调（调整权重）→ 再阳生...，直到收敛。

**循环内权限分工**：执行工具（write / bash / recursive_decompose / causal_verify——变更世界的工具面）收敛于 Fitting 相位；收集工具（read / search / webfetch——只读信息收集与网络核实）为三相共有，Meta / Causal 相位仅持有收集工具、无执行工具。分工是角色性的（执行者 / 认知者 / 裁判者），由工具注册面天然保证，不可被 LLM 动态改变。

### 1.3 神经与符号统一 (Neural-Symbolic Integration)

LLM 是微观概率性的体现——每次 prompt 调用随机、不可精确重现。**归藏是概率迹的符号压缩产物**——prompts/verifications/models/skills 不是"知识"，而是历史 周易执行迹经连山压缩后固化的可复用符号模式。周易循环就是这两种表象的交替：概率采样产生迹（神经侧）→ 连山压缩为符号更新（桥梁）→ 归藏固化为可复用资产（符号侧）→ 下一轮周易被符号资产赋能（神经侧）。

**概率系统不能验证概率系统（V33 定论）**：CausalAgent（阴）验证 FittingAgent（阳）的输出，若验证本身也是 LLM 概率采样，则构成**同源概率回路**——阳与阴共享同一盲区（同语料 / 同训练分布 / 同风格偏好），验证结果不可靠且有实证：MM-JudgeBias（ACL 2026）26 个 SOTA judge 普遍存在**验证完整性失败**（judge 本职是 conditional verification，却退化为 unconditional prediction——按表面流畅度给分）；Reliability without Validity（arXiv 2606.19544）21 个裁判模型「高可靠性低有效性」（一致但不准确）；verbosity / self-preference / position 偏置系统性存在，**scale ≠ reliability**（判断可靠性与通用能力正交）。因此阴面的收敛验证必须**符号化**：确定性验证优先，LLM 验证只在符号层无法表达时介入（§6.6 验证三权分立）。

### 1.4 泛化-压缩循环（周易→连山→归藏，V42 定稿）

taiji 的核心动态不是一个执行引擎加上一个知识库。它是一条**泛化→压缩→固化→赋能**的循环。三个名称不是三个模块，而是同一循环的三个相：

```
                         ┌─────────────────────────┐
                         │     周易（变·泛化）        │
                         │                          │
                         │  周易执行 = 马尔可夫链     │
                         │  · 任务拆解与并行探索      │
                         │  · 阳生（概率采样）        │
                         │  · 阴克（验证/裁决）       │
                         │  · 元调（路由/再指导）     │
                         │                          │
                         │  产出：高维执行迹          │
                         │  (model × prompt × task   │
                         │   × depth × tools ×       │
                         │   cost × pass/fail)       │
                         └──────────┬──────────────┘
                                    │ traces（高维迹）
                                    ▼
                         ┌─────────────────────────┐
                         │    连山（藏·压缩）         │
                         │                          │
                         │  非线性流形发现与压缩      │
                         │  · 贝叶斯后验（α/β）      │
                         │  · UCB 探索/利用          │
                         │  · MCTS 四算子            │
                         │    fork/merge/prune/      │
                         │    backprop               │
                         │  · 模型路由（model_stats） │
                         │                          │
                         │  纯符号层——零 LLM 调用     │
                         └──────────┬──────────────┘
                                    │ 低维符号更新
                                    ▼
                         ┌─────────────────────────┐
                         │    归藏（藏·固化）         │
                         │                          │
                         │  压缩后的符号晶体：        │
                         │  · prompts  = 冻结的      │
                         │    阳行为模板             │
                         │  · verifications = 冻结的 │
                         │    阴验证判据             │
                         │  · models = 冻结的        │
                         │    信念分布（α/β）         │
                         │  · skills = 冻结的        │
                         │    工具使用模式           │
                         └──────────┬──────────────┘
                                    │ UCB 检索注入
                                    ▼
                    (回到 周易——下一轮执行被赋能)
```

**泛化（Generalization）= 周易执行**：周易的每一次任务执行都是一次在高维概率空间中的蒙特卡洛 rollout。产生的是原始的高维迹（哪个模型 × 哪个 prompt × 什么任务类型 × 几层递归 × 用了什么工具 × 消耗多少 token × 通过还是失败）。这些迹的集合构成了非线性流形——某些 (model, prompt, task_type) 组合成功率高、某些低、某些在特定条件下涌现——但原始迹太稀疏太高维，无法直接用于指导下一轮执行。

**压缩（Compression）= 连山发现与压缩**：连山不是"后台数据挖掘"——它是**非线性流形上的压缩算子**。贝叶斯后验更新（α/β）把成功/失败迹压缩为二维信念分布；UCB 排序把多维 (tag × stats) 压缩为一维检索序；fork/merge/prune 把迹的散点聚类为资产变体树；model_stats 把 (model × tag × pass_rate × cost) 压缩为路由表。**所有压缩都是纯符号层的（零 LLM），压缩后的符号资产具有比原始迹低得多的维度、高得多的可复用性。**

**固化（Crystallization）= 归藏存储**：压缩后的符号资产作为独立文件持久化——prompt、verification、model、skill 各一个 YAML。它们不再是"文档"或"配置"，而是**冻结的执行经验**——曾经在某个 周易节点上验证过的行为模板、判据、信念分布。

**赋能（Empowerment）= 归藏回注周易**：下一轮 周易执行时，MetaAgent 通过 UCB 检索加载匹配当前任务的资产，编排为 system prompt（prompts）、验证契约（verifications）、工具注册（skills），注入执行流。此时的 周易节点携带了历史上所有相关任务的压缩经验——它的上下文被**无限扩展**了（不是字节数，而是经验的维度）。

**这就是"压缩即智能"在 taiji 中的精确含义：智能的提升不是更好的 LLM，而是泛化-压缩循环的每一轮都让归藏符号系统积累更多可复用经验，从而让下一轮 周易的推理计算更精准、更省 token、更少失败。四维权重（pass/cost/rounds/quality）的持续增强是这个循环的可测量边界。**

### 1.5 产物契约与交接文件 (Artifact Contract & Handoff)

**执行事实是唯一记忆。** 跨层、跨时间传递的只有产出物（deliverables / task_output / 交接文件）。中间记忆（chat_history、meta_ctx 推理过程）只服务于本节点内部，不得向上传播、不作为结果的事实来源。

**产出即交接：** 每个瞬态 agent（概率拟合）结束时有且仅有三种去向——完成（写最终产出）、上下文超限（写交接产出）、失败/取消（写交接产出）。**交接物 = `deliverables/handoff.md`，是产出物之一**——YAML front matter 携带结构化字段（failure_reason / degraded / output_refs），正文为环境信息（进度 / 剩余工作 / 决策 / 约束状态）。置于 `deliverables/` 内保证**可发现性**：父层（parent_deliverables 注入）、同任务其他 agent（verify/converge 逐文件核验）、元校准（BACK_TO_META 读产出）全部经既有路径自动可见，**不引入新的查找机制**。产出物是递归拆解、恢复、路由判定、元校准的唯一输入物。**V30 会盟扩展**：兄弟贡品（同级子任务 deliverables/）跨兄弟公开可发现可读——分封时注入兄弟贡品索引（`YangPrompt.sibling_deliverables`），读取经既有 read 工具，不引入新查找机制（§8.20）。

- **上下文窗口是单次拟合的采样空间，不是记忆仓库。** 上下文超限 = 采样空间装不下任务 = 任务粒度错误 = 编排失败的运行时硬证据 → 返回阳，阳基于产出文件递归分解
- **不做上下文压缩（特意设计）。** 压缩是把中间记忆塞回下一次拟合、污染新采样；交接是结束本次拟合、留下干净事实、开启新拟合
- **阴（验证/收敛）基于产出核验**：CausalAgent 只读产出文件与交接文件裁决，不消费对话过程
- **恢复 = 前一瞬态产出继承**：崩溃恢复从 `deliverables/`（含 handoff.md）重建，chat_history 仅作本节点断点续聊的最终兜底

### 1.6 第一性原理 (First Principles)

复杂事物由简单事物结构化组成。一个 FittingAgent 可以执行也可以递归拆解（不需要两种类型）、一个 EngineContext 携带 task_dir 根节点和子节点用它做同一件事、一个 Task 结构在不同层代表不同粒度但不改变结构。

### 1.7 心流 (Flow) — 压缩与消溶

taiji 归藏资产按模型分区（§6.1），在 周易执行的不同深度展现不同的"压缩态"：

| 资产类型 | 浅层执行（舒张期） | 深层执行（心流·收缩期） | 压缩态 |
|:---:|------|------|------|
| **prompts/** | UCB 检索 → LLM 编排注入 system prompt | **消溶** — 角色叙事溶解，不再显式出现于 prompt；行为引导内化为 LLM 的选模型式偏好 | 文本→行为 |
| **verifications/** | 注入 verify/converge prompt | **持续** — 机械检查项全程运行，不消溶 | 契约→判定 |
| **models/** | UCB 排序权重（利用 + 探索） | **持续** — α/β 后验持续影响路由与选择 | 迹→信念 |
| **skills/** | 工具注册面（Rig ToolDyn） | **沉淀** — 高频成功模式的统计积累，深层由 skill 统计直接驱动行为（不再依赖 prompt 教学） | 教学→习惯 |

**消溶不是"移除"**：prompts 在深层执行中不再显式注入 system prompt，是因为它们的教学信息已被 shallow layers 内化为 LLM 的行为偏好——prompt 资产从"文本教学"压缩为"统计权重"（通过 models/ 的 α/β 间接影响 UCB 排序）。**心流的本质是泛化-压缩循环在单一任务纵深上的微观投射：浅层泛化（教学引导）→ 深层压缩（消溶/沉淀）→ 产出固化（deliverables + 统计回传）。**

递归加深不是训练，是同一任务的反复穿透。每次穿透的产物：统计数据（四维 stats → 连山 backprop）+ 行为模板更新（归藏 prompts/models 版本递增）。所有资产更新通过连山压缩算子（dmn_consumer）在符号层（YAML 文件）完成，纯云端架构无需本地模型。

### 1.8 类比与隐喻 (Analogies and Metaphors)

taiji 的核心理念植根于两个千年结构的统一：中国古典哲学中的变化与累积模型（周易·连山·归藏），以及现代概率算法（蒙特卡洛/贝叶斯推理/多臂老虎机）。

#### 1.8.1 周易 — 周易执行 · 蒙特卡洛方法

周易三相位循环与周易三爻、MCMC 三步之间的结构同构：

| 周易 (Zhouyi) | 周易递归树 | 现代算法 |
|---|---|---|
| **三爻** (初、中、上) | 三相位 (元Meta / 阳Fitting / 阴Causal) | MCMC 三步：proposal → sampling → acceptance |
| **六爻** (重卦：两经卦相叠) | 两层递归 × 三相位 = 6 步执行路径 | 2-level Monte Carlo rollout |
| **八卦** (2³ = 8 种卦象) | 路由三分支 (PASS/BACK_TO_TPN/BACK_TO_META) 在递归树中展开 = 8 种拓扑路径 | MCTS 8-node search frontier |
| **变卦** (爻变产生新卦) | BACK_TO_TPN / BACK_TO_META → 子任务重入 → 路径分叉 | MCTS backpropagation + re-route |

周易的每一次循环（权重更新 → 概率拟合 → 因果验证 → 路由决策）就是周易中的一次"起卦"——系统在不确定性中做一次概率采样，然后由因果验证裁定吉凶（PASS / 回退）。递归树的展开就是 MCTS 的 selection → expansion → simulation → backpropagation 循环。

#### 1.8.2 连山 — 非线性流形压缩 · 非线性流形发现

"连山"意为连绵的山脉——**非线性流形的地形线**。连山不是后台数据挖掘，而是发现高维执行迹空间中的"山脊线"（哪些 (model × prompt × task × depth) 组合通往成功）并沿山脊线压缩。

| 连山操作 | 流形语义 | 现代对应 |
|---|---|---|
| **贝叶斯后验 (α/β)** | 每个资产在流形上的局部曲率估计（信念分布） | Beta-Bernoulli conjugate model |
| **UCB 排序** | 沿流形边界的探索-利用权衡（高均值 exploit / 高不确定 explore） | Upper Confidence Bound (bandit) |
| **fork** | 在山脊分叉处生成新假设路线 | MCTS expansion |
| **merge** | 相邻平行路线合并（同一山脊） | MCTS node merging |
| **prune** | 谷底路线终止（低回报 + 高变异） | MCTS pruning |
| **model_stats** | 全局地形概览（哪些模型擅长哪些任务类型） | Contextual bandit |

**连山的核心约束：纯符号层。** 所有压缩操作是确定性数学运算（贝叶斯公式 / UCB 不等式 / 统计聚合），不调用 LLM。连山不产生新内容——fork 的新资产内容是参数变体（strictness 档位），不是 LLM 生成的文本。内容演化留给人（手写种子资产）或未来的 SkillCompiler（从迹中提取可复用模板——仍然是纯符号，不调 LLM 生成）。

#### 1.8.3 归藏 — 符号固化 · 压缩即智能

"归藏"意为归藏万物——**万物（执行迹）经过压缩后归入符号仓库**。归藏不是知识库、不是 RAG、不是向量存储。它是**压缩后的执行经验的晶体化**：

| 归藏资产类型 | 压缩了什么 | 消费方 |
|---|---|---|
| **prompts/** | 阳 Agent 的**成功行为模板**——哪些教学指令在哪些任务类型上被验证有效 | MetaAgent → LLM 编排 → Fitting/Causal system prompt |
| **verifications/** | 阴 Agent 的**成功验证判据**——哪些检查项在哪些任务上有效拦截了不合格产出 | ContractEngine 机械执行 → LLM 裁决 |
| **models/** | 每个资产的**信念分布（α/β）**——该资产在历史上的通过/失败经验压缩为 Beta 分布 | UCB 排序 / 演化决策 |
| **skills/** | **工具使用模式**——哪些工具调用序列在哪些场景下成功率高 | SkillRegistry → Rig Tool 注册 |

**每一个资产 = 一段曾经有效的执行经验的压缩投影。** 资产的 confidence（人工种子先验）→ stats 四维统计（连山回传）→ ModelAsset α/β（贝叶斯后验）→ 演化决策（fork/merge/prune）——这个生命周期就是"迹→压缩→固化→再执行→再迹"的循环在资产维度的体现。

#### 1.8.4 三位一体：周易·连山·归藏的统一

```
    周易（变·泛化）              连山（藏·压缩）           归藏（藏·固化）
    ─────────                   ─────────                ─────────
    马可夫链 + 递归树          贝叶斯 + UCB + MCTS      符号化的可复用资产
    执行 · 探索 · 生成         发现 · 压缩 · 演化        存储 · 检索 · 赋能
          │                        │                        │
          │  traces（高维迹）      │  低维符号更新          │  注入
          ├──────────────────────►├───────────────────────►│
          │                        │                        │
          │                        │  fork/merge/prune       │  prompts
          │                        │  backprop (α/β)         │  verifications
          │                        │  UCB re-rank            │  models
          │                        │                         │  skills
          │                        │                         │
          │◄───────────────────────┴────────────────────────┘
          │         下一轮执行被更优资产赋能
          │
    泛化（执行产生新迹）─────────► 压缩（迹→统计→符号更新）
    ◄────────────────────────── 压缩即智能
```

三者不是三个模块、五个层。它们是**同一股认知扭矩在三个时间尺度上的表达**：
- **周易** = 秒~分钟级的执行（单个 周易循环）
- **连山** = 分钟~小时级的压缩（任务结束后 backprop + evolve）
- **归藏** = 跨任务的持久积累（资产树的代际演化）

**异层同构的最终形态（V42）：周易递归任务树 (task tree) 与归藏资产变体树 (asset variant tree) 是同构的——fork = decompose、merge = converge、prune = FAIL 终止、backprop = child→parent 统计上浮。归藏不是"另一个系统"，它是 周易在符号层的压缩投影。BCP 人类可读的蓝图协议也将被压缩为 skills（太极项目式标准化可复用程序），最终反作用于单任务节点的执行效率——完成压缩-泛化的完整闭环。**

---


## 2. 系统概览

### 核心概念

| 组件 | 角色 | 运行时行为 | 周易-连山-归藏 |
|------|------|------|:---:|
| **归藏 (Guizang)** | **压缩固化后的可复用符号系统**（V42 阴阳对偶结构） | 按模型分区的符号资产树（yang/yin 阴阳对偶 + manifold 流型拓扑 + skills 标准化程序 + models 贝叶斯后验），周易执行期 UCB 检索注入→只读，连山压缩期 backprop+evolve→单写。`{model_key}/yang/`（阳轨生成资产：prompts/workflows/skills）+ `{model_key}/yin/`（阴轨验证资产：prompts/verifications/skills）+ `{model_key}/manifold/`（流型拓扑：BCP+AGENTS+接口契约+环境）+ `{model_key}/skills/`（标准化 Skills，从 manifold 经周易压缩而来）+ `{model_key}/models/`（跨阴阳贝叶斯后验）。`model_stats.yaml` 恒在 knowledge 根。 | 归藏 |
| **MetaAgent** | 权重更新·元 | 查询归藏（UCB 选择 prompts + verifications + models + skills）→ LLM 编排 system prompt → 决策阴阳配对模式 + 模型路由 → 产出 MetaContext | 周易 |
| **FittingAgent** | 概率拟合·阳 | 瞬态 Rig Agent，注册 5 L1 Skills + causal_verify（全节点）+ recursive_decompose（仅编排模式）。受上下文预算约束（V29） | 周易 |
| **CausalAgent** | 因果验证·阴 | 瞬态 Rig Agent（双模式 verify/converge）。前置管线：ConstraintEngine → ContractEngine 机械执行 → LLM 裁决 | 周易 |
| **ChatAgent** | 前端对话 Agent | 长生命周期 Rig Agent，注册 5 L1 Skills + SafetyHook。**不进 周易循环**，会话历史持久化到 `.taiji/chat/` | 周易（旁路） |
| **连山 (DMN)** | **非线性流形压缩算子**（V42 重定性——非"后台消费者"） | 被动学习（周易 PASS → pending → backprop 四维 stats + 贝叶斯后验 → evolve fork/merge/prune）+ 主动学习（空闲窗口 UCB 探索任务）。**纯符号层，零 LLM 调用**。代码已实现，`--with-dmn` flag 激活 | 连山 |
| **AgentFactory** | 瞬态 Agent 工厂 | 中枢组件，持有基础设施 Arc 引用（ProviderRegistry / GuizangClient / WorkerPool / ConstraintEngine） | — |

### 技术栈

| 组件 | 选型 |
|------|------|
| 语言 | Rust 2024 edition |
| LLM Agent | Rig v0.39（Agent + dynamic_context + structured output） |
| LLM Provider | Rig deepseek::Client |
| 知识库 | 文件系统 YAML（`.taiji/knowledge/`） |
| 异步 | tokio（spawn 并发子任务 + WebSocket + 流式 Agent） |
| 流式 | Rig `stream_chat()` → `MultiTurnStreamItem` → WS chunk 推送 |
| CLI | clap（run/trace/list/init/mcp/**serve**） |
| 序列化 | serde + serde_json + serde_yaml |
| 追踪 | tracing + TraceHook + 手动 TraceWriter |
| **Web 服务** | **axum + tower-http（Rust 核心进程内嵌 HTTP 静态托管 + WebSocket 双向）** |
| **前端 UI** | **React 18 + TypeScript + TailwindCSS（Vite 构建，纯浏览器运行）** |
| **图渲染** | **React Flow（纺锤树 + 周易流程图）** |
| **动画** | **Framer Motion（节点生长 / 状态变色）** |
| **实时推送** | **WebSocket 双向（tokio-tungstenite，Rust ↔ React 事件 + 请求/响应）** |
| **太极图** | **纯 CSS/SVG 动画（60s 旋转 + 状态联动光晕）** |

### 架构总纲

```mermaid
flowchart TD
    USER["taiji run <description>"] --> CONFIG["TaijiConfig::load()"]
    CONFIG --> PVR["ProviderRegistry::init(config)"]
    PVR --> GUIZANG["GuizangClient::new(.taiji/knowledge/)"]
    GUIZANG --> FACTORY["AgentFactory::new(config, guizang, providers)"]
    FACTORY --> RUNNER["RecursiveRunner::new(factory)"]
    RUNNER --> EXECUTE["runner.execute(task_id, desc)"]
    EXECUTE --> INIT["init task dir (data/tasks/{task_id}/)"]

    subgraph "周易循环"
        INIT --> META["① 权重更新 (元·MetaAgent)\n标签匹配 Prompts + 置信度排序 → MetaContext"]
        META -->         FIT["② 概率拟合 (阳·FittingAgent) LLM loop（上下文预算 §8.19）\nrecursive_decompose / causal_verify\n5 个内置 L1 Skills (read/write/bash/search/webfetch)"]
        FIT --> VERIFY["③ 因果验证 (阴)\nConstraintEngine → ContractEngine → LLM 裁决\nverify() → VerificationReport"]
    end

    VERIFY --> ROUTE{"因果验证路由"}
    ROUTE -->|"执行偏差: BACK_TO_TPN"| FIT
    ROUTE -->|"认知偏差: BACK_TO_META"| META
    ROUTE -->|"收敛: PASS"| DONE["输出 TPNResult → 连山"]
```

---



---

## 周易与连山的关系

周易、连山、归藏是同一泛化-压缩循环的三个相（§1.4），不是两个模块之间的接口。

### 三位一体：周易·连山·归藏

| 相 | 代码中的体现 | 方向 | 语义 |
|------|------|------|------|
| **周易）** | `RecursiveRunner` + `TpnCycle` + `FittingAgent`/`CausalAgent`/`MetaAgent` | 前向·泛化 | 执行马尔可夫链——每次任务 = 一次蒙特卡洛 rollout，产生高维迹 |
| **连山（连山）** | `dmn_consumer` + `cognition_evolver` + `ModelRouter` | 反向·压缩 | 非线性流形发现——把高维迹压缩为低维符号更新（α/β、UCB 排序、fork/merge/prune） |
| **归藏（存储）** | `LiluoClient` + `knowledge/` 资产树 | 固化 | 低维符号持久化——yang/yin 阴阳对偶 + manifold 流型拓扑 + skills 标准化程序 + models 贝叶斯后验 |

同一棵资产树：周易在树上前向消费（检索注入），连山在树上反向压缩（统计回传），归藏是树的持久态。

### 权限关系（§8.3）

- **周易）执行期只读归藏**——任何 Agent（Meta / Fitting / Causal / ContractEngine）不得写资产
- **连山是唯一写者**（单线程后台任务，`--with-dmn` 激活），写路径 = pending / experiments 队列
- **分区一致性**：任务级路由下任务内所有 Agent 使用同一分区（按路由模型 model_key），`MetaContext.model` 是唯一载体；V37 相位级路由激活后各相位按其路由模型用对应分区（§8.8）

### 数据流：归藏 → 周易（前向 · 检索注入）

```
ModelRouter（读 model_stats 元权重表，纯符号层）
  → LiluoClient.for_model(model_key) 分区检索
  → UCB 排序（利用 + 探索，§6.3）
  → MetaAgent LLM 编排 system prompt（模式决策 + 资产组合）
  → MetaContext { mode, model, assets_used, prompts } 注入 Fitting / Causal
另外两路只读消费：
  → ContractEngine 加载 verifications/ 机械验证（§8.22）
  → ConstraintEngine L0 输出健全性检查（内置硬编码，Hard 短路——V38 起不再读 truths/ 资产层）
```

### 数据流：周易 → 连山（反向 · 统计回传）

```
周易 PASS
  → enqueue_dmn_pending（pending/{task_id}.json：assets_used + checks + passed + model_key）
  → 连山消费（单写者，指数退避轮询）
  → backprop：频率四维（n / pass_count / cost / rounds / quality）+ 贝叶斯后验（α/β，§6.4.1）
  → evolve_contracts：fork / merge / prune 四算子（verifications 与 prompts 对称）
  → model_stats 更新（元权重表，模型路由数据源）
  → 下轮周易自动加载更新后的认知偏置（藏 → 变）
```

### 主动学习（连山 → 周易 反向触发）

空闲窗口（pending 空 + 预算内）→ DMN 选 UCB 探索分最大的活跃变体资产 → 写入 `experiments/` 队列 → TPN runner 执行模板化探索任务（Execution / 最小预算 / 不递归）→ ContractEngine 机械验证变体契约 → CheckResult 回传 pending → DMN 更新。护栏：探索任务不产生新探索任务，学习环有界（§6.4）。

### 触发链时序

```
周易执行（只读归藏）→ 产出 deliverables / trace / verify_state
  → PASS 入队 pending ──→ 连山压缩算子 回传（backprop → evolve → model_stats）
  → 资产版本++（分区写入）──→ 下轮 MetaAgent 检索到新资产 → 周易行为被引导
```

### 章节导航

| 编号 | 内容 | 章节 |
|------|------|------|
| §1 · §2 | 设计哲学 · 系统概览 | 本文档开头 |
| §3 · §4 · §5 | 模块架构 · 核心类型 · 周易执行流 | 一、TPN |
| §6.6 | 验证三权分立（周易验证机制） | 一、TPN |
| §7 | 运行时布局 | 一、TPN |
| §8（周易侧 16 项） | 8.1/8.2/8.4-8.6/8.9-8.11/8.14-8.20/8.22 | 一、TPN |
| §8.7 | Rig Vendor（工程基建） | 一、TPN 末尾 |
| §9 | 前端架构 | 一、TPN |
| §6（0-5, 6.4.1） | 归藏本体 · 检索 · 演化 · 真值维护 | 二、DMN |
| §8侧 5 项） | 8.3/8.8/8.12/8.21/8.23（8.13 并入 §6.5） | 二、DMN |
| 附录 A | 版本历史 | 文末 |

---

---

## 一、TPN（任务处理网络）

## 3. 模块架构

### 七层模块图

```mermaid
flowchart TB
    subgraph "L6 入口"
        MAIN["main.rs — clap CLI"]
    end

    subgraph "L5 MCP"
        MCP_SRV["mcp/server.rs — 暴露 taiji 工具"]
        MCP_CLI["mcp/client.rs — 消费外部 MCP 工具"]
    end

    subgraph "L4 编排"
        RUNNER["runner — RecursiveRunner (薄包装)"]
        CONST["constraint_engine — ConstraintEngine"]
        CONTRACT["contract_engine — ContractEngine (V33 验证契约机械执行)"]
        TRIG["trigger_engine — SkillTriggerEngine"]
        WORKER["worker_pool — WorkerPool"]
        DMN["dmn_consumer — 连山压缩算子 (后台，可激活)"]
    end

    subgraph "L3 Agent"
        FACTORY["factory — AgentFactory (中枢)"]
        META_B["meta — MetaAgent 构建器"]
        FIT_B["fitting — FittingAgent 构建器"]
        CAUSAL_B["causal — CausalAgent 构建器"]
        PLAN_B["plan — PlanBuilder (预演编排)"]
        CHAT_B["chat — ChatAgentBuilder (聊天面板)"]
        TOOLS["tools/ — recursive_decompose, causal_verify"]
    end

    subgraph "L2 Hook"
        SAFETY["safety — ToolSafetyGuard (AgentHook)"]
        TRACE_H["trace — TraceHook (AgentHook)"]
    end

    subgraph "L1 基础设施"
        PROVIDER["provider — ProviderRegistry"]
        GUIZANG["knowledge — GuizangClient (文件系统读写)"]
        CONFIG["config — TaijiConfig"]
        ERR["error — TaijiError"]
        TRACE_W["trace — TraceWriter (JSONL)"]
        TSPEC["task_spec — TaskSpec 解析"]
    end

    subgraph "L7 前端"
        WEB["taiji-web React App (浏览器)"]
    end

    subgraph "L6 实时事件 + HTTP"
        WS_SRV["ws/server.rs — WebSocket 事件推送 + 请求响应"]
        WS_HANDLER["ws/handler.rs — 客户端请求分发"]
        WS_TYPES["ws/types.rs — TaskTreeSnapshot / TpnPhaseState / ClientMessage / ServerResponse"]
        HTTP_SRV["main.rs serve — axum HTTP 静态托管 (dist/)"]
    end

    subgraph "L0 基础类型"
        TYPES["types/ — task, agent, verification, execution, frontend"]
    end

    MAIN --> CONFIG & RUNNER
    RUNNER --> FACTORY
    FACTORY --> PROVIDER & GUIZANG & TRIG & TYPES
    FACTORY --> META_B & FIT_B & CAUSAL_B & PLAN_B
    FIT_B --> TOOLS & SAFETY & TRACE_H
    TOOLS --> FACTORY
    META_B --> GUIZANG
    CAUSAL_B --> CONST
    CAUSAL_B --> CONTRACT
    连山 --> GUIZANG
    MCP_SRV --> FACTORY
    MCP_CLI --> FIT_B
    WS_SRV --> RUNNER & FACTORY & TYPES
    WS_HANDLER --> FACTORY & TYPES
    WS_HANDLER --> CHAT_B
    CHAT_B --> FACTORY
    CHAT_B --> SAFETY
    WEB --> WS_SRV
    HTTP_SRV --> WEB
```

### 模块职责

| 层 | 模块 | 职责 |
|----|------|------|
| L0 | types/ | Task, MetaContext, VerificationReport, TaskTreeSnapshot, TpnPhaseState 等核心类型定义 |
| L1 | infra/config | TaijiConfig 加载与验证 |
| L1 | infra/error | TaijiError 枚举（含 context 字段） |
| L1 | infra/provider | ProviderRegistry：Rig client 管理（创建/复用/fallback） |
| L1 | infra/knowledge | KnowledgeStore：**按模型分区的归藏读写 + 标签搜索 + UCB 聚合查询 + model_stats 读写 + 验证契约加载**（V32 重构 / V33 契约读取） |
| L1 | infra/trace | TraceWriter：JSONL 写入 + 10MB 轮转 + read_tree 合并 |
| L2 | hooks/safety | ToolSafetyGuard：路径穿越 / 命令注入 / SSRF 拦截 |
| L2 | hooks/trace | TraceHook：自动捕获 StepEvent 写入 trace.jsonl |
| L3 | agents/factory | AgentFactory：持有所有 Arc 引用，创建三种瞬态 Agent |
| L3 | agents/meta | MetaAgentBuilder：动态上下文注入，**UCB 检索归藏 + 模型路由决策**（V32） |
| L3 | agents/fitting | FittingAgentBuilder：recursive_decompose + causal_verify + 5 个内置 Skills（read/write/bash/search/webfetch），同时支持前端 agent 通过 MCP ExternalContext 注入额外上下文 |
| L3 | agents/causal | CausalAgentBuilder：verify 模式 + converge 模式。verify 前置 ContractEngine（L0/L1 机械检查）→ LLM 只裁决 llm_judgement 项（L2 兜底，V33 §6.6） |
| L3 | agents/chat | ChatAgentBuilder：前端聊天面板 Rig Agent。组装 5 个 L1 Skills + SafetyHook，`stream_chat()` 推流，`max_turns=20`。会话持久化到 `chat_history.json`。与 周易循环完全解耦 |
| L3 | agents/tools | recursive_decompose / causal_verify（Skills 不再内置于此模块） |
| L3 | agents/plan | PlanBuilder：MetaAgent + LLM 编排执行计划，输出 PlanSummary（不进 周易循环） |
| L4 | orchestration/runner | RecursiveRunner：创建根任务 + 周易循环 |
| L4 | orchestration/constraint_engine | 加载 Truths 约束 + 前置检查 |
| L4 | orchestration/contract_engine | **V33 新增**：加载 verifications/ 结构化验证契约 → 机械执行 checks（file_exists / schema_valid / reference_resolves / command_succeeds / llm_judgement）→ 产出 ContractReport（L0 机械 + L1 契约确定性裁决，hard 失败直接短路，LLM 不可翻案——§6.6/§8.22） |
| L4 | orchestration/trigger_engine | 正则 + 标签匹配 Skills |
| L4 | orchestration/worker_pool | Semaphore 限并发 + RateLimiter |
| L4 | orchestration/dmn_consumer | 后台轮询 pending 队列（被动学习）+ experiments 队列（主动学习，空闲窗口+预算），执行 MCTS 四算子 + model_stats 更新（代码已实现，可激活 — 见 §8.12/§8.21） |
| L5 | mcp/server | MCP Server：暴露 TPN/DMN/归藏 操作，6 个工具（taiji_plan / taiji_run / taiji_explain / taiji_trace / taiji_list / taiji_status） |
| L5 | mcp/client | MCP Client Manager：连接外部服务器 |
| L6 | ws/server | WebSocket Server：接受客户端连接，广播 TaskEvent 事件 + 接收 ClientMessage 请求，通过 handler 分发并返回 ServerResponse |
| L6 | ws/handler | WS 请求分发器：execute_task / submit_review / list_tasks / get_task_tree / get_tpn_state / chat_message（委托 ChatAgent，通过 mpsc 逐 chunk 推流） |
| L6 | ws/types | WebSocket 消息类型：`TaskEvent`（广播）、`ClientMessage`（前端→核心）、`ServerResponse`（请求响应） |
| L6 | main.rs serve | axum HTTP 服务器：托管 `taiji-web/dist/` 静态文件 + 可选自动打开浏览器（xdg-open） |
| L7 | taiji-web | 纯浏览器 React 前端：纺锤树（SpindleTree）、TPN 弹窗（TpnPopup）、太极背景（TaijiBg）、聊天面板（ChatPanel） |

### 关键接口契约

| # | 契约 | 说明 |
|---|------|------|
| 1 | `RecursiveDecomposeTool.execute(subtasks: Vec[SubtaskSpec]) -> DecomposeResult` | 输入 LLM 拆解的子任务 → spawn 子 FittingAgent → JoinSet 收集 → CausalAgent.converge() → 返回收敛结果。**仅编排模式 FittingAgent 注册**（执行模式 LLM 不可见拆解工具）；递归终止由 depth guard 保证；WorkerPool permit 在工具入口 acquire（并行分解节点上限），join 完成后释放，无嵌套持有 → 无死锁。**V30 会盟**：spawn 时收集兄弟贡品索引注入子 `YangPrompt.sibling_deliverables`（BTreeMap 有序扫描，排除自身，失败上抛——无降级 §8.20）。**V31 失败汇报**：子任务任务级失败**不整体上抛**——构造 Diverged 失败条目（`failure_reason`/`failure_kind` + handoff 交接产物路径）进 child_results，收敛树不中断；取消/panic 仍硬中止（§8.18） |
| 2 | `AgentFactory.create_fitting_agent(depth, meta_ctx, engine_ctx, cancel) -> FittingAgentBuilder` | 从 MetaContext（含 `mode`）+ EngineContext + CancellationToken + 归藏 创建阳 Agent，模式随 meta_ctx 传递 |
| 3 | `FittingAgentBuilder { depth, mode, meta_ctx, engine_ctx, factory, model, cancel: CancellationToken }` | 阳 Agent 构建器，**按模式选模板**（编排模板 / 执行模板）；recursive_decompose 仅编排模式注册。**V30 身份自觉**：run() 注入「身份与地位」段（身份册 + mode + 兄弟贡品索引，`build_identity_section`，读册失败上抛——无降级 §8.20） |
| 4 | `SafetyHook (AgentHook)` | 在 ToolCall 事件上检查路径穿越/命令注入/SSRF，返回 Flow::cont() 或 Flow::skip() |
| 5 | `ConstraintEngine.check_constraints(output, constraints) -> ConstraintResult` | CausalAgent.verify 前置检查，Hard 违反直接短路返回 BACK_TO_META |
| 6 | `MetaAgentBuilder.run(task_description, task_type_tags, handoff: Option<HandoffContext>) -> MetaContext`（builder 经 `depth()` / `max_depth()` 注入递归层数规则） | 查询归藏 Prompts 标签匹配 → 置信度排序 → **按深度规则 + 难度决策配对模式** → LLM 编排三份 system prompt（fitting/verify/converge，与所选模式配对）→ 注入 MetaContext（含 mode）；无归藏资产时降级返回 MetaContext::empty()（mode 默认 Orchestration）。**V28：BACK_TO_META 重跑时 `handoff` 注入前一瞬态产出摘要**（deliverables/ 索引 + handoff.md 内容），基于产出校准权重与资产，不再空手重跑 |
| 7 | `连山压缩算子 (独立 tokio::spawn)` | 指数退避轮询 pending/ 队列（被动学习）+ experiments/ 队列（主动学习，空闲窗口 + 预算上限），执行 **MCTS 四算子**：δ-backprop（trace 统计回传，父节点 γ=0.5 衰减）→ δ-fork（低回报资产扩展变体，复制+降权，内容修订走人工通道）→ δ-merge（相似变体合并）→ δ-prune（N≥5 且低于组内最优 >2σ 淘汰）——单写者更新归藏 + model_stats。**纯符号层确定性操作，不涉及 LLM**。数据源：`pending/{id}.json` 携带 assets_used 链 → TraceRewardExtractor 提取 (资产 × 回报) |
| 8 | `CausalVerifyAgentBuilder.verify(output, tool_results, meta_ctx) -> VerificationReport` | **V33 前置管线（§6.6/§8.22）**：ConstraintEngine（Truths Hard 短路）→ ContractEngine 机械执行 verifications checks（hard 失败直接短路，LLM 不可翻案）→ 剩余 llm_judgement 项 + ContractReport 注入 LLM 裁决。优先使用 meta_ctx.verify_system_prompt，None 时按 `meta_ctx.mode` 降级到 VERIFY_ORC / VERIFY_EXEC 硬编码模板（编排-验证 / 执行-验证配对）。`tool_results` 由 `TpnCycle.collect_tool_results()` 从 trace.jsonl 自动提取最近 10 条工具调用输出，非空数组 |
| 9 | `CausalConvergeAgentBuilder.converge(subtask_results, meta_ctx) -> ConvergenceDecision` | 优先使用 meta_ctx.converge_system_prompt，None 时按 `meta_ctx.mode` 降级到 CONVERGE_ORC / CONVERGE_EXEC 硬编码模板（编排-收敛 / 执行-收敛配对）。**V31 完整汇报输入**：subtask_results 含成功与失败（Diverged）条目——LLM 基于失败原因/交接产物裁决 Partial/Diverged，并把**失败分析与 rerun 建议输出到 task_summary**（决策进 LLM，不加结构化字段）；父阳（阳·管理）据此 rerun_of 再启用或接受残缺综合 |
| 10 | `RecursiveRunner.execute(description, external_ctx, max_depth) -> TPNResult` | runner.execute() 的增强版本，接受来自前端 agent 的 ExternalContext（文件、工具结果、对话总结），将文件物化到 `task_dir/context/files/` 并写入 `context/meta.json`，设置 `engine_ctx.context_dir` → FittingAgent 模板注入 External Context 节。可选 `max_depth` 参数覆盖配置中的递归深度限制 |
| 11 | `PlanBuilder.plan(description, task_type_tags) -> PlanSummary` | 运行 MetaAgent（权重更新+提示词编排）获取 MetaContext，随后调用 LLM 将 MetaContext + 任务描述编排为结构化的 PlanSummary（含子任务预估、技能推荐、复杂度评估），**不进 周易循环**，不触发 FittingAgent/CausalAgent |
| 12 | `TaijiMcpServer.handle_explain(task_id) -> ExplainReport` | 读取 `meta.json` + 递归 `trace.jsonl` + `deliverables/` 目录，解析 TraceRecord 的 phase/cycle/round 字段构建阶段时间线和路由决策树，产出人类可读 ExplainReport（含 summary 自然语言总结） |
| 13 | `AgentFactory.create_chat_agent(session_id, context_task_id, model, provider_name) -> ChatAgentBuilder` | 创建前端聊天面板的 ChatAgent builder。LLM 配置从 `agent_overrides["chat"]` 解析（model/provider_name 为 None 时使用解析后的默认值）。构造出的 builder 持有 `session_id`、`context_task_id`、`providers: Arc<ProviderRegistry>`、`safety_hook`、`config`、`data_root`、`model`、`provider_name` 八个字段（**不持有 AgentFactory 引用**——AgentFactory 无 Clone）。自动注册 5 个 L1 Skills + SafetyHook。`max_turns=20`。**不进 周易循环** |
| 14 | `ChatAgentBuilder.chat(message, chat_history: &mut Vec<Message>, on_chunk: Box<dyn Fn(String) + Send + Sync>) -> Result<String, TaijiError>` | 单轮对话执行。`on_chunk` 回调接收每个文本 delta（Rig `StreamedAssistantContent::Text` 解包后的纯文本），需 `Send + Sync` 以跨 await 传递到 WS mpsc 通道。内部使用 `agent.stream_chat()` → 遍历 `MultiTurnStreamItem` → 提取 Text/ReasoningDelta → 回调。`chat_history` 可变借用，完成后内部自动 `save_json_atomic` 持久化。返回完整响应文本。`context_task_id` 是 builder 构造时字段，非 per-message 参数 |
| 15 | `ChatAgentBuilder.build_system_prompt() -> String`（**同步** `fn`，V40 降级） | 构建 ChatAgent 的 system prompt。若 `context_task_id` 非空，注入任务描述（从 `{data_root}/tasks/{id}/meta.json` 读取 description/status/depth）。**V40 起不再注入归藏摘要**（guizang_digest 已删除：归藏 prompts/verifications 是任务执行链 Meta/Fitting/Causal 的编排模板，对对话角色语义错配；ChatAgent 的记忆 = 会话历史 `.taiji/chat/{session_id}.json`，经 stream_chat history 回填）。无 context_task_id 时使用通用助手模板 |

---


## 4. 核心类型契约

```mermaid
classDiagram
    class Task {
        +id: String
        +description: String
        +depth: u32
        +status: TaskStatus
        +parent_id: Option[String]
        +subtask_ids: Vec[String]
    }

    class SubtaskSpec {
        +description: String
        +verification_spec: String
        +mode: AgentMode
        +context: Value
        +rerun_of: Option[usize]
    }

    class DecomposeResult {
        +summary: String
        +status: ConvergenceStatus
        +subtask_count: u32
        +deliverables: Vec[String]
        +task_id: String
        +rounds: u32
        +tools_used: Vec[String]
    }

    class TPNResult {
        +task_id: String
        +content: String
        +tools_used: Vec[String]
        +deliverables: Vec[String]
        +depth: u32
        +rounds: u32
    }

    class MetaContext {
        +constraints: Vec[TruthConstraint]
        +matched_skills: Vec[SkillRef]
        +yang_prompt: YangPrompt
        +mode: AgentMode
        +model: Option[ModelKey]  %% V32: 元权重模型路由结果 (None=配置默认)
        +assets_used: Vec[AssetRef]  %% V32: 本次编排选用的资产引用（含分区，连山回传依据）
        +temperature: Option[f32]
        +fitting_system_prompt: Option[String]
        +verify_system_prompt: Option[String]
        +converge_system_prompt: Option[String]
    }

    class AgentMode {
        <<enum>>
        Orchestration
        Execution
    }

    class PromptAsset {
        +asset_type: String
        +layer: u32
        +id: String
        +name: String
        +description: String
        +tags: Vec[String]
        +confidence: f64
        +version: u32
        +content: String
        +agent_target: String
        +temperature: Option[f32]
        +usage_count: u32
        +success_rate: f64
        +env_tags: Vec[String]  %% V32: 环境维度 (空=环境无关)
        +parent_id: Option[String]  %% V32: fork 来源 (None=根资产)
        +variant_of: Option[String]  %% V32: 同源变体组
        +stats: AssetStats  %% V32: MCTS 统计（V35/MVP-6 回传写入，serde default 零迁移）
    }

    class WorkflowAsset {
        %% V32 新增·阳轨: 特殊工作流+稳定涌现文本+脚本模板
        +id: String
        +tags: Vec[String]
        +confidence: f64
        +version: u32
        +content: String  %% 步骤序列/命令/验收要点
        +agent_target: String
        +env_tags: Vec[String]
        +parent_id: Option[String]
        +variant_of: Option[String]
        +stats: AssetStats
    }

    class VerificationAsset {
        %% V32 新增·阴轨: 收敛验证契约
        %% V33 结构化: checks 可机械执行（本体论 TBox 的最小形式，§6.0）
        +id: String
        +tags: Vec[String]
        +confidence: f64
        +version: u32
        +content: String  %% 契约语义描述（人读）
        +checks: Vec[CheckSpec]  %% V33: 结构化检查项（机器执行）
        +env_tags: Vec[String]
        +parent_id: Option[String]
        +variant_of: Option[String]
        +stats: AssetStats
    }

    class CheckSpec {
        %% V33 新增·验证契约的最小单元（本体论规则/公理）
        +id: String
        +kind: CheckKind  %% file_exists|schema_valid|reference_resolves|command_succeeds|llm_judgement
        +target: String  %% 相对 deliverables/ 的路径或 glob
        +params: Value  %% kind 相关参数（schema 路径 / 命令 / 引用规则）
        +severity: CheckSeverity  %% hard|soft（hard 失败 = 验证失败，LLM 不可翻案）
        +pass_condition: String  %% 人读判据（llm_judgement 类注入 LLM prompt）
    }

    class CheckKind {
        <<enum>>
        %% V33 新增；V34: TraceConsistency（断言引用完整性）
        FileExists
        SchemaValid
        ReferenceResolves
        CommandSucceeds
        LlmJudgement
        TraceConsistency  %% V34: [证据: 工具名] 引用 → trace tool_call::* 存在性（§8.22）
    }

    class CheckResult {
        %% V33 新增·契约执行记录（随 verify_state.json 持久化，零新增文件）
        +check_id: String
        +kind: CheckKind
        +passed: bool
        +detail: String
        +duration_ms: u64
    }

    class ContractReport {
        %% V33 新增·ContractEngine 输出（注入 verify LLM prompt）
        +passed: bool  %% 任一 hard 项失败 → false
        +results: Vec[CheckResult]
        +summary: String
    }

    class SkillRef {
        +id: String
        +name: String
        +tool_name: String
        +match_weight: f64
    }

    class YangPrompt {
        +task_description: String
        +constraint_summaries: Vec[String]
        +parent_deliverables: Vec[String]
        +sibling_deliverables: Vec[String]  %% V30 会盟：兄弟贡品索引（serde default 空）
    }

    class TruthConstraint {
        +id: String
        +name: String
        +description: String
        +severity: ConstraintSeverity
        +justification: Option[String]
    }

    class ConstraintSeverity {
        <<enum>>
        Hard
        Soft
    }

    class VerificationReport {
        +route: VerificationRoute
        +confidence: f64
        +summary: String
        +constraint_violations: Vec[String]
    }

    class ConvergenceDecision {
        +status: ConvergenceStatus
        +task_summary: String
    }

    class ExternalContext {
        +files: Vec[ExternalFile]
        +tool_results: Vec[ExternalToolResult]
        +session_summary: Option[String]
    }

    class ExternalFile {
        +path: String
        +content: String
    }

    class ExternalToolResult {
        +tool: String
        +output: String
    }

    class EngineContext {
        +task_id: String
        +depth: u32
        +task_dir: PathBuf
        +cycle: u32
        +round: u32
        +context_dir: Option[PathBuf]
    }

    class VerificationRoute {
        <<enum>>
        Pass
        BackToTpn
        BackToMeta
    }

    class PlanSummary {
        +task_analysis: String
        +estimated_subtasks: Vec[SubtaskPlan]
        +recommended_skills: Vec[String]
        +expected_deliverables: Vec[String]
        +estimated_complexity: String
        +matched_prompts_summary: String
        +relevant_constraints: Vec[String]
    }

    class SubtaskPlan {
        +description: String
        +verification_approach: String
        +required_skills: Vec[String]
    }

    class ExplainReport {
        +task_id: String
        +description: String
        +status: String
        +total_cycles: u32
        +total_rounds: u32
        +total_depth: u32
        +total_duration_ms: u64
        +timeline: Vec[PhaseSummary]
        +decisions: Vec[DecisionSummary]
        +final_deliverables: Vec[String]
        +summary: String
    }

    class PhaseSummary {
        +phase: String
        +cycle: u32
        +round: u32
        +depth: u32
        +duration_ms: u64
        +tools_used: Vec[String]
        +key_output: String
    }

    class DecisionSummary {
        +cycle: u32
        +round: u32
        +verdict: String
        +reason: String
        +constraint_violations: Vec[String]
    }

    class ConvergenceStatus {
        <<enum>>
        Converged
        Partial
        Diverged
    }

    class AssetStats {
        %% V32 新增·serde default 零迁移
        +n: u64  %% 采样次数
        +pass_count: u64
        +cost_tokens_sum: u64  %% trace usage.input_tokens 累加
        +cost_tokens_sq_sum: u64  %% 增量方差
        +quality_sum: f64  %% 质量分累加
        +verify_rounds_sum: u64  %% BACK_TO_TPN 次数
        +avg_reward(): f64
        +pass_rate(): f64
    }

    class AssetRef {
        %% V32 新增
        +partition: ModelKey
        +id: String
        +kind: String  %% prompt|workflow|verification
    }

    class ModelStats {
        %% V32 新增·元权重表
        +rows: BTreeMap[(ModelKey × Tag), StatsRow]
        %% StatsRow: n / pass_count / cost_sum / quality_sum
    }

    class ModelRouter {
        %% V32 新增·bandit 路由
        +route(tag, task_desc) -> ModelKey
        %% UCB: avg_reward + C·√(ln N_total / N_model_tag)；成本感知：贵模型需通过率显著更高
    }

    class UcbRanker {
        %% V32 新增
        +rank(candidates: Vec[AssetNode], c: f64 = 1.414) -> Vec[AssetNode]
        %% score = avg_reward + C·√(ln N_total / N_node)；N=0 → 最大探索分
    }

    MetaContext --> TruthConstraint : contains
    MetaContext --> SkillRef : contains
    MetaContext --> YangPrompt : contains
    MetaContext --> AgentMode : decides
    MetaContext --> ModelRouter : routes (V32)
    PromptAsset --> AssetStats : tracks (V32)
    WorkflowAsset --> AssetStats : tracks (V32)
    VerificationAsset --> AssetStats : tracks (V32)
    ModelRouter --> ModelStats : reads (V32)
    UcbRanker --> AssetStats : ranks (V32)
    ContractEngine --> VerificationAsset : loads (V33)
    ContractEngine --> CheckSpec : executes (V33)
    PlanSummary --> SubtaskPlan : contains
    ExplainReport --> PhaseSummary : contains
    ExplainReport --> DecisionSummary : contains
    PlanBuilder ..> PlanSummary : produces
    TPNResult ..> ExplainReport : analyzed by
```

---


## 5. 周易执行流

### 5.1 根任务执行序列

```mermaid
sequenceDiagram
    participant U as User
    participant RR as RecursiveRunner
    participant AF as AgentFactory
    participant MA as MetaAgent (元)
    participant FA as FittingAgent (阳)
    participant CA as CausalAgent (阴)
    participant DMN as 连山压缩算子

    U->>RR: execute(description)
    RR->>RR: create task dir + meta.json
    RR->>AF: create_meta_agent(task_id, depth, max_depth)
    AF-->>RR: MetaAgentBuilder
    RR->>MA: run(description, task_type_tags)
    MA->>MA: 查询归藏 prompts/（标签匹配 + 置信度排序）
    MA->>MA: 深度规则 + 难度评估 → 决策配对模式 (Orchestration | Execution)
    alt 有高置信度提示词资产
        MA->>MA: LLM 编排三份 system prompt（与所选模式配对：
        MA->>MA: 编排→编排拟合+收敛；执行→执行拟合+验证）
    else 无匹配资产
        MA->>MA: 降级 → mode 默认 Orchestration，模板全为 None
    end
    MA-->>RR: MetaContext (mode + reasoning paths + constraints + skills + prompts)

    loop 周易循环 (max_cycles × max_rounds)
        RR->>AF: create_fitting_agent(depth, meta_ctx, engine_ctx)
        AF-->>RR: FittingAgentBuilder
        RR->>FA: run(description)
        Note over FA: LLM loop（上下文预算 §8.19） + recursive_decompose + causal_verify\n内置 L1 Skills (read/write/bash/search/webfetch)\n前端 agent 可通过 MCP ExternalContext 注入额外上下文\nV28: 上下文超限/失败/取消 → 先写 deliverables/handoff.md 再返回（§8.18）
        FA-->>RR: TPNResult

        RR->>AF: create_causal_verify_agent(engine_ctx)
        AF-->>RR: CausalVerifyAgentBuilder
        RR->>CA: verify(output, tool_results, meta_ctx)
        Note over CA: tool_results 从 trace.jsonl 自动提取最近 10 条工具调用\n优先 meta_ctx.verify_system_prompt → 降级到硬编码模板\nV33: ConstraintEngine (Hard 短路) → ContractEngine 机械执行 checks → LLM 只裁决 llm_judgement 项
        CA-->>RR: VerificationReport

        alt route = PASS
            Note over RR,DMN: 周易 PASS — enqueue 连山（当前 连山压缩算子 未激活，入队逻辑待实现）
            RR-->>U: TPNResult
        else route = BACK_TO_TPN
            RR->>RR: round++，读取 deliverables/（含 handoff.md）→ FittingAgent 基于前一瞬态产出递归分解\nV28: 不再以原 description + chat_history 重放重跑（§8.18）
        else route = BACK_TO_META
            RR->>RR: cycle++, round=0\nMetaAgent 基于 deliverables/ 产出校准权重与认知资产（§8.18）
        end
    end
```

### 5.2 递归分解序列

```mermaid
sequenceDiagram
    participant FA as FittingAgent (parent, depth=N)
    participant RDT as RecursiveDecomposeTool
    participant AF as AgentFactory
    participant CFA as Child FittingAgent (depth=N+1)
    participant CCA as CausalAgent.converge

    FA->>RDT: execute(subtasks: Vec[SubtaskSpec])
    Note over FA, RDT: 每个 SubtaskSpec 携带 verification_spec + mode（父 LLM 按难度分配）+ context
    Note over FA, RDT: 由 assemble_child_description() 拼入子任务描述\n**此工具仅编排模式 FittingAgent 注册**（执行模式 LLM 不可见）；工具内部 mode guard 兜底
    RDT->>RDT: 父 TPNResult.deliverables → 注入子 MetaContext.parent_deliverables
    RDT->>RDT: V30 会盟：collect_sibling_deliverables（BTreeMap 扫描兄弟贡品）→ 注入子 YangPrompt.sibling_deliverables

    RDT->>RDT: guard: depth < max_depth + subtasks ≤ max_subtasks + mode == Orchestration
    RDT->>RDT: check cancel token + create child_token
    RDT->>RDT: WorkerPool.acquire() — 入口持 1 permit（并行分解节点上限），join 后释放

    loop for each subtask
        RDT->>RDT: 子模式 = subtask.mode；depth+1 >= max_depth 时强制覆盖为 Execution（深度规则兜底）
        RDT->>RDT: generate child task_id + child_token
        RDT->>AF: create_fitting_agent(depth+1, meta_ctx(mode=子模式), child_ctx, child_token)
        AF-->>RDT: FittingAgentBuilder
        RDT->>CFA: run(subtask.description)
        Note over CFA: 子节点模式由 SubtaskSpec.mode 携带（父 LLM 难度判断），深度规则兜底；
        Note over CFA: BACK_TO_META 时子节点 MetaAgent 重新决策
        Note over CFA: deliverables 字段列出所有产物绝对路径
        Note over CFA: TPNResult 携带 rounds / tools_used 供 converge 参考
        CFA-->>RDT: TPNResult (含 deliverables / rounds / tools_used)
    end

    RDT->>RDT: JoinSet.join_next() — 流式收集，子任务完成即处理
    RDT->>RDT: V31 失败汇报：任务级失败 → build_failure_entry（Diverged 条目：failure_reason/failure_kind + handoff 交接路径）进 prior_results，不整体上抛；join panic / 取消仍硬中止
    RDT->>RDT: 聚合子 deliverables → DecomposeResult.deliverables
    RDT->>RDT: 映射子 rounds / tools_used → child DecomposeResult 数组（含失败条目）传 CausalAgent.converge
    RDT->>AF: create_causal_converge_agent(child_ctx)
    AF-->>RDT: CausalConvergeAgentBuilder
    RDT->>CCA: converge(subtask_results, parent_meta_ctx)
    Note over CCA: 模板按 parent_meta_ctx.mode 选 CONVERGE_ORC（编排节点收敛）
    Note over CCA: 接收子 deliverables 路径，硬编码要求 read 工具逐文件检查
    Note over CCA: V31 含失败条目——基于失败原因/交接产物裁决，task_summary 输出失败分析与 rerun 建议
    CCA-->>RDT: ConvergenceDecision（status=Partial/Diverged + task_summary 分析）
    RDT-->>FA: DecomposeResult (含 deliverables)
```

### 5.3 周易路由决策

| 路由 | 触发条件 | 行为 | 计数器 |
|------|---------|------|--------|
| **PASS** | 交付件通过 L4 Truth 约束检查 + **ContractEngine 契约检查全过（V33：hard 项零失败）** + LLM 裁决 llm_judgement 项收敛 | 输出 TPNResult → 入队连山 | — |
| **BACK_TO_TPN** | 执行偏差（交付件不满足验证规格）或 **V28 结构化信号：`failure_reason = context_overflow / output_missing`**（任务粒度错误） | 读取 `deliverables/`（含 `handoff.md`），FittingAgent **基于前一瞬态产出递归分解**（V28：不再以原 description + chat_history 重放重跑）；验证报告注入作定向修正参考 | `round++`，达 max_rounds → FAIL |
| **BACK_TO_META** | 认知偏差（推理路径错误、缺少必要约束）或 **V28 结构化信号：`failure_reason = constraint_violation(Hard) / cognitive`** | 读取 `deliverables/`（含 `handoff.md`），重新运行 MetaAgent **基于产出校准权重与认知资产**（V28：不再空手重跑），重新获取推理路径 | `cycle++` / `round=0`，达 max_cycles → FAIL |

路由判定 = **V28 结构化失败信号优先 + CausalAgent LLM 裁决兜底**（§8.18 分流表）。约束检查（ConstraintEngine.check_constraints）在 LLM 调用之前执行：Hard 违反直接返回 BACK_TO_META，Soft 违反注入 LLM prompt 由 LLM 裁定。**V33：ContractEngine 机械检查（L0/L1）先于 LLM 裁决，hard 项失败直接短路，LLM 的 PASS 不可覆盖机械 FAIL（§6.6）**。

CausalAgent.verify() 接收的 `tool_results` 由 `TpnCycle.collect_tool_results()` 从 `trace.jsonl` 中自动提取最近 10 条工具调用输出，确保验证 LLM 可交叉比对工具结果与任务输出。

---


## 6.6 验证三权分立（周易验证机制）


阴面验证分为三层，**确定性优先、概率兜底**：

| 层 | 执行者 | 内容 | 失败语义 |
|:---:|------|------|------|
| **L0 机械验证** | ContractEngine（确定性，零 LLM） | file_exists / schema_valid / reference_resolves / command_succeeds 类检查项——文件存在性、schema 校验、引用完整性、可执行命令 | hard 失败 → 直接短路（BACK_TO_META / FAIL），**LLM 不可翻案** |
| **L1 契约验证** | ContractEngine 加载 verifications/ 结构化契约（V38 起唯一阴轨资产层——truths 已并入） | 契约条件匹配 → 断言机械执行 → 结构化通过/失败记录（CheckResult）；**含 TraceConsistency（V34：断言引用 → trace 工具调用存在性，§8.22）** | 同上；soft 失败注入 LLM prompt 供参考 |
| **L2 LLM 验证** | CausalAgent LLM（概率层，最后兜底） | 仅 llm_judgement 类检查项（语义合理性 / 设计决策 / 跨领域一致性） | LLM 裁决只影响 llm_judgement 项；机械检查失败时 LLM 的 PASS 无效 |

**裁决优先级（硬约束）**：`L0/L1 机械失败 > LLM 任何裁决`。机械检查失败直接短路（不经 LLM），LLM 只对剩余项裁决；LLM 的 PASS 不能覆盖机械 FAIL。

**反偏置注入（L2 对抗）**：llm_judgement 检查项的 pass_condition 注入 verify prompt 时附带反偏置指令（「表面流畅不算数，必须引用具体证据；禁止因篇幅长 / 风格好加分」），并要求 read 工具逐文件取证——降低 verbosity / self-preference 偏置（§1.3 实证）。

**契约执行记录**：CheckResult 数组随 verify_state.json 持久化（复用既有文件，§8.1 清单不变），供恢复链与 连山回传消费。

---



## 7. 运行时布局

### 7.1 递归同构目录树

```
data/                               ← 默认 data_root
├── .taiji/
│   ├── config.json                 ← TaijiConfig
│   ├── pending/                    ← 连山任务队列
│   │   └── dead/                   ← 死信队列
│   ├── knowledge/                  ← 归藏 认知仓库 (§6)
│   └── tasks/
│       └── {task_id}/            ← 根任务（`{简述slug}-{YYYYMMDD-HHMMSS}`，见 §8.1）
│           ├── meta.json           ← Task { id, depth:0, status }
│           ├── trace.jsonl         ← 根层执行轨迹
│           ├── deliverables/       ← LLM 产出（含 handoff.md 交接物，V28 §8.18）
│           └── children/           ← 递归子任务
│               ├── 0/              ← depth:1
│               │   ├── meta.json
│               │   ├── trace.jsonl
│               │   ├── deliverables/
│               │   └── children/   ← 可继续递归
│               └── 1/
│                   └── ...
```

### 7.2 追踪系统

双层追踪，与递归目录树同构：

| 组件 | 追踪方式 |
|------|---------|
| 权重更新 (元) | 手动 TraceWriter::write() — 单条记录 |
| 概率拟合 (阳) | Rig TraceHook — 自动捕获所有 StepEvent |
| 因果验证 (阴) | 手动 TraceWriter::write() — 结构化输出 |

每层任务目录独立 `trace.jsonl`。`read_tree()` 递归遍历所有 `**/trace.jsonl` 按时间戳合并。单文件超过 10MB 自动轮转，保留最近 5 代。敏感信息（API Key）写入前脱敏。

TraceHook 的 `on_tool_call` 同时收集**真实工具调用名**：FittingAgent 的 `tools_used` 统计读此记录（不解析 LLM 响应文本，避免 LLM 正文提及工具名的伪阳性）。对话历史快照职责见 §8.1（ChatHistorySnapshotHook）。

---


## 8. 关键架构决策

### 8.1 瞬态任务节点生命周期

**任务节点 = 单个三相循环（TpnCycle 实例），而非循环内的某个 Agent。** 生成树 / 收敛树的每个节点是完整的「权重更新 → 概率拟合 → 因果验证 → 路由决策」循环（`TpnCycle.execute()`），递归分解 spawn 的是**子循环节点**（`TpnCycle::new`，同一段代码），不是子 Agent。

循环内的 Agent（Meta / Fitting / Causal）是节点的**相位执行器**，生命周期从属于所属节点：

```
AgentFactory.create_*_agent() → AgentBuilder.run() → 结构化输出 → AgentBuilder drop
```

- 每轮循环（round）新建 FittingAgent 与 CausalAgent 实例；每次 BACK_TO_META（cycle++）重建 MetaAgent 实例——用完即弃，状态不跨调用保留
- 认知更新通过归藏 YAML 文件持久化，下轮加载时自动生效
- 整个系统 = 多瞬态任务节点系统：节点实例 = round × cycle × depth 的笛卡尔积，沿生成树展开（蒙特卡洛树式概率探索）、沿收敛树归并（马尔可夫链式状态转移与收敛），每一层递归与每一轮循环都是一次概率采样

瞬态性保证：节点销毁后磁盘状态（checkpoint / deliverables / trace）按 §7 原子持久化，崩溃恢复按恢复优先级链重建节点。**V28 恢复优先级链 = 产出继承**：`deliverables/`（含 `handoff.md`）> `decompose_result.json` > 重跑（`resume_history`/`chat_history` 仅作本节点断点续聊的最终兜底，**不再作为结果重建来源**——执行事实是唯一记忆，§1.4）。

**恢复链对根任务与子任务同构生效**：子任务恢复由 RecursiveDecomposeTool 扫描 `children/` 时复用旧结果（rerun_of 索引）；根任务恢复由 `taiji run --resume <task_id>` 触发——runner 复用既有 task_id（不生成新 UUID），恢复 EngineContext（depth 从 meta.json 读取）后进入同一 `TpnCycle.execute` 恢复链。根/子共享同一段恢复代码，无特例。

**对话历史增量快照**：Rig `chat()` 在 LLM 调用出错时提前返回、不回写 `chat_history`（仅成功时 `extend`）——仅靠 FittingAgent 成功路径的全量 save 会导致失败任务磁盘上恒为空历史，`--resume` 只能从空历史重跑整个 Fitting 阶段。为此在 FittingAgent 注册 **ChatHistorySnapshotHook**：每次 LLM 调用前（`on_completion_call`，含工具循环内每次调用）将完整对话（调用前 `history` + 本轮 `prompt`，均为 `rig::completion::Message`）按 `save_json_atomic` 原子快照到 `{task_dir}/chat_history.json`。失败/超时任务最多丢失最后一轮 in-flight 请求；成功路径的全量 save 保留作为最终一致性收尾。快照对根任务 `--resume` 与子任务 rerun 恢复同样生效。**V28 定位降级**：chat_history 仅为本节点断点续聊兜底（省 token），不作为跨层传递物、不作为结果事实来源（§1.4 / §8.18）。

**任务目录持久化文件清单（唯一事实——新增文件必须先入此清单，只写不读者禁止引入）**：

| 文件 | 内容 | 写者 | 读者 | 用途 |
|------|------|------|------|------|
| `meta.json` | Task{id,desc,depth,status,parent_id,subtask_ids} | runner / TpnCycle | 前端、恢复链 | 任务元数据 + 生命周期状态 |
| `checkpoint.json` | {phase,round,cycle} | TpnCycle 每阶段 | TpnCycle 崩溃恢复 | 循环进度（PASS 后删除） |
| `meta_ctx.json` | MetaContext | TpnCycle（MetaDone 后） | TpnCycle 崩溃恢复 | 元阶段产出上下文 |
| `chat_history.json` | Vec\<Message\> | SnapshotHook + Fitting 收尾 | resume 增量恢复 | Fitting 对话（失败点续跑） |
| `verify_state.json` | {report,round,cycle} | CausalAgent.verify | TpnCycle（VerifyDone 恢复） | 验证报告缓存（路由决策） |
| `decompose_result.json` | DecomposeResult/TPNResult | TpnCycle（PASS） | 缓存返回、子任务复用 | 完成标记 + 结果缓存 |
| `deliverables/` | 产物文件 | FittingAgent | 聚合、前端 | 交付物实体 |
| `children/` | 子任务目录 | RecursiveDecomposeTool | 扫描复用 | 递归树实体 |
| `trace.jsonl` | 事件审计（脱敏） | TraceHook / 手动 | read_tree | 审计与工具结果提取 |
| `deliverables/handoff.md` | 交接产出物：front matter 结构化字段（failure_reason/degraded/output_refs）+ 正文环境信息（进度/剩余/决策/约束状态） | Fitting 超限/失败/取消路径（V28） | 父层、verify/converge、Meta 校准、恢复链（均经 deliverables/ 既有路径发现） | 产出即交接，残缺产出继承载体（§8.18） |


**任务 ID 格式**：`{简述slug}-{YYYYMMDD-HHMMSS}`（如 `分析源码架构-20260807-061530`），由 `src/infra/task_id.rs` 生成——slug 取描述前 24 字符路径安全化（非字母数字→`-`、折叠连续破折号、去首尾破折号、空描述→`task`），时间戳为本地时间秒级。唯一性：根任务经 `ensure_unique` 检查 `tasks/` 目录已存在则追加 `-2/-3`；子任务追加 `-{index}`（同父并行不撞，跨父碰撞概率可忽略且无文件冲突——子任务目录在 `children/<idx>/`，task_id 仅作标识）。**chat session_id 保持 UUID**（`{data_root}/chat/{session_id}.json`，会话文件已持久化，不属任务 ID）。task_id 为纯字符串，无任何代码假设其 UUID 格式，`--resume`/`taiji trace` 输入与前端树显示同步可读化。

**子任务状态一致性**：RecursiveDecomposeTool 错误路径 `abort_all()` 终止子任务后，`children/` 下 status=Running 的子任务必须统一落盘为 Failed（写失败仅 warn，不阻断父任务错误传播）——「超时/失败/取消正确落盘」声明覆盖所有任务节点，含被父任务中止的子任务；中止不产生虚假的 Running 残留。

### 8.2 异层同构（结构同构，提示词按模式配对）

`depth` 只改变编号，不改变目录布局、周易循环结构、上下文预算与恢复路径。根任务和子任务执行**同一段代码、同一套配置**。但每个节点的**提示词与工具注册面由元 Agent 权重更新时决策的阴阳配对模式决定**：

- **模式决策**：MetaAgent 按递归层数规则（depth/max_depth，叶节点 `depth+1 >= max_depth` 硬性强制 Execution）+ 任务难易程度（复杂/多步/跨多维→Orchestration，原子/单步→Execution）决策 `MetaContext.mode`。根节点与 BACK_TO_META 重跑时由 MetaAgent 决策；子节点由父 LLM 在 `SubtaskSpec.mode` 按难度分配，`RecursiveDecomposeTool` 按深度规则兜底强制叶节点 Execution
- **配对提示词**：Orchestration → 阳用编排模板（拆解+综合）、阴用收敛模板；Execution → 阳用执行模板（直接产出）、阴用验证模板
- **工具面随模式分化**：`recursive_decompose` 仅编排模式注册（执行模式 LLM 不可见拆解工具，工具内部 mode guard 兜底）；5 L1 Skills + causal_verify 两模式均注册
- 单上下文预算：全相位（Meta / Fitting / Causal）统一 250k 交接 / 300k 硬截止（V29 §8.19）；不再使用 max_turns 轮次限制
- 递归层间通过 `MetaContext`（推理偏置注入 + mode）和 `ConvergenceDecision`（收敛结果上浮）传递信息
- 递归终止仅靠 depth guard：`depth >= max_depth` 时 RecursiveDecomposeTool 拒绝拆解（MaxDepthExceeded）

**权限同构（异层同构的权限维度）**：任务节点在任意深度保持相同的三相分工与权限配置——每个子循环节点与根节点一样：Fitting 相位持有执行工具（5 L1 Skills + causal_verify；编排模式另加 recursive_decompose）并受同一 SafetyHook 约束、Meta / Causal 相位持有只读收集工具（read / search / webfetch）且无执行工具。**权限模式与配置不随 depth 变化，权限边界随位置（task_dir）变化**（见 §8.9 工作区即权限边界）——不同深度不存在任何权限梯度，模式分化只影响提示词内容与拆解工具可见性。

### 8.4 路由内部化（结构化信号 + LLM 裁决）

周易循环的路由决策（PASS / BACK_TO_TPN / BACK_TO_META）由 CausalAgent 的 LLM 根据 VerificationReport 裁决。RecursiveRunner 只执行路由结果（递增循环计数器、重入对应阶段），不硬编码路由逻辑。**V28：结构化失败信号优先**——`failure_reason`（context_overflow / output_missing / constraint_violation / cognitive / degraded / other）由交接文件携带，命中分流表（§8.18）时直接路由；仅模糊地带（degraded / other）交 LLM 裁决兜底。

### 8.5 Hook 安全模型

SafetyHook 和 TraceHook 以 `AgentHook` trait 实现，注册到带工具的 Rig Agent 上（FittingAgent / MetaAgent / CausalAgent）。SafetyHook 在 ToolCall 事件上拦截危险操作（路径穿越、命令注入、SSRF），拦截时返回 `Flow::skip()`。非白名单 MCP 工具强制执行安全检查。

**循环内权限分工的实现机制**：SafetyHook 挂载在**所有注册了工具的相位**上（Fitting / Meta / Causal），因为收集工具虽然只读，仍持有文件系统访问面（read / search）——这是 §1.2 相位分工的安全落地，而非偶然：

| 相位 | 工具注册 | SafetyHook | 权限角色 |
|------|:---:|:---:|------|
| MetaAgent | read + search + webfetch（只读收集 / 联网核实） | **挂载** | 认知者 + 收集者：LLM 收集任务上下文 / 父层 deliverables / 归藏资产与网络信息后更新权重并决策配对模式，无执行面 |
| FittingAgent | 5 L1 Skills + causal_verify（两模式）；recursive_decompose（**仅编排模式**） | **挂载**（+ TraceHook） | 执行者：唯一持有变更世界工具、受安全约束的权限面；编排节点可拆解，执行节点专注直接产出 |
| CausalAgent | read + webfetch（只读验证 / 联网核实） | **挂载** | 裁判者 + 收集者：LLM 逐文件核验 deliverables、联网核实外部事实后裁决路由（编排节点收敛模板 / 执行节点验证模板），无执行面 |

**节点间权限同构**：所有任务节点（任意 depth / round / cycle）共享同一进程级 `SafetyHook` 单例（`build_engine` 创建一次，`Arc` 注入全部带工具的 Agent），规则一致、白名单一致——权限配置在节点间完全同构，不存在按深度 / 轮次 / 层级的权限分化。

**带工具必有安全钩子（硬约束）**：任何相位只要注册工具（含只读收集工具），就必须挂载 SafetyHook——「无工具的相位允许不挂载，带工具的相位必须挂载」是相位权限闭合的底线。CausalAgent 的 LLM 验证路径（verify / converge 真实 LLM 调用 + read 逐文件核验）已在此约束下落地。

**Rig 0.39 hook 挂载机制**：`AgentBuilder::hook()` 是单槽覆盖式——链式 `.hook(a).hook(b).hook(c)` 只有 `c` 生效，多 hook 必须组合为一次挂载。FittingAgent 的 safety / trace / snapshot 三个 hook 经 `FittingHookSet` 组合（safety 优先、首个非 Continue 短路，违规工具不进入 trace 记录）；Meta / Causal / Chat 单 hook 直接挂载。任何相位新增第二个 hook 必须先查现有挂载点是否单槽。

**L1 Skills 工具参数契约**：SkillTool 是单参 `input` 包装（Rig ToolDefinition 暴露 `input: string`，`call` 内对 input 值做二级 JSON 解析——JSON 字符串解析为对象，失败保留原文）。各内置工具的参数键必须与 LLM 可用的传参形式兼容：BashTool 读 `command`、ReadTool 读 `path`，**必须同时支持 `input` 键直读**（`args.get("input")` 为纯字符串时直接当命令/路径）——否则 LLM 按 schema 传 `{"input":"ls"}` 永远报 missing 参数，被迫试错摸索 `{"input":"{\"command\":\"ls\"}"}`（每次 resume 重跑重新踩坑，系统性吞噬预算）。ToolDefinition 的 description 必须包含用法示例（双保险：实现容错 + schema 引导）。write/search/webfetch 参数键同理自查。

### 8.6 递归防护

| 防护层 | 机制 | 默认值 |
|--------|------|--------|
| 深度限制 | `RecursiveDecomposeTool` 检查 `depth < max_depth` | 2 |
| 子任务上限 | `subtasks.len() ≤ max_subtasks` | 4 |
| TPN 轮次 | `round_counter ≤ max_rounds` | 10 |
| 周易循环 | `cycle_counter ≤ max_cycles` | 3 |
| 上下文预算 | `usage.input_tokens ≥ handoff_tokens` → 交接（context_overflow）；`≥ hard_cutoff_tokens` → 硬截止 FAIL（V29 §8.19） | 250k / 300k |
| 取消传播 | `CancellationToken` 传递到所有递归层（parent→child_token 链接） | — |
| 嵌套 task_id | 每层使用可读 task_id（`{简述slug}-{时间戳}`，子任务追加 `-{index}`），`parent_id` 指向父层 | — |
| 执行超时 | tokio::timeout 包裹整个 execute()（超时 → cancel + 写 Failed） | 600s |

> 默认值统一以 `config.rs` RuntimeConfig 为准（此表为真实默认值），配置文件可覆盖。

### 8.9 绝对路径单向传递与权限收敛

多层递归中，每层 Agent 产出的文件路径必须在 prompt 中**硬编码传递**（不依赖 LLM 推测），遵循单向向下覆盖原则：

**传递链：**

```
父 Yang → 产出文件 → TPNResult.deliverables (绝对路径)
    │
    │ recursive_decompose 注入子 MetaContext
    ▼
子 YangPrompt.parent_deliverables → 子读取(只读) → 产出自己的 deliverables
    │
    │ 子 TPNResult.deliverables 向上聚合
    ▼
DecomposeResult.deliverables → 父 CausalAgent.converge() 逐文件检查
```

**权限模型：**

> 本节的路径权限与 §8.5 的相位权限分工共同构成节点间权限同构：每个任务节点（任意 depth）都遵循相同的「父→子只读、子→父聚合、兄弟隔离」目录规则——权限同构覆盖工具面（§8.5）与数据面（本节）两个维度。
>
> **工作区即权限边界**：节点权限范围 = 其 `task_dir`（根任务为 `{task_id}/`，子任务为 `children/N/`）——位置与权限一体两面：区内自由读写、区外不可达。本节路径规则（父→子只读、**V30：兄弟贡品公开只读**、绝对路径单向传递）正是这一边界的载体。

| 方向 | 规则 | 保证方式 |
|------|------|---------|
| 父→子 | 父 deliverables 绝对路径注入子 `YangPrompt.parent_deliverables`，**只读参照** | 硬编码模板指令：子只能 read，不能 write 父目录 |
| 子→父 | 子 deliverables 绝对路径通过 `DecomposeResult.deliverables` 返回父层 | `recursive_decompose` 中硬编码聚合 `tpn_result.deliverables` |
| 兄弟（V30 收窄） | 兄弟贡品（deliverables/）**公开可发现可读**（会盟注入目录 + read 工具）；**写入封闭**——write 目标必须在**本任务 task_dir 内**（封地自治，FittingHookSet 域校验强制）；兄弟任务目录内**非 deliverables 文件（中间记忆）不可见** | 文件系统布局保证：`children/{0}/` 与 `children/{1}/` 各自独立；SafetyHook 黑名单 + FittingHookSet 写路径域校验（§8.20 会盟） |

**硬编码保证（不可被 LLM 绕过）：**

1. **阳 Fitting 模板（按模式配对）**：必须明确列出所有产物文件的绝对路径。编排模板引导「拆解优先 + 综合」（recursive_decompose 可用，含子任务模式分配指南）；执行模板引导「直接产出」（无 recursive_decompose，专注 L1 工具完成）；子产物在 convergent 阶段可见。**V30 身份段**：模板注入「身份与地位」段（内容/类别/父/子/兄弟贡品索引/权限教学，§8.20）
3. **阴 verify 模板（按模式配对）**：接收 `deliverables` 路径，调用 `read` 工具逐文件检查（编排节点查 MECE 完备性与综合质量，执行节点查直接产出合规）
4. **阴 converge 模板（按模式配对）**：接收所有子 `deliverables`，调用 `read` 逐文件检查跨子任务一致性（编排节点收敛子结果）

绝对路径以 `task_dir` 为根——每层递归有独立的 `task_dir`（`data/tasks/{root}/children/{i}/...`），子层不会因为路径冲突覆盖父层文件。

### 8.10 四象温度（Base 模板默认温度）

六个 Base 硬编码模板（Fitting 编排/执行、Causal 验证/收敛各按模式配对）根据各自职责设置不同温度，引导 LLM 行为偏向：

| Base 模板 | 默认 temperature | 设计依据 |
|-----------|:---:|------|
| FittingAgent 编排（Orchestration） | `0.8` | 高温度鼓励拆解探索与多方案发散 |
| FittingAgent 执行（Execution） | `0.5` | 中低温度聚焦直接产出，减少漂移 |
| CausalAgent 验证（verify，两模式） | `0.2` | 低温度严格控制，严格对照约束逐条检查 |
| CausalAgent 收敛（converge，两模式） | `0.2` | 低温度严格判决，不引入额外噪声 |

温度优先级：`PromptAsset.temperature`（最高）→ Base 模板默认值 → `TaijiConfig` 全局默认值（`0.7`）。

### 8.11 心流分层通道 (Flow Channel)

分层资产全部运行在符号通道（归藏文件系统，V32 起按模型分区）。周易循环操作符号通道：Prompts/Workflows（行为与流程模板）是引导脚手架，在深层执行中消溶；Verifications（验证契约）与 Truths 持续；Skills 的统计信息通过 连山压缩算子 在 YAML 中维护和更新。纯云端架构下所有资产更新限于归藏文件系统，不涉及模型权重。

**选择理由：** Prompts（含原 L5 叙事 + L3 角色定义）是提示词层面的软引导——它们在任务开始时提供方向，但深层执行需要精准的、无干扰的纯技能驱动。消溶不是"移除"，而是"不再显式注入 prompt"——角色和叙事的信息密度已达到饱和，转为背景知识。

### 8.14 流式输出协议 (ChatAgent Streaming)

决策：ChatAgent 用 Rig 原生 `agent.stream_chat()` 实现逐 token 流式输出，经 WS 定向 mpsc 通道推送（不经过广播），`ServerResponse` 新增 `chunk` / `stream_done` 两个可选字段（`skip_serializing_if`），完全向后兼容。完整协议定义（struct + 前端消费逻辑）见 [`taiji-web/FRONTEND.md`](./taiji-web/FRONTEND.md) §4.2。

### 8.15 多 Provider 配置生态

从单一 `deepseek::Client` 扩展到 config 驱动的多 provider 注册表：

```rust
/// 配置文件中的 provider 条目。
pub struct ProviderEntry {
    pub name: String,        // "openai" | "anthropic" | "local-llama"
    pub base_url: String,    // API endpoint（OpenAI 兼容格式）
    pub api_key: String,     // 该 provider 的 API key（空则沿用全局 key）
    pub model: String,       // 默认模型名
}
```

`LlmConfig` 新增 `providers: Vec<ProviderEntry>` 字段。`ProviderRegistry` 内部分为两类客户端：
- **deepseek 客户端**：`HashMap<String, Arc<deepseek::Client>>`（现有，默认）
- **OpenAI 兼容客户端**：`HashMap<String, Arc<openai::Client>>`（新增，`ProviderEntry.name` 为 key）

选择理由：所有主流 LLM provider 均提供 OpenAI 兼容 API，`rig::providers::openai::Client` 配合自定义 `base_url` 即可覆盖 30+ provider。不做 trait object 动态派发（避免 `dyn CompletionClient` 的 Send + Sync 复杂度），保持简单。

### 8.16 ChatAgent 生命周期与隔离

ChatAgent 与 周易 Agent 的根本差异：

| 维度 | 周易 FittingAgent | ChatAgent |
|------|-----------------|-----------|
| 生命周期 | 瞬态（单次 run() → drop） | 会话级（24h 超时，可跨多次对话） |
| 工具集 | 5 Skills + recursive_decompose + causal_verify | 5 Skills 纯（无递归拆解/因果验证工具） |
| 循环 | 周易三相循环（Meta→Fitting→Causal） | 无循环（纯对话轮次，`max_turns=20`） |
| 历史 | task_dir/chat_history.json 内 STATE） | `{data_root}/chat/{session_id}.json`（会话独立） |
| 认知注入 | MetaAgent 编排的 MetaContext | 任务 meta（V40 起 ChatAgent 不再注入归藏摘要——对话角色不消费编排资产） |

ChatAgent **不进 周易循环**：它是旁路对话系统，不参与三相递归。ChatMessage 处理中不注册 `recursive_decompose` 和 `causal_verify` 工具。

### 8.17 会话历史持久化

聊天会话历史独立于任务目录存储：

```
{data_root}/
├── chat/
│   └── {session_id}.json    ← Vec<Message>（Rig Chat 历史，JSON 序列化）
└── tasks/
    └── ...
```

- **session_id**：由前端生成（`crypto.randomUUID()`），首次聊天时发送到后端；后端无 session_id 时自动生成 UUID v4
- **写入模式**：每次 `ChatAgentBuilder.chat()` 调用完成后，`save_json_atomic()` 原子写入完整历史
- **读取模式**：ChatAgent 构造时从文件加载历史；文件不存在 → 空历史
- **24h 清理**：`chat/` 目录下超过 24h 未修改的 `.json` 文件可被后台 GC 清理（轻量实现：每次新连接时扫描删除过期文件）

### 8.18 交接文件机制与失败分流 (Artifact Handoff & Failure Routing)

**原则：执行事实是唯一记忆，产出即交接。** 瞬态 agent（Meta / Fitting / Causal 相位执行器）结束即弃，唯一留存是产出物。中间记忆（chat_history / meta_ctx 推理过程）不跨层传播、不作为恢复与路由的事实来源（§1.4）。

**交接物 = `deliverables/handoff.md`——产出物之一，不设独立交接文件。** 写者：Fitting 超限/失败/取消路径；读者：父层、同任务其他 agent、恢复链、MetaAgent 校准。置于 `deliverables/` 内保证**可发现性**：

- **父 agent**：RecursiveDecomposeTool 注入 `parent_deliverables`（目录索引）→ 交接物自动可见；**V31 失败汇报**：失败子任务的交接产物路径同时进入 `ChildResultSummary.deliverables`（失败条目）→ 父阳读交接产物后精准再指导
- **同任务其他 agent（阴侧）**：CausalAgent verify/converge 本来就逐文件核验 `deliverables/` → 自然读到；**V31**：converge 输入含失败条目（Diverged 状态 DecomposeResult）→ 基于失败原因/交接产物裁决 Partial/Diverged，task_summary 输出失败分析与 rerun 建议
- **元校准**：BACK_TO_META 读 `deliverables/` 全部产出（含 handoff.md）
- **同级任务 agent**：独立任务互不读取；需协作时信息经父层聚合传递
- **恢复链**：产出继承 = 读 `deliverables/`

```markdown
---
phase: fitting
failure_reason: context_overflow | output_missing | constraint_violation | cognitive | degraded | other
degraded: false
output_refs: [deliverables/xxx.md]
---
# 交接信息（环境信息）

## 进度
已完成 A、B，未完成 C

## 剩余工作
- C 需分解为 C1/C2

## 决策
- 选用方案 X

## 约束状态
无违规
```

- **触发**：FittingAgent 上下文长度 ≥ 250k（V29 精准 token 统计，替换 max_turns 轮次）、LLM 降级、失败、取消——一律先写 `deliverables/handoff.md` 再返回，禁止裸 `LLMCallFailed` 上抛（**残缺产出 > 无产出**）
- **收尾调用（LLM 压缩收尾，交接 = 压缩产物）**：交接文件是上下文压缩的产物——超限/失败时用**一次聚焦的瞬态调用**把本拟合对话压缩为结构化交接正文（进度 / 剩余 / 决策 / 约束 / 失败原因），只做「收尾写 handoff.md」不续聊。这与 Prime Agent compaction 同构（结构化摘要 + 保留执行状态），但**消费方向不同**（摘要回注入同会话 vs 跨层传给下一瞬态 agent 作恢复）且**多了编排失败语义**（超限触发本身就是任务粒度错误 = 编排失败的硬证据，驱动 BACK_TO_TPN / 连续超限强制残缺产出——Prime Agent 无此信号，其压缩是常规操作）。
  - **压缩输入**：chat_history 序列化（`[User]/[Assistant]/[Tool result]` 格式，工具结果截断 2000 字符）→ 截断到 `compress_input_tokens`（默认 20k，**首部 2k 保留任务目标 + 尾部最新状态**，中间省略标记）——超限路径不得再花一次大调用
  - **压缩输出**：结构化 Markdown 正文（## 进度 / ## 剩余工作 / ## 决策 / ## 约束状态 / ## 已产出文件），max_tokens 2048，temperature 0.2
  - **降级链**：LLM 压缩失败 / 超时（30s）→ 降级静态正文（v1 确定性收尾），仅 `warn!` 不阻断错误传播——交接文件写失败与压缩失败均不得阻断「残缺产出 > 无产出」
  - **禁止对话换皮**：交接正文只含从对话中可证实的执行事实（环境信息），不含对话过程本身——否则就是中间记忆跨层（§1.4 违规）
- **环境信息精炼**：handoff.md 只含环境事实（进度 / 剩余 / 决策 / 约束 / 失败原因）与产出引用，**不含对话过程**——否则就是中间记忆换皮（LLM 压缩收尾只做提取，不做转录）
- **连续超限上限**：同一路径连续 2 次因超限回退 → 强制「残缺产出即最终产出」，不再拆解（防止拆解粒度错误导致递归超限）

**失败分流（结构化信号运行时捕获优先，LLM 裁决兜底）**：failure_reason 由 Fitting 错误路径**运行时直接捕获**（≥ 250k → context_overflow、≥ 300k → hard_cutoff 等，V29 §8.19），随返回路径传给 TpnCycle 路由，**不依赖解析交接文件**；写入 handoff.md 仅作审计与 LLM 消费。

| failure_reason | 路由 | 语义 |
|---|---|---|
| context_overflow | BACK_TO_TPN | 粒度错误 → 阳基于产出递归分解 |
| output_missing | BACK_TO_TPN | 同上（无产出 = 任务未拆到位） |
| constraint_violation (Hard) | BACK_TO_META | 约束缺失 → 元校准 Truths 与权重 |
| cognitive | BACK_TO_META | 策略/资产问题 → 元基于产出校准 |
| degraded | LLM 裁决 | 降级产物质量存疑 |
| other | LLM 裁决 | 兜底 |

**恢复优先级链（V28 修订）**：`deliverables/`（含 handoff.md）> `decompose_result.json` > 重跑——chat_history 仅本节点断点续聊兜底，不再作为结果重建来源（§8.1 同步）。

**BACK_TO_TPN 语义（V28 修订）**：不再以「原 description + chat_history 重放」重跑——读取 `deliverables/`，FittingAgent **基于前一瞬态产出递归分解**。

**BACK_TO_META 语义（V28 修订）**：MetaAgent 输入增加前一瞬态产出摘要（`MetaAgentBuilder.run(description, tags, handoff)`，契约 6），基于失败产物**校准权重与认知资产**（归藏保持只读，校准结果注入 MetaContext），不再空手重跑。

**不做上下文压缩（特意设计）**：上下文窗口是单次概率拟合的采样空间。超限即粒度错误信号，动作为交接 + 拆解，而非压缩后续跑——压缩把过期中间记忆重新注入新采样，污染拟合（§1.4）。

### 8.19 上下文窗口预算 (Context Window Budget)

**轮次不反映上下文消耗，弃用 max_turns。** Rig `max_turns` 是 LLM 调用轮数计数器（旧默认：Meta 6 / Fitting 30 / Causal 10），与 token 消耗不对应——一次工具调用可返回 10k tokens 工具结果，30 轮可能远超窗口。V29 起 TPN 内瞬态 agent（Meta / Fitting / Causal）统一使用**精准上下文长度统计**：

- **统计源**：`CompletionResponse.usage.input_tokens`（provider 报告的真实请求 token 数，含历史重放与工具结果），经 `on_completion_response` hook 累计（FittingHookSet 内 ContextLimiter；Meta / Causal 同机制挂载）
- **阈值**（`config.json → context_limits`，默认值）：

| 阈值 | 动作 |
|---|---|
| `handoff_tokens` = 250k | 超限 → `HookAction::Terminate("context_overflow")` → **必须写 `deliverables/handoff.md`**（残缺产出 + 环境信息，§8.18）→ failure_reason=context_overflow → BACK_TO_TPN → 阳基于产出递归分解 |
| `hard_cutoff_tokens` = 300k | 硬截止 → `Terminate("hard_cutoff")` → 写交接文件 → **直接上报 FAIL**，不进 BACK_TO_* 循环（预算保护） |
| `compress_input_tokens` = 20k | 收尾压缩输入截断上限（§8.18 LLM 压缩收尾）：序列化对话截断到此量（首部 2k + 尾部，中间省略标记），防超限路径再花大调用 |

- **余量设计**：250k→300k 的 50k 余量即「收尾写交接」预算（§8.18 收尾调用）——触发后 LLM 状态已差也不影响交接落盘
- **路由信号**：failure_reason = context_overflow / hard_cutoff 由运行时捕获随返回路径传递（§8.18 分流表；hard_cutoff 等效 context_overflow 但强制 FAIL）
- **轮次计数器降级**：`max_rounds`（BACK_TO_TPN 重试上限）/ `max_cycles` 保留为循环防护（§8.6），不再承担上下文管理职责——计数器防死循环，token 预算管上下文，职责分离
- **ChatAgent 例外**：交互式对话保留 `max_turns=20`（单轮交互语义，非长程概率拟合，不适用交接/拆解回路）

---

### 8.20 分封制：任务自我认知（身份 + 地位）与会盟

**管理模型 = 分封制。** 根任务（天子）分封子任务（诸侯），诸侯可再分封；封地（task_dir）自治，贡品（deliverables/）公开陈列，中间记忆（chat_history / meta_ctx / trace 等）仅本节点可见；瞬态生命周期——任务即用即弃，唯一遗存是产出（§1.4）。

**双相位治理模型（V31 补全）**：阳相位 = **管理**（递归泛化拆解 / 接受汇报 / 汇总子任务产出 / 得出最终产出 / 子任务再恢复与再指导）；阴相位 = **裁判**（本任务节点收敛 converge / 本任务节点验证 verify / **向上父任务汇报**——裁决载体 = DecomposeResult 完整返回（含失败条目），失败场景不断流 / **路由重试本任务节点**——verify → route → BACK_TO_TPN/BACK_TO_META，本节点自我纠错回路）。子任务失败由父阳决策（rerun_of 再启用 + 修正指导 / 接受残缺综合 / 整体失败上抛），防护 = rerun_of 同轮去重 + max_rounds（§8.6）。

**任务自我认知**（注入阳 Agent system prompt 的「身份与地位」段，`build_identity_section`）：

| 要素 | 内容 | 来源（确定性） |
|------|------|------|
| 身份·内容 | task description | meta.json.description（创建时入册） |
| 身份·类别 | 编排/执行（阳）、验证/收敛（阴） | **元权重更新阶段确定**：MetaContext.mode（§8.8）；模板已教学 |
| 身份·兄弟 | 同级子任务贡品索引 | 会盟注入：YangPrompt.sibling_deliverables |
| 身份·父 | parent_id + 父 description | meta.json.parent_id → 父 meta.json（根任务注明「根任务（天子）」） |
| 身份·子 | subtask_ids | meta.json.subtask_ids |
| 地位·层级 | depth / max_depth | EngineContext + config（§8.6） |
| 地位·权限 | 可读写本任务 deliverables/；父产出与兄弟贡品只读；中间记忆仅本节点可见 | SafetyHook 执行层强制（§8.5/§8.9）+ 教学层显式告知 |

**确定性原则**：身份与地位全部由系统赋予——创建时入册（内容/父/子）、元阶段决策（类别）、递归结构派生（层级）、分封时快照（兄弟）——**禁止 LLM 分类或运行时推断**。同一条创建路径 → 同一身份，可复现、可审计。

**会盟（兄弟贡品发现）**：RecursiveDecomposeTool 分封时向子任务注入**兄弟贡品陈列室目录**（`children/<idx>/deliverables/` 绝对路径，BTreeMap 有序扫描，排除自身——注入目录而非文件快照：同批并行兄弟在分封时点尚无产出，目录 = 动态发现入口，子任务执行中可经 read 工具随时发现陆续陈列的贡品；跨轮/rerun 同样有效）。读取由子任务自行 read（贡品公开陈列语义）。

**能看不能写（执行层强制）**：兄弟关系是**单向观摩**——read 开放（贡品公开陈列，父产出与兄弟贡品可读），write 封闭（封地自治：写入必须落在本任务 `task_dir` 内）。执行层强制 = `FittingHookSet` 写路径域校验（`on_tool_call` 对 write 工具目标路径做归一化前缀检查，`task_dir` 外一律 `ToolCallHookAction::skip` + warn）——SafetyHook 黑名单只拦 `..`/`~`/`/etc` 等，绝对路径直写兄弟目录（无 `..`）不触发，必须域校验兜底（与全局单例 SafetyHook 不冲突：域校验持有 per-agent task_dir，放 FittingHookSet 转发链）。兄弟任务目录内非 deliverables 文件（中间记忆）不可读不可写；兄弟间一切通信汇总由父层处理（聚合 → converge → BACK_TO_TPN 注入）。

**贡品可见性修订（§8.9 兄弟隔离条款收窄）**：兄弟隔离收窄为「兄弟任务目录内非 deliverables 文件不可见」——贡品跨兄弟**公开可发现可读**；中间记忆仍隔离。SafetyHook 黑名单（`..` / `~` / 系统路径）不拦截任务树内贡品绝对路径。

**无降级原则（V30 起新代码）**：禁止降级兜底——新代码读身份册失败 / 会盟扫描失败一律错误上抛（`TaijiError`），问题暴露后修根因，不用默认值掩盖。「无父（根任务，parent_id=None）」与「无兄弟（children/ 为空）」是**状态分支**，非降级。既有降级点（MetaContext::empty、Base 模板、压缩静态正文、load_json_optional 等）维持现状，改造另立章节。

**注入实现**：`build_identity_section(engine_ctx, meta_ctx) -> Result<String>`（fitting.rs 同步函数）读本册 + 父册 + meta_ctx.mode + 兄弟索引 → 「身份与地位」段 push 到 system_prompt 末尾（归藏资产与 Base 模板统一生效，与 §8.19 预算纪律同模式）。不注入 Meta/Causal（Causal 核验本任务贡品无需兄弟；Meta 校准走既有 handoff 路径）。

### 8.22 验证契约引擎（ContractEngine）

**职责**：CausalAgent.verify 前置的确定性验证执行器——加载当前模型分区 `verifications/` 结构化契约，机械执行 checks，产出 ContractReport。**确定性保证：同一契约 + 同一产出 → 同一结果**，与 LLM 无关。

**执行顺序（verify 内部管线，V33 修订）**：

```
ConstraintEngine（Truths Hard 短路）→ ContractEngine（verifications checks 机械执行）
    → 若 hard 项全过 → LLM 裁决 llm_judgement 项 → VerificationReport
```

**LLM 输入**：ContractReport（passed + results + summary）注入 verify prompt——LLM 看到的不是「自由裁量」，而是「机械检查结果 + 待裁决项」（§6.6 L2）。

**工具注册**：ContractEngine 是 Rust 内部函数（非 LLM 工具）——LLM 不可调用、不可绕过。与 ConstraintEngine 同构（确定性引擎，hard 短路语义一致）。

**契约命令安全面（V33 预埋）**：CheckSpec 中 command_succeeds 类检查项可执行命令——**MVP-1 仅允许白名单安全命令**（编译 / 测试 / 静态检查），白名单与 SafetyHook 同源审批，禁止任意 shell 命令进契约——防契约资产被污染后变成任意代码执行面（契约由连山 fork/人工种子写入，是潜在注入面）。

**TraceConsistency 检查项（V34，MVP-4：断言证据链）**：CheckKind 第 6 类，L1 扩展——**断言引用完整性**（reference_resolves 从文件推广到 trace 记录）：扫描产出文件（target glob）中 `[证据: 工具名]` 格式引用 → 校验任务 trace.jsonl `tool_call::*` 记录中存在该工具调用（存在性 + 类型匹配）。纯机械零 LLM；**只对精确格式引用做存在性判定，无匹配/无标记一律视为推测处理——宁漏勿误，零误报优先**（防硬短路误伤）。`(推测)` 标记计数（speculation_count）注入 CheckResult.detail 作质量信号。params 键约定（复用 `params: Value`，零 schema 变更）：`evidence_pattern`（默认 `[证据: {tool}]`）、`speculation_marker`（默认 `(推测)`）、`allowed_tools`（默认 webfetch/search/read/bash）、`trace_glob`（默认 trace.jsonl）。

**断言分级教学（V34，Fitting 侧）**：build_system_prompt 追加「断言分级规则」段（预算纪律后）：证据断言必须附 `[证据: 工具名]`（引用真实工具调用）、推测断言必须标 `(推测)`、禁止编造证据引用。教学层与检查层是双保险：检查层独立运作（对已有标记仍可判定），LLM 完全不标记时检查退化为空转——推测占比统计经 连山演化淘汰高推测诱发资产。**激励闭环**：虚假证据 = 机械 FAIL（hard 短路 → backprop 贝叶斯 β++ → 资产降权淘汰）；无证据 = 显式标注 + 统计降权；真实证据 = 唯一稳定通过策略——诚实成为占优策略（§6.0 ABox 证据链）。

**随机审计（V34 预留，P2）**：`runtime.dmn.audit_rate`（默认 0）——概率触发深度复查（webfetch 重放来源 URL + LLM 语义复核）。MVP-4 不实现（依赖网络 + LLM，成本高），字段预留、激活条件后置。

**与归藏的关系**：契约资产经 MetaAgent UCB 检索（与 prompts 同通道，§8.8），命中即注入 verify 流程；**无契约资产时 verify 退化为纯 LLM 验证（现状保留）**——降级路径不改，MVP-1 阶段种子契约逐步补齐（§8.23）。

**与 连山的关系**：CheckResult 随 verify_state.json 既有路径回传——检查项通过率是 连山统计与 MCTS 演化的数据源（§6.4 V33 统计粒度）。


## 9. 前端架构（taiji-web 纯 Web 应用）

> 详细设计见 [`taiji-web/FRONTEND.md`](./taiji-web/FRONTEND.md)。本节仅保留架构决策表与 WS 接口契约。

### 9.1 前台架构决策

| 决策 | 选择 | 理由 |
|------|------|------|
| 前端框架 | React + TypeScript | 生态最成熟，React Flow 原生支持 |
| 应用壳 | **无（纯浏览器）** | 绕过 WebKitGTK DMA-BUF bug，Chromium 不受影响 |
| HTTP 静态托管 | axum + tower-http（Rust 核心内嵌） | 单进程方案，零额外服务 |
| 图渲染 | React Flow | 自定义节点 + 自定义布局 + 动画支持 |
| 动画 | Framer Motion | 声明式动画，状态过渡自动处理 |
| CSS | TailwindCSS | 快速布局，暗色主题 |
| 通信 | WebSocket 双向（tokio-tungstenite） | 事件广播 + 请求响应同一连接，低延迟 |
| 太极图 | SVG + CSS Animations | 纯前端实现，无额外依赖 |
| 浏览器打开 | xdg-open | Linux 桌面标准，跨平台可扩展 |

### 9.2 接口契约（续 §3）

> 编号续接 §3 关键接口契约（1-15）。前端消费方的 TypeScript 接口见 FRONTEND.md。`ChatAgentBuilder.chat` / `build_system_prompt` 已在 §3 #14/#15 列出，此处不重复。

| # | 契约 | 说明 |
|---|------|------|
| 16 | `WsServer::broadcast(event: TaskEvent)` | WebSocket 广播：将 TaskEvent 推送至所有连接的 WebSocket 客户端（无变化） |
| 17 | `TaskTreeBuilder::build(root_task_id) -> TaskTreeSnapshot` | 扫描 `data/tasks/{root}/children/` 递归目录树，构建 SpindleNode 列表 + 边 |
| 18 | `WsHandler::submit_review(intervention: YinIntervention, data_root: &Path) -> Result<()>` | 前端审批提交：将人工干预写入 `review.json` |
| 19 | `WsHandler::handle_chat_message(message, session_id, context_task_id, state, on_chunk: Box<dyn Fn(String) + Send + Sync>) -> Result<(String, String), TaijiError>` | WS handler 层：解析/生成 session_id（session_id 为空时 `Uuid::new_v4()`），调用 `AgentFactory.create_chat_agent(session_id, context_task_id, None, None)` → `builder.chat()`。`on_chunk` 转发到 `WsServer::send_to` 逐 chunk 推送（`ServerResponse::chunk`）。完成时 `ServerResponse::stream_done` 携带 `{"text": final_text, "sessionId": resolved_session_id}`。返回 `(final_text, resolved_session_id)` |
| 20 | `WsHandler::get_task_tree(root_task_id: &str, data_root: &Path) -> Result<TaskTreeSnapshot>` | 前端主动拉取完整任务树快照 |
| 21 | `WsHandler::list_tasks(data_root: &Path) -> Result<Vec<String>>` | 列出所有根任务 ID（按 mtime 倒序） |
| 22 | `WsHandler::get_tpn_state(task_id: &str, data_root: &Path) -> Result<TpnPhaseState>` | 获取指定任务的 TPN 相位详情 |
| 23 | `WsHandler::execute_task(description: String, factory: &AgentFactory, config: &TaijiConfig, data_root: &Path) -> Result<TaskTreeSnapshot>` | 执行新任务并返回快照（异步，RecursiveRunner） |



---

---


---

## 工程基建（Rig 本地化，原 §8.7）

Rig 0.39（rig-core + rig-derive）已 vendor 到 `vendor/` 目录，Cargo.toml 通过 `[patch.crates-io]` 重定向。原因：

1. **Rig 仍处于 0.x 不稳定阶段** — 频繁 API 变更导致上游 breaking change 不可控
2. **简化依赖** — 剔除不需要的 feature flag 和可选依赖（qdrant、lancedb、fastembed 等）
3. **自定义修改** — 允许在 vendor 内对 Rig 源码做最小修补

Vendor 策略：

| 层 | 原始 crate | vendor 路径 | 说明 |
|----|------------|-------------|------|
| 应用入口 | `rig` | `vendor/rig/` | 薄 facade，re-export rig_core::* |
| 核心库 | `rig-core` | `vendor/rig-core/` | Agent/工具/提供者/补全核心 |
| 过程宏 | `rig-derive` | `vendor/rig-derive/` | Tool derive 宏 |

taiji 使用 `rig = { version = "0.39" }`（语法占位）+ `[patch.crates-io]` 指向 vendor。上游 Rig 的非核心可选依赖（companion crates）被剥离。
重新 vendor 的操作：`cargo package --allow-dirty` 可验证 vendor 目录自恰性。

---

## 二、归藏与连山（符号固化与压缩算子）

## 6. 归藏 (Guizang) 符号系统

> 归藏 = 周易执行迹经连山压缩后的符号固化。周易递归任务树的低维投影。不是知识库、不是 RAG、不是本体论工程。

### 6.0 归藏重定性（V42 同构统一）

**归藏不是"规范性本体论（验证契约库 + 生成资产库）"——那是 V33 的阶段性理解。V42 定论：归藏 = 冻结的执行经验。**

归藏有三类资产，对应同一个泛化-压缩循环的不同阶段：

| 资产类 | 目录 | 作用域 | 压缩源 | 消费方 |
|------|------|------|------|------|
| **阴阳对偶资产** | `yang/` + `yin/` | 单任务节点 | 单节点 周易执行迹 | MetaAgent UCB 检索 → FittingAgent/CausalAgent 注入 |
| **贝叶斯后验** | `models/` | 跨阴阳（按资产 id 关联） | 所有相关任务的 PASS/FAIL 迹 | UCB 排序 / fork/merge/prune 决策 |
| **非线性流型拓扑** | `manifold/` | 整个根任务执行 / 主动学习 | 根任务树 + 跨任务统计 + BCP 协议 | MetaAgent 宏观调控 / 模型路由 / 演化策略 |
| **标准化 Skills** | `skills/` | 单任务节点（反作用） | 从 manifold/ 经 周易执行压缩而来 | SkillRegistry → Rig Tool 动态注册 |

**阴阳对偶资产**——作用于单个 周易节点的生成与验证两侧：

| 目录 | 内容 | 消费方 | 消溶/沉淀 |
|------|------|------|------|
| `yang/prompts/` | 阳的系统提示词——FittingAgent 编排/执行模板（拆解教学、综合引导、模式分配） | MetaAgent → FittingAgent system prompt | 深层消溶 |
| `yang/workflows/` | 工作流定义——步骤序列、命令模板、验收要点 | MetaAgent → FittingAgent system prompt | 深层消溶 |
| `yang/skills/` | 生成 skills——write/bash/git-commit/cargo-test 等**变更世界**的工具 | SkillRegistry → FittingAgent 工具注册 | 深层沉淀 |
| `yin/prompts/` | 阴的系统提示词——CausalAgent verify/converge 模板（核验教学、收敛判决） | MetaAgent → CausalAgent system prompt | 持续 |
| `yin/verifications/` | 验证契约——ContractEngine 机械执行的 checks（file_exists/schema_valid/...） | ContractEngine 机械执行 → LLM 裁决 | 持续 |
| `yin/skills/` | 验证 skills——read/webfetch/search/trace_consistency 等**只读验证**的工具 | SkillRegistry → CausalAgent 工具注册 | 深层沉淀 |

**阴阳 skills 的区分是权限性的，非功能性的**：yang/skills 注册给 FittingAgent（执行权——可变更世界），yin/skills 注册给 CausalAgent（裁判权——只读验证）。同一工具路径（如 read）可同时出现在两侧（阳需要读上下文、阴需要核验产出），但注册面天然隔离——阳不可见阴的验证专用 skills（如 trace_consistency），阴不可见阳的执行 skills（如 bash）。这与 §1.2 三相权限分工一致。

**非线性流型拓扑（manifold/）**——连山压缩整个根任务执行/主动学习的高维迹后固化的低维拓扑文件：

| 文件 | 内容 | 压缩了什么 |
|------|------|------|
| `bcp.yaml` | BCP 蓝图-完型协议的结构化版本——接口契约、数据流、模块边界的机器可读定义 | 人类 BCP 文档 → YAML schema |
| `agents.yaml` | AGENTS.md 避坑规则的结构化版本——约束清单、必检项、禁止模式 | 人类 AGENTS.md → 可机械检查的规则表 |
| `topology.yaml` | 流型拓扑图——模块依赖图、数据流图、调用关系图 | 代码结构 → 图结构 |
| `contracts.yaml` | 接口契约定义表——所有 §3 关键接口的输入/输出/错误类型 | BCP §3 → 接口定义表 |
| `env.yaml` | 环境信息——模型版本、配置参数、分区键、运行时约束 | 运行时环境 → 参数表 |

**manifold/ 的定位**：它是 BCP 蓝图协议（人类可读）的**机器可消费版本**。BCP-蓝图-完型协议.md 是人类维护的事实源，manifold/ 是其结构化投影——两者的关系如同源代码与 AST：人类编辑 .md，连山压缩为 .yaml，周易消费 .yaml 执行任务。

**标准化 Skills（skills/）**——从 manifold/ 经周易 周易执行压缩而来的可复用程序：

```
manifold/bcp.yaml + agents.yaml + topology.yaml + contracts.yaml + env.yaml
        │
        │ 作为上下文注入一次周易任务
        │ "将 BCP 契约 X 压缩为标准化 skill"
        ▼
    周易 周易执行（泛化）
        │ 阳 FittingAgent：拆解 BCP 契约 → 提取可复用模式 → 编写 skill 程序
        │ 阴 CausalAgent：验证 skill 符合接口契约 + 安全约束
        │ 元 MetaAgent：路由决策
        │
        ▼
    skills/{skill-name}.yaml（压缩固化）
        │
        │ 反作用于未来单任务节点
        ▼
    下一轮 周易节点的执行效率提升（四维权重增强）
```

**这就是"压缩即智能"的最终闭环**：BCP 人类设计 → manifold 结构化投影 → 周易执行压缩 → skills 可复用程序 → 反作用周易节点 → 执行效率提升 → 四维权重反馈 → 演化 manifold 拓扑 → BCP 修订。这不是离线编译——**BCP→Skills 的每一次压缩本身就是一次 周易任务执行（周易），产生的 skills 是 deliverable（归藏），统计信号回传更新 models/（连山）。**

**归藏资产树与 周易任务树的同构映射（V42 定论）：**

```
周易任务树（周易执行空间）              归藏资产树（连山压缩投影）
┌────────────────────────┐          ┌──────────────────────────┐
│ Root Task               │          │ manifold/（根级拓扑）     │
│  ├─ decompose           │  压缩    │  ├─ skills/（标准化产物）  │
│  │  ├─ child-0 execute  │ ──────→  │  ├─ yang/prompts          │
│  │  │  ├─ yang (生成)   │          │  │  ├─ fork → variant-1   │
│  │  │  └─ yin (验证)   │          │  │  └─ backprop (stats)   │
│  │  ├─ child-1 execute  │          │  ├─ yin/verifications     │
│  │  │  └─ verify FAIL   │          │  │  ├─ fork → variant-2   │
│  │  └─ converge         │          │  │  └─ merge?             │
│  └─ BACK_TO_TPN reroute │          │  └─ models/（跨阴阳后验）  │
└────────────────────────┘          └──────────────────────────┘

同构映射：
  decompose → fork              (开新分支)
  converge  → merge             (收敛近邻)
  FAIL      → prune             (淘汰低效)
  child→parent stats → backprop (四维回传)
  BCP（人类设计）→ manifold/    (人类→机器可消费)
  manifold → skills/            (经 周易执行压缩为可复用程序)
```


### 6.1 按模型分区的资产树模型（V32 重构 / V36 实现层定稿 / V42 阴阳对偶 + 流型拓扑）

> **状态：V36 落地**（V32 蓝图承诺，V33-35 未兑现，V36 实现）。V42 目录结构重新设计（阴阳对偶 + manifold + skills）——**本次为蓝图定稿，代码尚未迁移**。落地要点：① `LiluoClient` 支持 `root_dir`（knowledge 根）+ `data_dir`（活动目录）双路径——根 client 的 `for_model(key)` 派生分区 client（`data_dir = root/{model_key}`），`model_stats.yaml` 恒在根级；② 迁移函数 `migrate_to_partitioned(root, default_key)`（幂等：旧根资产目录 → 默认模型分区）；③ 检索/写回均走分区 client——MetaAgent 按路由结果分区检索（§8.8），DMN 按 pending 的 `model_key` 分区回传（§6.4）；④ `MetaContext.model` 是分区唯一载体（§8.3 分区一致性）。

**归藏按模型分区（V42 阴阳对偶结构）**：每个模型（`model_key = {provider}-{model}` slug）拥有独立的资产树：

```
.taiji/knowledge/
├── {model_key}/                 ← 该模型的资产树分区
│   │
│   ├── yang/                    ← 阳轨资产（单任务节点·生成侧）
│   │   ├── prompts/             ← 阳的系统提示词（FittingAgent 编排/执行模板）
│   │   │                          消溶：深层执行中不再显式注入
│   │   ├── workflows/           ← 工作流定义（步骤序列、命令模板、验收要点）
│   │   │                          消溶：深层执行中不再显式注入
│   │   └── skills/              ← 生成 skills（write/bash/git-commit/cargo-test...）
│   │                              沉淀：深层执行中统计积累
│   │
│   ├── yin/                     ← 阴轨资产（单任务节点·验证侧）
│   │   ├── prompts/             ← 阴的系统提示词（CausalAgent verify/converge 模板）
│   │   │                          持续：全程有效
│   │   ├── verifications/       ← 验证契约（ContractEngine 机械 checks）
│   │   │                          持续：全程有效
│   │   └── skills/              ← 验证 skills（read/webfetch/search/trace_consistency...）
│   │                              沉淀：深层执行中统计积累
│   │
│   ├── models/                  ← 贝叶斯后验（跨阴阳，按资产 id 关联）
│   │                              每个 yang/yin 资产同名关联一个 ModelAsset（α/β）
│   │
│   ├── manifold/                ← 非线性流型拓扑（根任务级·连山压缩）
│   │   ├── bcp.yaml             ← BCP 蓝图-完型协议结构化版本
│   │   ├── agents.yaml          ← AGENTS.md 避坑规则结构化版本
│   │   ├── topology.yaml        ← 流型拓扑图（模块依赖/数据流/调用关系）
│   │   ├── contracts.yaml       ← 接口契约定义表（所有 §3 接口的签名/错误类型）
│   │   └── env.yaml             ← 环境信息（模型版本/配置参数/分区键/运行时约束）
│   │
│   └── skills/                  ← 标准化 Skills（从 manifold/ 经 周易执行压缩而来）
│       │                          太极项目式：LLM 辅助、可复用程序为主的标准化步骤
│       ├── rust-project.yaml    ← Rust 项目标准流程
│       ├── git-workflow.yaml    ← 标准化 Git 工作流
│       └── ...                  ← 更多经 周易验证的可复用程序模板
│
├── model_stats.yaml             ← V32 元权重表：(model_key × tag) → 统计，ModelRouter 数据源
│                                  恒在 knowledge 根（跨分区共享）
└── (V41：根级无资产层目录)
```

**V42 相对旧结构的迁移映射：**

| 旧路径（V36-V41） | 新路径（V42） | 语义变化 |
|---|---|---|
| `prompts/orch-fitting.yaml` | `yang/prompts/orch-fitting.yaml` | 阳模板归入 yang/ |
| `prompts/exec-verify.yaml` | `yin/prompts/exec-verify.yaml` | 阴模板归入 yin/ |
| `verifications/v-*.yaml` | `yin/verifications/v-*.yaml` | 验证契约归入 yin/ |
| `models/*.yaml` | `models/*.yaml` | 不变（跨阴阳） |
| `skills/`（当前空目录） | `yang/skills/` + `yin/skills/` + `skills/` | 分裂为三类 |
| — | `manifold/` | 全新：流型拓扑 |

**分区运行时行为（V42 阴阳对偶）：**

| 层 | 资产类型 | 舒张期（浅层 周易执行） | 收缩期（深层 Flow） | 压缩态 |
|:---:|------|------|------|------|
| `yang/prompts/` | 阳系统提示词 | MetaAgent UCB 检索 → FittingAgent system prompt 注入 | 消溶：教学文本溶解，内化为 LLM 行为偏好 | 文本→行为 |
| `yang/workflows/` | 工作流定义 | 与 prompts 同通道检索注入 | 消溶 | 步骤→习惯 |
| `yang/skills/` | 生成 skills（执行工具） | SkillRegistry → FittingAgent 工具注册 | 沉淀：高频成功模式统计积累 | 教学→技能 |
| `yin/prompts/` | 阴系统提示词 | MetaAgent UCB 检索 → CausalAgent system prompt 注入 | 持续：全程有效 | 文本→行为 |
| `yin/verifications/` | 验证契约（机械 checks） | ContractEngine 机械执行 → LLM 裁决 | 持续：全程有效 | 判据→判定 |
| `yin/skills/` | 验证 skills（只读工具） | SkillRegistry → CausalAgent 工具注册 | 沉淀 | 教学→技能 |
| `models/` | 贝叶斯后验（α/β） | UCB 排序权重 | 持续：后验持续影响路由与选择 | 迹→信念 |
| `manifold/` | 流型拓扑（BCP+AGENTS+接口契约+环境） | MetaAgent 宏观调控 / 模型路由 / 演化策略决策 | 持续：作为根任务级认知基线 | 设计→拓扑 |
| `skills/` | 标准化 Skills（manifold→TPN 压缩产物） | SkillRegistry → Agent 工具注册 | 沉淀：反作用单节点执行效率 | 拓扑→程序 |

**模型-领域学习单元（V37 语义定稿）**：分区不只是资产隔离，而是**学习单元**——每个分区是 (模型 × 约束系统) 的绑定主体：**模型提供概率地形**（猜想源：LLM 生成候选），**约束系统**（prompts/workflows/verifications，V38 起 truths 内置化不属资产）**提供机械判据**（反驳源：ContractEngine 验证候选），**分区统计**（stats / models/ 贝叶斯后验）**提供累积**（选择源：连山回传与演化），三者绑定为一个**独立演化的领域学习单元**——模型不变，学习发生在围绕它的符号结构。推论（机制已存在，V37 显式化）：**契约难度随模型能力自适应**——各分区统计独立 → 弱模型分区通过率低 → fork 宽松变体（`strictness="loose"`）；强模型分区通过率高 → fork 严格变体（`strictness="strict"`）；同一领域契约在不同分区按各自统计独立演化（难度 = f(模型能力)），**变体树不跨分区**（fork/merge/prune 只作用于本分区资产）。

周易执行期间只读，连山（连山）单写者更新（**分区维度：任务级路由下任务内所有 Agent 使用同一分区**——按路由模型的 model_key，MetaContext.model 是唯一载体；V37 相位级路由激活后各相位按其路由模型用对应分区，§8.8）。

**Skills 与归藏的关系：** 5 个内置 Skill（read/write/bash/search/webfetch）在 Rust 中硬编码注册到 FittingAgent，**不读取** `skills/` 目录。归藏中的技能统计（success_rate/use_count）作为元数据由连山（连山）维护。未来 SkillCompiler 激活后，skills/ 将成为完整的归藏第四类可演化资产（§1.4 泛化-压缩循环）。

**种子复制（V39，模型切换路径命令化）**：`taiji seed <target_key> [--from <source_key>]`——把源分区的**活跃种子资产**（`prompts/` + `verifications/` 中 status != pruned）文件级复制到目标分区（默认源 = 配置的默认模型分区）。复制范围定界（§6.1 学习单元语义）：**不复制 `models/`（贝叶斯后验 = 该模型的累积，新单元从零开始；复制旧统计会污染路由 UCB——用旧模型的成功经验指导新模型）**；不复制 skills/（内置硬编码）；version 保持原值（内容快照，非演化写）。幂等：目标已存在同名资产 → 跳过不覆盖；目标分区自动创建（`for_model`）。机械检查项（L0/L1）与模型无关可直接复用；llm_judgement/prompts 教学资产随模型地形可能失配，由目标分区 连山演化自然淘汰（贝叶斯降权 + prune）。源分区缺失 → 上抛（无降级原则）。


### 6.2 资产字段契约

**通用字段（所有层共享）：**

| 字段 | 类型 | 说明 |
|------|------|------|
| `id` | String | 唯一标识（如 `prompt:orch-fitting`） |
| `type` | String | prompt / truth（由目录隐式确定） |
| `layer` | u32 | 4 / 5（沿用原层号；V38 起 truths 层移除，layer=4 仅存在于历史资产） |
| `name` | String | 名称 |
| `description` | String | 描述 |
| `tags` | Vec[String] | 搜索标签 |
| `confidence` | f64 | [0, 1] 置信度 |
| `version` | u32 | 版本号（保存时递增） |

**类型特有字段：**

| 层 | 额外字段 |
|----|---------|
| Prompts | `content: String`（行为模板正文，含角色定义 + 工作流）, `agent_target: String`（"FittingAgent" \| "CausalAgent"）, `temperature: Option<f32>`（可选温度覆盖，None 时使用 Base 模板默认值）, `usage_count: u32`, `success_rate: f64`, **`stats: AssetStats`（V35：任务级 MCTS 统计，实现层补齐 §6.2 通用树字段承诺——MVP-6 回传写入）** |
| Workflows（V32） | `content: String`（步骤序列/命令/验收要点）, `agent_target: String`, `usage_count: u32`, `success_rate: f64` |
| Verifications（V32/V33） | `content: String`（契约语义描述，人读）, **`checks: Vec[CheckSpec]`（V33：结构化检查项，机器执行——§4）**, `agent_target: String`（"CausalAgent" 为主）, `usage_count: u32`, `success_rate: f64` |
| Truths（V38 移除） | ~~`severity: String`; `justification`; `env_tags`~~ → 不再资产化；原语义内置为 ConstraintEngine L0 检查（severity 映射 ConstraintSeverity，justification 审计字段废弃） |
| models/ | **贝叶斯后验层（MVP-3.5 激活，原「预留层」）** — 每验证契约一个资产（id 与 verification 同名关联）：`alpha: f64`, `beta: f64`（Beta-Bernoulli 共轭后验），`steering_vector: Option<Vec<f32>>`（介入向量，仍预留） |

**V32 通用树结构字段（所有可检索资产层共享，serde default 零迁移）：**

| 字段 | 类型 | 说明 |
|------|------|------|
| `env_tags` | `Vec<String>` | 环境维度（空 = 环境无关）；检索时与当前环境指纹不匹配则降权 |
| `parent_id` | `Option<String>` | fork 来源（None = 根资产） |
| `variant_of` | `Option<String>` | 同源变体组 id（fork 树分组） |
| `stats` | AssetStats | MCTS 统计块：`n / pass_count / cost_tokens_sum / cost_tokens_sq_sum / quality_sum / verify_rounds_sum` |

**index.yaml 已移除（V38）**：标签检索改实时目录扫描（`scan_assets` 内存构建 tag → AssetRef 映射，不落盘；资产量级几十个，扫描毫秒级）。`relations`、`justification_depends_on`、`dependency_index` 不在资产模型中（历史：V22 起 index.yaml 仅剩 tag_index，V38 整个移除）。


### 6.3 检索（V32：UCB 选择替代纯 confidence 排序）

```mermaid
flowchart LR
    subgraph "MetaAgent 加载归藏（当前模型分区）"
        QUERY["task_type_tags → 标签匹配 assets"]
        QUERY --> LOAD["加载候选资产（prompts + workflows + verifications）"]
        LOAD --> RANK["UCB 排序（利用 + 探索）"]
        RANK --> MC["产出 → MetaContext（含 assets_used）"]
    end

    subgraph "{model_key} 分区"
        P1["prompts/*.yaml 节点"]
        P2["workflows/*.yaml 节点（V32）"]
        P3["verifications/*.yaml 节点（V32）"]
        S["AssetStats 统计"]
    end

    RANK --> S
```

检索策略：标签精确匹配 → 关键词子串搜索 → **UCB 排序**（非纯 confidence）：

```
score = avg_reward + C · √(ln N_total / N_node)
        └─利用：已验证好资产─┘ └───探索：样本少/新变体的加分──┘
```

- `avg_reward` 来自 AssetStats（§6.4 回报函数）；`N_total` 为候选集总采样数，`N_node` 为节点采样数
- **N=0 新资产给最大探索分**——保证冷启动资产能被采样，避免好资产被饿死
- **统计选择门槛**：`n < min_samples`（默认 3）的资产不参与利用排序，只走探索分——防止小样本假置信
- `confidence` 字段保留为**初始先验**（人工种子/经验值），进入利用排序后由 avg_reward 主导
- env_tags 与当前环境指纹不匹配的候选降权（V32 环境维度）
- 不支持向量嵌入，无关系图扩散

**实现层定稿（V35，MVP-5：检索数学化）**——prompts 检索排序兑现上述 UCB 设计（meta.rs 现状为手填 confidence 降序，非学习统计）：

```
score(id) = μ(id) + C · √( ln N_total / (n_id + 1) )

μ(id) = models/{id}.yaml 后验均值      （存在 ModelAsset）
      | §6.4.1 先验映射 α=1+k·c, β=1+k·(1−c) → μ=α/(α+β)（无 ModelAsset，未采样）
n_id  = usage_count（prompts 任务级回传计数，MVP-6 起增长）
C     = 1.414（常量，不随资产量调整——UCB1 渐近最优性，§6.3 设计不变）
```

**确定性保证（硬约束）**：n+1 平滑而非 n=0→∞ 特判——全冷启动时 score = 先验 μ 降序（确定性二级键，与 read_dir 顺序无关）；μ 缺失时回退 confidence 直接映射（同一公式，非新先验）。**过滤防线保留**：confidence ≥ 0.3 阈值过滤仍先于排序执行（零资产降级路径不变）。排序位置：knowledge.rs `search_prompts` 调用后（返回前），MetaAgent 消费顺序即 UCB 序——装配顺序：`tags 匹配 → 阈值过滤 → 加载 → UCB 排序`。

**信号粒度说明（V35）**：prompts 的采样信号是**任务级**（任务 PASS → 该任务编排所选 prompts 各记一次成功；FAIL/BACK_TO_META → 记失败），与 verifications 的**检查项级**（CheckResult 逐项）粒度不同——同一 backprop 管道、两套信号源（§8.21 MVP-6）。


### 6.4 连山演化（V32：MCTS 四算子 + 被动/主动双轨）

> **状态：** `dmn_consumer.rs` + `cognition_evolver.rs` 已实现（V31 及之前为占位/部分实现），V32 将占位算子升级为真实 MCTS 实现。纯云端架构下 DMN 在符号层（YAML）独立运作，不依赖本地模型。日常 `taiji run` 默认不激活以保持 周易只读模式，可通过 `--with-dmn` flag 启用。
>
> **激活条件：** 归藏各层有足够资产（每层至少 5 个）+ 累积 50+ 周易执行轨迹；统计选择需 `n ≥ min_samples`（3）。

**回报函数（写死进 BCP，config `runtime.dmn.reward_weights` 可覆盖）：**

```
reward = w_pass·pass_rate + w_quality·avg_quality − w_cost·avg_cost_tokens − w_rounds·avg_verify_rounds
默认: w_pass=0.5  w_quality=0.3  w_cost=0.2  w_rounds=0.1
```

- `pass_rate`：PASS 占比（stats.pass_count / n）
- `avg_quality`：质量分均值——**派生而非新增字段**：route 映射（PASS=1.0 / BACK_TO_TPN=0.4 / BACK_TO_META=0.2）× VerificationReport.confidence（不改 VerificationReport schema）
- `avg_cost_tokens`：trace `completion_response.usage.input_tokens` 累加均值（已在记录，零新增）
- `avg_verify_rounds`：BACK_TO_TPN 次数均值（验证轮数 = 收敛速度倒数）

**四维信号全部来自既有数据——零新增持久化文件。** 回报函数即 连山的改进方向（更省 token / 更精准 / 更快收敛 / 更高通过率），由系统价值判断写死，不由 LLM 自定。**V33 统计粒度：** 统计对象从「资产」精确到「检查项」（CheckResult 逐项通过率 / 耗时，随 verify_state.json 既有路径回传）——MCTS 演化的对象是契约有效性空间（fork/merge/prune 操作契约），资产级统计由检查项聚合（§8.21）。

```mermaid
flowchart LR
    PASS["周易 PASS → enqueue pending/{id}.json（携带 assets_used）"] --> READ["TraceRewardExtractor\n读 meta_ctx.assets_used + trace usage + verify_state"]

    subgraph "连山压缩算子（后台 tokio::spawn）"
        READ --> BP["δ-backprop: 统计回传（父节点 γ=0.5 衰减）"]
        BP --> FORK["δ-fork: 低回报资产 → 变体扩展（复制+降权+标记，内容修订走人工通道）"]
        FORK --> MRG["δ-merge: 相似变体合并（内容相似 + 回报无显著差异）"]
        MRG --> PRN["δ-prune: N≥5 且低于组内最优 >2σ → 淘汰"]
        PRN --> WRITE["write YAML → 分区归藏 (version++, 单写者) + model_stats"]
    end

    WRITE --> NEXT["下轮 MetaAgent 自动读取最新认知偏置"]

    subgraph "主动学习（空闲窗口）"
        ACTIVE["pending 空 + 预算允许 → 高不确定性节点\n（低N/高方差，即 UCB 探索项最大者）"]
        ACTIVE --> EXP["模板化探索任务\n（Execution/最小预算/不递归/每窗口限量）"]
        EXP --> RUN["experiments/ 队列 → runner 执行 → trace 回传"]
    end
```

**被动学习（任务驱动）**：周易 PASS → pending 入队 → 统计回传——只能在任务发生时学习。

**主动学习（信息增益驱动）**：连山在 **pending 空 + 预算内**的空闲窗口，选择高不确定性节点（低 N / 高方差——即 UCB 探索项最大者）→ 生成**模板化探索任务**（静态模板，不调 LLM："用工作流 W 完成类型 X 的最小任务并记录 token 消耗与结果"）→ 入 experiments/ 队列执行（Execution 模式 + 最小预算 + **不递归** + 每窗口限量 + token 成本上限）→ trace 照常回传。**护栏：探索任务不产生新探索任务（无递归）；连山纯符号层承诺保持（不调 LLM 生成资产内容）。**

**时序分离**：周易执行与 连山写入不并发（周易只读，单写者互斥，§8.3）；主动学习在空闲窗口进行。

**元权重表（model_stats.yaml，V36 落地）**：`model_key → StatsRow(n/pass_count/cost_sum/quality_sum/rounds_sum)`（serde default 零迁移），存于 knowledge 根（跨分区共享），由 连山回传更新（dmn_consumer 在 backprop 分支读取 pending 的 `model_key` + checks 首项四维聚合——同任务摊派值一致，与 CheckResult 摊派同构），ModelRouter 读取（§8.8）——同一 UCB/bandit 机制服务资产选择与模型路由。**回传数据源全部来自既有 pending 负载**（`model_key`/`checks[].cost_tokens|verify_rounds|quality`），零新增持久化文件。模型级 `quality` 用任务级 passed 映射（PASS=1.0，pending 仅 PASS 入队 → 恒 1.0，字段保留供未来 FAIL 入队扩展）。


### 6.4.1 贝叶斯后验接入（MVP-3.5，models/ 层激活）

> **状态：** `ModelAsset`（header + alpha/beta）V22 已定义，本轮激活写入者与消费方（`bayesian_update` 由 log-only 占位升级为持久化）。频率统计（CheckStats）保留——n/pass_count 仍是贝叶斯更新的数据源，兼容既有消费方。

**Beta-Bernoulli 共轭更新**（每验证契约一个后验，id 同名关联）：

```
先验映射（ModelAsset 初始化，§6.3 confidence=初始先验语义落地）：
  α = 1 + k·confidence    β = 1 + k·(1 − confidence)   （k = prior_strength，默认 10）
后验更新（backprop 双轨写入）：
  α += success    β += fail    （success/fail 为该资产全部检查项的聚合成败）
后验均值：  μ = α / (α + β)
后验标准差：σ = √( α·β / ((α+β)² · (α+β+1)) )
```

**演化决策升级**（`bayesian_enabled` 默认 true；false 时回退频率路径）：

| 算子 | 频率版（MVP-3，既有） | 贝叶斯版（MVP-3.5） |
|------|------|------|
| fork 阈值 | pass_rate < 0.6 | 后验均值 < 0.6（低采样自动收缩向先验，偶然失败不误触发） |
| merge 判定 | 通过率差 < 0.1 | 后验均值差 < 0.1（同分根优先保留，既有规则不变） |
| prune 淘汰 | 组内最优 − 2σ（组内率标准差） | 组内最优后验均值 − 2·σ(候选自身 Beta 后验) |

**设计要点**：① 先验强度 k 配置化（`runtime.dmn.prior_strength`），k 大 → 低采样结果更贴先验；② fork 变体（`{root}-v1`）对应独立 ModelAsset（同名 id）——变体后验天然隔离，与 check_id 重命名机制同构；③ 主动学习探索分的 avg_reward 用后验均值（`bayesian_enabled` 开时）；④ 单写者约束保持——`bayesian_update` 仅在 `backprop_checks` 内被调用，backprop 仅被 连山压缩算子 调用；⑤ **惩罚通道（V34）**：TraceConsistency 机械 FAIL 的 CheckResult（passed=false）经既有 pending/backprop 路径 β++ ——编造诱发的资产自动降权，无需新算子。


### 6.5 真值维护 (Truth Maintenance — 精简版)

真值维护采用**精简版**：无依赖传播（PROPAGATE/dependency_index/stale 标记不存在），仅保留 ASSERT/RETRACT 两种状态操作。连山接入或 Truths 资产累积后再评估恢复传播机制。

**精简理由（原 §8.13 并入）：**
> - **空集运行** — L4 Truths 目录当前为空，TMS 传播引擎从未被真实数据触发，复杂性未经验证
> - **与 ConstraintEngine 职责重叠** — ConstraintEngine 已通过 Hard/Soft severity + active/retracted 状态过滤实现核心校验，TMS 的依赖传播是叠加的增量收益
> - **连山接入后重新评估** — 若未来 Truths 资产累积且需要跨约束依赖推理，再恢复 PROPAGATE 机制

**保留的机制：**

```
ASSERT:  写入新 Truth 资产 → 标记 active → ConstraintEngine 下次加载可见
RETRACT: 手动标记 truth 为 retracted → ConstraintEngine 不再加载
```

**与 ConstraintEngine 的关系：** ConstraintEngine 只加载 `active` 状态的 Truth。`retracted` 或 `stale` 的 Truth 不参与前置检查，防止过时约束错误拒绝合法输出。状态持久化于 YAML `status` 字段。

**移除的机制：** `justification_depends_on` 依赖链、`dependency_index` 反向索引、PROPAGATE BFS 传播、跨层权重几何平均聚合。`justification` 字段保留作为审计信息。


> **原 §8.13（真值维护精简）已并入本节**——「TruthConstraint 无 `justification_depends_on` 字段；`PropagationEngine` / `GridRewireEngine` / `RelationEngine` 模块不存在于代码中」。

## 连山关键架构决策（摘自原 §8）

### 8.3 周易只读 / 连山单写者（V32：分区维度）

周易执行期间只读归藏。连山压缩算子 设计为唯一的写者（单线程后台任务），避免读写竞争。**当前 连山压缩算子 代码已实现但未激活（参见 §8.12）**——日常 周易运行中归藏为完全只读模式。激活后，周易 PASS → enqueue 连山 → 单写者更新归藏资产（**按模型分区写入**，一个任务只触碰其路由模型的分区），下轮 MetaAgent 加载时自动获取最新认知基础。

**分区一致性**：一个任务内所有 Agent（Meta/Fitting/Causal）必须使用同一分区（按路由模型的 model_key）——MetaContext.model 是唯一载体（与 mode 同机制传播），防止跨分区资产混编。


### 8.8 动态提示词编排（V32：元权重 = 模式决策 + 模型路由 + UCB 检索）

所有 Agent 的 system prompt 不再硬编码在 `src/agents/*.rs` 中，而是由 MetaAgent 在每次 周易循环开始时动态编排：

1. **模型路由（V36 定稿，先于检索）** — 纯符号层先行（V32 plan.md 阻塞点 #1 修正：分区检索依赖路由结果，而路由是读 model_stats 的符号决策，不需要 LLM）：读根级 model_stats 元权重表，经 **ModelRouter（bandit/UCB）** 决策 `model_key`。候选 = 配置 providers × models（default + `llm.providers` 中 deepseek 系条目）；score = avg_reward（w_pass·pass_rate + w_quality·avg_quality − w_cost·avg_cost_norm − w_rounds·avg_rounds，成本组内归一化）+ C·√(ln N_total/(n+1))；**全部无统计 → 配置默认模型**（探索由 MetaAgent 首次采样开启）；tie 按候选声明顺序（确定性）。路由失败（model_stats 损坏）→ 空表 + warn 按未采样处理（衍生数据，无重建源）
2. **路由分层（V37 定稿）** — 模型路由分三级，各级独立决策、低级继承高级默认：
   - **任务级**（已实现，V36）：一次任务选一个执行模型——`MetaContext.model`，全任务默认同分区（§6.1）
   - **相位级**（V37 已实现——异源裁判 MVP 闭环）：Meta / Fitting / Causal 可分别路由——Meta 用小模型省钱（权重决策成本低）、Fitting 用任务级执行模型（强模型）、Causal 用**异源模型**（裁判与运动员不同模型 = 「概率系统不验证概率系统」的又一缓解：同源 LLM 自我验证存在 self-preference / position 偏置，§1.3 实证）。实现：`MetaContext.verify_model`（serde default，None = 继承 model）由 MetaAgent 在 `runtime.model_routing.heterogeneous_verifier=true`（默认 false）时经 `ModelRouter.route_verifier` 决策（从非主候选按 UCB 同公式，候选 <2 → None 继承）；factory 两处 Causal 构造 `verify_model.as_ref().or(model.as_ref())` 消费；Causal 契约加载随 verify_model 分区（§6.1 学习单元语义）。静态 `agent_overrides` 保留为按 agent 类型配模型的雏形；动态相位统计（model_stats 扩展 (model_key × tag × phase)）仍为 MVP 边界，route_verifier 复用任务级 stats
   - **子任务级**（V37 已实现）：`SubtaskSpec.model`（serde default，None = 继承父任务模型）——父 LLM 拆解时可按子任务难度/领域分配不同模型；`RecursiveDecomposeTool` 经 `apply_subtask_model` 覆盖子 `MetaContext.model`，子任务 verify_model 随父继承（异源方向不逐层重决策）
   - 分区跟随路由：每个相位/子任务按其模型用对应分区检索资产；**资产编排（LLM 组合）始终在执行模型分区**（MetaContext.model 对应分区）进行
3. **候选演进路径（V37 定稿）** — 当前 MVP 边界 = default + deepseek 系（base_url 空或 name=="deepseek"，V36）；演进方向 = **本地模型纳入候选**（ollama / vllm / llama.cpp 的 OpenAI-compat 端点）：本地解锁可重复实验 / 隐私 / 离线 / 零边际 token 成本，是多模型分治的经济前提；候选判定从 name/base_url 启发式升级为显式 `llm.model_roles` 配置（role → provider/model 映射，default 兜底）；本地端点按 OpenAI-compat 协议接入（ProviderEntry 已有 base_url 通道，无需新协议）。
4. **查询归藏** — 按路由结果经 `LiluoClient::for_model(model_key)` 分区检索**该模型分区**（`{model_key}/`）的资产（prompts + workflows + verifications），按 §6.3 **UCB 排序**（利用 avg_reward + 探索项；`n < min_samples` 只走探索分；env_tags 不匹配降权）
5. **置信度过滤** — `confidence >= 0.3` 作为**初始先验门槛**（新资产/无统计资产仍有探索机会）
6. **模式决策** — 结合递归层数规则（builder 注入 depth / max_depth：`depth+1 >= max_depth` 必须 Execution，其余按深度倾向）+ 任务难易程度（复杂/多步/跨多维→Orchestration，原子/单步→Execution），决策当前节点 `mode`
7. **LLM 编排** — 将匹配的 prompt 资产、任务描述、深度规则与难度评估一起传给 LLM，**按所选模式配对**组合三份完整 system prompt：Orchestration → 编排拟合 + 收敛（verify 可省略）；Execution → 执行拟合 + 验证（converge 可省略）。输出含 `mode` 字段
8. **温度提取** — 从最高置信度的匹配 PromptAsset 提取 `temperature` 字段；若未设置，回退到 Base 模板默认温度（见 §8.10）
9. **注入 MetaContext** — 三份提示词作为 `Option<String>` 字段 + `mode` + `model`（第 1 步路由结果）+ **`assets_used`**（本次选用资产引用列表，连山回传依据，serde default）注入 MetaContext，传递到下游 Agent
10. **降级路径** — 无归藏资产或 LLM 编排失败时，提示词全部设为 `None`、mode 默认 Orchestration；**model 保持路由结果**（模型选择与资产编排解耦——降级的是资产编排，不是模型路由；Fitting/Causal 仍按路由模型执行）；仅当路由本身异常（model_stats 读失败）时 model=None（配置默认），下游 Agent 按 mode 自动使用对应的内置硬编码模板

**下游消费规则：**

| Agent | 方法 | 优先级 | 降级 |
|-------|------|--------|------|
| FittingAgent | `build_system_prompt()` | `meta_ctx.fitting_system_prompt` → `Some` 时直接返回，不编译模板 | 按 `meta_ctx.mode` 选编排模板 / 执行模板；recursive_decompose 仅编排模式注册 |
| CausalAgent.verify | `verify(output, ..., meta_ctx)` | `meta_ctx.verify_system_prompt` → 作为 system prompt | 按 `meta_ctx.mode` 选 `VERIFY_ORC_SYSTEM_PROMPT` / `VERIFY_EXEC_SYSTEM_PROMPT` |
| CausalAgent.converge | `converge(results, ..., meta_ctx)` | `meta_ctx.converge_system_prompt` → 作为 system prompt | 按 `meta_ctx.mode` 选 `CONVERGE_ORC_SYSTEM_PROMPT` / `CONVERGE_EXEC_SYSTEM_PROMPT` |


### 8.12 连山延迟接入 (连山 Deferral)

连山压缩算子 代码已完整实现并测试通过，但日常 `taiji run` 不启动。延迟原因：

1. **连山的运作依赖符号层统计数据** — V32 MCTS 四算子（backprop/fork/merge/prune）需要充分的执行轨迹积累（回报信号、模型路由统计）。纯云端架构下 连山在 YAML 符号层独立运作，不依赖本地模型
2. **归藏的填充需要积累** — 连山压缩算子 写回资产的前提是有足够执行轨迹。当前归藏只有 6 个手动种子 Prompt，Truths 层为空，models/ 预留。过早激活连山 会产生空操作（无资产可回传、无统计可对比）
3. **不影响核心 周易循环** — MetaAgent → FittingAgent → CausalAgent 三相循环完全自洽。连山是增强层而非基础层

**激活条件（V32 修订）：** 归藏各层有足够资产（每层至少 5 个） + 累积 50+ 周易执行轨迹；统计选择启用门槛 `n ≥ min_samples`（3）。激活方式：`taiji run` 命令行增加 `--with-dmn` flag。**主动学习**需 pending 空 + 预算内（`runtime.dmn.active_learning`：每窗口限量 + token 成本上限）才在空闲窗口发起。


### 8.21 连山-MCTS 认知树：归藏按模型分区的蒙特卡洛学习

**设计原则（与生成式模型一体两面）**：LLM 只能接龙（预测下一项），其能力上限由预训练地形决定且无法后训练。taiji 不改变模型，而是**配合模型的生成范式**——把任务组织成模型训练过的任务形式（完形填空/接龙），并用**系统结构**（验证/回退/拆解/沉淀）补偿模型的结构性缺陷。连山-MCTS 就是这套结构的训练侧：**周易是执行的马尔可夫链（每次执行 = 一次 rollout），连山是蒙特卡洛探索 fork 树（持久累积认知）**，共用同一棵资产树——训练与生成一体两面（回报函数 / UCB 选择 / 四算子定义见 §6.3 / §6.4）。

**归藏记录什么（选择标准）**：只记录**模型仍未覆盖且已验证**的知识——① 模型覆盖度低（私有环境、时效知识、长尾技能、特定工作流）；② 复用频次高；③ 已验证（多次复现 + 验证通过）；④ 稳定（易变知识带 env_tags 或时效标记）。模型已经会的（通用知识）不记——记录会与模型自身知识冲突。

| 轨道 | 资产层 | 记录内容 | 消费方 |
|------|--------|----------|--------|
| 阳轨（生成侧） | prompts/ | 角色模板（行为风格） | MetaAgent 编排 → Fitting |
| 阳轨（生成侧） | workflows/（V32） | 特殊工作流 + 稳定涌现文本 + 可执行脚本模板 | MetaAgent 编排 → Fitting |
| 阴轨（验证侧） | verifications/（V32/V33） | 收敛验证契约：结构化 checks（file_exists / schema_valid / reference_resolves / command_succeeds / llm_judgement） | ContractEngine 机械执行（L0/L1）→ LLM 只裁决 llm_judgement（L2，§6.6） |
| 硬约束（V38 起内置） | ~~truths/~~ → ConstraintEngine 内置 L0 检查（summary 非空/有依据/可审计 + code-safety） | 环境事实 + 不可违反规则 | CausalAgent.verify 前置（Hard 短路），不资产化、不演化 |
| 统计层 | models/ | 激活（MVP-3.5）：alpha/beta 贝叶斯后验，steering_vector 仍预留 | 激活 |

**V33/MVP-3 契约空间定量化（实现层定稿）**：
- **δ-fork**：资产级通过率 < 0.6 且采样 ≥ `min_samples`（3）的**根资产**（含 llm_judgement 项）→ 生成 strict 档变体——复制 + `params.strictness="strict"`（CausalAgent 按档位注入从严裁决指令）+ check id 重命名 `{base}@{variant}`（防 backprop 撞名，回传精确落位变体）+ stats 清零（独立采样）+ confidence×0.8 + `variant_of` 链接。防重复：已有变体的根不重复 fork；变体不 fork 变体。
- **δ-merge**：同组（variant_of 同根）双方采样 ≥ `min_samples` 且通过率差 < 0.1 → 统计按 check 位置并入最优者，次者 `status="pruned"`。**同分时根资产优先保留**（read_dir 顺序不确定，无二级键会把根误淘汰）。
- **δ-prune**：组内采样 ≥ `min_samples` 成员中通过率低于组内最优 > 2σ（σ = 组内通过率标准差）→ `status="pruned"`——保留文件供审计，加载/回传一律过滤（`load_all_verifications` 只返回 active）。
- **激活门槛**（§8.12）：backprop 无条件（数据积累期）；fork/merge/prune 需资产 ≥5 且总采样 ≥50（`runtime.dmn.activation_min_assets/activation_min_samples` 可覆盖）。
- **四维统计**：`CheckStats = { n, pass_count, cost_sum, rounds_sum, quality_sum }`——cost/rounds/quality 为任务级信号（trace usage / verify_state.round / route×confidence 派生）摊派给同任务所有检查项，随 CheckResult 入队（§6.4 零新增持久化文件承诺保持）。

**主动学习契约化定稿（V33/MVP-3）**：探索目标 = **活跃变体资产**（variant_of 存在）中 UCB 探索分最大者（N_node=0 → 最大探索分）；探索任务 = **静态模板**（注入变体契约 target/pass_condition，零 LLM 调用）写入 `experiments/` 队列（单执行器防堆积：队列非空不再入队，每窗口限量）；执行器消费：RecursiveRunner（Execution 最小预算）执行 → **产物由 ContractEngine 机械检查变体契约（零 LLM 裁决，§6.6 探索裁决符号化）** → CheckResult 入队 pending 回传 → 删除 experiments 文件；失败任务改名 `.failed` 留证。默认关闭（`runtime.dmn.active_learning_enabled=false`）；探索任务描述教学层含「不递归、不分解、完成即止」。护栏：探索任务不产生新探索任务；学习环有界。

**元权重 = 模式决策 + 模型路由**：MetaAgent 权重更新时一并决策 `MetaContext.model`——ModelRouter 读 model_stats.yaml（`(model_key × tag)` 统计，同一 UCB 机制）按任务标签/难度路由到最优模型；多小模型分治（便宜模型兜底简单任务，强模型只留给难任务，成本感知）。模型路由与资产选择共用 bandit 机制，模型路由本身不进探索任务实验对象（防自指循环）。**V37 多级路由（方向承诺）**：元权重从单点任务决策扩展为三级路由——任务级（已实现，§8.8 第 1 步）+ 相位级（元权重表加相位维度 (model_key × tag × phase)，静态 agent_overrides 为雏形，Meta 省 / Fitting 强 / Causal 异源）+ 子任务级（SubtaskSpec.model，None 继承父）——成本感知从任务粒度细化为相位粒度；「一个模型 + 它的约束系统 = 一个领域学习单元」是分区的完整语义（§6.1）：每个模型分区独立演化，契约难度随模型能力自适应。

**数据流断点修复**：`MetaContext.assets_used`（serde default）记录本次编排选用的资产引用列表（含分区）→ enqueue pending 时携带 → TraceRewardExtractor 据此回传——**这是 连山回传的唯一依据，缺失则无法学习**。token 成本（trace usage）与质量信号（VerificationReport 派生）已在既有数据中。

**V35/MVP-6 定稿：assets_used 接线 + prompts 对称演化**：
- **接线**：MetaAgent 编排时将选中资产（prompts + verifications，UCB 序消费的引用）写入 `MetaContext.assets_used`（`Vec<AssetRef>`：type/id/partition 三元组）→ enqueue_dmn_pending 携带 → backprop 按 assets_used 分发：verifications 走检查项级（既有 `backprop_checks` 按 check_id 匹配），prompts 走**任务级信号**（任务 PASS → 引用 prompts 各记 success；FAIL/BACK_TO_META → 记 fail）——同一 pending 负载、两套信号源（§6.3 实现层定稿）。
- **任务级信号源**：enqueue_dmn_pending 现有入参（checks）之外增加 task 结果信号（`passed: bool`）与 assets_used；无 assets_used 的历史 pending 零迁移（serde default 空 → 仅 checks 路径）。
- **四算子对称**：fork/merge/prune 同一 reward 函数（§6.4）作用于 prompts——fork 门槛改由 prompts 的任务级 pass_rate 判定（同一 `FORK_PASS_RATE_THRESHOLD=0.6`）；merge 同组差 < 0.1；prune `μ < best − 2σ`（贝叶斯版，与 verifications 同式）；激活门槛（资产 ≥5 / 总采样 ≥50）两层分别独立判定。prompts 的 ModelAsset 同名 id 关联同样生效（先验映射同 §6.4.1）。
- **演化顺序**：verifications 四算子 → prompts 四算子（同一次 evolve_contracts 调用内串行，单写者保持）。

**激活条件**：§8.12（每层 ≥5 资产 + 50+ 轨迹 + `--with-dmn`）。


### 8.23 归藏重构 MVP 路径（V33 三步走，最小 MVP 开发范式）

**开发范式确认（BCP 演进本质 = 最小可行闭环）**：V28→V32 的每次迭代都是一个最小 MVP——先让闭环跑通、再逐步完备（产物契约 → 上下文预算 → 分封会盟 → MCTS 认知树）。归藏重构同样**不推倒重来**，分三个最小 MVP，每一步可独立交付、可验收：

| 步骤 | 范围 | 依赖 | 验收标准 |
|------|------|------|------|
| **MVP-1 契约化（V33 起）** | verifications/ 结构化 schema（checks）+ ContractEngine（L0 机械 + L1 契约执行）+ 种子契约 5-10 条（通用收敛契约：产出存在 / schema 合法 / 引用解析）+ CausalAgent 接线（verify 前置 + ContractReport 注入） | **不依赖 DMN**（人工种子 + 代码实现） | `cargo test` 通过；verify 流程机械检查先于 LLM；种子契约可对简单任务真实执行并短路失败 |
| **MVP-2 统计回传** | DMN 被动学习激活：ContractReport 检查项统计 → pending 入队 → backprop 回传检查项通过率 | MVP-1（契约执行记录）+ 50+ 轨迹 | 检查项通过率可见；`--with-dmn` 激活后归藏 YAML 统计更新 |
| **MVP-3 契约演化** | MCTS 完整四算子作用于契约空间（fork 契约假设 / merge 相似 / prune 低效）+ 主动学习 | MVP-2 | 低效契约被淘汰、变体契约优胜劣汰有统计支撑 |
| **MVP-4 断言证据链（V34）✅ 已实现** | TraceConsistency 检查项（CheckKind 第 6 类）+ 断言分级教学段 + 种子契约 v-assertion-evidence（severity=soft 起步）+ 推测占比质量信号 | MVP-1（ContractEngine）/ MVP-3（贝叶斯惩罚通道） | 虚假证据引用被机械 FAIL；推测标记计数进 CheckResult.detail；无标记产出零误报通过；`cargo test --lib` ≥ 243 + 新增 ≥5 |
| **MVP-5 UCB 检索落地（V35）✅ 已实现** | prompts 检索排序从 confidence 降序升级为 `score = μ + C·√(ln N_total/(n+1))`（后验 μ + (n+1) 平滑，§6.3 实现层定稿） | MVP-3.5（ModelAsset 后验通道） | 冷启动退化为先验 μ 降序（确定性）；n>0 资产按 UCB 序消费；检索确定性单测；`cargo test --lib` 无回归 |
| **MVP-6 prompts 对称演化（V35）✅ 已实现** | PromptAsset 补 stats + `MetaContext.assets_used` 接线（enqueue 携带 + 任务级 PASS/FAIL 回传）+ 四算子对称作用于 prompts（同一 reward/阈值/贝叶斯框架） | MVP-2（pending 回传通道）/ MVP-5（UCB 检索提供排序消费面） | 任务 PASS 后引用 prompt 的 stats.n++/pass_count++；prompts fork/merge/prune 与 verifications 同式；`cargo test --lib` 新增 ≥4 |

**MVP-1 是纯 周易侧改动**：不激活连山、不依赖轨迹积累——补齐「LLM 泛化执行与 LLM 收敛验证不可靠」的符号验证根基（§1.3），是 周易收尾的最后一块拼图，也是归藏从「知识库」到「本体论工程」的转型起点。

---


---

---

## 附录 A：版本历史（全文存档）

> 版本头仅保留摘要；以下为各版本完整变更记录，按版本倒序。

> **V42 变更（BCP 同构统一——泛化-压缩循环·三位一体）**：§1 设计哲学全面重写——taiji 的核心动态不再是「周易执行引擎 + 连山压缩期消费者 + 归藏文件系统」三模块，而是**周易（泛化执行）→ 连山（非线性流形压缩）→ 归藏（符号固化）三位一体**的同构循环（§1.4）。归藏资产树与 周易递归任务树异层同构：fork=decompose、merge=converge、prune=FAIL 终止、backprop=子→父统计上浮（§1.1 三尺度同构映射、§6.0 归藏重定性）。§6「归藏认知仓库」重写为「归藏符号系统」——连山重新定性为纯符号层压缩算子（§1.8.2），归藏资产重新定性为冻结的执行经验（§1.8.3）。V33 的「规范性本体论」阶段性理解升级为 V42 的「压缩固化的可复用符号系统」。现有代码不动——本次仅 BCP 统一。
>
> **V41 变更**：归藏根目录净化——根 client 不再创建资产层目录，`ensure_dirs` 仅由 `for_model` 分区 client 调用。
>
> **V40 变更**：ChatAgent 提示词简单化——移除归藏摘要注入，`build_system_prompt` 降为同步 fn。
>
> **V39 变更**：`taiji seed` 种子复制命令——活跃种子资产跨分区复制，stats 不复制。
>
> **V38 变更**：归藏瘦身——移除 index.yaml + truths 资产层。
>
> **V37 变更（模型-领域学习单元 + 多级路由定稿 + 微调边界）**：① **§6.1 语义显式化**——分区 = (模型 × 约束系统) 学习单元：模型提供概率地形（猜想源）、约束系统提供机械判据（反驳源）、分区统计提供累积（选择源），绑定独立演化；推论 = 契约难度随模型能力自适应（弱分区 fork 宽松 / 强分区 fork 严格；机制已在 = 分区独立 stats + fork strictness 参数，V37 将语义显式化，变体树不跨分区）。② **§8.8 路由分层**——任务级（V36 已实现）→ 相位级（Meta 小模型省钱 / Fitting 执行模型 / Causal 异源模型——裁判与运动员不同模型是「概率系统不验证概率系统」的又一缓解；静态 agent_overrides 为雏形，动态相位路由 = model_stats 加相位维度，MVP 后置）→ 子任务级（SubtaskSpec.model，serde default，None 继承父，MVP 后置）；资产编排始终在执行模型分区进行。③ **候选演进路径**——本地模型（ollama/vllm OpenAI-compat 端点）纳入候选是方向（可重复实验/隐私/离线/零边际成本），候选判定升级为显式 `llm.model_roles` 配置（MVP 后置；当前 MVP 边界 = deepseek 系，V36 保持）。④ **微调边界（架构定论）**——权重微调是模型厂家的事，taiji 不设计微调通道；models/ 层 steering_vector 仅预留字段（不承诺、不设计介入向量）。
>
> **V36 变更（归藏按模型分区 + 分区路由落地，V32 蓝图承诺兑现）**：实现 V32 承诺、V33-35 未兑现的分区设计，使归藏资产按模型地形隔离。① **LiluoClient 双路径**：`root_dir`（knowledge 根，恒为根）+ `data_dir`（活动目录 = 根或 `root/{model_key}`）；`for_model(key)` 派生分区 client（自动建分区目录 + 五资产层 + 空 index），`partition_key()` 读回；model_stats.yaml 恒在根级（跨分区共享）。② **迁移**：`migrate_to_partitioned(root, default_key)` 幂等（目标已存在即跳过），main.rs build_engine 失败上抛 + cmd_init 失败仅提示，各调一次。③ **路由先于检索**（V32 plan.md 阻塞点 #1 修正）：MetaAgent.run() 第一步为 ModelRouter（纯符号层，读 model_stats，无 LLM）→ `for_model` 分区检索 → LLM 编排；MetaContext.model = 路由结果（降级路径也保持；None 仅当路由异常）。④ **路由候选仅 deepseek 系**（default + `llm.providers` 中 base_url 为空或 name=="deepseek" 的条目；OpenAI-compat 不参与，MVP 边界）；`resolve_model` 按候选表精确匹配。⑤ **连山回传分区**：pending 负载带 `model_key`（serde default 零迁移），dmn_consumer 按 `partition_liluo(model_key)` 派生回传，backprop 后按 checks 首项四维聚合回传 model_stats（失败仅 warn）。⑥ **`--with-dmn` 等待 pending 清空**（轮询 60s/1s，dead/ 不计）替代固定 3s（消费者指数退避下固定等待失效）。⑦ **探索任务回传**用 main.rs 传默认分区 client（§6.1/§6.4/§8.8）。
>
---

> **V35 变更（检索/演化侧数学化：UCB 检索落地 + 生成资产对称演化，MVP-5/MVP-6 设计定稿）**：兑现 §6.3/§8.21 已承诺后置的两块缺口，使归藏两层（生成 prompts / 判断 verifications）共享同一套数学结构——UCB 选择 + 贝叶斯后验 + 阈值算子。① **MVP-5 UCB 检索落地**：prompts 检索从「手填 confidence 降序」（meta.rs 现状，非学习统计）升级为 `score = μ + C·√(ln N_total / (n+1))`——μ 取 models/ 后验均值（无 model → §6.4.1 先验映射），n 从 usage_count 起步，(n+1) 平滑保证 n=0 时仍有有限探索分且退化为先验 μ 降序（确定性保持）；confidence 阈值过滤（0.3）保留为确定性防线。② **MVP-6 prompts 对称演化**：PromptAsset 补 `stats: AssetStats`（§6.2 契约本有，实现层补齐，serde default 零迁移）+ `MetaContext.assets_used` 接线（§8.21 数据流断点修复：编排所选资产引用 → pending 携带 → backprop 按任务级 PASS/FAIL 信号回传 prompts，粒度区别于 verifications 的检查项级）+ 四算子对称作用于 prompts（同一 reward 函数，§6.4）。③ **拒绝项防回归（架构定论重申）**：向量嵌入/向量库/图库/分布式归藏/并行写（破坏单写者 §8.3）/TS 随机采样（破坏决策确定性）一律不引入（§6.0/§6.3/§8.21）。
>

---

> **V34 变更（委托-代理机制设计：断言证据链 + 一致性检查，MVP-4）**：针对「agent 为偷懒蒙骗用户、编造虚假事实」的激励问题，引入博弈论**机制设计**（激励相容，非均衡求解）三件套：① **断言分级教学**（Fitting system prompt：证据断言必须附 `[证据: 工具名]`、推测断言必须标 `(推测)`）；② **TraceConsistency 检查项**（CheckKind 第 6 类，L1 扩展，纯机械零 LLM）——断言引用的工具调用必须在任务 trace.jsonl `tool_call::*` 记录中存在（引用完整性，reference_resolves 的推广），推测标记计数注入 CheckResult.detail 作质量信号；③ **惩罚闭环全复用既有管道**——虚假证据引用 = 机械 FAIL → hard 短路 → backprop 贝叶斯 β++ → 资产降权淘汰，零新增持久化文件（§6.0 / §6.6 / §8.22 / §8.23）。**V33 定论划界**：LLM 不能验证 LLM（事实真伪裁决需 ground truth）依旧成立，但**激励问题不需要 ground truth**——一致性检查（断言 vs 执行轨迹）是机械可判定的，恰好落在定论边界之外。种子契约 severity=soft 起步（防误伤纯推理任务），推测占比统计进 DMN 后按演化升级。
>

---

> **V33 变更（归藏本体论重构：验证三权分立 + 结构化验证契约）**：归藏重新定性为**本体论工程**——不是 RAG 知识库，而是「验证契约库 + 生成资产库」：阴轨资产（verifications/ + truths/）从自由文本升级为**结构化验证契约**（`checks: Vec<CheckSpec>`，可机械执行的检查项），新增 **ContractEngine** 在 CausalAgent LLM 调用之前执行 L0 机械验证 + L1 契约验证，LLM 验证降级为 L2 兜底（只裁决 llm_judgement 类检查项）——**机械检查失败直接短路，LLM 不可翻案**（§1.3 / §6.0 / §6.6 / §8.22）。实证依据：LLM-as-Judge 研究（MM-JudgeBias ACL 2026：26 个 SOTA judge 验证完整性失败——conditional verification 退化为 unconditional prediction；Reliability without Validity arXiv 2606.19544：21 个裁判模型「高可靠性低有效性」；verbosity / self-preference / position 偏置）——**概率系统不能验证概率系统**，收敛验证的符号化是阴面的本体论根基。连山统计对象从「资产」精确到「检查项」（契约通过率），MCTS 四算子作用于**契约有效性空间**（§6.4 / §8.21）。重构按 BCP 最小 MVP 开发范式分四步落地（§8.23）：MVP-1 契约 schema + ContractEngine（纯 周易侧，不依赖 DMN）→ MVP-2 DMN 被动学习统计回传 → MVP-3 MCTS 完整四算子 → **MVP-4 断言证据链（V34）**。**实现状态（2026-08 全落地）**：MVP-1/2/3 已实现并测试（四维 CheckStats 回传、fork/merge/prune 定量化、主动学习契约化、贝叶斯后验接入见 §6.4/§6.4.1/§8.21 定稿）；**MVP-4 断言证据链已实现并测试**（check_trace_consistency + 断言分级教学 + 种子契约 v-assertion-evidence，§8.22）；**MVP-5/6 已实现并测试（`cargo test --lib` 257 pass）**——UCB 检索（rank_prompts_by_ucb）与 prompts 对称演化（backprop_prompts + 四算子 + 共享公式 stats_pass_rate）见 §6.3/§8.21 定稿。V32 其余承诺（模型分区 / model_stats / 元权重模型路由）按最小 MVP 范式后置。
>

---

> **V32 变更（连山-MCTS 认知树：归藏按模型分区 + 蒙特卡洛学习）**：归藏从静态知识库升级为**按模型分区的蒙特卡洛探索 fork 树**——周易是执行的马尔可夫链（生成侧/前向），连山是 MCTS 树（认知侧/反向），两者共用同一棵资产树（一体两面，§8.21）。核心变更：① 归藏按模型分区（`.taiji/knowledge/{model_key}/`，不同模型资产隔离——模型预训练地形不同，稳定涌现文本/验证契约不可跨模型混用，§6.1）；② **回报函数**驱动自我改进（通过率/质量分/token 成本/验证轮数四维，写死进 §6.4）；③ **UCB 选择**替代纯 confidence 排序（利用+探索，§6.3）；④ MCTS 四算子（backprop/fork/merge/prune）替换 δ₀-δ₂ 占位实现；⑤ **被动+主动学习双轨**（trace 回传 + 空闲窗口探索任务，§6.4）；⑥ **元权重 = 模式决策 + 模型路由**（MetaContext 新增 `model`，多小模型分治，§8.8）；⑦ 新增 workflows/（阳轨·生成工作流+稳定涌现文本）与 verifications/（阴轨·收敛验证契约）资产层 + env_tags 环境维度；⑧ 数据流断点修复：MetaContext 新增 `assets_used`（连山回传依据）。
>

---

> **V31 变更（收敛树补齐：阴·向上汇报 / 阳·接受汇报与再指导）**：子任务失败不再断流——任务级失败转为**结构化汇报条目**（`ChildResultSummary.failure_reason/failure_kind` + handoff 交接产物路径）进 child_results，不整体上抛；converge 收到完整汇报（成功+失败）裁决 Partial/Diverged + 失败分析与 rerun 建议（task_summary）；父阳（阳·管理：递归泛化/接受汇报/汇总产出/得出最终产出/子任务再恢复与再指导）读汇报后 rerun_of 再启用（注入修正指导）或接受残缺综合；阴（阴·裁判：本节点收敛/验证/**向上父任务汇报**/**路由重试本节点**）回路保持（verify→BACK_TO_*）。取消/panic 仍硬中止（§5.2/§8.18/§8.20）。
>

---

> **V30 变更（分封制：任务自我认知 + 会盟）**：管理模型 = 分封制——瞬态任务知道自己的身份（内容/类别/父/子/兄弟）与地位（层级/权限），全部系统确定性赋予（身份册 meta.json 既有字段 + MetaContext.mode + 分封时快照），禁止 LLM 分类；会盟：子任务注入兄弟贡品索引（YangPrompt.sibling_deliverables），贡品跨兄弟公开只读、中间记忆仍隔离（§8.9 修订）；**无降级原则**：新代码读册/扫描失败一律错误上抛，问题暴露后修根因（§8.20）。
>

---

> **V29 变更（上下文窗口预算）**：用精准 token 计数替换 max_turns 轮次机制——`usage.input_tokens` 累计，250k 超限必须写交接产出（context_overflow → 阳拆解）、300k 硬截止直接上报 FAIL；统一 Meta/Fitting/Causal 预算，轮次计数器降级为循环防护；ChatAgent 保留 max_turns=20（§8.19）。

---

> **V28 变更（产物契约）**：执行事实是唯一记忆——产出即交接：交接物 = `deliverables/handoff.md`（产出物之一，§1.4 / §8.18），上下文超限/失败一律先写交接产出再返回；恢复优先级链改为产出继承（deliverables → decompose_result → 重跑，chat_history 降级为兜底）；路由按结构化失败原因分流（超限→阳拆解、认知→元校准，LLM 裁决兜底）；BACK_TO_TPN 改为基于产出递归分解、BACK_TO_META 改为 MetaAgent 注入产出校准；不做上下文压缩（特意设计）。
>

---

