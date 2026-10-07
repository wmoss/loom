import { expect, test } from "../fixtures/weaver";
import type { Locator } from "@playwright/test";
import { writeFileSync } from "fs";
import { join } from "path";

// The diff view's per-line "add a comment" control is a `+` button that only
// becomes interactive once its row is hovered (`@git-diff-view/vue` renders
// it `invisible` until `group-hover`), scoped to one side's table so the old
// and new gutters never collide.
async function addWidgetButton(
  fileArticle: Locator,
  side: "old" | "new",
  line: number,
): Promise<Locator> {
  const row = fileArticle.locator(
    `table[data-mode="${side}"] tr[data-line="${line}"]`,
  );
  await row.hover();
  return row.locator("button.diff-add-widget");
}

test("preserves Changes drafts through refresh and peer-submit conflicts", async ({
  page,
  weaver,
}) => {
  const session = await weaver.seedSession({
    goal: "review a changing worktree",
    name: "changes-review",
  });
  const changedPath = join(session.work_dir, "review.txt");
  writeFileSync(changedPath, "first\nsecond\n");

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  // Files render expanded by default now, so there's no toggle to click first.
  const fileToggle = page.getByRole("button", { name: /review\.txt/ });
  const article = page.locator("article").filter({ has: fileToggle });
  await (await addWidgetButton(article, "new", 1)).click();
  const composer = page.getByTestId("change-comment-composer");
  const input = composer.locator("textarea");
  await input.fill("Explain why this line belongs here.");

  writeFileSync(changedPath, "first\nchanged\nthird\n");
  const refreshedChanges = page.waitForResponse(
    (response) =>
      response.ok() &&
      response.request().method() === "POST" &&
      new URL(response.url()).pathname === "/api/sessions/changes" &&
      (response.request().postDataJSON() as { session?: string })?.session ===
        session.id,
  );
  await page.getByRole("button", { name: "Refresh" }).click();
  await refreshedChanges;
  await expect(input).toHaveValue("Explain why this line belongs here.");
  const staleSave = page.waitForResponse(
    (response) =>
      response.status() === 409 &&
      response.request().method() === "POST" &&
      ["/api/reviews/create", "/api/reviews/comments/create"].includes(
        new URL(response.url()).pathname,
      ),
  );
  await composer.getByRole("button", { name: "Add pending comment" }).click();
  await staleSave;
  await expect(input).toHaveValue("Explain why this line belongs here.");
  await (await addWidgetButton(article, "new", 2)).click();
  await expect(input).toHaveValue("Explain why this line belongs here.");
  await composer.getByRole("button", { name: "Add pending comment" }).click();

  const tray = page.getByTestId("review-tray");
  await expect(tray).toContainText("1 pending");
  // Saving an inline comment must not pop the review tray open on its own.
  await expect(page.getByTestId("review-tray-toggle")).toHaveAttribute(
    "aria-expanded",
    "false",
  );
  await page.getByTestId("review-tray-toggle").click();
  const overall = page.getByTestId("review-overall-note");
  const initialSave = page.waitForResponse(
    (response) =>
      response.ok() &&
      response.request().method() === "POST" &&
      new URL(response.url()).pathname === "/api/reviews/update",
  );
  await overall.fill("Shared overall note.");
  await overall.press("Tab");
  await initialSave;

  const peer = await page.context().newPage();
  await peer.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  await peer.getByTestId("review-tray-toggle").click();
  await overall.fill("Keep this local edit after the conflict.");
  const peerSubmit = peer.waitForResponse(
    (response) =>
      response.ok() &&
      response.request().method() === "POST" &&
      new URL(response.url()).pathname === "/api/reviews/submit",
  );
  await peer.getByTestId("submit-review").click();
  await peerSubmit;

  const saveConflict = page.waitForResponse(
    (response) =>
      response.status() === 409 &&
      response.request().method() === "POST" &&
      new URL(response.url()).pathname === "/api/reviews/update",
  );
  await overall.focus();
  await overall.press("Tab");
  await saveConflict;
  await expect(overall).toHaveValue("Keep this local edit after the conflict.");

  const retrySave = page.waitForResponse(
    (response) =>
      response.ok() &&
      response.request().method() === "POST" &&
      new URL(response.url()).pathname === "/api/reviews/update",
  );
  await overall.focus();
  await overall.press("Tab");
  await retrySave;
  await expect(tray).toContainText("0 pending");
  await peer.close();
});

