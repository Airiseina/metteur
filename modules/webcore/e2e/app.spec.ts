import { test, expect } from '@playwright/test'

/**
 * Smoke E2E covering the app scaffold.
 *
 * Kept intentionally light until the feature UI lands; grows together with the
 * Workspace / Blueprint / Execution surfaces.
 */
test('app boots and renders the shell', async ({ page }) => {
  const pageErrors: string[] = []
  page.on('pageerror', (err) => pageErrors.push(err.message))

  await page.goto('/')

  await expect(page.locator('#app')).toBeAttached()

  // Document title comes from <head>; the sidebar brand is the visible shell anchor.
  await expect(page).toHaveTitle(/Metteur/)
  await expect(page.getByText('Metteur', { exact: true })).toBeVisible()

  // No uncaught errors should surface during a plain boot.
  expect(pageErrors).toEqual([])
})
