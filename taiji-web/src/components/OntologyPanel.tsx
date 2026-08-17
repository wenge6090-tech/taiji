import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { wsClient } from "../lib/wsClient";
import type {
  FailureGroup,
  OntologyEdge,
  OntologyRule,
  OntologyView,
  SemanticType,
} from "../types";

const W = 900;
const H = 600;

const REPULSION = 14000;
const SPRING = 0.06;
const REST = 160;
const GRAVITY = 0.008;
const DAMPING = 0.85;
const MAX_VEL = 24;
const SETTLE_FRAMES = 160;

const SOURCE_COLOR: Record<SemanticType["source"], string> = {
  human: "#fbbf24",
  mined: "#22d3ee",
  compiled: "#4ade80",
};

const SOURCE_LABEL: Record<SemanticType["source"], string> = {
  human: "人工种子",
  mined: "连山挖掘",
  compiled: "编译命名",
};

const EDGE_COLOR: Record<OntologyEdge["kind"], string> = {
  weak_dependency: "#22d3ee",
  sequence: "#fbbf24",
};

interface SimNode {
  id: string;
  x: number;
  y: number;
  vx: number;
  vy: number;
}

function initSim(ids: string[]): Map<string, SimNode> {
  const map = new Map<string, SimNode>();
  for (const id of ids) {
    const angle = Math.random() * Math.PI * 2;
    const r = 40 + Math.random() * 160;
    map.set(id, {
      id,
      x: W / 2 + Math.cos(angle) * r,
      y: H / 2 + Math.sin(angle) * r,
      vx: 0,
      vy: 0,
    });
  }
  return map;
}

function step(sim: Map<string, SimNode>, edges: OntologyEdge[]) {
  const arr = [...sim.values()];
  for (let i = 0; i < arr.length; i++) {
    for (let j = i + 1; j < arr.length; j++) {
      const a = arr[i];
      const b = arr[j];
      const dx = a.x - b.x;
      const dy = a.y - b.y;
      const dist2 = dx * dx + dy * dy + 0.01;
      const dist = Math.sqrt(dist2);
      const f = REPULSION / dist2;
      const fx = (dx / dist) * f;
      const fy = (dy / dist) * f;
      a.vx += fx;
      a.vy += fy;
      b.vx -= fx;
      b.vy -= fy;
    }
  }
  for (const e of edges) {
    const a = sim.get(e.from);
    const b = sim.get(e.to);
    if (!a || !b) continue;
    const dx = b.x - a.x;
    const dy = b.y - a.y;
    const dist = Math.sqrt(dx * dx + dy * dy) || 1;
    const f = SPRING * (dist - REST);
    const fx = (dx / dist) * f;
    const fy = (dy / dist) * f;
    a.vx += fx;
    a.vy += fy;
    b.vx -= fx;
    b.vy -= fy;
  }
  for (const n of sim.values()) {
    n.vx += (W / 2 - n.x) * GRAVITY;
    n.vy += (H / 2 - n.y) * GRAVITY;
    n.vx *= DAMPING;
    n.vy *= DAMPING;
    const v = Math.sqrt(n.vx * n.vx + n.vy * n.vy);
    if (v > MAX_VEL) {
      n.vx = (n.vx / v) * MAX_VEL;
      n.vy = (n.vy / v) * MAX_VEL;
    }
    n.x += n.vx;
    n.y += n.vy;
  }
}

function StatBadge({ label, value }: { label: string; value: number }) {
  return (
    <span className="flex items-center gap-1 text-xs text-slate-400">
      <span className="text-slate-500">{label}</span>
      <span className="font-mono text-slate-200">{value}</span>
    </span>
  );
}

function EmptyHint({ children }: { children: React.ReactNode }) {
  return (
    <p className="rounded border border-dashed border-slate-800 px-2 py-1.5 text-xs text-slate-500">
      {children}
    </p>
  );
}

