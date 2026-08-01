import type { NodeStatus, SpindleNode } from "../types";

/** 状态 → 节点底色(验收标准)。 */
const STATUS_COLOR: Record<NodeStatus, string> = {
  Pending: "#facc15",
  Running: "#fbbf24",
  Converged: "#4ade80",
  Diverged: "#f87171",
  Failed: "#f87171",
  Cancelled: "#94a3b8",
  AwaitingHumanReview: "#fb923c",
};

interface SpindleNodeProps {
  node: SpindleNode;
  selected: boolean;
  onSelect: (node: SpindleNode) => void;
}

/** 单个纺锤节点:圆形缩写 + round/cycle 小字 + 状态光晕 + 模式徽标。 */
export default function SpindleNode({ node, selected, onSelect }: SpindleNodeProps) {
  const abbr = node.description.trim().slice(0, 2) || "◦";
  const glowClass =
    node.status === "Running"
      ? "pulse-glow"
      : node.status === "Diverged" || node.status === "Failed"
        ? "pulse-glow-red"
        : node.status === "AwaitingHumanReview"
          ? "pulse-glow-orange"
          : "";

  return (
    <div
      className="relative flex cursor-pointer flex-col items-center transition-all duration-300 hover:scale-110"
      onClick={() => onSelect(node)}
      title={node.description}
    >
      <div
        className={`relative rounded-full transition-all duration-300 ${
          selected ? "ring-2 ring-yang ring-offset-2 ring-offset-bg-deep" : ""
        }`}
      >
        <div
          className={`relative flex h-[44px] w-[44px] items-center justify-center rounded-full ${glowClass}`}
          style={{
            backgroundColor: STATUS_COLOR[node.status],
            border: "1px solid rgba(2, 6, 23, 0.6)",
            color: "#0f172a",
          }}
        >
          <span className="text-sm font-bold tracking-wide">{abbr}</span>
          {node.mode === "Orchestration" && (
            <span className="absolute -right-1 -top-1 h-3 w-3 rounded-full border border-bg-deep bg-yang" />
          )}
        </div>
      </div>
      <span className="mt-1 font-mono text-[10px] leading-none text-slate-400">
        r{node.round} c{node.cycle}
      </span>
    </div>
  );
}
