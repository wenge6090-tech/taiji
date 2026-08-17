import { useCallback, useEffect, useRef, useState } from "react";
import type { TaskEvent, TaskTreeSnapshot } from "../types";
import { wsClient } from "../lib/wsClient";
import { eventTouchesTask, useWebSocket } from "./useWebSocket";

/**
 * 任务树数据源:
 * - 初始经 WS 请求-响应拉取快照
 * - WS 事件到达后自动重新拉取(节流 500ms)
 * - 多任务下拉切换 rootTaskId;新根任务(TaskCreated, parentId=null)自动切换
 */
export function useTaskTree() {
  const [snapshot, setSnapshot] = useState<TaskTreeSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [currentRoot, setCurrentRoot] = useState<string | null>(null);
  const currentRootRef = useRef<string | null>(null);
  const refreshTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const setRoot = useCallback((id: string) => {
    currentRootRef.current = id;
    setCurrentRoot(id);
    // 切根时清空旧快照,避免新树短暂沿用旧树的适配视口
    setSnapshot(null);
  }, []);

  const refresh = useCallback(async (id: string) => {
    setLoading(true);
    try {
      const resp = await wsClient.send("GetTaskTree", { rootTaskId: id });
      setSnapshot(resp.data as TaskTreeSnapshot);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!currentRoot) return;
    refresh(currentRoot);
  }, [currentRoot, refresh]);

  // 防抖刷新:多个事件连发时合并为一次重拉
  const scheduleRefresh = useCallback(() => {
    if (refreshTimer.current) return;
    refreshTimer.current = setTimeout(() => {
      refreshTimer.current = null;
      const id = currentRootRef.current;
      if (id) refresh(id);
    }, 500);
  }, [refresh]);

  const onEvent = useCallback(
    (e: TaskEvent) => {
      // 新根任务出现(无父任务)时自动切换视图
      if (e.type === "TaskCreated" && e.data.parentId === null) {
        setRoot(e.data.taskId);
      }
      if (eventTouchesTask(e, currentRootRef.current)) scheduleRefresh();
    },
    [scheduleRefresh, setRoot]
  );

  const wsStatus = useWebSocket(onEvent);

  return { snapshot, error, loading, wsStatus, currentRoot, setRoot };
}