export default function OntologyPanel({ onClose }: { onClose?: () => void }) {
  const [view, setView] = useState<OntologyView | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [positions, setPositions] = useState<Map<string, { x: number; y: number }>>(
    new Map(),
  );
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const simRef = useRef<Map<string, SimNode> | null>(null);
  const rafRef = useRef<number | null>(null);

  const stopSim = useCallback(() => {
    if (rafRef.current !== null) {
      cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    }
  }, []);

  const startSim = useCallback((edges: OntologyEdge[], typeIds: string[]) => {
    stopSim();
    if (typeIds.length === 0) return;
    const sim = initSim(typeIds);
    simRef.current = sim;
    let frame = 0;
    const tick = () => {
      if (!simRef.current) return;
      step(simRef.current, edges);
      const next = new Map<string, { x: number; y: number }>();
      for (const n of simRef.current.values()) {
        next.set(n.id, { x: n.x, y: n.y });
      }
      setPositions(next);
      frame += 1;
      if (frame < SETTLE_FRAMES) {
        rafRef.current = requestAnimationFrame(tick);
      } else {
        rafRef.current = null;
      }
    };
    rafRef.current = requestAnimationFrame(tick);
  }, [stopSim]);

  useEffect(() => {
    let cancelled = false;
    wsClient
      .send("GetOntologyView", {})
      .then((resp) => {
        if (cancelled) return;
        const v = resp.data as OntologyView;
        setView(v);
        setLoading(false);
        startSim(v.edges, v.types.map((t) => t.id));
      })
      .catch((e) => {
        if (cancelled) return;
        setError(String(e));
        setLoading(false);
      });
    return () => {
      cancelled = true;
      stopSim();
    };
  }, [startSim, stopSim]);

  /** 类型 id → 映射到它的资产 id 列表。 */
  const assetsByType = useMemo(() => {
    const map = new Map<string, string[]>();
    if (!view) return map;
    for (const [assetId, typeId] of Object.entries(view.asset_type_map)) {
      const list = map.get(typeId) ?? [];
      list.push(assetId);
      map.set(typeId, list);
    }
    return map;
  }, [view]);

  const selected = selectedId
    ? (view?.types.find((t) => t.id === selectedId) ?? null)
    : null;

  // 先验状态摘要
  const priorActive = view ? view.edges.length > 0 || view.rules.length > 0 : false;

  return (
    <div className="fixed inset-0 z-40 flex flex-col bg-bg-deep/95 backdrop-blur">
      {/* 顶栏 */}
      <div className="flex items-center justify-between border-b border-slate-800 px-5 py-3">
        <div className="flex items-center gap-4">
          <h2 className="text-lg font-semibold text-yang">语义层·本体</h2>
          <span
            className={`rounded px-2 py-0.5 text-xs ${
              priorActive
                ? "bg-green-500/15 text-green-300"
                : "bg-slate-800 text-slate-400"
            }`}
          >
            {priorActive ? "先验已激活" : "先验未激活"}
          </span>
        </div>
        <div className="flex items-center gap-4">
          <StatBadge label="类型" value={view?.types.length ?? 0} />
          <StatBadge label="边" value={view?.edges.length ?? 0} />
          <StatBadge label="规则" value={view?.rules.length ?? 0} />
          <StatBadge label="共现" value={view?.cooccur.length ?? 0} />
          <StatBadge label="失败" value={view?.failures.length ?? 0} />
          <StatBadge label="映射" value={Object.keys(view?.asset_type_map ?? {}).length} />
          <button
            onClick={onClose}
            className="rounded-lg border border-slate-600 bg-slate-900 px-3 py-1.5 text-sm text-slate-200 transition-colors duration-300 hover:border-yang hover:text-yang"
          >
            返回纺锤视图
          </button>
        </div>
      </div>

      {/* 主体 */}
      <div className="flex flex-1 overflow-hidden">
        {/* 图 */}
        <div className="relative flex-1 overflow-hidden">
          {loading ? (
            <div className="flex h-full items-center justify-center text-sm text-slate-400">
              加载语义层…
            </div>
          ) : error ? (
            <div className="flex h-full items-center justify-center text-sm text-red-400">
              {error}
            </div>
          ) : !view || view.types.length === 0 ? (
            <div className="flex h-full flex-col items-center justify-center gap-2 text-sm text-slate-500">
              <span>词汇表为空（types.yaml 未种子）</span>
              <span className="text-xs">
                这是元的先验智能的启动钥匙——需人工种语义类型
              </span>
            </div>
          ) : (
            <svg
              className="h-full w-full"
              viewBox={`0 0 ${W} ${H}`}
              preserveAspectRatio="xMidYMid meet"
              onClick={() => setSelectedId(null)}
            >
              {/* 边 */}
              {view.edges.map((e, i) => {
                const a = positions.get(e.from);
                const b = positions.get(e.to);
                if (!a || !b) return null;
                const color = EDGE_COLOR[e.kind];
                return (
                  <line
                    key={`${e.from}-${e.to}-${i}`}
                    x1={a.x}
                    y1={a.y}
                    x2={b.x}
                    y2={b.y}
                    stroke={color}
                    strokeDasharray={e.kind === "weak_dependency" ? "6 4" : ""}
                    strokeOpacity={0.55}
                    strokeWidth={1.5}
                  />
                );
              })}

              {/* 节点 */}
              {view.types.map((t) => {
                const p = positions.get(t.id);
                if (!p) return null;
                const assetCount = assetsByType.get(t.id)?.length ?? 0;
                const r = Math.min(22, Math.max(9, 8 + assetCount * 5));
                const color = SOURCE_COLOR[t.source];
                const isSel = t.id === selectedId;
                return (
                  <g
                    key={t.id}
                    transform={`translate(${p.x}, ${p.y})`}
                    className="cursor-pointer"
                    onClick={(ev) => {
                      ev.stopPropagation();
                      setSelectedId(isSel ? null : t.id);
                    }}
                  >
                    <title>
                      {`${t.name} (${t.id})\n来源:${SOURCE_LABEL[t.source]}\n映射资产:${assetCount}\n${t.description}`}
                    </title>
                    {isSel && (
                      <circle r={r + 5} fill="none" stroke={color} strokeWidth={1.5} opacity={0.6} />
                    )}
                    <circle
                      r={r}
                      fill={color}
                      fillOpacity={0.85}
                      stroke="rgba(2,6,23,0.6)"
                      strokeWidth={1}
                    />
                    <text
                      y={r + 12}
                      textAnchor="middle"
                      fontSize={11}
                      fill="#cbd5e1"
                      style={{ pointerEvents: "none" }}
                    >
                      {t.name.length > 6 ? `${t.name.slice(0, 6)}…` : t.name}
                    </text>
                  </g>
                );
              })}
            </svg>
          )}

          {/* 图例 */}
          {view && view.types.length > 0 && (
            <div className="absolute bottom-4 left-4 flex flex-wrap gap-3 rounded-lg border border-slate-800 bg-slate-900/85 px-3 py-2 text-[11px] text-slate-400">
              {(["human", "mined", "compiled"] as const).map((s) => (
                <span key={s} className="flex items-center gap-1">
                  <span className="h-2 w-2 rounded-full" style={{ background: SOURCE_COLOR[s] }} />
                  {SOURCE_LABEL[s]}
                </span>
              ))}
              <span className="flex items-center gap-1">
                <span className="inline-block h-0.5 w-4 border-t border-dashed border-cyan-400/70" />
                软依赖
              </span>
              <span className="flex items-center gap-1">
                <span className="inline-block h-0.5 w-4 border-t border-yang/70" />
                时序
              </span>
            </div>
          )}
        </div>

        {/* 右侧详情面板 */}
        <aside className="flex w-80 shrink-0 flex-col overflow-y-auto border-l border-slate-800 bg-slate-900/40">
          {/* 选中类型详情 */}
          {selected && (
            <section className="border-b border-slate-800 p-4">
              <h3 className="text-sm font-semibold text-slate-200">{selected.name}</h3>
              <p className="mt-1 font-mono text-xs text-slate-500">{selected.id}</p>
              <p className="mt-2 text-xs leading-relaxed text-slate-300">{selected.description}</p>
              <div className="mt-2 flex items-center gap-2 text-xs text-slate-400">
                <span
                  className="rounded px-1.5 py-0.5"
                  style={{ background: `${SOURCE_COLOR[selected.source]}22`, color: SOURCE_COLOR[selected.source] }}
                >
                  {SOURCE_LABEL[selected.source]}
                </span>
                {selected.parent && <span>父类:{selected.parent}</span>}
              </div>
              <div className="mt-3">
                <p className="text-xs font-semibold text-slate-500">映射资产</p>
                {(assetsByType.get(selected.id) ?? []).length > 0 ? (
                  <ul className="mt-1 space-y-1">
                    {assetsByType.get(selected.id)!.map((a) => (
                      <li key={a} className="truncate rounded border border-slate-800 bg-slate-900/60 px-2 py-1 font-mono text-xs text-slate-300" title={a}>
                        {a}
                      </li>
                    ))}
                  </ul>
                ) : (
                  <p className="mt-1 text-xs text-slate-500">无资产映射（前瞻种子类型）</p>
                )}
              </div>
            </section>
          )}

          {/* 规则 */}
          <section className="border-b border-slate-800 p-4">
            <h4 className="text-xs font-semibold uppercase tracking-wide text-slate-500">
              逻辑规则 rules.yaml
            </h4>
            {view && view.rules.length > 0 ? (
              <ul className="mt-2 space-y-2">
                {view.rules.map((r) => (
                  <RuleItem key={r.id} rule={r} />
                ))}
              </ul>
            ) : (
              <div className="mt-2">
                <EmptyHint>无规则——需失败样本 ≥50 且失败率 100% 才产出（Extract_Constraint）</EmptyHint>
              </div>
            )}
          </section>

          {/* 共现 */}
          <section className="border-b border-slate-800 p-4">
            <h4 className="text-xs font-semibold uppercase tracking-wide text-slate-500">
              共现原料 cooccur.yaml
            </h4>
            {view && view.cooccur.length > 0 ? (
              <ul className="mt-2 space-y-1.5">
                {view.cooccur.slice(0, 30).map((c, i) => (
                  <li key={`${c.a}-${c.b}-${i}`} className="flex items-center justify-between rounded border border-slate-800 bg-slate-900/60 px-2 py-1 text-xs">
                    <span className="truncate font-mono text-slate-300">
                      {c.a} ↔ {c.b}
                    </span>
                    <span className="shrink-0 font-mono text-slate-500">
                      {c.pass}/{c.co}
                    </span>
                  </li>
                ))}
              </ul>
            ) : (
              <div className="mt-2">
                <EmptyHint>无共现——跑任务后由连山从 assets_used 累积（门槛 ≥50）</EmptyHint>
              </div>
            )}
          </section>

          {/* 失败 */}
          <section className="p-4">
            <h4 className="text-xs font-semibold uppercase tracking-wide text-slate-500">
              失败分组 failures.yaml
            </h4>
            {view && view.failures.length > 0 ? (
              <ul className="mt-2 space-y-1.5">
                {view.failures.map((f, i) => (
                  <FailureItem key={`${f.check_kind}-${f.env_tags.join("-")}-${i}`} f={f} />
                ))}
              </ul>
            ) : (
              <div className="mt-2">
                <EmptyHint>无失败分组——需跑任务累积 check × env 失败样本</EmptyHint>
              </div>
            )}
          </section>
        </aside>
      </div>
    </div>
  );
}

