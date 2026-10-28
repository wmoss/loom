import { expect, test } from '../fixtures/weaver';
import type { Locator } from '@playwright/test';
import { execFileSync } from 'child_process';
import { rmSync, writeFileSync } from 'fs';
import { join } from 'path';

// The diff view's per-line "add a comment" control is a `+` button that only
// becomes interactive once its row is hovered (`@git-diff-view/vue` renders
// it `invisible` until `group-hover`), scoped to one side's table so the old
// and new gutters never collide.
/** Hovers a diff row to reveal its add-widget button. Callers click the
 * result with `force: true`: the button only exists visually while its row is
 * hovered, and a just-saved comment's card can reflow the rows under the
 * stationary pointer between this hover and the click — a plain click then
 * blocks on a visibility check it can never re-satisfy, because Playwright
 * never re-hovers. The forced click's own mouse move re-engages the row's
 * hover, and the press lands on the widget either way. */
async function addWidgetButton(
  fileArticle: Locator,
  side: 'old' | 'new',
  line: number,
): Promise<Locator> {
  const row = fileArticle.locator(`table[data-mode="${side}"] tr[data-line="${line}"]`);
  await row.hover();
  return row.locator('button.diff-add-widget');
}

test('preserves Changes drafts through refresh and peer submit', async ({
  page,
  weaver,
}) => {
  const session = await weaver.seedSession({
    goal: 'review a changing worktree',
    name: 'changes-review',
  });
  const changedPath = join(session.work_dir, 'review.txt');
  writeFileSync(changedPath, 'first\nsecond\n');
  writeFileSync(join(session.work_dir, 'uv.lock'), 'version = 1\n');

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  // Files render expanded by default now, so there's no toggle to click first —
  // except lock files, whose generated diffs start collapsed.
  const fileToggle = page.getByRole('button', { name: /review\.txt/ });
  const lockToggle = page.getByRole('button', { name: /uv\.lock/ });
  await expect(fileToggle).toHaveAttribute('aria-expanded', 'true');
  await expect(lockToggle).toHaveAttribute('aria-expanded', 'false');
  // Opening one by hand is a reviewer's choice; it must survive a reload.
  await lockToggle.click();
  await expect(lockToggle).toHaveAttribute('aria-expanded', 'true');
  const article = page.locator('article').filter({ has: fileToggle });
  await (await addWidgetButton(article, 'new', 1)).click({ force: true });
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
  // The reviewer's explicit open survived the reload; a fresh lock file
  // would have collapsed again.
  await expect(lockToggle).toHaveAttribute('aria-expanded', 'true');
  // The refreshed snapshot retargets the open composer: its save lands
  // against the new version (the anchored line survived the edit) instead of
  // dead-ending on a version no refresh could ever bring back.
  const retargetedSave = page.waitForResponse(
    (response) =>
      response.ok() &&
      response.request().method() === 'POST' &&
      new URL(response.url()).pathname === '/api/reviews/comments/create',
  );
  await composer.getByRole('button', { name: 'Add pending comment' }).click();
  await retargetedSave;

  const tray = page.getByTestId('review-tray');
  await expect(tray).toContainText('1 pending');
  // Saving an inline comment must not pop the review tray open on its own.
  await expect(page.getByTestId('review-tray-toggle')).toHaveAttribute('aria-expanded', 'false');
  await page.getByTestId('review-tray-toggle').click();
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

  // The peer's submit opens the envelope while this page holds an unsaved
  // local note: the dirty text survives the peer's submit — a clean reload
  // is refused while a note is mid-flight in the composer — and reads as
  // the one pending row against what the session received. The tray is the
  // same drop-down a follow-up gets.
  await expect(overall).toHaveValue('Keep this local edit after the conflict.');
  await expect(page.getByTestId('review-tray-toggle')).toContainText('Review · 1 pending');
  await expect(page.getByTestId('pending-note-row')).toContainText(
    'Keep this local edit after the conflict.',
  );

  // Submitting flushes that local note onto the envelope and sends it as a
  // follow-up round — the freeze owns the flush, so the click's own blur
  // cannot lose it.
  const localRound = page.waitForResponse(
    (response) => response.ok() && new URL(response.url()).pathname === '/api/reviews/submit',
  );
  await page.getByTestId('submit-review').click();
  await localRound;
  const reviewsJson = await page.evaluate(async (sessionId) => {
    const response = await fetch('/api/reviews/list', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        subject_kind: 'changes',
        subject_key: 'changes',
        session: sessionId,
      }),
    });
    return (await response.json()) as Array<{
      summary: string;
      queued_summary: string | null;
      delivered_summary: string;
      delivery_key: string;
    }>;
  }, session.id);
  expect(reviewsJson[0].summary).toBe('Keep this local edit after the conflict.');
  expect(reviewsJson[0].delivery_key).toContain('#');
  // The round carries the note — in flight or delivered, no longer pending.
  await expect(page.getByTestId('review-tray-toggle')).toContainText('Review · 0 pending');
  await expect(page.getByTestId('submit-review')).toBeDisabled();
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
  // Opening the tray puts focus straight into the overall note.
  await expect(note).toBeFocused();
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

  // Escape in the overall note minimizes the tray too, flushing the dirty note.
  const reopenedNote = page.getByTestId('review-overall-note');
  await expect(reopenedNote).toBeFocused();
  await reopenedNote.fill('Kept through Escape.');
  const escapeSave = page.waitForResponse(
    (response) =>
      response.ok() &&
      response.request().method() === 'POST' &&
      ['/api/reviews/create', '/api/reviews/update'].includes(new URL(response.url()).pathname),
  );
  await reopenedNote.press('Escape');
  await escapeSave;
  await expect(reopenedNote).toHaveCount(0);
  await expect(toggle).toHaveAttribute('aria-expanded', 'false');

  await toggle.click();
  await expect(page.getByTestId('review-overall-note')).toHaveValue('Kept through Escape.');
});

