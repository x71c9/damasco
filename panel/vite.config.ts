import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// During `npm run dev`, API calls go to a local axioma started with
// dev/axioma.toml, addressed as the panel hostname.
export default defineConfig({
  plugins: [react()],
  build: {
    rolldownOptions: {
      output: {
        // The text editor loads only with the page editor; its libraries go
        // in chunks of their own, each a size the browser handles well.
        advancedChunks: {
          groups: [
            { name: 'prosemirror', test: /node_modules[\\/](prosemirror-|@tiptap|orderedmap|rope-sequence|w3c-keyname)/ },
            { name: 'mantine', test: /node_modules[\\/]@mantine/ },
          ],
        },
      },
    },
  },
  server: {
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:4100',
        headers: { host: 'panel.localhost' },
      },
    },
  },
})
