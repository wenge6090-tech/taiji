import { useCallback, useEffect, useRef, useState } from "react";
import { wsClient } from "../lib/wsClient";
import type {
  GuizangGraph as GuizangGraphData,
  GuizangGraphEdge,
  GuizangGraphNode,
} from "../types";

/** 仿真画布坐标空间。 */
const W = 900;
const H = 600;

/** 力导向物理参数(经验值,保证收敛不爆炸)。 */
const REPULSION = 9000;
const SPRING = 0.06;
const REST = 130;
const GRAVITY = 0.006;
const DAMPING = 0.85;
const MAX_VEL = 24;
const SETTLE_FRAMES = 160;

const TYPE_COLOR: Record<GuizangGraphNode["assetType"], string> = {
  prompt: "#fbbf24",
  skill: "#a78bfa",
  model: "#22d3ee",
};

const TYPE_LABEL: Record<GuizangGraphNode["assetType"], string> = {
  prompt: "提示词",
  skill: "技能",
  model: "模型",
};

const EDGE_STYLE: Record<
  GuizangGraphEdge["kind"],
  { stroke: string; dash: string; opacity: number }
> = {
  dual: { stroke: "#22d3ee", dash: "6 4", opacity: 0.5 },
  model: { stroke: "#4ade80", dash: "", opacity: 0.55 },
  fork: { stroke: "#94a3b8", dash: "2 4", opacity: 0.45 },
};

interface SimNode {
  id: string;
  x: number;
  y: number;
  vx: number;
  vy: number;
}

function initSim(nodes: GuizangGraphNode[]): Map<string, SimNode> {
  const map = new Map<string, SimNode>();
  for (const n of nodes) {
    const angle = Math.random() * Math.PI * 2;
    const r = 40 + Math.random() * 180;
    map.set(n.id, {
      id: n.id,
      x: W / 2 + Math.cos(angle) * r,
      y: H / 2 + Math.sin(angle) * r,
      vx: 0,
      vy: 0,
    });
  }
  return map;
}

