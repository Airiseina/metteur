import { test, expect, type Page } from '@playwright/test'

/**
 * Shell-level regressions: the composer's controls, the transcript's scroll
 * affordances and the app-wide tooltip.
 */

async function openChat(page: Page): Promise<void> {
  await page.goto('/')
  await page.getByLabel('Workspace path').fill('D:/metteur-demo/metrics')
  await page.getByRole('button', { name: 'Open', exact: true }).click()
  await expect(page.getByTestId('file-tree')).toBeVisible({ timeout: 15_000 })
  await page.getByRole('button', { name: 'Chat', exact: true }).first().click()
  await expect(page.getByRole('log', { name: 'Conversation' })).toBeVisible({ timeout: 15_000 })
}

test('the composer has one send control, not two', async ({ page }) => {
  await openChat(page)
  const controls = page.locator('.chat-deliver-group')
  await expect(controls).toHaveCount(1)
  // One primary action plus its alternatives: a single send-lookalike, no more.
  await expect(controls.getByRole('button', { name: 'Send' })).toHaveCount(1)
  await expect(controls.getByRole('button', { name: 'Delivery options' })).toHaveCount(1)
})

test('the tooltip closes when the pointer leaves', async ({ page }) => {
  await openChat(page)
  // An icon-only control: its accessible label becomes the tooltip.
  await page.getByRole('button', { name: 'Add context' }).hover()
  const tooltip = page.locator('.app-tooltip')
  await expect(tooltip).toBeVisible({ timeout: 3_000 })
  await expect(tooltip).toHaveText('Add context')

  // Move the pointer somewhere without a tooltip: the bubble must go away.
  await page.mouse.move(10, 10)
  await expect(tooltip).toBeHidden({ timeout: 3_000 })
})

test('a short conversation shows no jump button', async ({ page }) => {
  await openChat(page)
  const input = page.getByRole('textbox', { name: 'Message' })
  await input.fill('Summarise the metrics workspace')
  await input.press('Enter')
  await expect(page.getByRole('button', { name: 'Copy answer' }).first()).toBeVisible({
    timeout: 15_000,
  })
  await expect(page.getByText('New messages')).toHaveCount(0)
})

test('the header keeps session controls and leaves the model to the composer', async ({ page }) => {
  await openChat(page)
  const header = page.locator('.chat-header')
  await expect(header.getByRole('button', { name: /Conversations/ })).toBeVisible()
  await expect(header.getByRole('button', { name: 'Plugins' })).toBeVisible()
  // The model, its effort and the permission mode live in the composer row only.
  await expect(header.getByRole('button', { name: /Model and reasoning effort/ })).toHaveCount(0)
  await expect(header.getByRole('button', { name: /Permission mode/ })).toHaveCount(0)
})