test("minimizes the review overlay on outside clicks", async ({
  page,
  weaver,
}) => {
  const session = await weaver.seedSession({
    goal: "overlay dismiss",
    name: "changes-overlay-dismiss",
  });
  const changedPath = join(session.work_dir, "review.txt");
  writeFileSync(changedPath, "first\nsecond\n");

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  const toggle = page.getByTestId("review-tray-toggle");
  await toggle.click();
  const note = page.getByTestId("review-overall-note");
  await expect(note).toBeVisible();
  // Opening the tray puts focus straight into the overall note.
  await expect(note).toBeFocused();
  await note.fill("Kept through minimizing.");

  // The textarea is removed before its blur can fire, so the outside click
  // must flush the dirty note itself.
  const noteSave = page.waitForResponse(
    (response) =>
      response.ok() &&
      response.request().method() === "POST" &&
      ["/api/reviews/create", "/api/reviews/update"].includes(
        new URL(response.url()).pathname,
      ),
  );
  await page.getByRole("button", { name: "Refresh" }).click();
  await noteSave;
  await expect(note).toHaveCount(0);
  await expect(toggle).toHaveAttribute("aria-expanded", "false");

  await toggle.click();
  await expect(page.getByTestId("review-overall-note")).toHaveValue(
    "Kept through minimizing.",
  );

  // Escape in the overall note minimizes the tray too, flushing the dirty note.
  const reopenedNote = page.getByTestId("review-overall-note");
  await expect(reopenedNote).toBeFocused();
  await reopenedNote.fill("Kept through Escape.");
  const escapeSave = page.waitForResponse(
    (response) =>
      response.ok() &&
      response.request().method() === "POST" &&
      ["/api/reviews/create", "/api/reviews/update"].includes(
        new URL(response.url()).pathname,
      ),
  );
  await reopenedNote.press("Escape");
  await escapeSave;
  await expect(reopenedNote).toHaveCount(0);
  await expect(toggle).toHaveAttribute("aria-expanded", "false");

  await toggle.click();
  await expect(page.getByTestId("review-overall-note")).toHaveValue(
    "Kept through Escape.",
  );
});

test("confirm before discarding a typed inline comment", async ({
  page,
  weaver,
}) => {
  const session = await weaver.seedSession({
    goal: "composer discard confirm",
    name: "changes-composer-discard",
  });
  const changedPath = join(session.work_dir, "review.txt");
  writeFileSync(changedPath, "first\nsecond\n");

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  const fileToggle = page.getByRole("button", { name: /review\.txt/ });
  const article = page.locator("article").filter({ has: fileToggle });
  const composer = page.getByTestId("change-comment-composer");
  const confirm = page.getByTestId("change-comment-discard-confirm");

  // Escape with nothing typed closes the composer outright.
  await (await addWidgetButton(article, "new", 1)).click();
  await composer.locator("textarea").press("Escape");
  await expect(composer).toHaveCount(0);

  // Escape with text typed presents the confirm, focused so Enter discards.
  await (await addWidgetButton(article, "new", 1)).click();
  const input = composer.locator("textarea");
  await input.fill("Do not keep this.");
  await input.press("Escape");
  await expect(confirm).toBeVisible();
  const discard = composer.getByRole("button", { name: "Discard comment" });
  await expect(discard).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(composer).toHaveCount(0);

  // Escape from the confirm returns to editing without discarding.
  await (await addWidgetButton(article, "new", 1)).click();
  await input.fill("Keep me.");
  await input.press("Escape");
  await expect(confirm).toBeVisible();
  await expect(
    composer.getByRole("button", { name: "Discard comment" }),
  ).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(confirm).toHaveCount(0);
  await expect(input).toBeFocused();
  await expect(input).toHaveValue("Keep me.");
});

test("comments whose lines leave the view persist in the outdated section", async ({
  page,
  weaver,
}) => {
  const session = await weaver.seedSession({
    goal: "review a shrinking file",
    name: "outdated-comments",
  });
  const changedPath = join(session.work_dir, "shrinking.txt");
  const twenty = Array.from({ length: 20 }, (_, index) => `line ${index + 1}`);
  writeFileSync(changedPath, twenty.join("\n") + "\n");

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  const fileToggle = page.getByRole("button", { name: /shrinking\.txt/ });
  const article = page.locator("article").filter({ has: fileToggle });
  await (await addWidgetButton(article, "new", 20)).click();
  const composer = page.getByTestId("change-comment-composer");
  await composer.locator("textarea").fill("This line fell off the end.");
  await composer.getByRole("button", { name: "Add pending comment" }).click();
  await expect(page.getByTestId("review-tray")).toContainText("1 pending");

  // Shrink the file and reload: line 20 is no longer rendered anywhere, so the
  // comment cannot sit inline — it must persist in the outdated section.
  writeFileSync(changedPath, twenty.slice(0, 10).join("\n") + "\n");
  await page.getByRole("button", { name: "Refresh" }).click();
  const section = page.getByTestId("outdated-comments");
  await expect(section).toBeVisible();
  // Open by default: the card and its anchor location are right there.
  await expect(section).toContainText("This line fell off the end.");
  await expect(section).toContainText("shrinking.txt");

  // The header collapses the section without losing anything.
  const toggle = page.getByTestId("outdated-comments-toggle");
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await expect(section).not.toContainText("This line fell off the end.");
  await expect(page.getByTestId("review-tray")).toContainText("1 pending");

  // And reopening brings the persisted comment back.
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  await expect(section).toContainText("This line fell off the end.");
});
