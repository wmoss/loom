import { expect, test } from "../fixtures/weaver";
import { execFileSync } from "child_process";
import { mkdirSync, writeFileSync } from "fs";
import { join } from "path";

test("the Commits tab lists the branch commits over its base", async ({
  page,
  weaver,
}) => {
  const session = await weaver.seedSession({
    goal: "list the branch commits",
    name: "session-commits",
  });

  const git = (args: string[]) =>
    execFileSync("git", ["-C", session.work_dir, ...args]);
  writeFileSync(join(session.work_dir, "feature.txt"), "one\n");
  git(["add", "-A"]);
  git(["commit", "-m", "Add the feature file"]);
  writeFileSync(join(session.work_dir, "feature.txt"), "two\n");
  git(["add", "-A"]);
  git([
    "commit",
    "-m",
    "Extend the feature file\n\nExplains why the second write happened.",
  ]);

  // Deep link opens the tab directly, newest commit first.
  await page.goto(`${weaver.baseUrl}/s/${session.id}/commits`);
  const panel = page.getByTestId("commits-panel");
  await expect(page.locator('[data-tab="commits"]')).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(panel).toContainText("2 on this branch");
  await expect(panel.locator("li").first()).toContainText(
    "Extend the feature file",
  );
  await expect(panel).toContainText("Add the feature file");
  await expect(panel).toContainText("Loom E2E");

  // The full message unfolds on request and re-folds; the row click still
  // opens the review.
  const bodyToggle = page.locator("[data-commit-body-toggle]").first();
  await expect(bodyToggle).toBeVisible();
  await bodyToggle.click();
  await expect(page.locator("[data-commit-body]")).toContainText(
    "Explains why the second write happened.",
  );
  await bodyToggle.click();
  await expect(page.locator("[data-commit-body]")).toHaveCount(0);

  // Clicking the commit line itself unfolds and re-folds the message too.
  await page.locator("[data-commit]").first().click();
  await expect(page.locator("[data-commit-body]")).toHaveCount(1);
  await page.locator("[data-commit]").first().click();
  await expect(page.locator("[data-commit-body]")).toHaveCount(0);

  // The tab also opens from a plain session page, and back again.
  await page.goto(`${weaver.baseUrl}/s/${session.id}`);
  await page.locator('[data-tab="commits"]').click();
  await expect(page).toHaveURL(new RegExp(`/s/${session.id}/commits$`));
  await expect(panel).toContainText("Extend the feature file");
  await page.locator('[data-tab="changes"]').click();
  await expect(page).toHaveURL(`${weaver.baseUrl}/s/${session.id}/changes`);
  await expect(page.locator('[data-tab="commits"]')).toHaveAttribute(
    "aria-selected",
    "false",
  );
});

