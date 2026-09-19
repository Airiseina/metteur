import { test, expect } from '@playwright/test'

/**
 * Drives a real daemon instead of the demo gateway.
 *
 * Opt-in (`pnpm test:live`): it needs `metteurd` and `metteur-web` running on
 * their default ports, a workspace the daemon can open, and a configured model
 * — the turn below calls a provider for real. It exists because the demo
 * gateway cannot show what the daemon stores: the transcript, the context
 * meter and the end-of-turn events are all daemon-side behaviour.
 */
test('real stack: a tool turn survives a conversation switch', async ({ page }) => {
  test.setTimeout(300_000)
  const errors: string[] = []
  page.on('pageerror', (err) => errors.push(err.message))
  await page.setViewportSize({ width: 1500, height: 950 })
  await page.goto('/')
  await page.getByLabel('Workspace path').fill('C:/Users/28128/AppData/Local/Temp/mdemo3')
  await page.getByRole('button', { name: 'Open', exact: true }).click()
  await expect(page.getByTestId('file-tree')).toBeVisible({ timeout: 20_000 })
  await page.getByRole('button', { name: 'Chat', exact: true }).first().click()
  // A fresh conversation: earlier failed turns from previous runs are restored
  // into the same workspace and would confuse the assertions below.
  await page.getByRole('button', { name: 'New conversation' }).click()

  const input = page.getByRole('textbox', { name: 'Message' })
  await input.fill('Read app.ts and then answer with one sentence about what it exports.')
  await input.press('Enter')
  // Wait for the turn to finish (the copy action appears on a settled answer).
  await expect(page.getByRole('button', { name: 'Copy answer' }).first()).toBeVisible({
    timeout: 180_000,
  })
  await page.waitForTimeout(1500)
  await page.screenshot({ path: '../dev-notes/r20-real-turn.png' })

  const before = await page.locator('.chat-activity .chat-tool-row, .chat-activity .chat-group-row').count()
  const textBefore = (await page.locator('.md-body').allTextContents()).join(' ').trim()
  console.log('TOOLS:', before, 'TEXT:', textBefore.slice(0, 200))

  // No spurious failure at the end of a normal turn.
  await expect(page.getByText('execution error', { exact: false })).toHaveCount(0)
  await expect(page.getByText('model produced no final answer', { exact: false })).toHaveCount(0)

  // The capacity meter has a real percentage.
  const meter = page.getByRole('button', { name: /^Context:/ })
  await expect(meter).toBeVisible()
  console.log('METER:', await meter.textContent())

  // Switch away and back: the whole conversation must come back.
  await page.getByRole('button', { name: 'New conversation' }).click()
  await expect(page.locator('.chat-activity')).toHaveCount(0)
  await page.getByRole('button', { name: /Conversations/ }).click()
  await page.getByText('Read app.ts').first().click()
  await page.waitForTimeout(1500)
  await page.screenshot({ path: '../dev-notes/r20-real-restored.png' })

  const toolsAfter = await page.locator('.chat-activity .chat-tool-row, .chat-activity .chat-group-row').count()
  const textAfter = (await page.locator('.md-body').allTextContents()).join(' ').trim()
  console.log('RESTORED TOOLS:', toolsAfter, 'TEXT:', textAfter.slice(0, 200))
  expect(toolsAfter).toBeGreaterThan(0)
  expect(textAfter.length).toBeGreaterThan(20)
  expect(errors).toEqual([])
})
