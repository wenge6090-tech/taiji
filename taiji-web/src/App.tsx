import { useEffect, useState } from "react";
import { wsClient } from "./lib/wsClient";
import ChatPanel from "./components/ChatPanel";
import GuizangGraph from "./components/GuizangGraph";
import SpindleTree from "./components/SpindleTree";
import TaijiBg from "./components/TaijiBg";
import TpnPopup from "./components/TpnPopup";
import { useTaskTree } from "./hooks/useTaskTree";
import { useTpnState } from "./hooks/useTpnState";

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
  } = useTpnState();

  const [taskList, setTaskList] = useState<string[]>([]);
  const [showGuizang, setShowGuizang] = useState(false);

  // 启动时拉取根任务列表,默认选中最新任务
  useEffect(() => {
    wsClient
      .send("ListTasks", {})
      .then((resp) => {
        const ids = resp.data as string[];
        setTaskList(ids);
        if (ids.length > 0) setRoot(ids[0]);
      })
      .catch(() => setTaskList([]));
  }, [setRoot]);

  // 快照刷新后同步列表(新任务出现)
  useEffect(() => {
    if (!snapshot?.rootTaskId) return;
    setTaskList((prev) =>
      prev.includes(snapshot.rootTaskId)
        ? prev
        : [snapshot.rootTaskId, ...prev]
    );
  }, [snapshot]);

  const onRunTask = (desc: string) => {
    wsClient
      .send("ExecuteTask", { description: desc })
      .then(() => {
        // 新根任务视图切换由 TaskCreated(parentId=null) 事件驱动,这里刷新列表兜底
        wsClient.send("ListTasks", {}).then((resp) => {
          setTaskList(resp.data as string[]);
        });
      })
      .catch((e) => console.error("执行任务失败", e));
  };

  const busy =
    loading || (snapshot?.nodes ?? []).some((n) => n.status === "Running");

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
                className="bg-slate-900 border border-slate-700 rounded px-2 py-0.5 text-xs text-slate-300 focus:outline-none"
                title="切换根任务"
              >
                {taskList.length === 0 && <option value="">暂无任务</option>}
                {taskList.map((id) => (
                  <option key={id} value={id}>
                    {id.slice(0, 8)}…
                  </option>
                ))}
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
            </div>
          </div>

          {/* 纺锤树 */}
          {error ? (
            <div className="flex h-full items-center justify-center text-red-400 text-sm">
              {error}
            </div>
          ) : (
            <SpindleTree
              nodes={snapshot?.nodes ?? []}
              edges={snapshot?.edges ?? []}
              onSelectNode={openNode}
              selectedTaskId={selectedNode?.taskId ?? null}
            />
          )}
        </main>
      </div>

      {/* TPN 弹窗 */}
      {selectedNode && (
        <TpnPopup
          node={selectedNode}
          phaseState={phaseState}
          loading={detailLoading}
          error={detailError}
          onClose={closeNode}
        />
      )}

      {/* 归藏图谱存根 */}
      {showGuizang && <GuizangGraph onClose={() => setShowGuizang(false)} />}
    </div>
  );
}
