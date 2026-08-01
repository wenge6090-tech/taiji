interface TaijiBgProps {
  active: boolean;
}

/** 全屏太极背景:阴阳 SVG 匀速旋转 + 金色光晕随 active 呼吸。 */
export default function TaijiBg({ active }: TaijiBgProps) {
  return (
    <div className="taiji-bg flex items-center justify-center">
      <div className="relative flex items-center justify-center">
        <div
          className="absolute rounded-full"
          style={{
            width: "80vmin",
            height: "80vmin",
            boxShadow: "0 0 140px 60px rgba(251, 191, 36, 0.28)",
            opacity: active ? 0.35 : 0.08,
            transition: "opacity 300ms ease",
          }}
        />
        <svg
          className="taiji-spin relative"
          viewBox="0 0 400 400"
          style={{ width: "90vmin", height: "90vmin", maxWidth: "90vmin" }}
        >
          <defs>
            <linearGradient id="yang-grad" x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor="#fbbf24" stopOpacity="0.35" />
              <stop offset="100%" stopColor="#fbbf24" stopOpacity="0.12" />
            </linearGradient>
          </defs>
          <circle cx="200" cy="200" r="180" fill="#1e293b" />
          <path
            d="M 200 20
               A 180 180 0 0 1 200 380
               A 90 90 0 0 0 200 200
               A 90 90 0 0 1 200 20
               Z"
            fill="url(#yang-grad)"
          />
          <circle cx="200" cy="110" r="24" fill="#1e293b" />
          <circle cx="200" cy="290" r="24" fill="#fbbf24" />
          <circle
            cx="200"
            cy="200"
            r="180"
            fill="none"
            stroke="#fbbf24"
            strokeWidth="2.5"
            strokeOpacity="0.65"
          />
          <circle
            cx="200"
            cy="200"
            r="192"
            fill="none"
            stroke="#fbbf24"
            strokeWidth="1"
            strokeOpacity="0.15"
          />
        </svg>
      </div>
    </div>
  );
}