/** 单步仿真(排斥 + 弹簧 + 向心 + 阻尼)。就地修改 sim。 */
function step(sim: Map<string, SimNode>, edges: GuizangGraphEdge[]) {
  const arr = [...sim.values()];
  // 排斥(两两)
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
  // 弹簧(边)
  for (const e of edges) {
    const a = sim.get(e.source);
    const b = sim.get(e.target);
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
  // 向心 + 阻尼 + 积分
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

function nodeRadius(n: GuizangGraphNode): number {
  const base = n.assetType === "model" ? 5 : 7;
  return Math.min(16, Math.max(5, base + n.confidence * 9));
}

export default function GuizangGraph({
  onClose,
}: {
  onClose?: () => void;
}) {
  const [data, setData] = useState<GuizangGraphData | null>(null);
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

  const startSim = useCallback(
    (g: GuizangGraphData) => {
      stopSim();
      const sim = initSim(g.nodes);
      simRef.current = sim;
      let frame = 0;
      const tick = () => {
        if (!simRef.current) return;
        step(simRef.current, g.edges);
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
    },
    [stopSim],
  );

  useEffect(() => {
    let cancelled = false;
    wsClient
      .send("GetGuizangGraph", {})
      .then((resp) => {
        if (cancelled) return;
        const g = resp.data as GuizangGraphData;
        setData(g);
        setLoading(false);
        startSim(g);
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

  const selected = selectedId
    ? (data?.nodes.find((n) => n.id === selectedId) ?? null)
    : null;

  const counts = data
    ? {
        prompt: data.nodes.filter((n) => n.assetType === "prompt").length,
        skill: data.nodes.filter((n) => n.assetType === "skill").length,
        model: data.nodes.filter((n) => n.assetType === "model").length,
      }
    : null;

  return (
    <div className="fixed inset-0 z-40 flex flex-col bg-bg-deep/95 backdrop-blur">
      {/* 顶栏 */}
      <div className="flex items-center justify-between border-b border-slate-800 px-5 py-3">
        <div className="flex items-center gap-4">
          <h2 className="text-lg font-semibold text-yang">归藏认知库图谱</h2>
          {counts && (
            <div className="flex items-center gap-3 text-xs">
              <span className="flex items-center gap-1 text-slate-400">
                <span className="h-2 w-2 rounded-full" style={{ background: TYPE_COLOR.prompt }} />
                提示词 {counts.prompt}
              </span>
              <span className="flex items-center gap-1 text-slate-400">
                <span className="h-2 w-2 rounded-full" style={{ background: TYPE_COLOR.skill }} />
                技能 {counts.skill}
              </span>
              <span className="flex items-center gap-1 text-slate-400">
                <span className="h-2 w-2 rounded-full" style={{ background: TYPE_COLOR.model }} />
                模型 {counts.model}
              </span>
            </div>
          )}
        </div>
        <div className="flex items-center gap-4 text-xs text-slate-500">
          <span className="flex items-center gap-1">
            <span className="inline-block h-0.5 w-4 border-t border-dashed border-cyan-400/70" />
            对偶
          </span>
          <span className="flex items-center gap-1">
            <span className="inline-block h-0.5 w-4 border-t border-green-400/70" />
            后验
          </span>
          <span className="flex items-center gap-1">
            <span className="inline-block h-0.5 w-4 border-t border-dotted border-slate-400/70" />
            变体
          </span>
          <button
            onClick={onClose}
            className="rounded-lg border border-slate-600 bg-slate-900 px-3 py-1.5 text-sm text-slate-200 transition-colors duration-300 hover:border-yang hover:text-yang"
          >
            返回纺锤视图
          </button>
        </div>
      </div>

      {/* 主体 */}
      <div className="relative flex-1 overflow-hidden">
        {loading ? (
          <div className="flex h-full items-center justify-center text-sm text-slate-400">
            加载归藏知识库…
          </div>
        ) : error ? (
          <div className="flex h-full items-center justify-center text-sm text-red-400">
            {error}
          </div>
        ) : !data || data.nodes.length === 0 ? (
          <div className="flex h-full flex-col items-center justify-center gap-2 text-sm text-slate-500">
            <span>归藏知识库为空</span>
            <span className="text-xs">运行任务或 `taiji run` 后由连山演化产出资产</span>
          </div>
        ) : (
          <svg
            className="h-full w-full"
            viewBox={`0 0 ${W} ${H}`}
            preserveAspectRatio="xMidYMid meet"
            onClick={() => setSelectedId(null)}
          >
            {/* 边 */}
            {data.edges.map((e, i) => {
              const a = positions.get(e.source);
              const b = positions.get(e.target);
              if (!a || !b) return null;
              const style = EDGE_STYLE[e.kind] ?? EDGE_STYLE.fork;
              return (
                <line
                  key={`${e.source}-${e.target}-${i}`}
                  x1={a.x}
                  y1={a.y}
                  x2={b.x}
                  y2={b.y}
                  stroke={style.stroke}
                  strokeDasharray={style.dash}
                  strokeOpacity={style.opacity}
                  strokeWidth={e.kind === "model" ? 1.5 : 1}
                />
              );
            })}

            {/* 节点 */}
            {data.nodes.map((n) => {
              const p = positions.get(n.id);
              if (!p) return null;
              const r = nodeRadius(n);
              const color = TYPE_COLOR[n.assetType];
              const isSel = n.id === selectedId;
              return (
                <g
                  key={n.id}
                  transform={`translate(${p.x}, ${p.y})`}
                  className="cursor-pointer"
                  onClick={(ev) => {
                    ev.stopPropagation();
                    setSelectedId(isSel ? null : n.id);
                  }}
                >
                  <title>
                    {`${n.label}\n类型:${TYPE_LABEL[n.assetType]}${
                      n.category ? ` (${n.category})` : ""
                    }\n置信度:${n.confidence.toFixed(2)}\n样本:${n.statsN}${
                      n.agentTarget ? `\n消费方:${n.agentTarget}` : ""
                    }`}
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
                    y={r + 11}
                    textAnchor="middle"
                    fontSize={10}
                    fill="#cbd5e1"
                    style={{ pointerEvents: "none" }}
                  >
                    {n.label.length > 10 ? `${n.label.slice(0, 10)}…` : n.label}
                  </text>
                </g>
              );
            })}
          </svg>
        )}

        {/* 节点详情侧栏 */}
        {selected && (
          <div className="absolute right-4 top-4 w-64 rounded-xl border border-slate-700 bg-slate-900/95 p-4 shadow-xl">
            <div className="flex items-center gap-2">
              <span
                className="h-3 w-3 rounded-full"
                style={{ background: TYPE_COLOR[selected.assetType] }}
              />
              <h3 className="truncate text-sm font-semibold text-slate-200">
                {selected.label}
              </h3>
            </div>
            <dl className="mt-3 space-y-2 text-xs">
              <div className="flex justify-between">
                <dt className="text-slate-500">类型</dt>
                <dd className="text-slate-300">
                  {TYPE_LABEL[selected.assetType]}
                  {selected.category ? ` / ${selected.category}` : ""}
                </dd>
              </div>
              <div className="flex justify-between">
                <dt className="text-slate-500">置信度</dt>
                <dd className="font-mono text-slate-300">
                  {selected.confidence.toFixed(2)}
                </dd>
              </div>
              <div className="flex justify-between">
                <dt className="text-slate-500">样本数</dt>
                <dd className="font-mono text-slate-300">{selected.statsN}</dd>
              </div>
              {selected.agentTarget && (
                <div className="flex justify-between">
                  <dt className="text-slate-500">消费方</dt>
                  <dd className="text-slate-300">{selected.agentTarget}</dd>
                </div>
              )}
              <div className="flex justify-between">
                <dt className="text-slate-500">状态</dt>
                <dd className="text-slate-300">{selected.status}</dd>
              </div>
            </dl>
          </div>
        )}
      </div>
    </div>
  );
}
