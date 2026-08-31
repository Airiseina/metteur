import { defineConfig, devices } from '@playwright/test'

/**
 * Playwright configuration for Metteur Web Core.
 *
 * Two projects share the same Vite dev server:
 * - `e2e`    : deterministic end-to-end flows located in `e2e/`.
 * - `monkey` : randomized interaction sweep located in `e2e/monkey/`.
 */
export default defineConfig({
  testDir: './e2e',
  // Fresh state per test; no cross-test pollution.
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  reporter: [['list']],
  use: {
    baseURL: 'http://localhost:5173',
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
  },
  projects: [
    {
      name: 'e2e',
      testMatch: '**/*.spec.ts',
      testIgnore: '**/monkey/**',
      use: { ...devices['Desktop Chrome'] },
    },
    {
      name: 'monkey',
      testMatch: '**/monkey/**',
      timeout: 180_000,
      use: { ...devices['Desktop Chrome'] },
    },
  ],
  webServer: {
    command: 'pnpm dev',
    url: 'http://localhost:5173',
    reuseExistingServer: !process.env.CI,
    timeout: 60_000,
  },
})