test('confirm before discarding a typed inline comment', async ({ page, weaver }) => {
  const session = await weaver.seedSession({
    goal: 'composer discard confirm',
    name: 'changes-composer-discard',
  });
  const changedPath = join(session.work_dir, 'review.txt');
  writeFileSync(changedPath, 'first\nsecond\n');

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  const fileToggle = page.getByRole('button', { name: /review\.txt/ });
  const article = page.locator('article').filter({ has: fileToggle });
  const composer = page.getByTestId('change-comment-composer');
  const confirm = page.getByTestId('change-comment-discard-confirm');

  // Escape with nothing typed closes the composer outright.
  await (await addWidgetButton(article, 'new', 1)).click({ force: true });
  await composer.locator('textarea').press('Escape');
  await expect(composer).toHaveCount(0);

  // Escape with text typed presents the confirm, focused so Enter discards.
  await (await addWidgetButton(article, 'new', 1)).click({ force: true });
  const input = composer.locator('textarea');
  await input.fill('Do not keep this.');
  await input.press('Escape');
  await expect(confirm).toBeVisible();
  const discard = composer.getByRole('button', { name: 'Discard comment' });
  await expect(discard).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(composer).toHaveCount(0);

  // Escape from the confirm returns to editing without discarding.
  await (await addWidgetButton(article, 'new', 1)).click({ force: true });
  await input.fill('Keep me.');
  await input.press('Escape');
  await expect(confirm).toBeVisible();
  await expect(composer.getByRole('button', { name: 'Discard comment' })).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(confirm).toHaveCount(0);
  await expect(input).toBeFocused();
  await expect(input).toHaveValue('Keep me.');
});

test('comments whose lines leave the view persist in the outdated section', async ({
  page,
  weaver,
}) => {
  const session = await weaver.seedSession({
    goal: 'review a shrinking file',
    name: 'outdated-comments',
  });
  const changedPath = join(session.work_dir, 'shrinking.txt');
  const twenty = Array.from({ length: 20 }, (_, index) => `line ${index + 1}`);
  writeFileSync(changedPath, twenty.join('\n') + '\n');

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  const fileToggle = page.getByRole('button', { name: /shrinking\.txt/ });
  const article = page.locator('article').filter({ has: fileToggle });
  await (await addWidgetButton(article, 'new', 20)).click({ force: true });
  const composer = page.getByTestId('change-comment-composer');
  await composer.locator('textarea').fill('This line fell off the end.');
  await composer.getByRole('button', { name: 'Add pending comment' }).click();
  await expect(page.getByTestId('review-tray')).toContainText('1 pending');

  // Shrink the file and reload: line 20 no longer renders, so the comment
  // cannot sit at a line — but its file still draws, so it persists in that
  // file's own strip under the header, not the global section below.
  writeFileSync(changedPath, twenty.slice(0, 10).join('\n') + '\n');
  await page.getByRole('button', { name: 'Refresh' }).click();
  const section = page.getByTestId('outdated-comments');
  await expect(section).toHaveCount(0);
  const strip = article.getByTestId('file-comment-strip-shrinking.txt');
  await expect(strip).toContainText('This line fell off the end.');
  await expect(strip).toContainText('shrinking.txt');

  // Collapsing the file hides the strip with it, losing nothing; the tray
  // still counts the comment as pending.
  await fileToggle.click();
  await expect(strip).toHaveCount(0);
  await expect(page.getByTestId('review-tray')).toContainText('1 pending');
  await fileToggle.click();
  await expect(strip).toContainText('This line fell off the end.');
});

