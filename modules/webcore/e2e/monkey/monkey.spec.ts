import { test, expect, type Page } from '@playwright/test'

/**
 * Randomized "monkey" sweep over the whole SPA.
 *
 * Plays a bounded sequence of arbitrary steps (clicks, typing, keyboard,
 * scrolling, reloads) against whatever is rendered, while recording any page
 * / console errors. The test fails if an uncaught error or a console error is
 * produced — the sign of a crash the deterministic suites would not reach.
 */
test('randomized interaction sweep does not crash the app', async ({ page }) => {
  const errors: string[] = []
  page.on('pageerror', (err) => errors.push(`pageerror: ${err.message}`))
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(`console: ${msg.text()}`)
  })

  await page.goto('/')

  const steps = 80
  for (let i = 0; i < steps; i++) {
    // Re-anchor to the root every so often so the sweep does not drift out of app.
    if (i > 0 && i % 15 === 0) await page.goto('/')
    await randomStep(page)
  }

  expect(errors).toEqual([])
})

async function randomStep(page: Page): Promise<void> {
  const interactives = page.locator(
    'a[href], button, [role="button"], input[type="text"], input[type="number"], textarea, select, [tabindex]:not([tabindex="-1"])',
  )
  const count = await interactives.count()

  if (count === 0) {
    await page.mouse.wheel(0, 400)
    await page.waitForTimeout(100)
    return
  }

  const el = interactives.nth(Math.floor(Math.random() * count))
  const roll = Math.random()

  try {
    if (roll < 0.6) {
      await el.click({ timeout: 1_500 })
    } else if (roll < 0.75) {
      await page.keyboard.press('Tab')
    } else if (roll < 0.85) {
      const tag = await el.evaluate((n) => n.tagName)
      if (tag === 'INPUT' || tag === 'TEXTAREA') {
        await el.fill('metteur', { timeout: 1_500 })
      } else {
        await page.mouse.wheel(0, Math.floor(Math.random() * 600) - 300)
      }
    } else {
      await page.mouse.wheel(0, Math.floor(Math.random() * 600) - 300)
    }
  } catch {
    // Element disappeared mid-action (navigation / re-render); that is allowed.
  }

  // Let the app settle between steps so Vue has time to react.
  await page.waitForTimeout(150)
}
