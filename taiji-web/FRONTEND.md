# taiji-web 前端架构

> taiji-web 前端架构自包含文档：技术选型、WS 接口契约、前端消费逻辑全在此。
> 后端接口实现以代码为准：`src/ws/types.rs` / `src/ws/server.rs` / `src/ws/handler.rs` / `src/types/frontend.rs`。
> 最后更新：2026-08。

---

## 1. 设计哲学

前端是 Zhouyi-Lianshan 认知过程在拓扑学上的真实投影，而非独立看板。设计原则：

- **宏观-中观-微观三层同构**：背景太极图（宏观系统态）→ 纺锤递归树（中观拓扑）→ Zhouyi 弹窗（微观状态机）
- **可视化即交互界面**：点击节点即操作，审批输入框直连因果验证路由
- **数据驱动 UI**：`data/tasks/` 文件系统 = 天然状态机，WebSocket 双向通信即 UI 更新
- **纯浏览器运行**：无桌面壳依赖，Rust 核心进程提供 HTTP 静态托管 + WS 双向通信，浏览器直连

## 2. 项目结构

```
taiji-web/                          ← 独立前端项目（与 taiji 核心平级）
├── package.json                    ← React
├── index.html                      ← 入口 HTML
├── vite.config.ts                  ← Vite 配置（端口 1420 strictPort，build outDir dist）
├── src/                            ← React 前端
│   ├── App.tsx                     ← 根组件（太极背景 + 纺锤树 + 聊天面板布局）
│   ├── components/
│   │   ├── TaijiBg.tsx             ← 背景太极图（CSS 动画 + 状态联动光晕）
│   │   ├── SpindleTree.tsx         ← 纺锤状递归树（React Flow 自定义布局）
│   │   ├── SpindleNode.tsx         ← 单个树节点（状态色 + 点击展开）
│   │   ├── ZhouyiPopup.tsx            ← Zhouyi 三相流程弹窗
│   │   ├── PhaseDetail.tsx         ← 弹窗内单相详情面板
│   │   ├── YinIntervene.tsx        ← 阴极审批输入框（驳回 + 建议注入）
│   │   ├── ChatPanel.tsx           ← 侧边栏前端 Agent 聊天面板
│   │   ├── LianshanPanel.tsx       ← 连山演化浮层（归藏四算子最近演化）
│   │   ├── StatusLegend.tsx        ← 底部状态图例 + 各状态计数
│   │   ├── GuizangGraph.tsx        ← 归藏星云图（2D 力导向：prompts/skills/models + 对偶/后验/变体边）
│   │   └── OntologyPanel.tsx       ← 语义层·本体视图（词汇表/type→type 边/规则/共现/失败 + 资产映射）
│   ├── hooks/
│   │   ├── useWebSocket.ts         ← WebSocket 连接 + 事件分发 + 请求/响应
│   │   ├── useTaskTree.ts          ← 任务树状态管理（TaskTreeSnapshot → React Flow 节点）
│   │   └── useZhouyiState.ts          ← Zhouyi 三相状态订阅
│   ├── types/
│   │   └── index.ts               ← TypeScript 类型（与 Rust ws/types + frontend 对应）
│   ├── lib/
│   │   └── wsClient.ts            ← WebSocket 请求-响应客户端封装（send + await response）
│   └── styles/
│       └── index.css               ← Tailwind + 太极动画 CSS
└── dist/                           ← Vite 构建产物（Rust serve 命令托管此目录）
```

## 3. 数据流（纯 Web）

```
┌──────────┐  WebSocket 双向   ┌──────────────┐  React State   ┌──────────────┐
│ taiji 核心│ ←──────────────→ │ useWebSocket  │ ────────────→ │ SpindleTree   │
│ (Rust)   │ TaskEvent (广播)  │ hook          │               │ + ZhouyiPopup    │
│          │ ClientMessage →   │ + wsClient    │               │ + TaijiBg     │
│          │ ← ServerResponse  │               │               │ ChatPanel     │
│          │                   │               │               │ YinIntervene  │
│          │ HTTP GET dist/*   │               │               │               │
│          │ ──────────────→   │ 浏览器        │               │               │
└──────────┘                   └──────────────┘               └──────────────┘
```

**通信方式：** 前端与核心之间 100% 通过 WebSocket 进行。WebSocket 承载两种消息：

| 方向 | 消息类型 | 说明 |
|------|---------|------|
| 核心 → 前端（广播） | `TaskEvent` | 所有已连接前端均收到（事件推送） |
| 核心 → 前端（定向） | `ServerResponse` | 仅发起请求的前端收到（请求响应） |
| 前端 → 核心 | `ClientMessage` | 操作请求（执行任务、审批、查询等） |

## 4. WebSocket 协议

### 4.1 客户端请求（ClientMessage）