test('a changed line keeps its comment in place, marked stale', async ({ page, weaver }) => {
  const session = await weaver.seedSession({
    goal: 'review an edited file',
    name: 'stale-content-comment',
  });
  const changedPath = join(session.work_dir, 'edited.txt');
  const lines = ['alpha', 'beta', 'gamma'];
  writeFileSync(changedPath, lines.join('\n') + '\n');

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  const fileToggle = page.getByRole('button', { name: /edited\.txt/ });
  const article = page.locator('article').filter({ has: fileToggle });
  await (await addWidgetButton(article, 'new', 2)).click({ force: true });
  const composer = page.getByTestId('change-comment-composer');
  await composer.locator('textarea').fill('About the beta line.');
  await composer.getByRole('button', { name: 'Add pending comment' }).click();
  await expect(page.getByTestId('review-tray')).toContainText('1 pending');

  // The revision moves and line 2 is rewritten in place. The view still
  // draws line 2, so the comment stays right there — marked stale, because
  // the text it was written against is gone. No exile to a bottom section.
  writeFileSync(changedPath, ['alpha', 'beta was replaced', 'gamma'].join('\n') + '\n');
  await page.getByRole('button', { name: 'Refresh' }).click();
  await expect(page.getByTestId('outdated-comments')).toHaveCount(0);
  const staleCard = article
    .locator('[data-review-card]')
    .filter({ hasText: 'About the beta line.' });
  await expect(staleCard).toBeVisible();
  await expect(staleCard.getByTestId('stale-card-marker')).toContainText('Stale');

  // The tray warns in red, non-blocking; the row carries the same flag, and
  // there is no acknowledgement checkbox any more.
  const tray = page.getByTestId('review-tray');
  await page.getByTestId('review-tray-toggle').click();
  await expect(page.getByTestId('review-stale-warning')).toContainText(
    '1 pending comment',
  );
  await expect(tray.getByRole('button', { name: /About the beta line/ })).toContainText('stale');
  await expect(page.getByTestId('review-stale-ack')).toHaveCount(0);

  // Unchanged code is indistinguishable from fresh: restoring the original
  // text clears the badge and the warning with it.
  writeFileSync(changedPath, lines.join('\n') + '\n');
  await page.getByRole('button', { name: 'Refresh' }).click();
  await expect(staleCard.getByTestId('stale-card-marker')).toHaveCount(0);
  await expect(page.getByTestId('review-stale-warning')).toHaveCount(0);

  // Stale again — and re-anchoring is the reviewer's action that resolves
  // it: the badge clears the moment the anchor lands on current code. (The
  // refresh click minimized the tray; the warning lives inside it.)
  writeFileSync(changedPath, ['alpha', 'beta was replaced', 'gamma'].join('\n') + '\n');
  await page.getByRole('button', { name: 'Refresh' }).click();
  if ((await page.getByTestId('review-tray-toggle').getAttribute('aria-expanded')) === 'false') {
    await page.getByTestId('review-tray-toggle').click();
  }
  await expect(page.getByTestId('review-stale-warning')).toContainText('1 pending comment');
  // The open tray overlays the page; close it to reach the card.
  await page.getByTestId('review-tray-toggle').click();
  await staleCard.getByRole('button', { name: 'Re-anchor' }).click();
  await (await addWidgetButton(article, 'new', 2)).click({ force: true });
  await page
    .getByTestId('change-comment-composer')
    .getByRole('button', { name: 'Move comment here' })
    .click();
  await expect(staleCard.getByTestId('stale-card-marker')).toHaveCount(0);
  await expect(page.getByTestId('review-stale-warning')).toHaveCount(0);

  // A stale comment submits with the warning and no gate: one click sends
  // the review as it stands.
  writeFileSync(changedPath, ['alpha', 'entirely new line', 'gamma'].join('\n') + '\n');
  await page.getByRole('button', { name: 'Refresh' }).click();
  if ((await page.getByTestId('review-tray-toggle').getAttribute('aria-expanded')) === 'false') {
    await page.getByTestId('review-tray-toggle').click();
  }
  await expect(page.getByTestId('review-stale-warning')).toContainText('1 pending comment');
  const submitted = page.waitForResponse(
    (response) => response.ok() && new URL(response.url()).pathname === '/api/reviews/submit',
  );
  await page.getByTestId('submit-review').click();
  await submitted;
  // The same dropdown, back to its quiescent shape: the submitted envelope
  // with nothing pending, its state line in place of the rows.
  await expect(tray).toContainText('Review · 0 pending');
  await expect(page.getByTestId('review-envelope-state')).toContainText('submitted');
});

test('several comment threads stay open at once', async ({ page, weaver }) => {
  const session = await weaver.seedSession({
    goal: 'review with two threads',
    name: 'two-threads',
  });
  const changedPath = join(session.work_dir, 'threads.txt');
  const lines = Array.from({ length: 10 }, (_, index) => `row ${index + 1}`);
  writeFileSync(changedPath, lines.join('\n') + '\n');

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  const fileToggle = page.getByRole('button', { name: /threads\.txt/ });
  const article = page.locator('article').filter({ has: fileToggle });
  const composer = page.getByTestId('change-comment-composer');

  // Two threads on two lines. Saving the second must not hide the first.
  await (await addWidgetButton(article, 'new', 3)).click({ force: true });
  await composer.locator('textarea').fill('First thread.');
  await composer.getByRole('button', { name: 'Add pending comment' }).click();
  await (await addWidgetButton(article, 'new', 8)).click({ force: true });
  await composer.locator('textarea').fill('Second thread.');
  await composer.getByRole('button', { name: 'Add pending comment' }).click();

  const openCards = article.locator('[data-review-card]');
  await expect(openCards).toHaveCount(2);
  await expect(openCards.filter({ hasText: 'First thread.' })).toBeVisible();
  await expect(openCards.filter({ hasText: 'Second thread.' })).toBeVisible();

  // Closing one leaves the other open — hiding one because another opened
  // was the noise this replaces.
  await openCards
    .filter({ hasText: 'First thread.' })
    .getByRole('button', { name: 'Close' })
    .click();
  await expect(article.locator('[data-review-collapsed]')).toHaveCount(1);
  await expect(openCards.filter({ hasText: 'Second thread.' })).toBeVisible();
});

