import { expect, test } from '../fixtures/weaver';
import type { Locator } from '@playwright/test';
import { execFileSync } from 'child_process';
import { rmSync, writeFileSync } from 'fs';
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
  await (await addWidgetButton(article, 'new', 1)).click();
  await composer.locator('textarea').press('Escape');
  await expect(composer).toHaveCount(0);

  // Escape with text typed presents the confirm, focused so Enter discards.
  await (await addWidgetButton(article, 'new', 1)).click();
  const input = composer.locator('textarea');
  await input.fill('Do not keep this.');
  await input.press('Escape');
  await expect(confirm).toBeVisible();
  const discard = composer.getByRole('button', { name: 'Discard comment' });
  await expect(discard).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(composer).toHaveCount(0);

  // Escape from the confirm returns to editing without discarding.
  await (await addWidgetButton(article, 'new', 1)).click();
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
  await (await addWidgetButton(article, 'new', 20)).click();
  const composer = page.getByTestId('change-comment-composer');
  await composer.locator('textarea').fill('This line fell off the end.');
  await composer.getByRole('button', { name: 'Add pending comment' }).click();
  await expect(page.getByTestId('review-tray')).toContainText('1 pending');

  // Shrink the file and reload: line 20 is no longer rendered anywhere, so the
  // comment cannot sit inline — it must persist in the outdated section.
  writeFileSync(changedPath, twenty.slice(0, 10).join('\n') + '\n');
  await page.getByRole('button', { name: 'Refresh' }).click();
  const section = page.getByTestId('outdated-comments');
  await expect(section).toBeVisible();
  // Open by default: the card and its anchor location are right there.
  await expect(section).toContainText('This line fell off the end.');
  await expect(section).toContainText('shrinking.txt');

  // The header collapses the section without losing anything.
  const toggle = page.getByTestId('outdated-comments-toggle');
  await toggle.click();
  await expect(toggle).toHaveAttribute('aria-expanded', 'false');
  await expect(section).not.toContainText('This line fell off the end.');
  await expect(page.getByTestId('review-tray')).toContainText('1 pending');

  // And reopening brings the persisted comment back.
  await toggle.click();
  await expect(toggle).toHaveAttribute('aria-expanded', 'true');
  await expect(section).toContainText('This line fell off the end.');
});

test('a stale comment whose line content changed is outdated, not mispinned', async ({
  page,
  weaver,
}) => {
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
  await (await addWidgetButton(article, 'new', 2)).click();
  const composer = page.getByTestId('change-comment-composer');
  await composer.locator('textarea').fill('About the beta line.');
  await composer.getByRole('button', { name: 'Add pending comment' }).click();
  await expect(page.getByTestId('review-tray')).toContainText('1 pending');

  // The revision moves and line 2 is rewritten in place. Its number still
  // exists — the range is contiguous — but the text the comment was written
  // against is gone: pinning it on the new line 2 would point it at code it
  // never saw. It belongs in the outdated section.
  writeFileSync(changedPath, ['alpha', 'beta was replaced', 'gamma'].join('\n') + '\n');
  await page.getByRole('button', { name: 'Refresh' }).click();
  const section = page.getByTestId('outdated-comments');
  await expect(section).toContainText('About the beta line.');
  await expect(article).not.toContainText('About the beta line.');

  // Rewriting the revision again so the original text returns re-places the
  // comment inline: the fingerprint, not just the number, decides.
  writeFileSync(changedPath, lines.join('\n') + '\n');
  await page.getByRole('button', { name: 'Refresh' }).click();
  await expect(section).toHaveCount(0);
  await expect(article).toContainText('About the beta line.');
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
  await (await addWidgetButton(article, 'new', 3)).click();
  await composer.locator('textarea').fill('First thread.');
  await composer.getByRole('button', { name: 'Add pending comment' }).click();
  await (await addWidgetButton(article, 'new', 8)).click();
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
  await (await addWidgetButton(article, 'new', 5)).click();
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
  // attributed to the operator by name, without touching resolve state.
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
  await page.getByTestId('review-reply-composer').getByRole('button', { name: 'Reply' }).click();
  await replied;
  const thread = card.getByTestId('review-comment-replies');
  await expect(thread).toContainText('Closing after the checks pass.');
  await expect(thread).toContainText('loom-e2e-owner');
  await expect(card).toContainText('Resolved');

  // The operator reply does not dead-end in the UI: the tray reports it
  // waiting to reach the session, and sending it re-queues the review for
  // delivery through the same path the original comments took.
  const toggle = page.getByTestId('review-tray-toggle');
  await expect(toggle).toContainText('Replies to send');
  if ((await toggle.getAttribute('aria-expanded')) === 'false') await toggle.click();
  const queue = page.locator('[data-testid^="review-reply-queue-"]');
  await expect(queue).toHaveCount(1);
  await expect(queue).toContainText('1 reply waiting to reach the session');
  await queue.locator('[data-testid^="review-send-replies-"]').click();
  await expect(queue).toHaveCount(0);
  await expect(toggle).not.toContainText('Replies to send');

  // The header collapses an open thread, same as the Close button.
  await card.getByTestId('review-comment-header').click();
  await expect(pill).toContainText('Resolved');
  // And it can be clicked open again to un-resolve.
  await pill.click();
  await card.getByRole('button', { name: 'Reopen' }).click();
  await card.getByRole('button', { name: 'Close' }).click();
  await expect(pill).toContainText('Submitted');

  // When the anchored line later leaves the view, the submitted comment moves
  // to the outdated section instead of vanishing.
  writeFileSync(changedPath, twenty.slice(0, 3).join('\n') + '\n');
  await page.getByRole('button', { name: 'Refresh' }).click();
  const section = page.getByTestId('outdated-comments');
  await expect(section).toBeVisible();
  await expect(section).toContainText('Submitted comments persist here.');
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
  await (await addWidgetButton(goneArticle, 'new', 2)).click();
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
  await (await addWidgetButton(otherArticle, 'new', 2)).click();
  await expect(page.getByTestId('change-comment-composer')).toBeVisible();
  await expect(
    page.getByTestId('change-comment-composer').locator('textarea'),
  ).toHaveValue('');
});
