import { defineConfig } from 'vite'

export default defineConfig({
  root: '.',
  css: {
    postcss: { plugins: [] },
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
})