test("clicking a commit reviews its own diff, with expand and the compose aside", async ({
  page,
  weaver,
}) => {
  const session = await weaver.seedSession({
    goal: "review one commit",
    name: "commit-review",
  });
  const git = (args: string[]) =>
    execFileSync("git", ["-C", session.work_dir, ...args]);
  const ten = (tag: string) =>
    Array.from({ length: 10 }, (_, index) => `${tag} ${index + 1}`).join("\n") +
    "\n";
  writeFileSync(join(session.work_dir, "feature.txt"), ten("v1"));
  git(["add", "-A"]);
  git(["commit", "-m", "seed the feature file"]);
  const v2 = ten("v1").split("\n");
  v2[4] = "v2 5";
  writeFileSync(join(session.work_dir, "feature.txt"), v2.join("\n"));
  mkdirSync(join(session.work_dir, "src/nested"), { recursive: true });
  writeFileSync(join(session.work_dir, "src/nested/feature.txt"), "nested\n");
  git(["add", "-A"]);
  git(["commit", "-m", "touch the middle line"]);

  // The newest commit's Review button opens Code Review scoped to its changes.
  await page.goto(`${weaver.baseUrl}/s/${session.id}/commits`);
  await page.getByTestId("commit-review-button").first().click();
  await expect(page).toHaveURL(
    new RegExp(`/s/${session.id}/changes\\?rev=[0-9a-f]{40}$`),
  );
  const panel = page.getByTestId("changes-panel");
  await expect(page.getByTestId("changes-commit-scope")).toContainText(
    "Reviewing commit",
  );
  await expect(panel).toContainText("feature.txt");
  // The scoped review shows the middle-line change, not the whole-file add:
  // in branch state this file would read as added, not modified.
  await expect(panel).toContainText("v2 5");
  await expect(panel).toContainText("modified");

  // The compose aside shows the change set as a collapsible file tree.
  await page.getByTestId("changes-aside-toggle").click();
  const aside = page.getByTestId("changes-aside");
  await expect(aside.locator("[data-aside-file]")).toHaveCount(2);
  await expect(aside.locator('[data-aside-folder="src"]')).toBeVisible();
  await expect(aside.locator('[data-aside-folder="src/nested"]')).toBeVisible();
  await page.getByTestId("changes-aside-filter").fill("nope");
  await expect(aside).toContainText("No files match.");
  await page.getByTestId("changes-aside-filter").fill("nested");
  await expect(aside.locator("[data-aside-file]")).toHaveCount(1);

  // Collapsing a folder hides its files; the filter view re-expands everything.
  await page.getByTestId("changes-aside-filter").fill("");
  await aside.locator('[data-aside-folder="src"]').click();
  await expect(aside.locator("[data-aside-file]")).toHaveCount(1);
  await aside.locator('[data-aside-folder="src"]').click();
  await expect(aside.locator("[data-aside-file]")).toHaveCount(2);

  // Dragging the divider resizes the rail, and a file entry still jumps.
  const before = await aside.boundingBox();
  const handle = page.getByTestId("changes-aside-resize");
  const handleBox = (await handle.boundingBox())!;
  await page.mouse.move(handleBox.x + handleBox.width / 2, handleBox.y + 50);
  await page.mouse.down();
  await page.mouse.move(handleBox.x - 100, handleBox.y + 50, { steps: 5 });
  await page.mouse.up();
  const after = await aside.boundingBox();
  expect(after!.width).toBeLessThan(before!.width);
  await aside.locator("[data-aside-file]").first().click();

  // Content-backed hunks expose expand controls that reveal real file lines:
  // line 1 sits outside the 3-line context until expanded upward. Scope to the
  // modified file's article — the whole-file addition also owns a line 1.
  const featureArticle = page
    .locator("article")
    .filter({ has: page.locator("code", { hasText: /^feature\.txt$/ }) })
    .first();
  const firstLine = featureArticle.locator('tr[data-line="1"]').first();
  await expect(firstLine).toBeHidden();
  await featureArticle.locator('button[title="Expand Up"]').click();
  await expect(firstLine).toBeVisible();
  await expect(firstLine).toContainText("v1 1");

  // Leaving the commit scope returns to the whole branch state.
  await page
    .getByTestId("changes-commit-scope")
    .getByRole("button", { name: "All changes" })
    .click();
  await expect(page).toHaveURL(new RegExp(`/s/${session.id}/changes$`));
  await expect(page.getByTestId("changes-commit-scope")).toHaveCount(0);

  // The header picker re-scopes the review from within Code Review itself.
  const scopeButton = page.getByTestId("changes-commit-scope-button");
  await expect(scopeButton).toContainText("All commits");
  await scopeButton.click();
  const picker = page.getByTestId("changes-commit-picker");
  await expect(picker).toContainText("Review all commits");
  await expect(picker).toContainText("Select a commit to review");
  await picker.getByTestId("commit-review-button").first().click();
  await expect(page).toHaveURL(
    new RegExp(`/s/${session.id}/changes\\?rev=[0-9a-f]{40}$`),
  );
  await expect(page.getByTestId("changes-commit-scope")).toContainText(
    "Reviewing commit",
  );
  await expect(scopeButton).toContainText("Commit ");

  // "Review all commits" from the picker returns to the whole branch state.
  await scopeButton.click();
  await picker.getByTestId("changes-review-all").click();
  await expect(page).toHaveURL(new RegExp(`/s/${session.id}/changes$`));
  await expect(page.getByTestId("changes-commit-scope")).toHaveCount(0);
  await expect(scopeButton).toContainText("All commits");
});

test("a branch with no own commits says so instead of an empty list", async ({
  page,
  weaver,
}) => {
  const session = await weaver.seedSession({
    goal: "empty commit listing",
    name: "session-commits-empty",
  });

  await page.goto(`${weaver.baseUrl}/s/${session.id}/commits`);
  const panel = page.getByTestId("commits-panel");
  await expect(panel).toContainText(
    "No commits on this branch beyond its base.",
  );
  await expect(panel).toContainText("0 on this branch");
});
