import { defineConfig } from 'vite'
import react, { reactCompilerPreset } from '@vitejs/plugin-react'
import babel from '@rolldown/plugin-babel'
import tailwindcss from '@tailwindcss/vite'

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), babel({ presets: [reactCompilerPreset()] }), tailwindcss()],
  server: {
    proxy: {
      // dev 同源代理到 fluxdown_server，规避 CORS（生产由 ServeDir 同源托管）。
      '/api': {
        target: 'http://localhost:18080',
        changeOrigin: true,
        ws: true,
      },
      '/ping': {
        target: 'http://localhost:18080',
        changeOrigin: true,
      },
    },
  },
})