test('mixed expand clicks never leave a dead hunk bar behind', async ({ page, weaver }) => {
  const session = await weaver.seedSession({
    goal: 'review a big diff',
    name: 'expand-bars',
  });
  const git = (args: string[]) => execFileSync('git', ['-C', session.work_dir, ...args]);
  // Base README with two far-apart edits: the commit's own diff carries a gap
  // of ~104 collapsible lines between its hunks, enough for the up/down
  // expand pair and the library's 40-line expansion step.
  const base = Array.from({ length: 260 }, (_, index) => `line ${index + 1}`);
  const work = [...base];
  work[4] = 'line 5 changed';
  work[154] = 'line 155 changed';
  writeFileSync(join(session.work_dir, 'README.md'), base.join('\n') + '\n');
  git(['add', '-A']);
  git(['commit', '-m', 'seed the readme']);
  writeFileSync(join(session.work_dir, 'README.md'), work.join('\n') + '\n');
  git(['add', '-A']);
  git(['commit', '-m', 'touch two distant lines']);

  await page.goto(`${weaver.baseUrl}/s/${session.id}/commits`);
  await page.getByTestId('commit-review-button').first().click();
  await expect(page).toHaveURL(new RegExp(`rev=`));
  const fileToggle = page.getByRole('button', { name: /README\.md/ });
  const article = page.locator('article').filter({ has: fileToggle });

  // Expand Down twice, then Expand Up, on the gap bar: the interleaving that
  // strands a hunk-header row between contiguous lines with dead buttons.
  const bar = article.locator('tr[data-state="hunk"]').filter({ hasText: '@@ -152' });
  await expect(bar).toHaveCount(1);
  await bar.locator('button[title="Expand Down"]').click();
  await bar.locator('button[title="Expand Down"]').click();
  await bar.locator('button[title="Expand Up"]').click();

  // Every remaining hunk bar must still guard genuinely hidden lines: none
  // may sit between line numbers that are already contiguous.
  await expect
    .poll(async () => {
      return article.evaluate((root) => {
        const tables = root.querySelectorAll('table[data-mode="old"]');
        let dead = 0;
        tables.forEach((table) => {
          Array.from(table.querySelectorAll('tr[data-state="hunk"]')).forEach((row) => {
            if (!row.querySelector('button')) return;
            const around = (dir: 1 | -1) => {
              let cur: Element | null = row;
              for (;;) {
                cur = dir === 1 ? cur.nextElementSibling : cur.previousElementSibling;
                if (cur === null) return null;
                const line = cur.getAttribute('data-line') ?? '';
                if (/^\d+$/.test(line)) return Number(line);
              }
            };
            const prev = around(-1);
            const next = around(1);
            if (prev != null && next != null && next === prev + 1) {
              dead++;
            }
          });
        });
        return dead;
      });
    })
    .toBe(0);
});

