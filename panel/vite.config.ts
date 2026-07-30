import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// During `npm run dev`, API calls go to a local axioma started with
// dev/axioma.toml, addressed as the panel hostname.
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:4100',
        headers: { host: 'panel.localhost' },
      },
    },
  },
})
