import { fileURLToPath, URL } from 'node:url'

import vue from '@vitejs/plugin-vue'
import tailwindcss from '@tailwindcss/vite'
import { defineConfig } from 'vite'
import { VitePWA } from 'vite-plugin-pwa'

export default defineConfig({
  plugins: [
    vue(),
    tailwindcss(),
    VitePWA({
      registerType: 'autoUpdate',
      manifest: {
        name: 'Metteur',
        short_name: 'Metteur',
        start_url: '/',
        display: 'standalone',
        background_color: '#0f172a',
        theme_color: '#0f172a',
        icons: [{ src: 'favicon.svg', sizes: '192x192', type: 'image/svg+xml' }],
      },
      // Monaco editor bundles exceed the 2 MiB workbox default.
      workbox: { maximumFileSizeToCacheInBytes: 10 * 1024 * 1024 },
    }),
  ],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  build: {
    target: 'es2022',
  },
  server: {
    // In dev the grpc-web traffic is proxied to the Web Server Client, while
    // the app itself is served by Vite (set VITE_MOCK=1 to use demo data).
    proxy: {
      '/metteur.Daemon': {
        target: 'http://127.0.0.1:8787',
        changeOrigin: true,
      },
    },
  },
})
