import { expect, test } from '../fixtures/weaver';
import type { Locator } from '@playwright/test';
import { execFileSync } from 'child_process';
import { mkdirSync, writeFileSync } from 'fs';
import { join } from 'path';

// The diff view's per-line "+" comment control only becomes interactive once
// its row is hovered — same shape as changes-review.spec.ts, kept local so
// each journey spec stays self-contained.
async function addWidgetButton(
  fileArticle: Locator,
  side: 'old' | 'new',
  line: number,
): Promise<Locator> {
  const row = fileArticle.locator(`table[data-mode="${side}"] tr[data-line="${line}"]`);
  await row.hover();
  return row.locator('button.diff-add-widget');
}

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
  git(['commit', '-m', 'Extend the feature file\n\nExplains why the second write happened.']);

  // Deep link opens the tab directly, newest commit first.
  await page.goto(`${weaver.baseUrl}/s/${session.id}/commits`);
  const panel = page.getByTestId('commits-panel');
  await expect(page.locator('[data-tab="commits"]')).toHaveAttribute('aria-selected', 'true');
  await expect(panel).toContainText('2 on this branch');
  await expect(panel.locator('li').first()).toContainText('Extend the feature file');
  await expect(panel).toContainText('Add the feature file');
  await expect(panel).toContainText('Loom E2E');

  // The full message unfolds on request and re-folds; the row click still
  // opens the review.
  const bodyToggle = page.locator('[data-commit-body-toggle]').first();
  await expect(bodyToggle).toBeVisible();
  await bodyToggle.click();
  await expect(page.locator('[data-commit-body]')).toContainText(
    'Explains why the second write happened.',
  );
  await bodyToggle.click();
  await expect(page.locator('[data-commit-body]')).toHaveCount(0);

  // Clicking the commit line itself unfolds and re-folds the message too.
  await page.locator('[data-commit]').first().click();
  await expect(page.locator('[data-commit-body]')).toHaveCount(1);
  await page.locator('[data-commit]').first().click();
  await expect(page.locator('[data-commit-body]')).toHaveCount(0);

  // The tab also opens from a plain session page, and back again.
  await page.goto(`${weaver.baseUrl}/s/${session.id}`);
  await page.locator('[data-tab="commits"]').click();
  await expect(page).toHaveURL(new RegExp(`/s/${session.id}/commits$`));
  await expect(panel).toContainText('Extend the feature file');
  await page.locator('[data-tab="changes"]').click();
  await expect(page).toHaveURL(`${weaver.baseUrl}/s/${session.id}/changes`);
  await expect(page.locator('[data-tab="commits"]')).toHaveAttribute('aria-selected', 'false');
});

