import type { SpindleNode, TpnPhase, TpnPhaseState, TraceRecordPreview } from "../types";

const TRACE_PHASE_COLOR: Record<string, string> = {
  Meta: "border-purple-500/40 bg-purple-500/10 text-purple-400",
  Fitting: "border-yang/40 bg-yang/10 text-yang",
  Causal: "border-cyan-500/40 bg-cyan-500/10 text-cyan-400",
  Converged: "border-green-500/40 bg-green-500/10 text-green-400",
  Idle: "border-slate-600/40 bg-slate-700/10 text-slate-400",
};

function formatTs(ts: string): string {
  return ts.length >= 19 ? ts.slice(11, 19) : ts;
}

function fileBaseName(path: string): string {
  const idx = path.lastIndexOf("/");
  return idx >= 0 ? path.slice(idx + 1) : path;
}

function TraceRow({ rec }: { rec: TraceRecordPreview }) {
  return (
    <li className="flex items-start gap-2 rounded border border-slate-800 bg-slate-900/60 px-2 py-1.5 text-xs">
      <span className="shrink-0 font-mono text-slate-500">{formatTs(rec.ts)}</span>
      <span
        className={`shrink-0 rounded border px-1 py-px text-[10px] ${
          TRACE_PHASE_COLOR[rec.phase] ?? TRACE_PHASE_COLOR.Idle
        }`}
      >
        {rec.phase}
      </span>
      <span className="truncate text-slate-300" title={rec.summary}>
        {rec.summary.length > 80 ? `${rec.summary.slice(0, 80)}…` : rec.summary}
      </span>
    </li>
  );
}

