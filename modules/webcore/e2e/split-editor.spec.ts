import { test, expect, type Locator, type Page } from '@playwright/test'

/**
 * Split-editor regression: two Monaco panes on `mbp` files.
 *
 * The bug this pins down: every `.mbp` model used one hard-coded Monaco URI, so
 * creating the second one threw inside `setupMonaco` and the pane stayed blank.
 * The same applies to the same file opened twice — which is why the model
 * registry keys by file identity rather than by language.
 */

/** Opens a demo workspace and returns once the explorer is visible. */
async function openWorkspace(page: Page): Promise<void> {
  await page.goto('/')
  await page.getByLabel('Workspace path').fill('D:/metteur-demo/metrics')
  await page.getByRole('button', { name: 'Open', exact: true }).click()
  await expect(page.getByTestId('file-tree')).toBeVisible({ timeout: 15_000 })
}

/** The explorer row of a tree entry, expanding folders on the way. */
async function treeRow(page: Page, name: string): Promise<Locator> {
  const tree = page.getByTestId('file-tree')
  const row = tree.getByRole('button', { name, exact: true })
  // The demo tree starts collapsed; expanding is a no-op once the row exists.
  for (const folder of ['metrics', 'src', 'blueprints']) {
    if ((await row.count()) > 0) break
    const folderRow = tree.getByRole('button', { name: folder, exact: true })
    if (!(await folderRow.locator('svg.rotate-90').count())) await folderRow.click()
    await page.waitForTimeout(120)
  }
  await expect(row).toHaveCount(1, { timeout: 10_000 })
  return row
}

/** Opens `name` in the split pane through the explorer context menu. */
async function openInSplit(page: Page, name: string): Promise<void> {
  await (await treeRow(page, name)).click({ button: 'right' })
  await page.getByText('Open in Split View').click()
}

test('two DSL files render side by side without a blank pane', async ({ page }) => {
  const pageErrors: string[] = []
  page.on('pageerror', (err) => pageErrors.push(err.message))

  await openWorkspace(page)
  await (await treeRow(page, 'collect.mbp')).click()
  await expect(page.locator('.monaco-editor').first()).toBeVisible({ timeout: 15_000 })
  await openInSplit(page, 'report.mbp')

  // Both panes must show editor content, not an empty container.
  const lines = page.locator('.monaco-editor .view-lines')
  await expect(lines).toHaveCount(2, { timeout: 15_000 })
  await expect(lines.nth(0)).toContainText('blueprint "Collect Samples"')
  await expect(lines.nth(1)).toContainText('blueprint "Report Samples"')

  // Opening in the split must not move the primary pane's tab highlight.
  await expect(page.getByTestId('tab-active')).toContainText('collect.mbp')

  expect(pageErrors).toEqual([])
})

test('the same file in both panes shares one buffer', async ({ page }) => {
  const pageErrors: string[] = []
  page.on('pageerror', (err) => pageErrors.push(err.message))

  await openWorkspace(page)
  await (await treeRow(page, 'collect.mbp')).click()
  await expect(page.locator('.monaco-editor').first()).toBeVisible({ timeout: 15_000 })
  await openInSplit(page, 'collect.mbp')

  const lines = page.locator('.monaco-editor .view-lines')
  await expect(lines).toHaveCount(2, { timeout: 15_000 })
  await expect(lines.nth(0)).toContainText('blueprint "Collect Samples"')
  await expect(lines.nth(1)).toContainText('blueprint "Collect Samples"')

  expect(pageErrors).toEqual([])
})

test('opening source in dark mode preserves the editor theme', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('metteur.theme', 'dark'))
  await openWorkspace(page)
  await (await treeRow(page, 'collect.mbp')).click()
  await expect(page.locator('.monaco-editor').first()).toBeVisible()
  await expect(page.locator('.monaco-editor').first()).toHaveClass(/vs-dark/)
})