test('submitted comments persist in the diff and resolve from collapsed', async ({
  page,
  weaver,
}) => {
  const session = await weaver.seedSession({
    goal: 'submit then resolve',
    name: 'submitted-comments',
  });
  const changedPath = join(session.work_dir, 'resolve.txt');
  const twenty = Array.from({ length: 20 }, (_, index) => `line ${index + 1}`);
  writeFileSync(changedPath, twenty.join('\n') + '\n');

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  const fileToggle = page.getByRole('button', { name: /resolve\.txt/ });
  const article = page.locator('article').filter({ has: fileToggle });
  await (await addWidgetButton(article, 'new', 5)).click({ force: true });
  const composer = page.getByTestId('change-comment-composer');
  await composer.locator('textarea').fill('Submitted comments persist here.');
  await composer.getByRole('button', { name: 'Add pending comment' }).click();
  await expect(article).toContainText('Submitted comments persist here.');

  // Submitting does not pull the comment out of the diff: it stays inline as
  // a collapsed submitted thread.
  await page.getByTestId('review-tray-toggle').click();
  const submitted = page.waitForResponse(
    (response) => response.ok() && new URL(response.url()).pathname === '/api/reviews/submit',
  );
  await page.getByTestId('submit-review').click();
  await submitted;
  // The card stayed open across the submit; its Resolve control proves the
  // thread is now submitted and still inline.
  const card = article
    .locator('[data-review-card]')
    .filter({ hasText: 'Submitted comments persist here.' });
  await expect(card.getByRole('button', { name: 'Resolve' })).toBeVisible();
  const resolved = page.waitForResponse(
    (response) =>
      response.ok() && new URL(response.url()).pathname === '/api/reviews/comments/resolve',
  );
  await card.getByRole('button', { name: 'Resolve' }).click();
  await resolved;
  const pill = article
    .locator('[data-review-collapsed]')
    .filter({ hasText: 'Submitted comments persist here.' });
  // Resolving closes the thread: the collapsed pill carries the state.
  await expect(pill).toContainText('Resolved');

  // Reopening from the pill keeps the thread open for reading, and the
  // reviewer can keep talking in it: a reply lands as the next entry,
  // attributed to the operator by name — and the reply reopens the closed
  // thread on its own, whoever answers.
  await pill.click();
  const replied = page.waitForResponse(
    (response) =>
      response.ok() && new URL(response.url()).pathname === '/api/reviews/comments/reply',
  );
  await card.getByRole('button', { name: 'Reply' }).click();
  await page
    .getByTestId('review-reply-composer')
    .locator('textarea')
    .fill('Closing after the checks pass.');
  await card.getByTestId('submit-reply').click();
  await replied;
  const thread = card.getByTestId('review-comment-replies');
  await expect(thread).toContainText('Closing after the checks pass.');
  await expect(thread).toContainText('loom-e2e-owner');
  // The reply reopened the resolved thread: the header chip is gone and the
  // resolution control offers Resolve again, not Reopen.
  await expect(card.getByTestId('review-comment-header')).not.toContainText('Resolved');
  await expect(card.getByRole('button', { name: 'Resolve' })).toBeVisible();
  await expect(card.getByRole('button', { name: 'Reopen' })).toHaveCount(0);

  // The reviewer's own words stay theirs to revise — the reply carries Edit
  // and Delete, the same affordances a pending comment has.
  const edited = page.waitForResponse(
    (response) =>
      response.ok() && new URL(response.url()).pathname === '/api/reviews/replies/update',
  );
  await card.getByTestId('review-reply-edit-button').click();
  await card.getByTestId('review-reply-edit').fill('Closing after the checks pass, all green.');
  await card.getByRole('button', { name: 'Save' }).click();
  await edited;
  await expect(thread).toContainText('all green.');

  // And they can be taken back entirely: a second reply, retracted before
  // the session ever receives it.
  const second = page.waitForResponse(
    (response) =>
      response.ok() && new URL(response.url()).pathname === '/api/reviews/comments/reply',
  );
  await card.getByRole('button', { name: 'Reply' }).click();
  await page
    .getByTestId('review-reply-composer')
    .locator('textarea')
    .fill('A reply to take back.');
  await card.getByTestId('submit-reply').click();
  await second;
  await expect(thread).toContainText('A reply to take back.');
  const retracting = page.waitForResponse(
    (response) =>
      response.ok() && new URL(response.url()).pathname === '/api/reviews/replies/delete',
  );
  await thread
    .locator('> div')
    .filter({ hasText: 'A reply to take back.' })
    .getByRole('button', { name: 'Delete' })
    .click();
  await card.getByRole('button', { name: 'Delete reply' }).click();
  await retracting;
  await expect(thread).not.toContainText('A reply to take back.');

  // The operator reply does not dead-end in the UI: it queues on the review
  // and looks exactly like a pending comment does — and a brand-new comment
  // on the submitted review queues the same way, on the same envelope.
  const toggle = page.getByTestId('review-tray-toggle');
  await expect(toggle).toContainText('Review · 1 pending');
  if ((await toggle.getAttribute('aria-expanded')) === 'false') await toggle.click();
  await expect(page.getByTestId('review-tray')).toContainText('all green.');
  await (await addWidgetButton(article, 'new', 7)).click({ force: true });
  await page
    .getByTestId('change-comment-composer')
    .locator('textarea')
    .fill('One more thing, brand new.');
  await page
    .getByTestId('change-comment-composer')
    .getByRole('button', { name: 'Add pending comment' })
    .click();
  await expect(article).toContainText('One more thing, brand new.');
  // Both kinds queue as the same rows, and one submit sends them together.
  // (The composer's clicks minimized the tray; the header count survives.)
  await expect(toggle).toContainText('Review · 2 pending');
  if ((await toggle.getAttribute('aria-expanded')) === 'false') await toggle.click();
  await expect(page.getByTestId('review-tray')).toContainText('One more thing, brand new.');
  await expect(page.getByTestId('review-tray')).toContainText('all green.');
  const roundSubmitted = page.waitForResponse(
    (response) => response.ok() && new URL(response.url()).pathname === '/api/reviews/submit',
  );
  await page.getByTestId('submit-review').click();
  await roundSubmitted;
  await expect(toggle).toContainText('Review · 0 pending');
  // The open tray overlays the page; close it before working the cards.
  await toggle.click();

  // The header collapses an open thread, same as the Close button. The reply
  // reopened the thread earlier, so the pill reads Submitted, not Resolved.
  await card.getByTestId('review-comment-header').click();
  await expect(pill).toContainText('Submitted');
  // And it can be clicked open again to re-resolve — resolving collapses the
  // thread on its own, the pill carrying the state.
  await pill.click();
  await card.getByRole('button', { name: 'Resolve' }).click();
  await expect(pill).toContainText('Resolved');

  // When the anchored line later leaves the view, the submitted comment
  // persists under its own file's strip — the file still renders, so its
  // comments stay with it instead of vanishing or sinking to a global bucket.
  writeFileSync(changedPath, twenty.slice(0, 3).join('\n') + '\n');
  await page.getByRole('button', { name: 'Refresh' }).click();
  await expect(page.getByTestId('outdated-comments')).toHaveCount(0);
  const strip = article.getByTestId('file-comment-strip-resolve.txt');
  await expect(strip).toContainText('Submitted comments persist here.');
  await expect(strip).toContainText('resolve.txt');
});

