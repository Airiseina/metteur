import { defineConfig, devices } from '@playwright/test'

/**
 * Playwright configuration for Metteur Web Core.
 *
 * Three projects share the same Vite dev server:
 * - `e2e`    : deterministic end-to-end flows located in `e2e/` (demo gateway).
 * - `monkey` : randomized interaction sweep located in `e2e/monkey/`.
 * - `live`   : flows against a real daemon (`e2e/live/`), run explicitly with
 *              `pnpm test:live` — they need the daemon, the web server and a
 *              configured model.
 */

/**
 * Dev-server port.
 *
 * `reuseExistingServer` trusts whatever answers on the port, so a foreign
 * server there would silently run every test against the wrong app. The
 * default stays off Vite's own 5173 for exactly that reason — another project
 * on this machine legitimately holds it — and `E2E_PORT` moves the run without
 * touching anyone else's process.
 */
const port = Number(process.env.E2E_PORT ?? 5317)
const baseURL = `http://localhost:${port}`

export default defineConfig({
  testDir: './e2e',
  // Fresh state per test; no cross-test pollution.
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  reporter: [['list']],
  use: {
    baseURL,
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
  },
  projects: [
    {
      name: 'e2e',
      testMatch: '**/*.spec.ts',
      testIgnore: ['**/monkey/**', '**/live/**'],
      // The full Chromium build is what @playwright/browser-chromium installs;
      // headless runs use it too instead of the separate headless shell.
      use: { ...devices['Desktop Chrome'], channel: 'chromium' },
    },
    {
      name: 'live',
      testMatch: '**/live/**',
      timeout: 300_000,
      // The daemon is already running for these; reuse it instead of starting
      // a mock dev server.
      use: { ...devices['Desktop Chrome'], channel: 'chromium', baseURL },
    },
    {
      name: 'monkey',
      testMatch: '**/monkey/**',
      timeout: 180_000,
      // The full Chromium build is what @playwright/browser-chromium installs;
      // headless runs use it too instead of the separate headless shell.
      use: { ...devices['Desktop Chrome'], channel: 'chromium' },
    },
  ],
  webServer: {
    command: `pnpm dev --port ${port}`,
    url: baseURL,
    reuseExistingServer: !process.env.CI,
    timeout: 60_000,
    // Every suite runs against the deterministic demo gateway: the daemon is
    // not assumed to be running, and the demo workspace is what makes editor,
    // chat and explorer flows reachable at all.
    env: { VITE_MOCK: '1' },
  },
})
