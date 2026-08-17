import { useEffect, useMemo, useState } from "react";
import { wsClient } from "./lib/wsClient";
import ChatPanel from "./components/ChatPanel";
import GuizangGraph from "./components/GuizangGraph";
import LianshanPanel from "./components/LianshanPanel";
import OntologyPanel from "./components/OntologyPanel";
import SpindleTree from "./components/SpindleTree";
import StatusLegend, { countStatuses } from "./components/StatusLegend";
import TaijiBg from "./components/TaijiBg";
import ZhouyiPopup from "./components/ZhouyiPopup";
import { useTaskTree } from "./hooks/useTaskTree";
import { useZhouyiState } from "./hooks/useZhouyiState";
import type { TaskListItem } from "./types";

const WS_STATUS_TEXT: Record<string, string> = {
  connecting: "连接中",
  open: "实时",
  closed: "离线",
};
const WS_STATUS_COLOR: Record<string, string> = {
  connecting: "#facc15",
  open: "#4ade80",
  closed: "#f87171",
};

export default function App() {
  const { snapshot, error, loading, wsStatus, currentRoot, setRoot } =
    useTaskTree();
  const {
    selectedNode,
    phaseState,
    detailLoading,
    detailError,
    openNode,
    closeNode,
  } = useZhouyiState();

  const [taskList, setTaskList] = useState<TaskListItem[]>([]);
  const [showGuizang, setShowGuizang] = useState(false);
  const [showOntology, setShowOntology] = useState(false);

  // 启动时拉取根任务列表,默认选中最新任务
  useEffect(() => {
    wsClient
      .send("ListTasks", {})
      .then((resp) => {
        const items = resp.data as TaskListItem[];
        setTaskList(items);
        if (items.length > 0) setRoot(items[0].id);
      })
      .catch(() => setTaskList([]));
  }, [setRoot]);

  // 快照刷新后同步列表(新任务出现)
  useEffect(() => {
    if (!snapshot?.rootTaskId) return;
    setTaskList((prev) =>
      prev.some((t) => t.id === snapshot.rootTaskId)
        ? prev
        : [
            {
              id: snapshot.rootTaskId,
              description: snapshot.rootDescription,
            },
            ...prev,
          ]
    );
  }, [snapshot]);

  const onRunTask = (desc: string) => {
    wsClient
      .send("ExecuteTask", { description: desc })
      .then(() => {
        // 新根任务视图切换由 TaskCreated(parentId=null) 事件驱动,这里刷新列表兜底
        wsClient.send("ListTasks", {}).then((resp) => {
          setTaskList(resp.data as TaskListItem[]);
        });
      })
      .catch((e) => console.error("执行任务失败", e));
  };

  const busy =
    loading || (snapshot?.nodes ?? []).some((n) => n.status === "Running");

  const statusCounts = useMemo(
    () => countStatuses(snapshot?.nodes ?? []),
    [snapshot]
  );

  return (
    <div className="relative h-screen w-screen overflow-hidden bg-bg-deep text-slate-200">
      {/* 太极背景 */}
      <TaijiBg active={busy} />

      {/* 布局:左聊天 + 右纺锤 */}
      <div className="relative z-10 flex h-full">
        <ChatPanel
          onRunTask={onRunTask}
          wsStatus={wsStatus}
          selectedTaskId={selectedNode?.taskId ?? null}
        />
        <main className="relative flex-1 overflow-hidden">
          {/* 顶栏 */}
          <div className="absolute top-0 left-0 right-0 z-20 flex items-center justify-between px-4 py-2 bg-bg-deep/60 backdrop-blur-sm border-b border-slate-800/60">
            <div className="flex items-center gap-3">
              <span className="text-glow text-sm font-semibold tracking-widest">
                太极·任务递归树
              </span>
              <select
                value={currentRoot ?? ""}
                onChange={(e) => e.target.value && setRoot(e.target.value)}
                className="max-w-[320px] bg-slate-900 border border-slate-700 rounded px-2 py-0.5 text-xs text-slate-300 focus:outline-none"
                title="切换根任务"
              >
                {taskList.length === 0 && <option value="">暂无任务</option>}
                {taskList.map((t) => {
                  const label = t.description.trim()
                    ? t.description.length > 24
                      ? `${t.description.slice(0, 24)}…`
                      : t.description
                    : `${t.id.slice(0, 8)}…`;
                  return (
                    <option
                      key={t.id}
                      value={t.id}
                      title={`${t.id} — ${t.description}`}
                    >
                      {label}
                    </option>
                  );
                })}
              </select>
            </div>
            <div className="flex items-center gap-3 text-xs">
              <span
                className="inline-block w-2 h-2 rounded-full"
                style={{ backgroundColor: WS_STATUS_COLOR[wsStatus] }}
                title="WebSocket 推送"
              />
              <span className="text-slate-400">{WS_STATUS_TEXT[wsStatus]}</span>
              <button
                onClick={() => setShowGuizang(true)}
                className="px-2 py-0.5 rounded border border-slate-700 text-slate-300 hover:border-yang hover:text-yang transition-colors duration-300"
              >
                归藏图谱
              </button>
              <button
                onClick={() => setShowOntology(true)}
                className="px-2 py-0.5 rounded border border-slate-700 text-slate-300 hover:border-yang hover:text-yang transition-colors duration-300"
              >
                语义层
              </button>
            </div>
          </div>

          {/* 纺锤树 */}
          {error ? (
            <div className="flex h-full items-center justify-center text-red-400 text-sm">
              {error}
            </div>
          ) : (
            <SpindleTree
              key={currentRoot ?? "none"}
              nodes={snapshot?.nodes ?? []}
              edges={snapshot?.edges ?? []}
              onSelectNode={openNode}
              selectedTaskId={selectedNode?.taskId ?? null}
            />
          )}

          {/* 底部状态图例(左) */}
          <div className="absolute bottom-4 left-4 z-20">
            <StatusLegend counts={statusCounts} />
          </div>

          {/* 连山演化浮层(右) */}
          <div className="absolute bottom-4 right-4 z-20">
            <LianshanPanel activity={snapshot?.lianshanActivity ?? null} />
          </div>
        </main>
      </div>

      {/* Zhouyi 弹窗 */}
      {selectedNode && (
        <ZhouyiPopup
          node={selectedNode}
          phaseState={phaseState}
          loading={detailLoading}
          error={detailError}
          onClose={closeNode}
        />
      )}

      {/* 归藏图谱 */}
      {showGuizang && <GuizangGraph onClose={() => setShowGuizang(false)} />}

      {/* 语义层（本体）视图 */}
      {showOntology && <OntologyPanel onClose={() => setShowOntology(false)} />}
    </div>
  );
}