test('the unresolved pill jumps between threads and rests when none remain', async ({
  page,
  weaver,
}) => {
  const session = await weaver.seedSession({
    goal: 'jump between unresolved threads',
    name: 'review-nav',
  });
  const changedPath = join(session.work_dir, 'navigate.txt');
  const lines = Array.from({ length: 12 }, (_, index) => `row ${index + 1}`);
  writeFileSync(changedPath, lines.join('\n') + '\n');

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  const article = page.locator('article').filter({
    has: page.getByRole('button', { name: /navigate\.txt/ }),
  });
  const composer = page.getByTestId('change-comment-composer');

  // Three threads, three lines. Pending words count as unresolved too — the
  // pill is wayfinding, not a delivery state.
  for (const [line, body] of [
    [2, 'Top thread.'],
    [6, 'Middle thread.'],
    [10, 'Bottom thread.'],
  ] as const) {
    await (await addWidgetButton(article, 'new', line)).click({ force: true });
    await composer.locator('textarea').fill(body);
    await composer.getByRole('button', { name: 'Add pending comment' }).click();
  }

  const pill = page.getByTestId('review-nav');
  await expect(pill).toContainText('/ 3');
  // Nothing has been navigated yet, so the cursor rests on the first thread —
  // it follows the reader's clicks, not the composer's saves.
  await expect(pill).toContainText('1 / 3');

  // Durable threads: submit the review, then close the middle one — a
  // resolved thread is done, wherever it sits. The cards stay open across
  // the submit, so each Resolve works from its own open card.
  await page.getByTestId('review-tray-toggle').click();
  await page.getByTestId('submit-review').click();
  await expect(page.getByTestId('review-envelope-state')).toContainText('submitted');
  // The open tray overlays the page; close it before working the cards.
  await page.getByTestId('review-tray-toggle').click();
  const cards = article.locator('[data-review-card]');
  await expect(cards).toHaveCount(3);

  await cards
    .filter({ hasText: 'Middle thread.' })
    .getByRole('button', { name: 'Resolve' })
    .click();
  await expect(pill).toContainText('/ 2');

  // Next jumps down the diff to the bottom thread, opening its card.
  await pill.getByRole('button', { name: 'Next unresolved comment' }).click();
  await expect(cards.filter({ hasText: 'Bottom thread.' })).toBeVisible();
  await expect(pill).toContainText('2 / 2');
  // Next again wraps to the top of the diff; previous wraps back down.
  await pill.getByRole('button', { name: 'Next unresolved comment' }).click();
  await expect(cards.filter({ hasText: 'Top thread.' })).toBeVisible();
  await expect(pill).toContainText('1 / 2');
  await pill.getByRole('button', { name: 'Previous unresolved comment' }).click();
  await expect(cards.filter({ hasText: 'Bottom thread.' })).toBeVisible();

  // Resolve the rest and the pill rests — no unresolved threads, no chrome.
  await cards
    .filter({ hasText: 'Bottom thread.' })
    .getByRole('button', { name: 'Resolve' })
    .click();
  await expect(pill).toContainText('1 / 1');
  await expect(pill.getByRole('button', { name: 'Next unresolved comment' })).toBeDisabled();
  await cards
    .filter({ hasText: 'Top thread.' })
    .getByRole('button', { name: 'Resolve' })
    .click();
  await expect(pill).toHaveCount(0);
});

