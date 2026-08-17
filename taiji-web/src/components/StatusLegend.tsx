import type { NodeStatus } from "../types";

const STATUS_LEGEND: Array<{ status: NodeStatus; label: string }> = [
  { status: "Pending", label: "待执行" },
  { status: "Running", label: "运行中" },
  { status: "Converged", label: "已收敛" },
  { status: "Diverged", label: "发散" },
  { status: "Failed", label: "失败" },
  { status: "Cancelled", label: "已取消" },
  { status: "AwaitingHumanReview", label: "待审批" },
];

const STATUS_DOT: Record<NodeStatus, string> = {
  Pending: "bg-node-pending",
  Running: "bg-node-running",
  Converged: "bg-node-converged",
  Diverged: "bg-node-diverged",
  Failed: "bg-node-failed",
  Cancelled: "bg-node-cancelled",
  AwaitingHumanReview: "bg-node-review",
};

/** 各状态节点计数(键为 status)。 */
export interface StatusCounts {
  pending: number;
  running: number;
  converged: number;
  diverged: number;
  failed: number;
  cancelled: number;
  review: number;
}

const COUNT_KEY: Record<NodeStatus, keyof StatusCounts> = {
  Pending: "pending",
  Running: "running",
  Converged: "converged",
  Diverged: "diverged",
  Failed: "failed",
  Cancelled: "cancelled",
  AwaitingHumanReview: "review",
};

export function countStatuses(nodes: Array<{ status: NodeStatus }>): StatusCounts {
  const counts: StatusCounts = {
    pending: 0,
    running: 0,
    converged: 0,
    diverged: 0,
    failed: 0,
    cancelled: 0,
    review: 0,
  };
  for (const n of nodes) {
    counts[COUNT_KEY[n.status]] += 1;
  }
  return counts;
}

/**
 * 底部状态图例 + 统计:颜色含义 + 各状态节点数。
 */
export default function StatusLegend({ counts }: { counts: StatusCounts }) {
  return (
    <div className="pointer-events-auto rounded-lg border border-slate-800 bg-slate-900/85 px-3 py-2 shadow-lg backdrop-blur-sm">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
        {STATUS_LEGEND.map(({ status, label }) => {
          const count = counts[COUNT_KEY[status]];
          return (
            <span
              key={status}
              className="flex items-center gap-1 text-[11px] text-slate-400"
              title={`${label}:${count}`}
            >
              <span className={`h-2 w-2 rounded-full ${STATUS_DOT[status]}`} />
              {label}
              <span className="font-mono text-slate-500">{count}</span>
            </span>
          );
        })}
      </div>
    </div>
  );
}
