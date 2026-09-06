import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    // Proxy API calls to the engine during development so the UI can use same-origin
    // relative paths (/health, /plugins, /ecu, /simulation, /hw, /doip, /events) and CORS is a
    // non-issue. A path missing from this list is not an error — it falls through to the SPA
    // and returns index.html, which the caller then fails to parse as JSON.
    proxy: {
      '/health': 'http://127.0.0.1:8080',
      '/plugins': 'http://127.0.0.1:8080',
      '/ecu': 'http://127.0.0.1:8080',
      '/simulation': 'http://127.0.0.1:8080',
      '/hw': 'http://127.0.0.1:8080',
      '/doip': 'http://127.0.0.1:8080',
      // The live traffic feed. It must not be buffered or compressed: an SSE stream that is
      // held back until some buffer fills is indistinguishable from an engine with nothing to
      // say, which is exactly the confusion the monitor exists to end.
      '/events': {
        target: 'http://127.0.0.1:8080',
        changeOrigin: true,
        headers: { 'Accept-Encoding': 'identity' },
      },
    },
  },
})
