// Static output only: the site is a folder of files, served by axioma.
import { defineConfig } from 'astro/config'

export default defineConfig({
  output: 'static',
  build: { format: 'directory' },
})
