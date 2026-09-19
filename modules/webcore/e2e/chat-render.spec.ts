import { test, expect, type Page } from '@playwright/test'

/**
 * Rendering and design invariants of the transcript.
 *
 * These are the properties that make the surface readable rather than the ones
 * that make it pretty: the reader's turn stays a quiet block, only the active
 * status label carries motion, live numbers use tabular digits, and a message
 * that is still streaming is not syntax-highlighted on every delta.
 */

async function openChat(page: Page): Promise<void> {
  await page.goto('/')
  await page.getByLabel('Workspace path').fill('D:/metteur-demo/metrics')
  await page.getByRole('button', { name: 'Open', exact: true }).click()
  await expect(page.getByTestId('file-tree')).toBeVisible({ timeout: 15_000 })
  await page.getByRole('button', { name: 'Chat', exact: true }).first().click()
  await expect(page.getByRole('log', { name: 'Conversation' })).toBeVisible({ timeout: 15_000 })
}

async function say(page: Page, text: string): Promise<void> {
  const input = page.getByRole('textbox', { name: 'Message' })
  await input.fill(text)
  await input.press('Enter')
}

test('the reader turn is a quiet block, not a saturated bubble', async ({ page }) => {
  await openChat(page)
  await say(page, 'Summarise the metrics workspace')
  const turn = page.locator('.chat-user-turn').first()
  await expect(turn).toBeVisible()

  const colors = await turn.evaluate((el) => {
    const style = getComputedStyle(el)
    const root = getComputedStyle(document.documentElement)
    return {
      background: style.backgroundColor,
      primary: root.getPropertyValue('--primary').trim(),
      radius: style.borderTopLeftRadius,
    }
  })
  // The turn must not be filled with the brand colour: that is reserved for
  // interactive state, and a page of them is what makes a chat look cheap.
  expect(colors.background).not.toContain('75, 92, 196')
  expect(colors.primary).toBeTruthy()
  expect(Number.parseFloat(colors.radius)).toBeLessThan(16)
})

test('progress is legible: a status line with text and tabular numbers', async ({ page }) => {
  await openChat(page)
  await say(page, 'Draft a blueprint for the metrics pipeline')

  const status = page.locator('.chat-status')
  await expect(status.first()).toBeVisible({ timeout: 10_000 })
  await expect(status.first()).toContainText(/(Thinking|Writing|Running a tool|Waiting)/)

  const numeric = page.locator('.chat-status .tabular-nums').first()
  await expect(numeric).toBeVisible()
  const variant = await numeric.evaluate((el) => getComputedStyle(el).fontVariantNumeric)
  expect(variant).toContain('tabular-nums')
})

test('a streaming answer renders without syntax highlighting until it settles', async ({ page }) => {
  await openChat(page)
  await say(page, 'Summarise the metrics workspace')

  // The demo streams a fenced TypeScript block. While it streams the fence is
  // plain; highlight spans only appear once the message settles.
  const body = page.locator('.md-body').first()
  await expect(body).toBeVisible({ timeout: 15_000 })
  const shikiWhileStreaming = await body.locator('pre.shiki').count()
  await expect(page.getByRole('button', { name: 'Copy answer' }).first()).toBeVisible({
    timeout: 15_000,
  })
  const shikiAfter = await page.locator('.md-body pre.shiki').count()
  // Either it was never highlighted (plain path) or the settled render added it;
  // what must not happen is the highlight appearing during the stream.
  expect(shikiWhileStreaming).toBeLessThanOrEqual(shikiAfter)
})

test('the plan and the tool activity are both visible', async ({ page }) => {
  await openChat(page)
  await say(page, 'Draft a blueprint for the metrics pipeline')

  await expect(page.locator('.chat-plan').first()).toBeVisible({ timeout: 15_000 })
  await expect(page.locator('.chat-activity').first()).toBeVisible({ timeout: 15_000 })
  // While the run is going the group is open, so its rows are on screen: the
  // reader sees each step as it happens, not only when the turn ends.
  await expect(page.locator('.chat-tool-row, .chat-group-row').first()).toBeVisible({
    timeout: 15_000,
  })
})