```rust
/// 前端 → 核心的请求消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all_fields = "camelCase")]
pub enum ClientMessage {
    /// 执行新任务（/run 命令）。
    ExecuteTask { request_id: String, description: String, max_depth: Option<u32> },
    /// 阴极审批提交。
    SubmitReview { request_id: String, intervention: YinIntervention },
    /// 列出所有根任务（新到旧），每条携带 `meta.json` 描述（`TaskListItem{id,description}`）。
    ListTasks { request_id: String },
    /// 获取指定根任务的任务树快照。
    GetTaskTree { request_id: String, root_task_id: String },
    /// 获取指定任务的 Zhouyi 相位详情。
    GetZhouyiState { request_id: String, task_id: String },
    /// 拉取归藏知识图（prompts/skills/models + dual/model/fork 边）供星云图渲染。
    GetGuizangGraph { request_id: String },
    /// 拉取语义层（本体）状态：types/relations/rules/cooccur/failures + asset_type_map。
    GetOntologyView { request_id: String },
    /// 内嵌 Agent 聊天（完整 Rig Agent + 流式输出）。
    ChatMessage { request_id: String, message: String, session_id: Option<String>, context_task_id: Option<String> },
}
```

### 4.2 服务端响应（ServerResponse）

```rust
/// 核心 → 前端 的定向响应（仅发起请求的客户端收到）。
///
/// # 流式扩展
///
/// `chunk` + `stream_done` 字段支持逐 token 流式推送。
/// 流式模式下：每个文本 delta 通过 mpsc 通道发送一帧，前端累积渲染。
/// 最后一帧设置 `stream_done: true`，前端 resolve Promise。
/// 非流式请求（ExecuteTask、ListTasks 等）不受影响——`chunk` 字段为 None，
/// 前端沿用现有 `data` 解析逻辑。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerResponse {
    pub request_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 流式文本 chunk（增量 delta）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk: Option<String>,
    /// 流式传输是否结束。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_done: Option<bool>,
}
```

**前端鉴别方式：** JSON 消息中若存在 `requestId` 字段 → `ServerResponse`（含可能的流式 chunk）；否则 → `TaskEvent`（广播事件）。

**流式处理逻辑：**
```
收到 ServerResponse:
  if msg.chunk != null && msg.streamDone != true:
      accumulate += msg.chunk; render(accumulate)
  else if msg.streamDone == true:
      resolve({ ok: true, data: accumulate })
  else:
      现有逻辑：resolve({ ok: msg.ok, data: msg.data, error: msg.error })
```

### 4.3 请求-响应处理流程

```
前端 wsClient.send(ClientMessage) → WS 连接 → Rust handle_connection 读循环
    → 解析 ClientMessage → ws/handler.rs 分发到对应处理函数
    → 处理函数执行（可能 spawn 异步任务） → 构造 ServerResponse
    → 通过 mpsc 通道发回 → handle_connection 写循环 → 前端 wsClient resolve Promise
```

每个 WebSocket 连接在 `handle_connection` 中持有：
- `broadcast::Receiver<TaskEvent>` — 接收广播事件
- `mpsc::UnboundedReceiver<ServerResponse>` — 接收定向响应

`select!` 同时监听广播事件、定向响应、客户端消息三个流。

## 5. serve 命令

### 5.1 CLI 接口

```
taiji serve [--port PORT] [--no-open]
```

- `--port`：HTTP 静态托管端口，默认 `8080`。
- `--no-open`：禁止自动打开浏览器（CI / headless 场景）。

### 5.2 启动流程

```
1. load_config() → TaijiConfig
2. build_engine(config) → Arc<AgentFactory>（复用 cmd_run 初始化链）
3. 构造 ServeState { factory, config, data_root }
4. 启动 WsServer（127.0.0.1:17890，固定端口）→ 初始化 event_bus
5. 启动 axum HTTP server（0.0.0.0:PORT）：
   - GET /          → 重定向到 /index.html
   - GET /*         → tower_http::services::ServeDir("taiji-web/dist/")
   - GET /ws        → WebSocket upgrade → handle_connection
6. 若未指定 --no-open：xdg-open http://localhost:PORT
7. tokio::signal::ctrl_c() 等待优雅退出
```

### 5.3 核心新增类型

```rust
/// serve 命令的共享状态（注入 axum 和 WS handler）。
pub struct ServeState {
    pub factory: Arc<AgentFactory>,
    pub config: TaijiConfig,
    pub data_root: PathBuf,
    pub ws_server: Arc<WsServer>,
}
```

## 6. 三个核心视图

### 6.1 太极背景图（TaijiBg）
- 极淡线框太极图，位于页面最底层，60 秒匀速旋转（CSS 动画）
- 状态联动：Yang 相 → 暖色光晕；Yin 相 → 冷色光晕；其余常态无光晕
- 纯 SVG/CSS 实现，无外部依赖