export default function PhaseDetail({
  phase,
  node,
  phaseState,
}: {
  phase: TpnPhase;
  node: SpindleNode;
  phaseState: TpnPhaseState | null;
}) {
  const deliverables = phaseState?.deliverables ?? [];
  const trace = phaseState?.tracePreview ?? [];

  return (
    <div className="flex flex-col gap-4 lg:flex-row">
      <div className="min-w-0 flex-1 space-y-4">
        {phase === "Meta" && (
          <section>
            <h4 className="mb-2 text-xs font-semibold uppercase tracking-wide text-purple-400">
              Meta 相位
            </h4>
            <p className="rounded-lg border border-slate-800 bg-slate-900/60 p-3 text-sm leading-relaxed text-slate-200">
              {phaseState?.metaSummary ?? "Meta 相:任务解析与决策上下文生成(待生成)"}
            </p>
            <dl className="mt-3 grid grid-cols-2 gap-2 text-xs sm:grid-cols-4">
              <div className="rounded border border-slate-800 bg-slate-900/60 px-2 py-1.5">
                <dt className="text-slate-500">深度</dt>
                <dd className="mt-0.5 font-mono text-slate-200">{node.depth}</dd>
              </div>
              <div className="rounded border border-slate-800 bg-slate-900/60 px-2 py-1.5">
                <dt className="text-slate-500">轮次</dt>
                <dd className="mt-0.5 font-mono text-slate-200">{node.round}</dd>
              </div>
              <div className="rounded border border-slate-800 bg-slate-900/60 px-2 py-1.5">
                <dt className="text-slate-500">周期</dt>
                <dd className="mt-0.5 font-mono text-slate-200">{node.cycle}</dd>
              </div>
            </dl>
          </section>
        )}

        {phase === "Fitting" && (
          <section>
            <h4 className="mb-2 text-xs font-semibold uppercase tracking-wide text-yang">
              Fitting 相位
            </h4>
            <p className="rounded-lg border border-slate-800 bg-slate-900/60 p-3 text-sm leading-relaxed text-slate-200">
              {phaseState?.fittingSummary ?? "Fitting 相:阳面拟合与递归分解进行中…"}
            </p>
            {node.toolsUsed.length > 0 && (
              <div className="mt-3">
                <p className="mb-1.5 text-xs text-slate-500">已使用工具</p>
                <div className="flex flex-wrap gap-1.5">
                  {node.toolsUsed.map((tool) => (
                    <span
                      key={tool}
                      className="rounded border border-slate-600/60 bg-slate-800 px-2 py-0.5 font-mono text-xs text-slate-300 transition-colors duration-300"
                    >
                      {tool}
                    </span>
                  ))}
                </div>
              </div>
            )}
          </section>
        )}

        {phase === "Causal" && (
          <section>
            <h4 className="mb-2 text-xs font-semibold uppercase tracking-wide text-cyan-400">
              Causal 相位
            </h4>
            {phaseState?.causalVerdict ? (
              <div className="space-y-3 rounded-lg border border-slate-800 bg-slate-900/60 p-3">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="rounded border border-cyan-500/40 bg-cyan-500/10 px-2 py-0.5 font-mono text-xs text-cyan-300">
                    {phaseState.causalVerdict.route}
                  </span>
                  <span className="text-xs text-slate-500">置信度</span>
                  <div className="h-1.5 min-w-0 flex-1 overflow-hidden rounded-full bg-slate-800">
                    <div
                      className="h-full rounded-full bg-cyan-400 transition-all duration-300"
                      style={{
                        width: `${Math.min(
                          100,
                          Math.max(0, phaseState.causalVerdict.confidence)
                        )}%`,
                      }}
                    />
                  </div>
                  <span className="font-mono text-xs text-cyan-300">
                    {Math.min(100, Math.max(0, phaseState.causalVerdict.confidence))}%
                  </span>
                </div>
                <p className="text-sm leading-relaxed text-slate-200">
                  {phaseState.causalVerdict.summary}
                </p>
                {phaseState.causalVerdict.violations.length > 0 && (
                  <ul className="space-y-1">
                    {phaseState.causalVerdict.violations.map((v) => (
                      <li key={v} className="flex items-start gap-1.5 text-xs text-red-400">
                        <span className="shrink-0">✗</span>
                        <span>{v}</span>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            ) : (
              <p className="rounded-lg border border-slate-800 bg-slate-900/60 p-3 text-sm leading-relaxed text-slate-400">
                Causal 相:因果验证等待执行
              </p>
            )}
          </section>
        )}

        {phase !== "Meta" && phase !== "Fitting" && phase !== "Causal" && (
          <section>
            <h4 className="mb-2 text-xs font-semibold uppercase tracking-wide text-slate-400">
              相位 {phase}
            </h4>
            <p className="rounded-lg border border-slate-800 bg-slate-900/60 p-3 text-sm text-slate-400">
              该节点当前无相位详情。
            </p>
          </section>
        )}
      </div>

      <aside className="w-full shrink-0 space-y-4 lg:w-72">
        <section>
          <h4 className="mb-2 text-xs font-semibold uppercase tracking-wide text-slate-500">
            产出文件
            <span className="ml-1.5 rounded bg-slate-800 px-1.5 py-px font-mono text-[10px] text-slate-400">
              {deliverables.length}
            </span>
          </h4>
          {deliverables.length > 0 ? (
            <ul className="space-y-1">
              {deliverables.map((path) => (
                <li
                  key={path}
                  className="truncate rounded border border-slate-800 bg-slate-900/60 px-2 py-1 font-mono text-xs text-slate-300"
                  title={path}
                >
                  {fileBaseName(path)}
                </li>
              ))}
            </ul>
          ) : (
            <p className="rounded border border-dashed border-slate-800 px-2 py-1.5 text-xs text-slate-500">
              暂无产出
            </p>
          )}
        </section>

        <section>
          <h4 className="mb-2 text-xs font-semibold uppercase tracking-wide text-slate-500">
            最近追踪
          </h4>
          {trace.length > 0 ? (
            <ul className="max-h-40 space-y-1 overflow-y-auto pr-1">
              {trace.map((rec, i) => (
                <TraceRow key={`${rec.ts}-${i}`} rec={rec} />
              ))}
            </ul>
          ) : (
            <p className="rounded border border-dashed border-slate-800 px-2 py-1.5 text-xs text-slate-500">
              暂无追踪记录
            </p>
          )}
        </section>
      </aside>
    </div>
  );
}