test('large files keep their syntax highlighting', async ({ page, weaver }) => {
  const session = await weaver.seedSession({
    goal: 'highlight large files',
    name: 'changes-highlight',
  });
  const git = (args: string[]) => execFileSync('git', ['-C', session.work_dir, ...args]);
  // 3200 lines of ~46 bytes is past the server's per-file content cap (so
  // the diff view composes its own skeleton from the hunks) and past the
  // highlighter's stock 2000-line safety cap — the shape of a big real file
  // that used to render entirely unhighlighted. The whole-file addition is
  // committed first so the reviewable diff is the small edit on top.
  const lines = Array.from({ length: 3200 }, (_, i) => `// filler line number ${i} to pad the file`);
  const bigPath = join(session.work_dir, 'big.rs');
  lines[3199] = 'fn the_edit() {}';
  writeFileSync(bigPath, `${lines.join('\n')}\n`);
  git(['add', '-A']);
  git(['commit', '-m', 'add the big file']);
  lines[3199] = 'fn edited_now() {}';
  writeFileSync(bigPath, `${lines.join('\n')}\n`);
  git(['add', '-A']);
  git(['commit', '-m', 'edit near the end']);
  const editOid = git(['rev-parse', 'HEAD']).toString().trim();

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes?rev=${editOid}`);
  const article = page
    .locator('article')
    .filter({ has: page.locator('code', { hasText: /^big\.rs$/ }) });
  await expect(article).toBeVisible();
  await expect(article).toContainText('fn edited_now() {}');
  // Past line 2000 the hunk still renders with real syntax spans: the
  // context lines as comments, the edit as keywords.
  await expect(article.locator('span.hljs-comment').first()).toBeVisible();
  await expect(article.locator('span.hljs-keyword').first()).toBeVisible();
});

test('vue files past the old content cap highlight their script region', async ({ page, weaver }) => {
  const session = await weaver.seedSession({
    goal: 'highlight vue script regions',
    name: 'changes-vue-highlight',
  });
  const git = (args: string[]) => execFileSync('git', ['-C', session.work_dir, ...args]);
  // A ~70 KB single-file component: past the old 64 KiB content cap — the
  // size that used to drop it to the hunk skeleton, whose missing
  // `<script setup>` opener left the script region unhighlighted while the
  // template still colored — and under the current cap, so whole-file
  // content rides along.
  const filler = (i: number) => `  const filler${i} = 'padding line number ${i} to size the file';`;
  const lines = [
    '<script setup lang="ts">',
    "import { ref } from 'vue';",
    ...Array.from({ length: 1200 }, (_, i) => filler(i)),
    '</script>',
    '<template>',
    '  <ul>',
    ...Array.from({ length: 480 }, (_, i) => `    <li>row ${i}</li>`),
    '  </ul>',
    '</template>',
  ];
  const vuePath = join(session.work_dir, 'BigPanel.vue');
  writeFileSync(vuePath, `${lines.join('\n')}\n`);
  git(['add', '-A']);
  git(['commit', '-m', 'add the big component']);
  lines[1] = "import { ref, computed } from 'vue';";
  lines[1204] = '    <li>extra row</li>';
  writeFileSync(vuePath, `${lines.join('\n')}\n`);
  git(['add', '-A']);
  git(['commit', '-m', 'touch script and template']);
  const editOid = git(['rev-parse', 'HEAD']).toString().trim();

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes?rev=${editOid}`);
  const article = page
    .locator('article')
    .filter({ has: page.locator('code', { hasText: /^BigPanel\.vue$/ }) });
  await expect(article).toBeVisible();
  // The script-region hunk colors as code, not as flat text: the import
  // line carries real keyword spans.
  const importRow = article.locator('tr').filter({ hasText: "import { ref, computed } from 'vue'" });
  await expect(importRow).toHaveCount(1);
  await expect(importRow.locator('span.hljs-keyword').first()).toBeVisible();
  // The template region colors too — both regions highlighted together is
  // the whole point.
  const templateRow = article.locator('tr').filter({ hasText: '<li>extra row</li>' });
  await expect(templateRow).toHaveCount(1);
  await expect(templateRow.locator('span.hljs-tag').first()).toBeVisible();
});

test('a pending comment whose file left the snapshot is dropped, not parked', async ({
  page,
  weaver,
}) => {
  const session = await weaver.seedSession({
    goal: 'vanishing composer file',
    name: 'changes-vanish',
  });
  // Two untracked files: the one the composer opens on, and the one the
  // reviewer reaches for afterwards.
  writeFileSync(join(session.work_dir, 'gone.txt'), 'one\ntwo\n');
  writeFileSync(join(session.work_dir, 'other.txt'), 'one\ntwo\n');

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  const goneArticle = page
    .locator('article')
    .filter({ has: page.locator('code', { hasText: /^gone\.txt$/ }) });
  await (await addWidgetButton(goneArticle, 'new', 2)).click({ force: true });
  await page
    .getByTestId('change-comment-composer')
    .locator('textarea')
    .fill('text typed against a file that is about to leave');

  // The session removes the file behind the reviewer's back. A refresh that
  // kept the pending would park every guarded action behind a discard
  // confirm only the vanished file's widget could have rendered.
  rmSync(join(session.work_dir, 'gone.txt'));
  await page.getByRole('button', { name: 'Refresh' }).click();
  await expect(page.getByTestId('change-comment-composer')).toHaveCount(0);

  // Navigation is free again immediately: another file's '+' opens a fresh
  // composer with no parked confirm in the way.
  const otherArticle = page
    .locator('article')
    .filter({ has: page.locator('code', { hasText: /^other\.txt$/ }) });
  await (await addWidgetButton(otherArticle, 'new', 2)).click({ force: true });
  await expect(page.getByTestId('change-comment-composer')).toBeVisible();
  await expect(
    page.getByTestId('change-comment-composer').locator('textarea'),
  ).toHaveValue('');
});