test('clicking a commit reviews its own diff, with expand and the compose aside', async ({
  page,
  weaver,
}) => {
  const session = await weaver.seedSession({
    goal: 'review one commit',
    name: 'commit-review',
  });
  const git = (args: string[]) => execFileSync('git', ['-C', session.work_dir, ...args]);
  const ten = (tag: string) =>
    Array.from({ length: 10 }, (_, index) => `${tag} ${index + 1}`).join('\n') + '\n';
  writeFileSync(join(session.work_dir, 'feature.txt'), ten('v1'));
  git(['add', '-A']);
  git(['commit', '-m', 'seed the feature file']);
  const v2 = ten('v1').split('\n');
  v2[4] = 'v2 5';
  writeFileSync(join(session.work_dir, 'feature.txt'), v2.join('\n'));
  mkdirSync(join(session.work_dir, 'src/nested'), { recursive: true });
  writeFileSync(join(session.work_dir, 'src/nested/feature.txt'), 'nested\n');
  git(['add', '-A']);
  git(['commit', '-m', 'touch the middle line']);

  // The newest commit's Review button opens Code Review scoped to its changes.
  await page.goto(`${weaver.baseUrl}/s/${session.id}/commits`);
  await page.getByTestId('commit-review-button').first().click();
  await expect(page).toHaveURL(new RegExp(`/s/${session.id}/changes\\?rev=[0-9a-f]{40}$`));
  const panel = page.getByTestId('changes-panel');
  await expect(page.getByTestId('changes-commit-scope')).toContainText('Reviewing commit');
  await expect(panel).toContainText('feature.txt');
  // The scoped review shows the middle-line change, not the whole-file add:
  // in branch state this file would read as added, not modified.
  await expect(panel).toContainText('v2 5');
  await expect(panel).toContainText('modified');

  // Drafting while commit-scoped anchors against the commit's own snapshot:
  // saving must succeed rather than 409 with a moved-version conflict.
  const reviewFileArticle = page
    .locator('article')
    .filter({ has: page.locator('code', { hasText: /^feature\.txt$/ }) })
    .first();
  await (await addWidgetButton(reviewFileArticle, 'new', 5)).click();
  const composer = page.getByTestId('change-comment-composer');
  await composer.locator('textarea').fill('Why did **this** line change?');
  await composer.getByRole('button', { name: 'Add pending comment' }).click();
  await expect(page.getByTestId('review-tray')).toContainText('1 pending');

  // The saved comment renders as markdown: the emphasis lands as real
  // markup in the inline card, not as literal asterisks.
  const savedCard = page.locator('[data-review-card]').first();
  await expect(savedCard.locator('strong')).toHaveText('this');

  // The compose aside shows the change set as a collapsible file tree — and
  // opening it must not shift the header row beside the toggle.
  const heading = panel.getByRole('heading', { name: 'Changes' });
  const headingBefore = await heading.boundingBox();
  await page.getByTestId('changes-aside-toggle').click();
  const aside = page.getByTestId('changes-aside');
  await expect(aside.locator('[data-aside-file]')).toHaveCount(2);
  // The rail lists files in the same order the diff pane renders them —
  // the served path order, folders interleaved by byte position rather
  // than hoisted above files.
  const asideFiles = aside.locator('[data-aside-file]');
  await expect(asideFiles.nth(0)).toHaveAttribute('data-aside-file', 'feature.txt');
  await expect(asideFiles.nth(1)).toHaveAttribute('data-aside-file', 'src/nested/feature.txt');
  // The rail's pending badge counts the saved draft on its file.
  await expect(
    aside.locator('[data-aside-file="feature.txt"] [title="Pending comments"]'),
  ).toHaveText('1');
  const headingAfter = await heading.boundingBox();
  expect(headingAfter!.x).toBe(headingBefore!.x);

  // The rail quietly marks the file the diff is currently showing: the
  // top-level feature file fills the scroller at the top, so its row lights
  // and the off-screen nested file's does not. The check matches the class
  // itself, not the `hover:bg-subtle` every row carries.
  const activeRow = /(^| )bg-subtle( |$)/;
  await expect(aside.locator('[data-aside-file="feature.txt"]')).toHaveClass(activeRow);
  await expect(aside.locator('[data-aside-file="src/nested/feature.txt"]')).not.toHaveClass(
    activeRow,
  );
  await expect(aside.locator('[data-aside-folder="src"]')).toBeVisible();
  await expect(aside.locator('[data-aside-folder="src/nested"]')).toBeVisible();
  await page.getByTestId('changes-aside-filter').fill('nope');
  await expect(aside).toContainText('No files match.');
  await page.getByTestId('changes-aside-filter').fill('nested');
  await expect(aside.locator('[data-aside-file]')).toHaveCount(1);

  // Collapsing a folder hides its files; the filter view re-expands everything.
  await page.getByTestId('changes-aside-filter').fill('');
  await aside.locator('[data-aside-folder="src"]').click();
  await expect(aside.locator('[data-aside-file]')).toHaveCount(1);
  await aside.locator('[data-aside-folder="src"]').click();
  await expect(aside.locator('[data-aside-file]')).toHaveCount(2);

  // Dragging the divider resizes the rail, and a file entry still jumps.
  const before = await aside.boundingBox();
  const handle = page.getByTestId('changes-aside-resize');
  const handleBox = (await handle.boundingBox())!;
  await page.mouse.move(handleBox.x + handleBox.width / 2, handleBox.y + 50);
  await page.mouse.down();
  await page.mouse.move(handleBox.x - 100, handleBox.y + 50, { steps: 5 });
  await page.mouse.up();
  const after = await aside.boundingBox();
  expect(after!.width).toBeLessThan(before!.width);
  await aside.locator('[data-aside-file]').first().click();

  // The jump lands the file's header at the top — the diff body must sit
  // below the sticky filename bar, not clipped underneath it.
  const jumped = page
    .locator('article')
    .filter({
      has: page.locator('code', { hasText: /^src\/nested\/feature\.txt$/ }),
    })
    .first();
  await expect
    .poll(async () => {
      const header = await jumped.locator('button').first().boundingBox();
      // Body rows carry data-state; a bare `tr` would hit the hidden thead.
      const row = await jumped.locator('tr[data-state]').first().boundingBox();
      if (!header || !row) return Number.NEGATIVE_INFINITY;
      return row.y - (header.y + header.height);
    })
    .toBeGreaterThanOrEqual(-1);

  // Content-backed hunks expose expand controls that reveal real file lines:
  // line 1 sits outside the 3-line context until expanded upward. Scope to the
  // modified file's article — the whole-file addition also owns a line 1.
  const featureArticle = page
    .locator('article')
    .filter({ has: page.locator('code', { hasText: /^feature\.txt$/ }) })
    .first();
  const firstLine = featureArticle.locator('tr[data-line="1"]').first();
  await expect(firstLine).toBeHidden();
  await featureArticle.locator('button[title="Expand Up"]').click();
  await expect(firstLine).toBeVisible();
  await expect(firstLine).toContainText('v1 1');

  // Leaving the commit scope returns to the whole branch state.
  await page
    .getByTestId('changes-commit-scope')
    .getByRole('button', { name: 'All changes' })
    .click();
  await expect(page).toHaveURL(new RegExp(`/s/${session.id}/changes$`));
  await expect(page.getByTestId('changes-commit-scope')).toHaveCount(0);

  // The commit-scoped comment stays honest in branch state: feature.txt is a
  // whole-file addition here, so its line 5 certainly exists — but the
  // comment was written against the commit's own diff, never against this
  // snapshot. It surfaces in the outdated section instead of borrowing
  // inline placement from lines it never referenced.
  const outdated = page.getByTestId('outdated-comments');
  await expect(outdated).toBeVisible();
  await expect(outdated).toContainText('Why did this line change?');
  const branchFeature = page
    .locator('article')
    .filter({ has: page.locator('code', { hasText: /^feature\.txt$/ }) })
    .first();
  await expect(branchFeature).not.toContainText('Why did this line change?');

  // The header picker re-scopes the review from within Code Review itself.
  const scopeButton = page.getByTestId('changes-commit-scope-button');
  await expect(scopeButton).toContainText('All commits');
  await scopeButton.click();
  const picker = page.getByTestId('changes-commit-picker');
  await expect(picker).toContainText('Review all commits');
  await expect(picker).toContainText('Select a commit to review');
  await picker.getByTestId('commit-review-button').first().click();
  await expect(page).toHaveURL(new RegExp(`/s/${session.id}/changes\\?rev=[0-9a-f]{40}$`));
  await expect(page.getByTestId('changes-commit-scope')).toContainText('Reviewing commit');
  await expect(scopeButton).toContainText('Commit ');

  // "Review all commits" from the picker returns to the whole branch state.
  await scopeButton.click();
  await picker.getByTestId('changes-review-all').click();
  await expect(page).toHaveURL(new RegExp(`/s/${session.id}/changes$`));
  await expect(page.getByTestId('changes-commit-scope')).toHaveCount(0);
  await expect(scopeButton).toContainText('All commits');
});

