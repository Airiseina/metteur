import { test, expect, type Page } from '@playwright/test'

/**
 * Chat surface: sending a turn, watching it stream, steering it while it runs.
 *
 * Runs against the demo gateway, which scripts both text and tool steps, so
 * every state the transcript renders is reachable without a daemon.
 */

/** Opens the demo workspace and the chat surface. */
async function openChat(page: Page): Promise<void> {
  await page.goto('/')
  await page.getByLabel('Workspace path').fill('D:/metteur-demo/metrics')
  await page.getByRole('button', { name: 'Open', exact: true }).click()
  // Wait for the workspace shell before using the activity rail.
  await expect(page.getByTestId('file-tree')).toBeVisible({ timeout: 15_000 })
  await page.getByRole('button', { name: 'Chat', exact: true }).first().click()
  await expect(page.getByRole('log', { name: 'Conversation' })).toBeVisible({ timeout: 15_000 })
}

/** Types a message and sends it with Enter. */
async function say(page: Page, text: string): Promise<void> {
  const input = page.getByRole('textbox', { name: 'Message' })
  await input.fill(text)
  await input.press('Enter')
}

test('a turn streams an answer and settles', async ({ page }) => {
  await openChat(page)
  await say(page, 'Summarise the metrics workspace')

  // The user's own turn is echoed immediately.
  await expect(page.getByText('Summarise the metrics workspace').first()).toBeVisible()

  // The answer arrives as markdown and the thinking block reports itself.
  await expect(page.getByText(/Got it|I'll wire up a blueprint/).first()).toBeVisible({ timeout: 15_000 })
  await expect(page.getByRole('log').getByText(/Thinking|Thought for/).first()).toBeVisible()

  // Copy affordances appear once the turn settles.
  await expect(page.getByRole('button', { name: 'Copy answer' }).first()).toBeVisible()
})

test('a tool call is visible while it runs and settles with a duration', async ({ page }) => {
  await openChat(page)
  // The demo scripts an EditFile step with a diff when the prompt mentions a
  // blueprint.
  await say(page, 'Draft a blueprint for the metrics pipeline')

  const activity = page.locator('.chat-activity').first()
  await expect(activity).toBeVisible({ timeout: 15_000 })
  await expect(activity.locator('.chat-tool-row, .chat-group-row').first()).toBeVisible()
  await expect(page.getByText('Edit').first()).toBeVisible()

  // Once the turn settles the group collapses; open it, then the row itself.
  await expect(page.getByRole('button', { name: 'Copy answer' }).first()).toBeVisible({
    timeout: 15_000,
  })
  const diff = page.locator('.diff-add').first()
  const editRow = page.locator('.chat-tool-row').filter({ hasText: 'Edit' }).first()
  if (!(await editRow.isVisible().catch(() => false))) {
    await page.locator('.chat-group-row').first().click()
  }
  await editRow.click()
  await expect(diff).toBeVisible({ timeout: 10_000 })
})

test('the composer stays usable while the agent works', async ({ page }) => {
  await openChat(page)
  await say(page, 'Draft a blueprint for the metrics pipeline')

  const input = page.getByRole('textbox', { name: 'Message' })
  await expect(input).toBeEnabled()
  // Enter during a run queues the message instead of dropping it.
  await input.fill('Also rename the file')
  await input.press('Enter')
  await expect(page.getByText('Also rename the file').first()).toBeVisible()

  // The turn can be stopped from the composer.
  await expect(page.getByRole('button', { name: 'Stop' })).toBeVisible()
})

test('switching away and back keeps the tool calls', async ({ page }) => {
  const pageErrors: string[] = []
  page.on('pageerror', (err) => pageErrors.push(err.message))

  await openChat(page)
  await say(page, 'Draft a blueprint for the metrics pipeline')
  await expect(page.locator('.chat-activity').first()).toBeVisible({ timeout: 15_000 })
  await expect(page.getByRole('button', { name: 'Copy answer' }).first()).toBeVisible({
    timeout: 15_000,
  })
  const toolsBefore = await page.locator('.chat-tool-row, .chat-group-row').count()
  expect(toolsBefore).toBeGreaterThan(0)

  // A new conversation clears the surface, then the stored thread is reopened.
  await page.getByRole('button', { name: 'New conversation' }).click()
  await expect(page.locator('.chat-activity')).toHaveCount(0)
  await page.getByRole('button', { name: /Conversations/ }).click()
  await page.getByText('Draft a blueprint for the metrics pipeline').first().click()

  await expect(page.locator('.chat-tool-row, .chat-group-row').first()).toBeVisible({
    timeout: 10_000,
  })
  expect(await page.locator('.chat-tool-row, .chat-group-row').count()).toBe(toolsBefore)
  expect(pageErrors).toEqual([])
})
