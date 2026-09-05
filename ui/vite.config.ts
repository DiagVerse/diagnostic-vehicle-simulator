import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    // Proxy API calls to the engine during development so the UI can use same-origin
    // relative paths (/health, /plugins, /ecu, /simulation) and CORS is a non-issue.
    proxy: {
      '/health': 'http://127.0.0.1:8080',
      '/plugins': 'http://127.0.0.1:8080',
      '/ecu': 'http://127.0.0.1:8080',
      '/simulation': 'http://127.0.0.1:8080',
    },
  },
})