### 6.2 纺锤状递归树（SpindleTree）
- **布局**：纺锤形——depth=0 顶部、中间深度最宽、max_depth 底部收窄（`spread = sin(π·depth/max_depth)`）
- **节点**：圆角矩形卡片，status 决定颜色（绿/黄/红），Framer Motion 过渡动画
- **连线**：父子贝塞尔曲线，颜色跟随子节点状态，新建连线有生长动画
- **交互**：悬停放大+tooltip（含深度/轮次/周期/子任务/产出/工具）；点击弹出 ZhouyiPopup；**滚轮缩放（光标为锚点）+ 拖拽平移 + 双击/「适配」按钮归位**（CSS transform 实现，无依赖）
- **底部工具栏**：放大/缩小/适配；**底部图例**（StatusLegend）显示状态色含义 + 各状态计数

### 6.3 Zhouyi 三相流程弹窗（ZhouyiPopup）
- **布局**：左侧三相流程图（Meta→Yang→Yin 垂直排列）+ 右侧详情面板（随选中相切换）+ 底部阴极干预区
- **三相流程**：卡片间箭头连线，当前执行中相有脉冲动画，已完成/失败相用 ✅/❌ 标记
- **详情面板**：根据选中相显示对应 trace 信息（Meta: prompt 预览；Yang: 工具调用日志；Yin: 验证报告/约束违反列表）
- **阴极干预区**：在 AwaitingHumanReview 或 Yin 相激活——输入建议（注入下一轮 Zhouyi 循环）+ 三个按钮（批准收敛/驳回重试/驳回改道）

## 7. ChatAgent 聊天面板

侧边栏内嵌的完整 Rig Agent，通过 WebSocket 与核心双向通信：

**后端 ChatAgent（`src/agents/chat.rs`）：**
- 完整 Rig Agent：注册 5 个 L1 Skills（read/write/bash/search/webfetch）+ SafetyHook
- 工具循环：`max_turns=20`，LLM 可在多轮中自主调用工具完成复杂任务
- 流式输出：`agent.stream_chat()` → `MultiTurnStreamItem` → 提取 `Text` delta → WS chunk 推送
- 对话记忆：`Chat::chat()` 语义，history 传入传出，跨轮次保持上下文
- 会话持久化：`chat_history.json` 原子写入 `{data_root}/chat/{session_id}.json`
- 任务感知：`context_task_id` 非空时注入 task description + 归藏知识摘要（标签匹配 top-3 Prompts + 全部 active Truths）

**前端 ChatPanel（`ChatPanel.tsx`）：**
- 输入：自然语言聊天消息（如"帮我写一个并发爬虫"）
- 命令解析：`/run <描述>` → `ClientMessage::ExecuteTask`（不变）；`/plan <描述>` → 规划模式（待上线）
- 普通消息：发送 `ClientMessage::ChatMessage { requestId, message, contextTaskId? }` → 核心 ChatAgent
- **流式渲染**：wsClient 按 requestId 匹配，`chunk` 字段存在时追加文本并直接累积渲染（非打字机效果；打字机仅用于 `/run` 等非流式命令的模拟回显）
- 上下文：可引用当前纺锤树中选中节点作为 context_task_id

**会话管理：**
- sessionId：前端生成 UUID（`crypto.randomUUID()`），首次消息时发送；后端无 sessionId 或为空时自动生成 UUID v4，经 `stream_done` 帧的 `data.sessionId` 返回前端
- 历史加载：ChatAgent 构造时从 `{data_root}/chat/{sessionId}.json` 加载
- 清空：前端 `/clear` 命令（`ChatPanel.tsx`）重置 `sessionId` 为新 UUID + 清空消息列表

## 8. MVP 范围

- ✅ 纺锤状递归树可视化 + 节点状态颜色
- ✅ 树视图平移/缩放/适配（滚轮 + 拖拽 + 双击 + 工具栏）
- ✅ 状态图例 + 各状态计数（StatusLegend）
- ✅ Zhouyi 三相流程弹窗（含详情面板 + 阴极审批输入框）
- ✅ 背景太极图（静态旋转 + 状态联动光晕）
- ✅ WebSocket 双向通信（事件广播 + 请求响应）
- ✅ 纯浏览器运行（核心进程 `taiji serve` 启动 HTTP + WS + 自动开浏览器）
- ✅ 前端 Agent 聊天面板（完整 Rig Agent：5 Skills + 工具循环 + 流式输出 + 对话记忆 + 任务感知）
- ⚠️ 连山演化浮层（LianshanPanel）：前端已就绪，但后端 `lianshan_activity` 恒为 `None`（`task_tree_builder` 的 `read_lianshan_activity` 读的 `lianshan_evolution.log` 无人写入）——待后端接上后浮层自动生效
- ✅ 归藏星云图（2D 力导向：`GetGuizangGraph` 拉取 prompts/skills/models，渲染对偶/后验/变体边 + 节点详情侧栏；3D 增强延迟到 V2）
- ✅ 语义层·本体视图（`GetOntologyView` 拉取 types/relations/rules/cooccur/failures + asset_type_map，力导向渲染类型节点 + type→type 边 + 规则/共现/失败/映射面板 + 先验状态摘要；空态如实展示「先验未激活」）
- ❌ 多任务并行视图（MVP 单任务聚焦）
