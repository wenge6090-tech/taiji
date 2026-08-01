/** @type {import('tailwindcss').Config} */
export default {
  content: ['./index.html', './src/**/*.{ts,tsx}'],
  theme: {
    extend: {
      colors: {
        // 节点状态色(验收标准):Pending 黄 / Converged 绿 / Diverged·Failed 红
        'node-pending': '#facc15',
        'node-running': '#fbbf24',
        'node-converged': '#4ade80',
        'node-diverged': '#f87171',
        'node-failed': '#f87171',
        'node-cancelled': '#94a3b8',
        'node-review': '#fb923c',
        // 太极背景
        yang: '#fbbf24',
        yin: '#1e293b',
        'bg-deep': '#020617',
      },
      animation: {
        // 太极 60s 一圈
        'spin-slow': 'spin 60s linear infinite',
        'pulse-glow': 'pulse-glow 1.6s ease-in-out infinite',
      },
      keyframes: {
        'pulse-glow': {
          '0%, 100%': { boxShadow: '0 0 6px 2px rgba(250, 204, 21, 0.35)' },
          '50%': { boxShadow: '0 0 18px 6px rgba(250, 204, 21, 0.65)' },
        },
      },
    },
  },
  plugins: [],
}