test('markdown source remains visible after tab changes and reload', async ({ page }) => {
  const errors: string[] = []
  page.on('pageerror', (error) => errors.push(error.message))
  await openWorkspace(page)
  await (await treeRow(page, 'README.md')).click()
  const lines = page.locator('.monaco-editor .view-lines').first()
  await expect(lines).toContainText('# Metrics')
  await (await treeRow(page, 'collect.mbp')).click()
  await expect(lines).toContainText('blueprint "Collect Samples"')
  await page.getByRole('button', { name: /README.md/ }).first().click()
  await expect(lines).toContainText('# Metrics')
  await page.reload()
  await expect(lines).toContainText('# Metrics')
  expect(errors).toEqual([])
})

test('slow editor initialization shows source and times out with an explanation', async ({ page }) => {
  let release: (() => void) | undefined
  const paused = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/src/lib/monaco.ts', async (route) => {
    await paused
    await route.abort()
  })
  try {
    await openWorkspace(page)
    const row = await treeRow(page, 'collect.mbp')
    await page.clock.install()
    await row.click()
    await expect(page.getByRole('textbox', { name: 'Loading file source' })).toHaveValue(/blueprint "Collect Samples"/)
    await page.clock.runFor(16_000)
    await expect(page.getByRole('alert')).toContainText('timed out')
    await expect(page.getByRole('textbox', { name: 'Read-only file source' })).toHaveValue(/blueprint "Collect Samples"/)
  } finally {
    release?.()
  }
})

test.describe('touch source viewer', () => {
  test.use({ hasTouch: true })
  test('opening a file displays its existing source', async ({ page }) => {
    const errors: string[] = []
    page.on('pageerror', (error) => errors.push(error.message))
    await page.addInitScript(() => localStorage.setItem('metteur.theme', 'dark'))
    await openWorkspace(page)
    await (await treeRow(page, 'collect.mbp')).click()
    await expect(page.locator('.cm-content')).toContainText('blueprint "Collect Samples"')
    await expect(page.locator('.cm-editor')).toHaveCSS('background-color', 'rgb(23, 23, 27)')
    await page.evaluate(async () => {
      const path = '/src/stores/theme.ts'
      const { useThemeStore } = await import(path)
      useThemeStore().mode = 'light'
    })
    await expect(page.locator('.cm-editor')).toHaveCSS('background-color', 'rgb(251, 251, 253)')
    await expect(page.locator('.cm-content')).toContainText('blueprint "Collect Samples"')
    await (await treeRow(page, 'report.mbp')).click()
    await expect(page.locator('.cm-content')).toContainText('blueprint "Report Samples"')
    await page.reload()
    await expect(page.locator('.cm-content')).toContainText('blueprint "Report Samples"')
    expect(errors).toEqual([])
  })
})

test('failed editor loading still allows reading the file source', async ({ page }) => {
  await page.route('**/src/lib/monaco.ts', (route) => route.abort())
  await openWorkspace(page)
  await (await treeRow(page, 'collect.mbp')).click()
  await expect(page.getByRole('alert')).toContainText('Showing read-only source')
  await expect(page.getByRole('textbox', { name: 'Read-only file source' })).toHaveValue(/blueprint "Collect Samples"/)
})

test('rounded file panel keeps its resize handle usable', async ({ page }) => {
  await openWorkspace(page)
  const panel = page.locator('.workspace-file-panel')
  await expect(panel).toHaveCSS('border-top-right-radius', '14px')
  await expect(page.locator('.workspace-editor-surface')).toHaveCSS('border-top-left-radius', '12px')
  const before = (await panel.boundingBox())!
  const handle = (await panel.getByRole('separator').boundingBox())!
  await page.mouse.move(handle.x + handle.width / 2, handle.y + handle.height / 2)
  await page.mouse.down()
  await page.mouse.move(handle.x + handle.width / 2 + 60, handle.y + handle.height / 2)
  await page.mouse.up()
  await expect.poll(async () => (await panel.boundingBox())!.width).toBeGreaterThan(before.width + 40)
})
