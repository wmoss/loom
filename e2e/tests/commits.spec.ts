import { expect, test } from '../fixtures/weaver';
import { execFileSync } from 'child_process';
import { writeFileSync } from 'fs';
import { join } from 'path';

test('the Commits tab lists the branch commits over its base', async ({ page, weaver }) => {
  const session = await weaver.seedSession({
    goal: 'list the branch commits',
    name: 'session-commits',
  });

  const git = (args: string[]) => execFileSync('git', ['-C', session.work_dir, ...args]);
  writeFileSync(join(session.work_dir, 'feature.txt'), 'one\n');
  git(['add', '-A']);
  git(['commit', '-m', 'Add the feature file']);
  writeFileSync(join(session.work_dir, 'feature.txt'), 'two\n');
  git(['add', '-A']);
  git(['commit', '-m', 'Extend the feature file']);

  // Deep link opens the tab directly, newest commit first.
  await page.goto(`${weaver.baseUrl}/s/${session.id}/commits`);
  const panel = page.getByTestId('commits-panel');
  await expect(page.locator('[data-tab="commits"]')).toHaveAttribute('aria-selected', 'true');
  await expect(panel).toContainText('2 on this branch');
  await expect(panel.locator('li').first()).toContainText('Extend the feature file');
  await expect(panel).toContainText('Add the feature file');
  await expect(panel).toContainText('Loom E2E');

  // The tab also opens from a plain session page, and back again.
  await page.goto(`${weaver.baseUrl}/s/${session.id}`);
  await page.locator('[data-tab="commits"]').click();
  await expect(page).toHaveURL(new RegExp(`/s/${session.id}/commits$`));
  await expect(panel).toContainText('Extend the feature file');
  await page.locator('[data-tab="changes"]').click();
  await expect(page).toHaveURL(`${weaver.baseUrl}/s/${session.id}/changes`);
  await expect(page.locator('[data-tab="commits"]')).toHaveAttribute('aria-selected', 'false');
});

test('a branch with no own commits says so instead of an empty list', async ({ page, weaver }) => {
  const session = await weaver.seedSession({
    goal: 'empty commit listing',
    name: 'session-commits-empty',
  });

  await page.goto(`${weaver.baseUrl}/s/${session.id}/commits`);
  const panel = page.getByTestId('commits-panel');
  await expect(panel).toContainText('No commits on this branch beyond its base.');
  await expect(panel).toContainText('0 on this branch');
});
