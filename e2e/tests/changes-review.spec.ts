import { expect, test } from '../fixtures/weaver';
import type { Locator } from '@playwright/test';
import { writeFileSync } from 'fs';
import { join } from 'path';

// The diff view's per-line "add a comment" control is a `+` button that only
// becomes interactive once its row is hovered (`@git-diff-view/vue` renders
// it `invisible` until `group-hover`), scoped to one side's table so the old
// and new gutters never collide.
async function addWidgetButton(
  fileArticle: Locator,
  side: 'old' | 'new',
  line: number,
): Promise<Locator> {
  const row = fileArticle.locator(`table[data-mode="${side}"] tr[data-line="${line}"]`);
  await row.hover();
  return row.locator('button.diff-add-widget');
}

test('preserves Changes drafts through refresh and peer-submit conflicts', async ({
  page,
  weaver,
}) => {
  const session = await weaver.seedSession({
    goal: 'review a changing worktree',
    name: 'changes-review',
  });
  const changedPath = join(session.work_dir, 'review.txt');
  writeFileSync(changedPath, 'first\nsecond\n');

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  // Files render expanded by default now, so there's no toggle to click first.
  const fileToggle = page.getByRole('button', { name: /review\.txt/ });
  const article = page.locator('article').filter({ has: fileToggle });
  await (await addWidgetButton(article, 'new', 1)).click();
  const composer = page.getByTestId('change-comment-composer');
  const input = composer.locator('textarea');
  await input.fill('Explain why this line belongs here.');

  writeFileSync(changedPath, 'first\nchanged\nthird\n');
  const refreshedChanges = page.waitForResponse(
    (response) =>
      response.ok() &&
      response.request().method() === 'POST' &&
      new URL(response.url()).pathname === '/api/sessions/changes' &&
      (response.request().postDataJSON() as { session?: string })?.session === session.id,
  );
  await page.getByRole('button', { name: 'Refresh' }).click();
  await refreshedChanges;
  await expect(input).toHaveValue('Explain why this line belongs here.');
  const staleSave = page.waitForResponse(
    (response) =>
      response.status() === 409 &&
      response.request().method() === 'POST' &&
      ['/api/reviews/create', '/api/reviews/comments/create'].includes(
        new URL(response.url()).pathname,
      ),
  );
  await composer.getByRole('button', { name: 'Add pending comment' }).click();
  await staleSave;
  await expect(input).toHaveValue('Explain why this line belongs here.');
  await (await addWidgetButton(article, 'new', 2)).click();
  await expect(input).toHaveValue('Explain why this line belongs here.');
  await composer.getByRole('button', { name: 'Add pending comment' }).click();

  const tray = page.getByTestId('review-tray');
  await expect(tray).toContainText('1 pending');
  const overall = page.getByTestId('review-overall-note');
  const initialSave = page.waitForResponse(
    (response) =>
      response.ok() &&
      response.request().method() === 'POST' &&
      new URL(response.url()).pathname === '/api/reviews/update',
  );
  await overall.fill('Shared overall note.');
  await overall.press('Tab');
  await initialSave;

  const peer = await page.context().newPage();
  await peer.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  await peer.getByTestId('review-tray-toggle').click();
  await overall.fill('Keep this local edit after the conflict.');
  const peerSubmit = peer.waitForResponse(
    (response) =>
      response.ok() &&
      response.request().method() === 'POST' &&
      new URL(response.url()).pathname === '/api/reviews/submit',
  );
  await peer.getByTestId('submit-review').click();
  await peerSubmit;

  const saveConflict = page.waitForResponse(
    (response) =>
      response.status() === 409 &&
      response.request().method() === 'POST' &&
      new URL(response.url()).pathname === '/api/reviews/update',
  );
  await overall.focus();
  await overall.press('Tab');
  await saveConflict;
  await expect(overall).toHaveValue('Keep this local edit after the conflict.');

  const retrySave = page.waitForResponse(
    (response) =>
      response.ok() &&
      response.request().method() === 'POST' &&
      new URL(response.url()).pathname === '/api/reviews/update',
  );
  await overall.focus();
  await overall.press('Tab');
  await retrySave;
  await expect(tray).toContainText('0 pending');
  await peer.close();
});

test('minimizes the review overlay on outside clicks', async ({ page, weaver }) => {
  const session = await weaver.seedSession({
    goal: 'overlay dismiss',
    name: 'changes-overlay-dismiss',
  });
  const changedPath = join(session.work_dir, 'review.txt');
  writeFileSync(changedPath, 'first\nsecond\n');

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  const toggle = page.getByTestId('review-tray-toggle');
  await toggle.click();
  const note = page.getByTestId('review-overall-note');
  await expect(note).toBeVisible();
  await note.fill('Kept through minimizing.');

  // The textarea is removed before its blur can fire, so the outside click
  // must flush the dirty note itself.
  const noteSave = page.waitForResponse(
    (response) =>
      response.ok() &&
      response.request().method() === 'POST' &&
      ['/api/reviews/create', '/api/reviews/update'].includes(new URL(response.url()).pathname),
  );
  await page.getByRole('button', { name: 'Refresh' }).click();
  await noteSave;
  await expect(note).toHaveCount(0);
  await expect(toggle).toHaveAttribute('aria-expanded', 'false');

  await toggle.click();
  await expect(page.getByTestId('review-overall-note')).toHaveValue('Kept through minimizing.');
});
