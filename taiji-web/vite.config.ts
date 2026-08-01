import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// 开发模式:端口固定 1420
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    outDir: 'dist',
    target: 'es2021',
    emptyOutDir: true,
  },
})
