import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { NodeStatus, SpindleEdge, SpindleNode } from "../types";
import SpindleNodeView from "./SpindleNode";

/** 纺锤最大水平散布(半宽)。 */
const MAX_SPREAD = 380;
const NODE_DIAMETER = 44;
const VERTICAL_GAP = 150;
const TOP_OFFSET = 40;
const PADDING = 70;

const MIN_SCALE = 0.2;
const MAX_SCALE = 3;

/** 边颜色与节点状态色一致。 */
const STATUS_COLOR: Record<NodeStatus, string> = {
  Pending: "#facc15",
  Running: "#fbbf24",
  Converged: "#4ade80",
  Diverged: "#f87171",
  Failed: "#f87171",
  Cancelled: "#94a3b8",
  AwaitingHumanReview: "#fb923c",
};

interface Point {
  x: number;
  y: number;
}

interface Layout {
  positions: Map<string, Point>;
  width: number;
  height: number;
}

interface Viewport {
  scale: number;
  x: number;
  y: number;
}

/**
 * 纺锤布局:f(d) = sin(π·d/max_depth) · MAX_SPREAD。
 * depth=0 居中,depth=max_depth 收拢回中心,中间层散布最大。
 * 同层 sibling 按 siblingIndex 在该层水平均分。
 */
function computeLayout(nodes: SpindleNode[]): Layout {
  if (nodes.length === 0) {
    return { positions: new Map(), width: 0, height: 0 };
  }

  const maxDepth = Math.max(...nodes.map((n) => n.depth), 1);

  const byDepth = new Map<number, SpindleNode[]>();
  let maxLayerSize = 1;
  for (const n of nodes) {
    const list = byDepth.get(n.depth) ?? [];
    list.push(n);
    byDepth.set(n.depth, list);
    maxLayerSize = Math.max(maxLayerSize, list.length);
  }

  // 层宽:基础 2·MAX_SPREAD,随单层节点数自适应加宽。
  const layerWidth = Math.max(MAX_SPREAD * 2, maxLayerSize * 110);
  const width = layerWidth + PADDING * 2;
  const height = maxDepth * VERTICAL_GAP + TOP_OFFSET + NODE_DIAMETER + PADDING * 2;

  const positions = new Map<string, Point>();
  for (const [depth, list] of byDepth) {
    const sorted = [...list].sort((a, b) => a.siblingIndex - b.siblingIndex);
    const spread = Math.sin((Math.PI * depth) / maxDepth) * MAX_SPREAD;
    const offsetX = PADDING + layerWidth / 2 + spread;
    const y = PADDING + depth * VERTICAL_GAP + TOP_OFFSET;
    const total = Math.max(
      ...sorted.map((n) => (n.totalSiblings > 0 ? n.totalSiblings : sorted.length)),
      1,
    );
    for (const n of sorted) {
      const x = offsetX + (layerWidth * (n.siblingIndex + 0.5)) / total;
      positions.set(n.taskId, { x, y });
    }
  }
  return { positions, width, height };
}

/** 沿 parentId 链向上判断 taskId 是否属于 ancestorId 子树(含自身)。 */
function isDescendantOf(
  taskId: string,
  ancestorId: string,
  parentMap: Map<string, string | null>,
): boolean {
  let current: string | null = taskId;
  while (current !== null) {
    if (current === ancestorId) {
      return true;
    }
    current = parentMap.get(current) ?? null;
  }
  return false;
}

interface SpindleTreeProps {
  nodes: SpindleNode[];
  edges: SpindleEdge[];
  onSelectNode: (node: SpindleNode) => void;
  selectedTaskId: string | null;
}

/**
 * 纺锤树:纯 SVG 边 + 绝对定位节点层,无外部布局库。
 * 支持滚轮缩放(以光标为锚点)、拖拽平移、双击/按钮适配归位。
 */
