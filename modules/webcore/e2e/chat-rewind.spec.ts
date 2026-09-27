import { test, expect, type Page } from '@playwright/test'

async function openChat(page: Page) {
  await page.goto('/')
  await page.getByLabel('Workspace path').fill('D:/metteur-demo/metrics')
  await page.getByRole('button', { name: 'Open', exact: true }).click()
  await expect(page.getByTestId('file-tree')).toBeVisible()
  await page.getByRole('button', { name: 'Chat', exact: true }).first().click()
  await expect(page.getByRole('log', { name: 'Conversation' })).toBeVisible()
}

async function say(page: Page, text: string) {
  await page.getByRole('textbox', { name: 'Message' }).fill(text)
  await page.getByRole('textbox', { name: 'Message' }).press('Enter')
  await expect(page.getByRole('button', { name: 'Stop', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Stop', exact: true })).toHaveCount(0)
}

async function requestRestore(page: Page, text: string) {
  const row = page.locator('.chat-user-turn').filter({ hasText: text }).locator('..')
  await row.hover()
  await row.getByRole('button', { name: 'Restore files and conversation to before this turn' }).click()
  await expect(page.getByText('Restore files and conversation?', { exact: true })).toBeVisible()
}

test('rewind removes the selected turn from UI, stored session and next-request history', async ({ page }) => {
  await openChat(page)
  await say(page, 'KEEP_THIS')
  await say(page, 'REMOVE_THIS blueprint')
  await requestRestore(page, 'REMOVE_THIS')
  await page.getByRole('button', { name: 'Restore', exact: true }).click()
  await expect(page.locator('.chat-user-turn').filter({ hasText: 'REMOVE_THIS' })).toHaveCount(0)
  await expect(page.locator('.chat-user-turn').filter({ hasText: 'KEEP_THIS' })).toBeVisible()
  await expect(page.locator('.chat-activity')).toHaveCount(0)
  const restored = await page.evaluate(async () => {
    const storePath = '/src/stores/chat.ts'
    const corePath = '/src/core/index.ts'
    const { useChatStore } = await import(storePath)
    const { gateway } = await import(corePath)
    const store = useChatStore()
    store.messages = []
    await store.restore()
    const send = gateway.sendChat.bind(gateway)
    gateway.sendChat = (...args: unknown[]) => {
      if (JSON.stringify(args[2]).includes('REMOVE_THIS')) throw new Error('rewound history leaked')
      return send(...args)
    }
    return JSON.stringify(store.messages)
  })
  expect(restored).toContain('KEEP_THIS')
  expect(restored).not.toContain('REMOVE_THIS')
  // Checkpoint survives session restoration, not just the live message bubble.
  await expect(page.getByRole('button', { name: 'Restore files and conversation to before this turn' })).toHaveCount(1)
  await say(page, 'FOLLOW_UP')
  await expect(page.getByRole('log')).not.toContainText('rewound history leaked')
})

test('rewinding the first turn clears the conversation and its plan', async ({ page }) => {
  await openChat(page)
  await say(page, 'REMOVE_ALL blueprint')
  await requestRestore(page, 'REMOVE_ALL')
  await page.getByRole('button', { name: 'Restore', exact: true }).click()
  await expect(page.locator('.chat-user-turn')).toHaveCount(0)
  await expect(page.locator('.chat-activity')).toHaveCount(0)
  const state = await page.evaluate(async () => {
    const path = '/src/stores/chat.ts'
    const { useChatStore } = await import(path)
    const chat = useChatStore()
    return { messages: chat.messages, todos: chat.todos, usage: chat.lastUsage, context: chat.contextStats }
  })
  expect(state).toEqual({ messages: [], todos: [], usage: null, context: null })
  await say(page, 'FRESH_TURN')
  await expect(page.locator('.chat-user-turn')).toHaveCount(1)
})

test('a failed or cancelled restore leaves the visible conversation intact', async ({ page }) => {
  await openChat(page)
  await say(page, 'KEEP_ON_FAILURE')
  await requestRestore(page, 'KEEP_ON_FAILURE')
  await page.getByRole('button', { name: 'Cancel', exact: true }).click()
  await expect(page.locator('.chat-user-turn')).toHaveCount(1)
  await page.evaluate(async () => {
    const path = '/src/core/index.ts'
    const { gateway } = await import(path)
    gateway.rewindChat = async () => ({ ok: false, error: 'Checkpoint unavailable' })
  })
  await requestRestore(page, 'KEEP_ON_FAILURE')
  await page.getByRole('button', { name: 'Restore', exact: true }).click()
  await expect(page.getByText('Checkpoint unavailable', { exact: true })).toBeVisible()
  await expect(page.locator('.chat-user-turn')).toHaveCount(1)
})

test('retry rewinds the old answer and later turns, resends automatically and keeps the composer draft', async ({ page }) => {
  await openChat(page)
  await page.evaluate(async () => {
    const corePath = '/src/core/index.ts'
    const { gateway } = await import(corePath)
    const send = gateway.sendChat.bind(gateway)
    let count = 0
    gateway.sendChat = (...args: unknown[]) => {
      count += 1
      const current = count
      const emit = args[3] as (message: Record<string, unknown>) => void
      if (current === 4) {
        if (args[1] !== 'RETRY_ORIGINAL') throw new Error('retry changed the original prompt')
        const options = args[4] as { retry?: boolean } | undefined
        if (!options?.retry) throw new Error('retry was not marked as session-bound')
        const history = JSON.stringify(args[2])
        if (history.includes('ANSWER_2') || history.includes('LATER_TURN') || history.includes('ANSWER_3')) {
          throw new Error('previous answer leaked into retry history')
        }
        if (!history.includes('KEEP_EARLIER')) throw new Error('retry discarded earlier context')
      }
      args[3] = (message: Record<string, unknown>) => {
        if (message.role === 'assistant' && !message.pending) message.content = `ANSWER_${current}`
        emit(message)
      }
      return send(...args)
    }
  })
  await say(page, 'KEEP_EARLIER')
  await say(page, 'RETRY_ORIGINAL')
  await say(page, 'LATER_TURN')
  const input = page.getByRole('textbox', { name: 'Message' })
  await input.fill('UNRELATED_DRAFT')
  await page.evaluate(async () => {
    const path = '/src/stores/chat.ts'
    const { useChatStore } = await import(path)
    useChatStore().queueFile({ path: 'unrelated.txt', name: 'unrelated.txt', kind: 'file' })
  })
  await page.getByRole('button', { name: 'Retry', exact: true }).nth(1).click()
  const dialog = page.getByText('Retry this turn?', { exact: true }).locator('..')
  await expect(dialog).toBeVisible()
  await dialog.getByRole('button', { name: 'Retry', exact: true }).click()
  await expect(page.getByRole('log')).toContainText('ANSWER_4')
  await expect(page.getByRole('log')).not.toContainText('ANSWER_2')
  await expect(page.getByRole('log')).not.toContainText('ANSWER_3')
  await expect(page.getByRole('log')).not.toContainText('LATER_TURN')
  await expect(page.locator('.chat-user-turn')).toHaveCount(2)
  await expect(input).toHaveValue('UNRELATED_DRAFT')
  const stored = await page.evaluate(async () => {
    const path = '/src/stores/chat.ts'
    const { useChatStore } = await import(path)
    const store = useChatStore()
    await store.restore()
    return { messages: JSON.stringify(store.messages), attachments: store.pendingFiles.map((file: { path: string }) => file.path) }
  })
  expect(stored.messages).not.toContain('ANSWER_2')
  expect(stored.messages).not.toContain('LATER_TURN')
  expect(stored.attachments).toEqual(['unrelated.txt'])
})

test('retry does not send anything when restoring the old context fails', async ({ page }) => {
  await openChat(page)
  await say(page, 'ORIGINAL_ON_FAILURE')
  await page.evaluate(async () => {
    const path = '/src/core/index.ts'
    const { gateway } = await import(path)
    gateway.rewindChat = async () => ({ ok: false, error: 'read-only source.txt' })
    gateway.sendChat = async () => { throw new Error('retry must not send after restore failure') }
  })
  await page.getByRole('button', { name: 'Retry', exact: true }).click()
  await page.getByText('Retry this turn?', { exact: true }).locator('..').getByRole('button', { name: 'Retry', exact: true }).click()
  await expect(page.getByText('read-only source.txt', { exact: true })).toBeVisible()
  await expect(page.locator('.chat-user-turn')).toHaveCount(1)
  await expect(page.getByRole('log')).not.toContainText('retry must not send')
})