test('the commit picker offers unstaged changes as their own scope', async ({ page, weaver }) => {
  const session = await weaver.seedSession({
    goal: 'review unstaged edits',
    name: 'session-unstaged',
  });

  const git = (args: string[]) => execFileSync('git', ['-C', session.work_dir, ...args]);
  writeFileSync(join(session.work_dir, 'feature.txt'), 'one\n');
  git(['add', '-A']);
  git(['commit', '-m', 'Add the feature file']);
  // A staged edit, an unstaged edit on top of it, and an untracked file:
  // only the unstaged edit belongs to the unstaged scope.
  writeFileSync(join(session.work_dir, 'feature.txt'), 'one\nstaged\n');
  git(['add', '-A']);
  writeFileSync(join(session.work_dir, 'feature.txt'), 'one\nstaged\nunstaged\n');
  writeFileSync(join(session.work_dir, 'fresh.txt'), 'fresh\n');

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  const scopeButton = page.getByTestId('changes-commit-scope-button');
  await scopeButton.click();
  const picker = page.getByTestId('changes-commit-picker');
  const unstagedRow = page.getByTestId('changes-unstaged-row');
  await expect(unstagedRow).toBeVisible();
  await expect(unstagedRow).toContainText('Unstaged changes');
  await expect(unstagedRow).toContainText('1 file not yet staged');

  // The row scopes the review to the working tree's unstaged edits alone.
  await unstagedRow.getByTestId('changes-unstaged-review-button').click();
  await expect(page).toHaveURL(new RegExp(`/s/${session.id}/changes\\?rev=unstaged$`));
  await expect(page.getByTestId('changes-unstaged-scope')).toContainText(
    'Reviewing unstaged changes',
  );
  await expect(scopeButton).toContainText('Unstaged changes');
  const featureArticle = page
    .locator('article')
    .filter({ has: page.locator('code', { hasText: /^feature\.txt$/ }) })
    .first();
  await expect(featureArticle).toContainText('unstaged');
  // The staged line is the context this diff applies to, and the untracked
  // file is no diff against the index at all — neither renders here.
  const freshArticle = page
    .locator('article')
    .filter({ has: page.locator('code', { hasText: /^fresh\.txt$/ }) });
  await expect(freshArticle).toHaveCount(0);

  // The banner's escape hatch returns to the whole branch state, where the
  // untracked file and the staged edit both belong again.
  await page
    .getByTestId('changes-unstaged-scope')
    .getByRole('button', { name: 'All changes' })
    .click();
  await expect(page).toHaveURL(new RegExp(`/s/${session.id}/changes$`));
  await expect(page.getByTestId('changes-unstaged-scope')).toHaveCount(0);
  await expect(
    page
      .locator('article')
      .filter({ has: page.locator('code', { hasText: /^fresh\.txt$/ }) }),
  ).toHaveCount(1);

  // A tree with nothing unstaged offers no such row: staged edits are not
  // unstaged, and the picker says so by omission.
  git(['add', '-A']);
  await scopeButton.click();
  await expect(picker).toBeVisible();
  await expect(page.getByTestId('changes-unstaged-row')).toHaveCount(0);
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
