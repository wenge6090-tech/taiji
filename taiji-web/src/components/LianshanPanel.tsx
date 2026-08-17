import type { LianshanActivity } from "../types";

/** δ 层标签:layer 0-3 → δ₀-δ₃(四算子)。 */
function layerLabel(layer: number): string {
  switch (layer) {
    case 0:
      return "δ₀ 约束";
    case 1:
      return "δ₁ 技能";
    case 2:
      return "δ₂ 贝叶斯";
    case 3:
      return "δ₃ 网格";
    default:
      return `δ${layer}`;
  }
}

/**
 * 连山演化浮层:渲染 TaskTreeSnapshot.lianshanActivity
 * (活跃节点数 + 最近归藏资产演化)。
 */
export default function LianshanPanel({
  activity,
}: {
  activity: LianshanActivity | null;
}) {
  if (!activity) return null;
  const { activeNodes, recentEvolutions } = activity;

  return (
    <div className="pointer-events-auto w-72 rounded-xl border border-slate-800 bg-slate-900/90 shadow-xl backdrop-blur-sm">
      <div className="flex items-center justify-between border-b border-slate-800 px-3 py-2">
        <h3 className="text-xs font-semibold tracking-widest text-yang">
          连山·归藏演化
        </h3>
        <span className="rounded bg-slate-800 px-1.5 py-0.5 font-mono text-[10px] text-slate-300">
          活跃 {activeNodes}
        </span>
      </div>
      {recentEvolutions.length > 0 ? (
        <ul className="max-h-52 space-y-1.5 overflow-y-auto p-2">
          {recentEvolutions.map((ev, i) => (
            <li
              key={`${ev.assetId}-${ev.timestamp}-${i}`}
              className="rounded border border-slate-800 bg-slate-900/60 px-2 py-1.5 text-xs"
            >
              <div className="flex items-center gap-1.5">
                <span className="shrink-0 rounded bg-purple-500/15 px-1 py-px text-[10px] text-purple-300">
                  {layerLabel(ev.layer)}
                </span>
                <span className="truncate font-mono text-slate-300" title={ev.assetId}>
                  {ev.assetId}
                </span>
              </div>
              <p className="mt-1 truncate text-slate-400" title={ev.delta}>
                {ev.delta}
              </p>
            </li>
          ))}
        </ul>
      ) : (
        <p className="px-3 py-3 text-xs text-slate-500">
          暂无演化记录(归藏消费者尚未产出)
        </p>
      )}
    </div>
  );
}