export default function SpindleTree({
  nodes,
  edges,
  onSelectNode,
  selectedTaskId,
}: SpindleTreeProps) {
  const layout = useMemo(() => computeLayout(nodes), [nodes]);
  const parentMap = useMemo(() => {
    const map = new Map<string, string | null>();
    for (const n of nodes) {
      map.set(n.taskId, n.parentId);
    }
    return map;
  }, [nodes]);

  const containerRef = useRef<HTMLDivElement | null>(null);
  const [viewport, setViewport] = useState<Viewport>({ scale: 1, x: 0, y: 0 });
  const dragRef = useRef<{
    startX: number;
    startY: number;
    originX: number;
    originY: number;
  } | null>(null);
  const userAdjustedRef = useRef(false);

  /** 适配:整棵树缩放到容器内并居中。 */
  const fit = useCallback(() => {
    const el = containerRef.current;
    if (!el) return;
    const cw = el.clientWidth;
    const ch = el.clientHeight;
    if (cw <= 0 || ch <= 0 || layout.width <= 0 || layout.height <= 0) return;
    const scale = Math.min(cw / layout.width, ch / layout.height, 1);
    setViewport({
      scale,
      x: (cw - layout.width * scale) / 2,
      y: (ch - layout.height * scale) / 2,
    });
    userAdjustedRef.current = false;
  }, [layout.width, layout.height]);

  /** 以指定屏幕坐标 (cx, cy) 为锚点缩放。 */
  const zoomAt = useCallback((cx: number, cy: number, factor: number) => {
    setViewport((prev) => {
      const scale = Math.min(MAX_SCALE, Math.max(MIN_SCALE, prev.scale * factor));
      const k = scale / prev.scale;
      return {
        scale,
        x: cx - (cx - prev.x) * k,
        y: cy - (cy - prev.y) * k,
      };
    });
    userAdjustedRef.current = true;
  }, []);

  const fitRef = useRef(fit);
  fitRef.current = fit;

  // 挂载后首次适配(仅一次,不随 layout 变化重跑)
  useEffect(() => {
    fitRef.current();
  }, []);

  // 容器尺寸变化:仅当用户未手动调整时跟随适配
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      if (!userAdjustedRef.current) fitRef.current();
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // 首次出现节点时适配(空→非空迁移,不随后续节点增减重跑)
  const renderedOnceRef = useRef(false);
  useEffect(() => {
    if (nodes.length === 0 || renderedOnceRef.current) return;
    renderedOnceRef.current = true;
    const raf = requestAnimationFrame(() => fitRef.current());
    return () => cancelAnimationFrame(raf);
  }, [nodes.length]);

  // 滚轮缩放(原生非 passive 监听以 preventDefault)
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const rect = el.getBoundingClientRect();
      const cx = e.clientX - rect.left;
      const cy = e.clientY - rect.top;
      zoomAt(cx, cy, e.deltaY < 0 ? 1.1 : 1 / 1.1);
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [zoomAt]);

  const onPointerDown = useCallback(
    (e: React.PointerEvent) => {
      if (e.button !== 0) return;
      if ((e.target as HTMLElement).closest("[data-node]")) return;
      dragRef.current = {
        startX: e.clientX,
        startY: e.clientY,
        originX: viewport.x,
        originY: viewport.y,
      };
      (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
      userAdjustedRef.current = true;
    },
    [viewport.x, viewport.y]
  );

  const onPointerMove = useCallback((e: React.PointerEvent) => {
    const d = dragRef.current;
    if (!d) return;
    setViewport((prev) => ({
      ...prev,
      x: d.originX + (e.clientX - d.startX),
      y: d.originY + (e.clientY - d.startY),
    }));
  }, []);

  const onPointerUp = useCallback(() => {
    dragRef.current = null;
  }, []);

  return (
    <div className="relative h-full w-full">
      <div
        ref={containerRef}
        className="absolute inset-0 cursor-grab select-none overflow-hidden active:cursor-grabbing"
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        onDoubleClick={(e) => {
          if (!(e.target as HTMLElement).closest("[data-node]")) fit();
        }}
      >
        {nodes.length === 0 ? (
          <div className="flex h-full w-full items-center justify-center">
            <span className="text-glow text-sm tracking-widest text-slate-400">
              暂无任务
            </span>
          </div>
        ) : (
          <div
            className="absolute left-0 top-0 origin-top-left"
            style={{
              transform: `translate(${viewport.x}px, ${viewport.y}px) scale(${viewport.scale})`,
            }}
          >
            <div className="relative" style={{ width: layout.width, height: layout.height }}>
              <div
                className="absolute inset-0 z-0"
                style={{
                  backgroundImage:
                    "linear-gradient(rgba(148, 163, 184, 0.05) 1px, transparent 1px), linear-gradient(90deg, rgba(148, 163, 184, 0.05) 1px, transparent 1px)",
                  backgroundSize: "48px 48px",
                }}
              />
              <svg
                className="absolute left-0 top-0 z-0"
                width={layout.width}
                height={layout.height}
              >
                {edges.map((edge) => {
                  const source = layout.positions.get(edge.source);
                  const target = layout.positions.get(edge.target);
                  if (!source || !target) {
                    return null;
                  }
                  const inSelectedSubtree =
                    selectedTaskId !== null &&
                    isDescendantOf(edge.source, selectedTaskId, parentMap) &&
                    isDescendantOf(edge.target, selectedTaskId, parentMap);
                  return (
                    <line
                      key={`${edge.source}-${edge.target}`}
                      x1={source.x}
                      y1={source.y}
                      x2={target.x}
                      y2={target.y}
                      stroke={STATUS_COLOR[edge.status]}
                      strokeOpacity={inSelectedSubtree ? 0.95 : 0.45}
                      strokeWidth={inSelectedSubtree ? 2.5 : 1.5}
                      strokeLinecap="round"
                    />
                  );
                })}
              </svg>
              <div className="absolute left-0 top-0 z-10">
                {nodes.map((node) => {
                  const pos = layout.positions.get(node.taskId);
                  if (!pos) {
                    return null;
                  }
                  return (
                    <div
                      key={node.taskId}
                      data-node="true"
                      className="absolute"
                      style={{
                        left: pos.x - NODE_DIAMETER / 2,
                        top: pos.y - NODE_DIAMETER / 2,
                      }}
                    >
                      <SpindleNodeView
                        node={node}
                        selected={node.taskId === selectedTaskId}
                        onSelect={onSelectNode}
                      />
                    </div>
                  );
                })}
              </div>
            </div>
          </div>
        )}
      </div>

      {/* 缩放/适配工具栏 */}
      {nodes.length > 0 && (
        <div className="absolute bottom-4 left-1/2 z-20 flex -translate-x-1/2 items-center gap-1 rounded-lg border border-slate-800 bg-slate-900/90 p-1 shadow-lg backdrop-blur-sm">
          <button
            onClick={() => {
              const el = containerRef.current;
              if (!el) return;
              zoomAt(el.clientWidth / 2, el.clientHeight / 2, 1.25);
            }}
            className="rounded px-2.5 py-1 text-sm text-slate-300 transition-colors duration-300 hover:bg-slate-800 hover:text-yang"
            title="放大"
          >
            +
          </button>
          <button
            onClick={() => {
              const el = containerRef.current;
              if (!el) return;
              zoomAt(el.clientWidth / 2, el.clientHeight / 2, 1 / 1.25);
            }}
            className="rounded px-2.5 py-1 text-sm text-slate-300 transition-colors duration-300 hover:bg-slate-800 hover:text-yang"
            title="缩小"
          >
            −
          </button>
          <span className="mx-1 h-4 w-px bg-slate-700" />
          <button
            onClick={fit}
            className="rounded px-2.5 py-1 text-xs text-slate-300 transition-colors duration-300 hover:bg-slate-800 hover:text-yang"
            title="适配视图(双击空白处亦可)"
          >
            适配
          </button>
        </div>
      )}
    </div>
  );
}
