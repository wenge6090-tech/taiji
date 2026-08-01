const NEBULA_DOTS = [
  { cx: 140, cy: 100, r: 4, opacity: 0.5, fill: "#fbbf24" },
  { cx: 220, cy: 240, r: 7, opacity: 0.3, fill: "#a78bfa" },
  { cx: 330, cy: 160, r: 3, opacity: 0.6, fill: "#22d3ee" },
  { cx: 420, cy: 320, r: 9, opacity: 0.25, fill: "#fbbf24" },
  { cx: 520, cy: 120, r: 5, opacity: 0.4, fill: "#a78bfa" },
  { cx: 600, cy: 260, r: 3, opacity: 0.55, fill: "#22d3ee" },
  { cx: 680, cy: 180, r: 6, opacity: 0.35, fill: "#fbbf24" },
  { cx: 260, cy: 420, r: 8, opacity: 0.2, fill: "#22d3ee" },
  { cx: 520, cy: 440, r: 4, opacity: 0.45, fill: "#a78bfa" },
  { cx: 720, cy: 400, r: 5, opacity: 0.3, fill: "#fbbf24" },
  { cx: 400, cy: 90, r: 3, opacity: 0.5, fill: "#a78bfa" },
  { cx: 90, cy: 360, r: 5, opacity: 0.35, fill: "#22d3ee" },
];

export default function GuizangGraph({
  onClose,
}: {
  onClose?: () => void;
}) {
  return (
    <div className="fixed inset-0 z-40 flex flex-col items-center justify-center gap-6 bg-bg-deep/90 backdrop-blur">
      <svg
        className="pointer-events-none absolute inset-0 h-full w-full"
        viewBox="0 0 800 500"
        preserveAspectRatio="xMidYMid slice"
      >
        {NEBULA_DOTS.map((d, i) => (
          <circle
            key={i}
            cx={d.cx}
            cy={d.cy}
            r={d.r}
            fill={d.fill}
            opacity={d.opacity}
          />
        ))}
      </svg>
      <div className="relative flex flex-col items-center gap-3 px-6 text-center">
        <h2 className="text-2xl font-semibold text-yang">归藏认知库图谱</h2>
        <p className="text-sm text-slate-400">3D 星云图即将上线(MVP 存根)</p>
      </div>
      {onClose && (
        <button
          onClick={onClose}
          className="relative rounded-lg border border-slate-600 bg-slate-900 px-4 py-2 text-sm text-slate-200 transition-colors duration-300 hover:border-yang hover:text-yang"
        >
          返回纺锤视图
        </button>
      )}
    </div>
  );
}
