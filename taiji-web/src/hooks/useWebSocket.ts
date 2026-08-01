import { useEffect, useState } from "react";
import type { TaskEvent } from "../types";
import { wsClient } from "../lib/wsClient";
import type { WsStatus } from "../lib/wsClient";

export type { WsStatus } from "../lib/wsClient";
export { eventTouchesTask } from "../lib/wsClient";

/**
 * 订阅 wsClient 单例的状态与事件。
 * 仅订阅不建连:连接由本 hook 触发一次(幂等),断线重连由 wsClient 内部负责。
 */
export function useWebSocket(onEvent: (e: TaskEvent) => void): WsStatus {
  const [status, setStatus] = useState<WsStatus>(wsClient.getStatus());

  useEffect(() => {
    const offEvent = wsClient.onEvent(onEvent);
    const offStatus = wsClient.onStatusChange(setStatus);
    wsClient.connect();
    return () => {
      offEvent();
      offStatus();
    };
  }, [onEvent]);

  return status;
}
