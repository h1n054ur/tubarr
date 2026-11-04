/// <reference types="vitest/config" />
import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig, loadEnv } from 'vite'

// The build lands in web/dist, which crates/tubarr/build.rs embeds into the binary.
// `bun run dev` proxies the API and sign-in calls to a local Tubarr on port 8480, or TUBARR_PORT.
export default defineConfig(({ mode }) => {
  const tubarr = `http://localhost:${loadEnv(mode, '.', '').TUBARR_PORT || '8480'}`
  return {
    plugins: [react(), tailwindcss()],
    server: {
      proxy: {
        '/admin/api': tubarr,
        '/api/health': tubarr,
        '/auth': tubarr,
      },
    },
    test: {
      environment: 'happy-dom',
      setupFiles: ['./src/test-setup.ts'],
    },
  }
})
