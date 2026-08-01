import { useEffect, useState } from "react";
import type { SpindleNode, TpnPhase, TpnPhaseState } from "../types";
import PhaseDetail from "./PhaseDetail";
import YinIntervene from "./YinIntervene";

const STATUS_DOT: Record<SpindleNode["status"], string> = {
  Pending: "bg-node-pending",
  Running: "bg-node-running",
  Converged: "bg-node-converged",
  Diverged: "bg-node-diverged",
  Failed: "bg-node-failed",
  Cancelled: "bg-node-cancelled",
  AwaitingHumanReview: "bg-node-review",
};

const TAB_PHASES: TpnPhase[] = ["Meta", "Fitting", "Causal"];

const PHASE_TAB_STYLE: Record<TpnPhase, string> = {
  Meta: "text-purple-400 border-purple-500/50",
  Fitting: "text-yang border-yang/50",
  Causal: "text-cyan-400 border-cyan-500/50",
  Idle: "text-slate-400 border-slate-600/50",
  Converged: "text-green-400 border-green-500/50",
};

export default function TpnPopup({
  node,
  phaseState,
  loading,
  error,
  onClose,
}: {
  node: SpindleNode;
  phaseState: TpnPhaseState | null;
  loading: boolean;
  error: string | null;
  onClose: () => void;
}) {
  const currentPhase = phaseState?.currentPhase ?? node.phase;
  const [activeTab, setActiveTab] = useState<TpnPhase>(
    TAB_PHASES.includes(currentPhase) ? currentPhase : "Meta"
  );

  useEffect(() => {
    setActiveTab(TAB_PHASES.includes(currentPhase) ? currentPhase : "Meta");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [node.taskId]);

  const title =
    node.description.length > 80
      ? `${node.description.slice(0, 80)}…`
      : node.description;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
      onClick={onClose}
    >
      <style>{`
        @keyframes taiji-phase-pulse {
          0%, 100% { box-shadow: 0 0 4px 1px rgba(251, 191, 36, 0.25); }
          50% { box-shadow: 0 0 14px 4px rgba(251, 191, 36, 0.55); }
        }
        .taiji-phase-pulse { animation: taiji-phase-pulse 1.6s ease-in-out infinite; }
      `}</style>
      <div
        className="flex max-h-[85vh] w-full max-w-3xl flex-col overflow-hidden rounded-xl border border-slate-700 bg-slate-900 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start justify-between gap-3 border-b border-slate-800 px-5 py-4">
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <span
                className={`h-2.5 w-2.5 shrink-0 rounded-full ${STATUS_DOT[node.status]}`}
                title={node.status}
              />
              <h3 className="truncate text-sm font-semibold text-slate-200" title={node.description}>
                {title}
              </h3>
            </div>
            <div className="mt-2 flex flex-wrap items-center gap-2 text-xs text-slate-400">
              <span
                className={`rounded border px-1.5 py-0.5 font-medium transition-colors duration-300 ${
                  node.mode === "Orchestration"
                    ? "border-yang/40 bg-yang/10 text-yang"
                    : "border-slate-600 bg-slate-800 text-slate-400"
                }`}
              >
                {node.mode === "Orchestration" ? "阳" : "阴"}
              </span>
              <span>round {node.round}</span>
              <span className="text-slate-600">·</span>
              <span>cycle {node.cycle}</span>
            </div>
          </div>
          <button
            onClick={onClose}
            className="rounded p-1 text-slate-500 transition-colors duration-300 hover:bg-slate-800 hover:text-slate-200"
            title="关闭"
          >
            ✕
          </button>
        </div>

        <div className="flex gap-2 border-b border-slate-800 px-5 py-3">
          {TAB_PHASES.map((phase) => {
            const isCurrent = phase === currentPhase;
            const isActive = phase === activeTab;
            return (
              <button
                key={phase}
                onClick={() => setActiveTab(phase)}
                className={`rounded-md border px-3 py-1.5 text-xs font-medium transition-colors duration-300 ${
                  PHASE_TAB_STYLE[phase]
                } ${isActive ? "bg-slate-800" : "bg-transparent hover:bg-slate-800/60"} ${
                  isCurrent ? "taiji-phase-pulse" : ""
                }`}
              >
                {phase}
              </button>
            );
          })}
        </div>

        <div className="min-h-[240px] flex-1 overflow-y-auto px-5 py-4">
          {loading ? (
            <p className="py-8 text-center text-sm text-slate-400">加载中…</p>
          ) : error ? (
            <p className="py-8 text-center text-sm text-red-400">{error}</p>
          ) : (
            <PhaseDetail phase={activeTab} node={node} phaseState={phaseState} />
          )}
        </div>

        <div className="border-t border-slate-800 px-5 py-3">
          <YinIntervene taskId={node.taskId} />
        </div>
      </div>
    </div>
  );
}
