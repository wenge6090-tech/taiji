// 与后端 src/types/frontend.rs + src/ws/types.rs 对齐的 TypeScript 类型。
// 后端 serde 序列化均为 camelCase，此处字段名与之完全一致。

/** 节点状态(后端 NodeStatus)。 */
export type NodeStatus =
  | "Pending"
  | "Running"
  | "Converged"
  | "Diverged"
  | "Failed"
  | "Cancelled"
  | "AwaitingHumanReview";

/** TPN 三相位(后端 TpnPhase)。 */
export type TpnPhase = "Idle" | "Meta" | "Fitting" | "Causal" | "Converged";

/** 纺锤树节点(后端 SpindleNode)。 */
export interface SpindleNode {
  taskId: string;
  description: string;
  depth: number;
  siblingIndex: number;
  totalSiblings: number;
  status: NodeStatus;
  phase: TpnPhase;
  round: number;
  cycle: number;
  parentId: string | null;
  childrenCount: number;
  deliverablesCount: number;
  toolsUsed: string[];
}

/** 纺锤树边(后端 SpindleEdge)。 */
export interface SpindleEdge {
  source: string;
  target: string;
  status: NodeStatus;
}

/** 归藏演进摘要(后端 EvolutionSummary)。 */
export interface EvolutionSummary {
  layer: number;
  assetId: string;
  delta: string;
  timestamp: string;
}

/** 认知库活跃度(后端 DmnActivity)。 */
export interface DmnActivity {
  activeNodes: number;
  recentEvolutions: EvolutionSummary[];
}

/** 任务树快照(后端 TaskTreeSnapshot)。 */
export interface TaskTreeSnapshot {
  rootTaskId: string;
  rootDescription: string;
  nodes: SpindleNode[];
  edges: SpindleEdge[];
  dmnActivity: DmnActivity | null;
}

/** 追踪记录预览(后端 TraceRecordPreview)。 */
export interface TraceRecordPreview {
  ts: string;
  phase: string;
  cycle: number;
  round: number;
  tool: string | null;
  summary: string;
}

/** 因果裁决(后端 CausalVerdict)。 */
export interface CausalVerdict {
  route: string;
  confidence: number;
  summary: string;
  violations: string[];
}

/** TPN 相位详情(后端 TpnPhaseState)。 */
export interface TpnPhaseState {
  taskId: string;
  currentPhase: TpnPhase;
  metaSummary: string | null;
  fittingSummary: string | null;
  causalVerdict: CausalVerdict | null;
  deliverables: string[];
  tracePreview: TraceRecordPreview[];
}

/** 审批动作(后端 InterventionAction)。 */
export type InterventionAction = "Approve" | "RejectRetry" | "RejectReroute";

/** 阴极干预(后端 YinIntervention)。 */
export interface YinIntervention {
  taskId: string;
  action: InterventionAction;
  suggestion: string;
}

/** WS 请求-响应(后端 ServerResponse, rename_all=camelCase)。 */
export interface ServerResponse {
  requestId: string;
  ok: boolean;
  data?: unknown;
  error?: string;
  /** 流式聊天文本增量;非流式响应无此字段。 */
  chunk?: string;
  /** 流式响应最终帧标记;非流式响应无此字段。 */
  streamDone?: boolean;
}

/** WebSocket 推送事件(后端 TaskEvent, tag=type, content=data)。 */
export type TaskEvent =
  | { type: "TaskCreated"; data: { taskId: string; description: string; parentId: string | null; depth: number } }
  | { type: "TaskStatusChanged"; data: { taskId: string; oldStatus: NodeStatus; newStatus: NodeStatus } }
  | { type: "PhaseChanged"; data: { taskId: string; phase: TpnPhase } }
  | { type: "ChildSpawned"; data: { parentTaskId: string; childTaskId: string; description: string; depth: number } }
  | { type: "ChildCompleted"; data: { childTaskId: string; status: NodeStatus; deliverables: string[]; rounds: number } }
  | { type: "TpnRouteDecision"; data: { taskId: string; route: string; cycle: number; round: number; verdict: string } }
  | { type: "DeliverableCreated"; data: { taskId: string; path: string; sizeBytes: number } }
  | { type: "DmnEvolution"; data: { evolutions: EvolutionSummary[] } }
  | { type: "TaskTreeHeartbeat"; data: { activeTasks: number; timestamp: string } };
