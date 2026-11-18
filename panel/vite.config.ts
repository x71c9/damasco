import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// During `npm run dev`, API calls go to a local alogeno started with
// dev/alogeno.toml, addressed as the panel hostname.
export default defineConfig({
  plugins: [react()],
  build: {
    rolldownOptions: {
      output: {
        // The text editor loads only with the page editor; its libraries go
        // in chunks of their own, each a size the browser handles well.
        codeSplitting: {
          groups: [
            { name: 'prosemirror', test: /node_modules[\\/](prosemirror-|@tiptap|orderedmap|rope-sequence|w3c-keyname)/ },
            { name: 'mantine', test: /node_modules[\\/]@mantine/ },
            // Markdown of the chat's answers: a chunk of its own keeps the
            // panel's under the size browsers handle well.
            {
              name: 'markdown',
              test: /node_modules[\\/](react-markdown|remark-|rehype-|micromark|mdast-|hast-|unist-|unified|vfile|property-information|decode-named-character-reference|character-entities|space-separated-tokens|comma-separated-tokens|html-url-attributes|zwitch|longest-streak|markdown-table|ccount|devlop|bail|is-plain-obj|trough|estree-util|style-to-|inline-style-parser|trim-lines|escape-string-regexp)/,
            },
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
