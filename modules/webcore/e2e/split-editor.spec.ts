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
    await tree.getByRole('button', { name: folder, exact: true }).click()
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
