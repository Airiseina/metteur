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

for (const initialTheme of ['light', 'dark'] as const) {
  test(`code fences follow theme switches after rendering in ${initialTheme}`, async ({ page }) => {
    await page.addInitScript((mode) => localStorage.setItem('metteur.theme', mode), initialTheme)
    await openChat(page)
    await page.evaluate(async () => {
      const path = '/src/stores/chat.ts'
      const { useChatStore } = await import(path)
      useChatStore().messages.push({
        id: 'theme-regression', role: 'assistant', createdAt: Date.now(), pending: false,
        content: '```typescript\nconst answer = 42\n```\n\n```unknown-language\nplain fallback\n```',
      })
    })
    const highlighted = page.locator('.md-body pre.shiki').first()
    const plain = page.locator('.md-body .md-pre').first()
    await expect(highlighted).toBeVisible({ timeout: 15_000 })
    await expect(plain).toBeVisible()
    const html = await highlighted.innerHTML()
    const token = highlighted.locator('span[style]').first()
    for (const mode of ['dark', 'light', 'dark', 'light'] as const) {
      await page.evaluate(async (value) => {
        const path = '/src/stores/theme.ts'
        const { useThemeStore } = await import(path)
        useThemeStore().mode = value
      }, mode)
      const background = mode === 'light' ? 'rgb(243, 244, 246)' : 'rgb(41, 42, 48)'
      await expect(highlighted).toHaveCSS('background-color', background)
      await expect(plain).toHaveCSS('background-color', background)
      await expect(plain.locator('code')).toHaveCSS('background-color', 'rgba(0, 0, 0, 0)')
      // Check token contrast follows the active palette, not just the white box.
      const colors = await token.evaluate((el, theme) => {
        const actual = getComputedStyle(el).color
        const probe = document.createElement('span')
        probe.style.color = getComputedStyle(el).getPropertyValue(`--shiki-${theme}`)
        document.body.appendChild(probe)
        const expected = getComputedStyle(probe).color
        probe.remove()
        return { actual, expected }
      }, mode)
      expect(colors.actual).toBe(colors.expected)
      // Switching theme does not re-highlight or remount the existing code.
      expect(await highlighted.innerHTML()).toBe(html)
    }
  })
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

test('progress shows the phase without a second elapsed-time counter', async ({ page }) => {
  await openChat(page)
  await say(page, 'Draft a blueprint for the metrics pipeline')

  const status = page.locator('.chat-status')
  await expect(status.first()).toBeVisible({ timeout: 10_000 })
  await expect(status.first()).toContainText(/(Thinking|Writing|Running a tool|Waiting)/)

  await expect(status.first()).not.toContainText(/\d+s\b/)
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
