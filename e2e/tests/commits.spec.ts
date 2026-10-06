import { expect, test } from "../fixtures/weaver";
import { execFileSync } from "child_process";
import { writeFileSync } from "fs";
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
  git(["commit", "-m", "Extend the feature file"]);

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
  git(["add", "-A"]);
  git(["commit", "-m", "touch the middle line"]);

  // Clicking the newest commit opens Code Review scoped to its own changes.
  await page.goto(`${weaver.baseUrl}/s/${session.id}/commits`);
  await page.locator("[data-commit]").first().click();
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

  // The compose aside lists the files; the filter narrows, the entry jumps.
  await page.getByTestId("changes-aside-toggle").click();
  const aside = page.getByTestId("changes-aside");
  await expect(aside).toContainText("feature.txt");
  await page.getByTestId("changes-aside-filter").fill("nope");
  await expect(aside).toContainText("No files match.");
  await page.getByTestId("changes-aside-filter").fill("feature");
  await expect(aside.locator("[data-aside-file]")).toHaveCount(1);
  await aside.locator("[data-aside-file]").click();

  // Content-backed hunks expose expand controls that reveal real file lines:
  // line 1 sits outside the 3-line context until expanded upward.
  const firstLine = panel.locator('tr[data-line="1"]').first();
  await expect(firstLine).toBeHidden();
  await panel.locator('button[title="Expand Up"]').click();
  await expect(firstLine).toBeVisible();
  await expect(firstLine).toContainText("v1 1");

  // Leaving the commit scope returns to the whole branch state.
  await page
    .getByTestId("changes-commit-scope")
    .getByRole("button", { name: "All changes" })
    .click();
  await expect(page).toHaveURL(new RegExp(`/s/${session.id}/changes$`));
  await expect(page.getByTestId("changes-commit-scope")).toHaveCount(0);
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