function RuleItem({ rule }: { rule: OntologyRule }) {
  const conds: string[] = [];
  if (rule.when.domain) conds.push(`domain=${rule.when.domain}`);
  if (rule.when.env) conds.push(`env=${rule.when.env}`);
  if (rule.when.action) conds.push(`action=${rule.when.action}`);
  return (
    <li className="rounded border border-slate-800 bg-slate-900/60 p-2 text-xs">
      <div className="flex items-center justify-between">
        <span className="font-mono text-slate-300">{rule.id}</span>
        <span
          className={`rounded px-1 py-px text-[10px] ${
            rule.severity === "hard" ? "bg-red-500/15 text-red-300" : "bg-yellow-500/15 text-yellow-300"
          }`}
        >
          {rule.severity}
        </span>
      </div>
      {conds.length > 0 && <p className="mt-1 text-slate-500">当 {conds.join(" · ")}</p>}
      {rule.require.length > 0 && (
        <p className="mt-1 text-slate-300">必须:{rule.require.join(", ")}</p>
      )}
      {rule.forbid.length > 0 && (
        <p className="mt-1 text-red-400">禁止:{rule.forbid.join(", ")}</p>
      )}
    </li>
  );
}

function FailureItem({ f }: { f: FailureGroup }) {
  const rate = f.total > 0 ? (f.fails / f.total) * 100 : 0;
  return (
    <li className="flex items-center justify-between rounded border border-slate-800 bg-slate-900/60 px-2 py-1 text-xs">
      <span className="truncate font-mono text-slate-300">
        {f.check_kind}
        {f.env_tags.length > 0 ? ` @${f.env_tags.join(",")}` : ""}
      </span>
      <span className="shrink-0 font-mono text-slate-500">
        {f.fails}/{f.total} ({rate.toFixed(0)}%)
      </span>
    </li>
  );
}
