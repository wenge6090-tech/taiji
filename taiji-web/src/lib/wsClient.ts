import type { ServerResponse, TaskEvent } from "../types";

export type WsStatus = "connecting" | "open" | "closed";

/** 前端 → 核心的请求类型(与后端 ClientMessage 判别变体一致)。 */
export type ClientMessageType =
  | "ExecuteTask"
  | "SubmitReview"
  | "ListTasks"
  | "GetTaskTree"
  | "GetZhouyiState"
  | "PlanMessage"
  | "ChatMessage";

interface PendingRequest {
  resolve: (value: unknown) => void;
  reject: (err: Error) => void;
  timer: ReturnType<typeof setTimeout>;
  /** 流式请求的增量回调;普通请求为 undefined。 */
  onChunk?: (text: string, accumulated: string) => void;
  /** 流式请求已累积文本。 */
  accumulated?: string;
}

const DEFAULT_TIMEOUT_MS = 30_000;
/** LLM 对话请求超时(可能耗时较长)。 */
export const CHAT_TIMEOUT_MS = 120_000;

/**
 * WebSocket 客户端单例:
 * - 一条连接承载双向通道:广播事件(ServerResponse/TaskEvent 判别) + 请求-响应(requestId 关联)
 * - 断线 3s 自动重连;重连期间 send 的请求先入 outbox,连上后补发
 * - 显式 close() 后不再重连
 */
class WsClient {
  private ws: WebSocket | null = null;
  private status: WsStatus = "connecting";
  private statusListeners = new Set<(s: WsStatus) => void>();
  private eventListeners = new Set<(e: TaskEvent) => void>();
  private pending = new Map<string, PendingRequest>();
  private outbox: Array<{ requestId: string; frame: string }> = [];
  private disposed = false;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private requestSeq = 0;

  getStatus(): WsStatus {
    return this.status;
  }

  onStatusChange(h: (s: WsStatus) => void): () => void {
    this.statusListeners.add(h);
    return () => {
      this.statusListeners.delete(h);
    };
  }

  onEvent(h: (e: TaskEvent) => void): () => void {
    this.eventListeners.add(h);
    return () => {
      this.eventListeners.delete(h);
    };
  }