test('a note edited on a submitted review rides a follow-up round', async ({
  page,
  weaver,
}) => {
  const session = await weaver.seedSession({
    goal: 'note follow-up',
    name: 'note-followup',
  });
  const changedPath = join(session.work_dir, 'note.txt');
  writeFileSync(changedPath, 'one\ntwo\nthree\n');

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  const article = page
    .locator('article')
    .filter({ has: page.locator('code', { hasText: /^note\.txt$/ }) });
  await (await addWidgetButton(article, 'new', 2)).click({ force: true });
  await page
    .getByTestId('change-comment-composer')
    .locator('textarea')
    .fill('First round.');
  await page
    .getByTestId('change-comment-composer')
    .getByRole('button', { name: 'Add pending comment' })
    .click();
  const submitted = page.waitForResponse(
    (response) => response.ok() && new URL(response.url()).pathname === '/api/reviews/submit',
  );
  await page.getByTestId('review-tray-toggle').click();
  await page.getByTestId('submit-review').click();
  await submitted;

  // The envelope is quiescent with nothing pending: the button rests.
  await expect(page.getByTestId('review-envelope-state')).toContainText('submitted');
  await expect(page.getByTestId('submit-review')).toBeDisabled();

  // A note typed on the submitted envelope is pending words. Its autosave
  // lands on the envelope — and the button must stay enabled, because the
  // note differs from what the session last received, not from what the
  // envelope now holds.
  const note = page.getByTestId('review-overall-note');
  const autosave = page.waitForResponse(
    (response) =>
      response.ok() &&
      response.request().method() === 'POST' &&
      new URL(response.url()).pathname === '/api/reviews/update',
  );
  await note.fill('One more overall thought.');
  await note.press('Tab');
  await autosave;
  // The note is pending words like any comment: the manifest counts it, the
  // same 'X pending' as anything else, and the quiescent state line yields.
  await expect(page.getByTestId('review-tray-toggle')).toContainText('Review · 1 pending');
  await expect(page.getByTestId('pending-note-row')).toContainText('One more overall thought.');
  await expect(page.getByTestId('review-envelope-state')).toHaveCount(0);
  await expect(page.getByTestId('submit-review')).toBeEnabled();

  // The preview speaks for this surface too: drilling in shows the round a
  // submit would send, note first — not the envelope's standing message.
  await page.getByTestId('conversation-preview').locator('summary').click();
  await expect(page.getByTestId('conversation-preview')).toContainText(
    'Overall:',
  );
  await expect(page.getByTestId('conversation-preview')).toContainText(
    'One more overall thought.',
  );

  // One submission sends the note as a round — the freeze flushes any
  // unsaved text first, so nothing is lost to the click's own blur.
  const round = page.waitForResponse(
    (response) => response.ok() && new URL(response.url()).pathname === '/api/reviews/submit',
  );
  await page.getByTestId('submit-review').click();
  await round;
  // The tray still composes against the same envelope, its note in place —
  // and now carried: in flight or delivered, it is no longer pending words.
  await expect(note).toHaveValue('One more overall thought.');
  await expect(page.getByTestId('review-tray-toggle')).toContainText('Review · 0 pending');
  await expect(page.getByTestId('submit-review')).toBeDisabled();
  // A 200 alone could be the no-op read of an unchanged envelope; the round
  // key proves a follow-up was actually queued.
  const reviewsJson = await page.evaluate(async (sessionId) => {
    const response = await fetch('/api/reviews/list', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        subject_kind: 'changes',
        subject_key: 'changes',
        session: sessionId,
      }),
    });
    return (await response.json()) as Array<{ delivery_key: string }>;
  }, session.id);
  expect(reviewsJson[0].delivery_key).toContain('#');
});

test('unresolved threads open by default and resolved ones stay closed', async ({
  page,
  weaver,
}) => {
  const session = await weaver.seedSession({
    goal: 'default open states',
    name: 'changes-open-defaults',
  });
  const changedPath = join(session.work_dir, 'defaults.txt');
  const lines = Array.from({ length: 10 }, (_, index) => `row ${index + 1}`);
  writeFileSync(changedPath, lines.join('\n') + '\n');

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  const article = page
    .locator('article')
    .filter({ has: page.locator('code', { hasText: /^defaults\.txt$/ }) });
  // Two comments: one to leave live, one to resolve before reloading.
  await (await addWidgetButton(article, 'new', 2)).click({ force: true });
  await page
    .getByTestId('change-comment-composer')
    .locator('textarea')
    .fill('stays open');
  await page
    .getByTestId('change-comment-composer')
    .getByRole('button', { name: 'Add pending comment' })
    .click();
  // Let the first card mount and the rows settle before the next hover —
  // under load, hovering into a reflowing table loses the row hover state.
  await expect(article).toContainText('stays open');
  await (await addWidgetButton(article, 'new', 8)).click({ force: true });
  await page
    .getByTestId('change-comment-composer')
    .locator('textarea')
    .fill('gets resolved');
  await page
    .getByTestId('change-comment-composer')
    .getByRole('button', { name: 'Add pending comment' })
    .click();
  await expect(article).toContainText('gets resolved');
  await page.getByTestId('review-tray-toggle').click();
  const submitButton = page.getByTestId('submit-review');
  await expect(submitButton).toBeVisible();
  const submitted = page.waitForResponse(
    (response) => response.ok() && new URL(response.url()).pathname === '/api/reviews/submit',
  );
  await submitButton.click();
  await submitted;

  const resolveCard = article.locator('[data-review-card]').filter({ hasText: 'gets resolved' });
  await resolveCard.getByRole('button', { name: 'Resolve' }).click();
  await expect(
    article.locator('[data-review-collapsed]').filter({ hasText: 'gets resolved' }),
  ).toContainText('Resolved');

  // Arriving at the code review page: the live thread is open, the resolved
  // one closed.
  await page.reload();
  await expect(article.locator('[data-review-card]').filter({ hasText: 'stays open' })).toBeVisible();
  await expect(
    article.locator('[data-review-collapsed]').filter({ hasText: 'stays open' }),
  ).toHaveCount(0);
  await expect(
    article.locator('[data-review-collapsed]').filter({ hasText: 'gets resolved' }),
  ).toContainText('Resolved');
});
