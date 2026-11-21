// Static output only: the site is a folder of files, served by damasco.
import { defineConfig } from 'astro/config'

export default defineConfig({
  output: 'static',
  build: { format: 'directory' },
})