  /** 建立连接(幂等:已有连接或已关闭时跳过)。 */
  connect(): void {
    if (this.disposed || this.ws) return;
    this.setStatus("connecting");
    const ws = new WebSocket("ws://127.0.0.1:17890");
    this.ws = ws;
    ws.onopen = () => {
      if (this.ws !== ws) return;
      this.setStatus("open");
      for (const item of this.outbox) ws.send(item.frame);
      this.outbox = [];
    };
    ws.onmessage = (ev) => this.handleFrame(ev.data as string);
    ws.onerror = () => ws.close();
    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.ws = null;
      this.rejectAllPending(new Error("引擎连接已断开"));
      this.setStatus("closed");
      if (!this.disposed) {
        this.retryTimer = setTimeout(() => this.connect(), 3000);
      }
    };
  }

  /** 关闭连接并拒绝所有在途请求。 */
  close(): void {
    this.disposed = true;
    if (this.retryTimer) clearTimeout(this.retryTimer);
    this.retryTimer = null;
    this.ws?.close();
    this.ws = null;
    this.rejectAllPending(new Error("连接已关闭"));
  }

  /** 发送请求-响应消息。默认 30s 超时, ChatMessage 用 120s。 */
  send(
    type: ClientMessageType,
    data: Record<string, unknown> = {},
    timeoutMs: number = DEFAULT_TIMEOUT_MS
  ): Promise<ServerResponse> {
    const requestId = `${Date.now().toString(36)}-${(this.requestSeq++).toString(36)}`;
    const frame = JSON.stringify({ type, data: { ...data, requestId } });
    return new Promise<ServerResponse>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(requestId);
        reject(new Error(`请求超时(${Math.round(timeoutMs / 1000)}s): ${type}`));
      }, timeoutMs);
      this.pending.set(requestId, { resolve: resolve as (v: unknown) => void, reject, timer });
      if (this.ws && this.ws.readyState === WebSocket.OPEN) {
        this.ws.send(frame);
      } else if (this.status === "connecting") {
        this.outbox.push({ requestId, frame });
      } else {
        clearTimeout(timer);
        this.pending.delete(requestId);
        reject(new Error("引擎未连接，请先运行 `taiji serve`"));
      }
    });
  }

  /**
   * 发送流式请求(聊天)。服务端以多个 chunk 帧回推文本增量,
   * 最终帧(streamDone)后 Promise resolve 为完整文本。
   */
  sendStreaming(
    type: ClientMessageType,
    data: Record<string, unknown>,
    onChunk: (text: string, accumulated: string) => void,
    timeoutMs: number = CHAT_TIMEOUT_MS
  ): Promise<string> {
    const requestId = `${Date.now().toString(36)}-${(this.requestSeq++).toString(36)}`;
    const frame = JSON.stringify({ type, data: { ...data, requestId } });
    return new Promise<string>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(requestId);
        reject(new Error(`流式请求超时(${Math.round(timeoutMs / 1000)}s): ${type}`));
      }, timeoutMs);
      this.pending.set(requestId, {
        resolve: resolve as (v: unknown) => void,
        reject,
        timer,
        onChunk,
        accumulated: "",
      });
      if (this.ws && this.ws.readyState === WebSocket.OPEN) {
        this.ws.send(frame);
      } else if (this.status === "connecting") {
        this.outbox.push({ requestId, frame });
      } else {
        clearTimeout(timer);
        this.pending.delete(requestId);
        reject(new Error("引擎未连接，请先运行 `taiji serve`"));
      }
    });
  }

  private handleFrame(raw: string): void {
    let msg: unknown;
    try {
      msg = JSON.parse(raw);
    } catch {
      return;
    }
    if (typeof msg !== "object" || msg === null) return;
    const m = msg as Record<string, unknown>;
    if (typeof m.requestId === "string") {
      const resp = m as unknown as ServerResponse;
      const p = this.pending.get(resp.requestId);
      if (!p) return;
      // 流式请求:chunk 帧只转发增量,streamDone 帧才完成
      if (p.onChunk) {
        if (resp.chunk != null && resp.streamDone !== true) {
          p.accumulated = (p.accumulated ?? "") + resp.chunk;
          p.onChunk(resp.chunk, p.accumulated);
          return; // 不清 timer,不 resolve
        }
        clearTimeout(p.timer);
        this.pending.delete(resp.requestId);
        if (resp.ok && resp.streamDone === true) {
          p.resolve(p.accumulated ?? "");
        } else {
          p.reject(new Error(resp.error ?? "引擎流式响应失败"));
        }
        return;
      }
      // 普通请求
      clearTimeout(p.timer);
      this.pending.delete(resp.requestId);
      if (resp.ok) p.resolve(resp);
      else p.reject(new Error(resp.error ?? "引擎返回失败"));
    } else if (typeof m.type === "string") {
      this.eventListeners.forEach((h) => h(m as unknown as TaskEvent));
    }
  }

  private rejectAllPending(err: Error): void {
    for (const [, p] of this.pending) {
      clearTimeout(p.timer);
      p.reject(err);
    }
    this.pending.clear();
  }

  private setStatus(s: WsStatus): void {
    this.status = s;
    this.statusListeners.forEach((h) => h(s));
  }
}

export const wsClient = new WsClient();

/** 仅当事件涉及指定任务(或其子树不可知时视为相关)时刷新。 */
export function eventTouchesTask(e: TaskEvent, taskId: string | null): boolean {
  if (!taskId) return false;
  switch (e.type) {
    case "TaskCreated":
      return e.data.parentId === taskId || e.data.taskId === taskId;
    case "ChildSpawned":
      return e.data.parentTaskId === taskId || e.data.childTaskId === taskId;
    default:
      // 状态/相位/产出/路由事件都携带 taskId 或 childTaskId
      return true; // 保守:任何事件都允许刷新(快照构建廉价)
  }
}
