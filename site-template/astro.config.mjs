// Static output only: the site is a folder of files, served by alogeno.
import { defineConfig } from 'astro/config'

export default defineConfig({
  output: 'static',
  build: { format: 'directory' },
})
