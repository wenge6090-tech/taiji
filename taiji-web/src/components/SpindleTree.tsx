import { useMemo } from "react";
import type { NodeStatus, SpindleEdge, SpindleNode } from "../types";
import SpindleNodeView from "./SpindleNode";

/** 纺锤最大水平散布(半宽)。 */
const MAX_SPREAD = 380;
const NODE_DIAMETER = 44;
const VERTICAL_GAP = 150;
const TOP_OFFSET = 40;
const PADDING = 70;

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

/** 纺锤树:纯 SVG 边 + 绝对定位节点层,无外部布局库。 */
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

  if (nodes.length === 0) {
    return (
      <div className="flex h-full w-full items-center justify-center">
        <span className="text-glow text-sm tracking-widest text-slate-400">暂无任务</span>
      </div>
    );
  }

  return (
    <div className="relative" style={{ width: layout.width, height: layout.height }}>
      <div
        className="absolute inset-0 z-0"
        style={{
          backgroundImage:
            "linear-gradient(rgba(148, 163, 184, 0.05) 1px, transparent 1px), linear-gradient(90deg, rgba(148, 163, 184, 0.05) 1px, transparent 1px)",
          backgroundSize: "48px 48px",
        }}
      />
      <svg className="absolute left-0 top-0 z-0" width={layout.width} height={layout.height}>
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
  );
}
