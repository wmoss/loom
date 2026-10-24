<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue';
import type { ComponentPublicInstance } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { DiffModeEnum, DiffViewWithMultiSelect, SplitSide, highlighter } from '@git-diff-view/vue';
import type { LineRange } from '@git-diff-view/vue';
import '@git-diff-view/vue/styles/diff-view-pure.css';

// The highlighter skips any file past a 2000-line safety cap — which in a
// real review silently un-highlights every large file (whole crates run past
// 4000 lines). The library already composes a line-aligned skeleton from the
// hunks when the server could not attach whole-file content, so raising the
// cap is all those files need; the per-file syntax cache and lazy mounting
// keep the parse cost bounded to files a reviewer is actually looking at.
highlighter.setMaxLineToIgnoreSyntax(10_000);
import {
  addReviewComment,
  createReview,
  replyReviewComment,
  setReviewCommentResolution,
  deleteReviewComment,
  discardReview,
  getChanges,
  getSessionCommits,
  listChangesReviews,
  retryReviewDelivery,
  getMe,
  submitReview,
  updateReview,
  updateReviewComment,
  ApiError,
} from '../api';
import type {
  ChangeAnchor,
  ChangeFile,
  ChangeHunk,
  ChangeLine,
  ChangeSet,
  ChangeSide,
  Review,
  ReviewComment,
  SessionCommit,
} from '../types';
import { ReviewDraftController } from '../lib/reviewDraftController';
import { changeBaseProblem } from '../lib/changeBaseProblem';
import { openSessionEvents, type SessionEventsHandle } from '../lib/sessionEvents';
import { isLockFile } from '../lib/lockFiles';
import { toGitDiffViewData, type GitDiffViewData } from '../lib/gitDiffAdapter';
import { buildChangeFileTree, type FileTreeNode } from '../lib/changeFileTree';
import { theme } from '../theme';
import CommitList from './CommitList.vue';
import ReviewCommentCard from './ReviewCommentCard.vue';
import ReviewTray from './ReviewTray.vue';

const props = defineProps<{ id: string }>();
const route = useRoute();
const router = useRouter();
/**
 * `?rev=<oid>` scopes the snapshot to one commit's own changes;
 * `?rev=unstaged` scopes it to the working tree's unstaged edits alone.
 */
const rev = computed(() => (typeof route.query.rev === 'string' ? route.query.rev : null));
const changes = ref<ChangeSet | null>(null);
const reviews = ref<Review[]>([]);
const loading = ref(false);
const error = ref('');
const notice = ref('');
const collapsed = reactive(new Set<string>());
const mounted = reactive(new Set<string>());
/** Files a reviewer opened by hand: their choice outlives a reload and
 * beats the lock-file default. Not rendered — plain bookkeeping. */
const expandedByUser = new Set<string>();
let fileObserver: IntersectionObserver | null = null;
const activeComments = reactive(new Set<number>());
/** Threads the reviewer closed by hand: refreshes re-open every live thread
 * except these. */
const closedByUser = new Set<number>();
/** The thread the tray's prev/next last landed on; threads themselves stay
 * open independently — hiding one because another opened was noise. */
const lastFocusedComment = ref<number | null>(null);
const reanchorComment = ref<number | null>(null);
const commentErrors = reactive<Record<number, string>>({});
const deliveryErrors = reactive<Record<number, string>>({});
const trayOpen = ref(false);
const trayError = ref('');
/** A save failure shown inside the composer that attempted it — the tray's
 * error surface is invisible while the tray stays closed. */
const composerError = ref('');
// The compose aside: a hierarchical, resizable file rail on the diff's left,
// off until asked for.
const asideOpen = ref(false);
/** The outdated-comments section starts open so nothing is hidden; it can
 * be folded away once read. */
const outdatedOpen = ref(true);
const asideFilter = ref('');
// Folders default expanded; membership tracks the collapsed ones.
const collapsedFolders = reactive(new Set<string>());
const MIN_ASIDE_WIDTH = 180;
const MAX_ASIDE_WIDTH = 720;
const ASIDE_WIDTH_KEY = 'loom.changesAsideWidth';
const asideEl = ref<HTMLElement | null>(null);
const asideWidth = ref(loadAsideWidth());

function loadAsideWidth(): number {
  const v = Number(localStorage.getItem(ASIDE_WIDTH_KEY));
  return Number.isFinite(v) && v >= MIN_ASIDE_WIDTH ? v : 288;
}

let asideDragging = false;
function onAsideDrag(event: MouseEvent) {
  if (!asideDragging || !asideEl.value) return;
  const left = asideEl.value.getBoundingClientRect().left;
  asideWidth.value = Math.min(Math.max(event.clientX - left, MIN_ASIDE_WIDTH), MAX_ASIDE_WIDTH);
}
function stopAsideDrag() {
  if (!asideDragging) return;
  asideDragging = false;
  localStorage.setItem(ASIDE_WIDTH_KEY, String(Math.round(asideWidth.value)));
  document.removeEventListener('mousemove', onAsideDrag);
  document.removeEventListener('mouseup', stopAsideDrag);
  document.body.style.userSelect = '';
}
function startAsideDrag(event: MouseEvent) {
  asideDragging = true;
  event.preventDefault();
  document.addEventListener('mousemove', onAsideDrag);
  document.addEventListener('mouseup', stopAsideDrag);
  // Suppress text selection while dragging the divider.
  document.body.style.userSelect = 'none';
}

function toggleFolder(path: string) {
  if (collapsedFolders.has(path)) collapsedFolders.delete(path);
  else collapsedFolders.add(path);
}
const overallNote = ref('');
const summaryDirty = ref(false);
const summarySaving = ref(false);
const acknowledgeOutdated = ref(false);
const submitting = ref(false);

// The signed-in operator: only a review's creator may submit it, so the
// continuation surface is gated on the name.
const operator = ref('');
void getMe()
  .then((me) => {
    operator.value = me.username ?? '';
  })
  .catch(() => {});
const discarding = ref(false);

type Pending = {
  anchor: ChangeAnchor;
  body: string;
  version: string;
  fileKey: string;
  lineNumber: number;
  side: ChangeSide;
  reanchorId?: number;
};
const pending = ref<Pending | null>(null);
const savingComment = ref(false);
const composerInput = ref<HTMLTextAreaElement | null>(null);
// A plain `ref="composerInput"` inside this v-for's scoped slot would make Vue
// collect it into an array (its v-for-ref behavior is lexical, not runtime),
// so bind with a function ref instead to keep a single element reference.
function setComposerInput(el: Element | ComponentPublicInstance | null) {
  composerInput.value = el instanceof HTMLTextAreaElement ? el : null;
}
// The composer's discard confirm; same function-ref constraint as above.
const confirmDiscard = ref(false);
const discardButton = ref<HTMLButtonElement | null>(null);
function setDiscardButton(el: Element | ComponentPublicInstance | null) {
  discardButton.value = el instanceof HTMLButtonElement ? el : null;
}

const diffDataCache = new Map<string, GitDiffViewData | null>();
/** The version whose entries `diffDataCache` currently holds. */
let cachedVersion: string | null = null;

const draft = computed(() => reviews.value.find((review) => review.status === 'draft') ?? null);

/** The creator's submitted review with undelivered follow-up work — the
 * comments and replies still on the reviewer's side of the loop. It is the
 * pending surface beside (and in the tray, in place of) a draft: one submit
 * sends its new comments and thread replies together. */
/** The creator's submitted reviews for this subject, newest first — one
 * review carries the whole conversation, so these are the open envelopes. */
const submittedEnvelopes = computed(() =>
  reviews.value
    .filter(
      (review) =>
        review.status === 'submitted' &&
        !review.legacy &&
        review.id > 0 &&
        review.created_by === operator.value,
    )
    .sort((a, b) => b.id - a.id),
);

/** The envelope a fresh composer comment attaches to when no draft is open:
 * the newest submitted one, pending or not. */
const openEnvelope = computed(() => submittedEnvelopes.value[0] ?? null);

const continuation = computed(() => {
  if (draft.value) return null;
  return (
    submittedEnvelopes.value.find(
      (review) =>
        review.comments.some((comment) => comment.status === 'pending') ||
        review.pending_replies > 0,
    ) ?? null
  );
});

/**
 * The snapshot a version string names: the oid of the one commit it pins, or
 * `'branch'` for the whole change-set spelling. Comments render inline only
 * in the snapshot they were written against — a commit's own comments in
 * that commit's view, branch comments in the branch view — so a line range
 * that merely still exists in another snapshot never borrows its placement.
 */
function snapshotScope(version: string | null | undefined): string {
  if ((version ?? '').startsWith('changes-unstaged-v1:')) return 'unstaged';
  const pinned = /^changes-commit-v1:([0-9a-f]+):/.exec(version ?? '');
  return pinned ? pinned[1]! : 'branch';
}

/** The snapshot this panel is currently rendering. */
const viewScope = computed(() => snapshotScope(changes.value?.version));

/** True when the comment was written against the snapshot being viewed. */
function inViewScope(comment: ReviewComment): boolean {
  return snapshotScope(comment.subject_version) === viewScope.value;
}

/** True when the anchor's side and line range is rendered by some hunk of its
 * file, with the lines still holding the text the comment was written
 * against. The contiguity matches what the backend's anchor validation
 * requires; the text fingerprint is the rule a moved-under revision demands —
 * a range whose content changed is pinned on the wrong code, so the comment
 * reads as outdated rather than misattached. */
function anchorPlaced(anchor: ChangeAnchor): boolean {
  const file = (changes.value?.files ?? []).find((item) => item.path.bytes === anchor.path.bytes);
  if (!file) return false;
  for (const hunk of file.hunks) {
    const eligible = hunk.lines
      .map((line) => (anchor.side === 'old' ? line.old_line : line.new_line))
      .filter((line): line is number => line != null);
    const first = eligible.indexOf(anchor.start_line);
    if (first < 0) continue;
    let resolves = true;
    for (let offset = 1; offset <= anchor.end_line - anchor.start_line; offset++) {
      if (eligible[first + offset] !== anchor.start_line + offset) {
        resolves = false;
        break;
      }
    }
    if (!resolves) continue;
    if (!fingerprintHolds(anchor, hunk, first)) continue;
    return true;
  }
  return false;
}

/** Compares the anchor's recorded line texts against what its range renders
 * now. Anchors without a recorded selection (empty `selected`) cannot be
 * verified and keep their place. */
function fingerprintHolds(anchor: ChangeAnchor, hunk: ChangeHunk, first: number): boolean {
  if (!anchor.selected.length) return true;
  const lines = hunk.lines
    .filter((line) => lineNumber(line, anchor.side) != null)
    .slice(first, first + anchor.selected.length);
  if (lines.length !== anchor.selected.length) return false;
  return lines.every((line, index) => line.text === anchor.selected[index]);
}

/** One change comment paired with the review it belongs to. Draft and
 * submitted reviews place comments through exactly one code path: this union
 * feeds both the inline widgets and the outdated section. */
interface ReviewCommentEntry {
  review: Review;
  comment: ReviewComment;
}

const changeCommentEntries = computed<ReviewCommentEntry[]>(() =>
  (reviews.value ?? []).flatMap((review) =>
    review.comments
      .filter((comment) => comment.anchor_kind === 'change')
      .map((comment) => ({ review, comment })),
  ),
);

/** Comments — draft or submitted — that cannot be placed inline in this
 * view: the comment belongs to another snapshot (a different commit's diff,
 * or the branch change-set while a commit is scoped), the anchored file left
 * the change set, or its lines are no longer rendered. Drafts still submit
 * with the review, so everything persists in the outdated section where
 * drafts can be edited, re-anchored, or deleted and submitted comments can
 * be resolved. */
const outdatedComments = computed(() =>
  changeCommentEntries.value.filter(
    (entry) => !inViewScope(entry.comment) || !anchorPlaced(entry.comment.anchor as ChangeAnchor),
  ),
);

const baseProblem = computed(() => changeBaseProblem(changes.value?.base, 'Changes'));

function replaceReview(next: Review) {
  const index = reviews.value.findIndex((review) => review.id === next.id);
  if (index < 0) reviews.value = [next, ...reviews.value];
  else reviews.value.splice(index, 1, next);
}

function conflictReview(cause: unknown): Review | null {
  if (!(cause instanceof ApiError) || cause.status !== 409) return null;
  const details = cause.body.details;
  if (!details || typeof details !== 'object') return null;
  const fresh = (details as { review?: unknown }).review;
  if (!fresh || typeof fresh !== 'object' || typeof (fresh as Review).id !== 'number') return null;
  const review = fresh as Review;
  if (review.status === 'draft') controller.reconcile(review);
  else {
    const dirtySummary = controller.summaryDirty ? overallNote.value : null;
    replaceReview(review);
    controller.clearOwnership();
    if (dirtySummary != null) controller.editSummary(dirtySummary);
    activeComments.clear();
    lastFocusedComment.value = null;
    reanchorComment.value = null;
  }
  return review;
}

function mutationMessage(cause: unknown): string {
  if (conflictReview(cause)) {
    return 'This draft changed elsewhere. The latest version is loaded; review it before retrying.';
  }
  return (cause as Error).message;
}

const controller = new ReviewDraftController<Review>({
  saveSummary: async (current, summary) => {
    const version = changes.value?.version;
    if (!version) throw new Error('The change-set version is unavailable.');
    const review =
      current ??
      (await createReview(props.id, {
        subject_kind: 'changes',
        subject_key: 'changes',
        subject_version: version,
      }));
    return updateReview(review.id, {
      expected_revision: review.draft_revision,
      summary,
    });
  },
  onDraft: (next) => {
    if (next) replaceReview(next);
  },
  onSummary: (summary, dirty) => {
    overallNote.value = summary;
    summaryDirty.value = dirty;
  },
});

async function load() {
  const epoch = controller.beginRefresh();
  loading.value = true;
  try {
    const [nextChanges, nextReviews] = await Promise.all([
      getChanges(props.id, rev.value ?? undefined),
      listChangesReviews(props.id),
    ]);
    const nextDraft = nextReviews.find((review) => review.status === 'draft') ?? null;
    // The snapshot is not the draft controller's to gate, but only the
    // freshest response may install it — an out-of-order older response
    // would overwrite a newer snapshot (and could drop a live composer via
    // the scope check). Same for the reviews list, which additionally waits
    // on the controller's own state below.
    const superseded = controller.isSuperseded(epoch);
    if (!superseded) installChanges(nextChanges);
    if (!controller.acceptRefresh(epoch, nextDraft)) {
      if (!superseded) {
        // A local cause — a queued command or an unsaved note — held the
        // reviews list back. Re-sync once the controller settles; if a
        // newer load started meanwhile, it owns the state instead.
        void controller
          .flush()
          .then(() => {
            if (!controller.isSuperseded(epoch)) void load();
          })
          .catch((cause) => {
            trayError.value = mutationMessage(cause);
          });
      }
      return;
    }
    reviews.value = nextReviews;
    // Threads open where the conversation still lives: every comment that is
    // not resolved arrives open, resolved ones arrive closed, and a
    // reviewer's own collapse survives the refresh.
    for (const review of nextReviews) {
      for (const comment of review.comments) {
        if (
          comment.anchor_kind === 'change' &&
          comment.status !== 'resolved' &&
          !closedByUser.has(comment.id)
        ) {
          activeComments.add(comment.id);
        }
      }
    }
    error.value = '';
  } catch (cause) {
    error.value = (cause as Error).message;
  } finally {
    loading.value = false;
  }
}

/** Installs a fetched snapshot: collapse defaults, and a composer opened
 * against a different snapshot spelling — a commit's diff, or the branch
 * change-set while a commit was scoped — closes rather than submitting into
 * a review whose envelope disagrees. A same-scope version move keeps it
 * open: its lines may still resolve, and the stale save's 409
 * reconciliation is the designed retry. */
function installChanges(next: ChangeSet) {
  changes.value = next;
  applyCollapseDefaults(next);
  if (!pending.value) return;
  if (snapshotScope(pending.value.version) !== snapshotScope(next.version)) {
    pending.value = null;
    confirmDiscard.value = false;
    reanchorComment.value = null;
    return;
  }
  // The composer renders inside its file's diff article; a file that left
  // snapshot scope took the only surface that could ever confirm or save it
  // along, and every guarded navigation would park behind a confirm that
  // can no longer render. This is the one case where typed text goes
  // without a confirm — the alternative is a composer holding the whole
  // panel's navigation hostage for a file nothing can anchor to anymore.
  if (!next.files.some((file) => fileKey(file) === pending.value!.fileKey)) {
    pending.value = null;
    confirmDiscard.value = false;
    deferredScopeNav.value = null;
    deferredWidgetClick.value = null;
    reanchorComment.value = null;
    return;
  }
  // Same scope, newer snapshot: the composer keeps its text and anchor and
  // saves against the new version — the server re-validates the anchor's
  // recorded line text and rejects it only if it genuinely no longer holds.
  // Otherwise the composer would sit on a version no refresh can ever
  // bring back and 409 forever. A snapshot without a version (truncated or
  // timed out) retargets nothing.
  if (next.version && pending.value.version !== next.version) {
    // In place, not by replacement: an in-flight save captured this very
    // object, and replacing it would defeat the capture check and leave a
    // saved draft open for a duplicate save.
    pending.value.version = next.version;
  }
}

function fileKey(file: ChangeFile): string {
  return file.path.bytes;
}

function isExpanded(file: ChangeFile): boolean {
  return !collapsed.has(fileKey(file));
}

function toggleFile(file: ChangeFile) {
  const key = fileKey(file);
  if (collapsed.has(key)) {
    collapsed.delete(key);
    expandedByUser.add(key);
  } else {
    collapsed.add(key);
    expandedByUser.delete(key);
  }
}

/** Machine-authored lock files start collapsed: their bulk-generated diffs
 * are review noise. A reviewer's explicit open — toggle, rail jump, comment
 * focus — outlives reloads. */
function applyCollapseDefaults(next: ChangeSet) {
  for (const file of next.files) {
    const key = fileKey(file);
    if (isLockFile(file.path.display) && !expandedByUser.has(key)) collapsed.add(key);
  }
}

/** Mounts a file's diff once its container scrolls near the viewport, instead of all at once. */
function ensureFileObserver(): IntersectionObserver {
  if (!fileObserver) {
    fileObserver = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (!entry.isIntersecting) continue;
          const key = (entry.target as HTMLElement).dataset.fileKey;
          if (key) mounted.add(key);
          fileObserver?.unobserve(entry.target);
        }
      },
      { rootMargin: '600px 0px' },
    );
  }
  return fileObserver;
}

/** Diff containers currently handed to the lazy-mount observer, by file —
 * a detached container (collapse, teardown) must stop being observed. */
const observedFiles = new Map<string, Element>();

function observeFile(el: unknown, file: ChangeFile) {
  const key = fileKey(file);
  if (el instanceof Element) {
    observedFiles.set(key, el);
    if (!mounted.has(key)) ensureFileObserver().observe(el);
    return;
  }
  // The container unmounted (collapsed away or the panel torn down): hold no
  // target for it, or collapse/expand cycles leak observed elements until
  // the observer disconnects.
  const observed = observedFiles.get(key);
  if (observed) {
    fileObserver?.unobserve(observed);
    observedFiles.delete(key);
  }
}

onBeforeUnmount(() => {
  fileObserver?.disconnect();
  activeFileObserver?.disconnect();
});

// --- The file the diff is currently showing, highlighted in the rail -------
const diffScrollEl = ref<HTMLElement | null>(null);
const activeFileKey = ref<string | null>(null);
/** Observed file articles, mapped to the file key each one renders. */
const activeFileEls = new Map<Element, string>();
/** Files intersecting the scroller right now, with how much of it they fill. */
const visibleFiles = new Map<string, { area: number; top: number }>();
let activeFileObserver: IntersectionObserver | null = null;

function ensureActiveFileObserver(): IntersectionObserver {
  if (!activeFileObserver) {
    activeFileObserver = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          const key = activeFileEls.get(entry.target);
          if (key == null) continue;
          if (!entry.isIntersecting) {
            visibleFiles.delete(key);
            continue;
          }
          visibleFiles.set(key, {
            area: entry.intersectionRect.width * entry.intersectionRect.height,
            top: entry.boundingClientRect.top,
          });
        }
        // The file filling the most of the scroller is the one being read;
        // a tie goes to whichever is nearest the top.
        let best: { key: string; area: number; top: number } | null = null;
        for (const [key, visible] of visibleFiles) {
          if (
            !best ||
            visible.area > best.area ||
            (visible.area === best.area && visible.top < best.top)
          ) {
            best = { key, ...visible };
          }
        }
        if (best) activeFileKey.value = best.key;
      },
      { root: diffScrollEl.value },
    );
  }
  return activeFileObserver;
}

/** Observes one file's article so the rail can follow the scroller. */
function observeActiveFile(el: unknown, file: ChangeFile) {
  const key = fileKey(file);
  if (el instanceof Element) {
    activeFileEls.set(el, key);
    ensureActiveFileObserver().observe(el);
    return;
  }
  // Unmount carries no element, so this file's article is found by its key.
  for (const [element, mapped] of activeFileEls) {
    if (mapped === key) {
      activeFileObserver?.unobserve(element);
      activeFileEls.delete(element);
      visibleFiles.delete(key);
    }
  }
}

function gitDiffData(file: ChangeFile): GitDiffViewData | null {
  const version = changes.value?.version;
  // An unstable snapshot (truncated, timed out) carries no version, so any
  // key would collide with every other unstable read of the same file and
  // serve stale hunks — those snapshots pay the adapter cost directly
  // instead of caching.
  if (!version) return toGitDiffViewData(file);
  const key = `${version}:${fileKey(file)}`;
  const cached = diffDataCache.get(key);
  if (cached !== undefined) return cached;
  const data = toGitDiffViewData(file);
  // One version resident at a time: a new version's load evicts the old
  // entries rather than growing without bound (content rides each entry,
  // up to the server's per-file cap).
  if (cachedVersion !== version) {
    diffDataCache.clear();
    cachedVersion = version;
  }
  diffDataCache.set(key, data);
  return data;
}

function lineNumber(line: ChangeLine, side: ChangeSide): number | null {
  return side === 'old' ? line.old_line : line.new_line;
}

function sideFromSplitSide(side: SplitSide): ChangeSide {
  return side === SplitSide.old ? 'old' : 'new';
}

/** Finds the range's containing hunk on `range.side` and clamps its end to that hunk's last line. */
function scopeToHunk(file: ChangeFile) {
  return (range: LineRange): LineRange | null => {
    // The multi-select side is already the wire spelling; `SplitSide` is a
    // numeric enum used only by the widget-click payload.
    const side: ChangeSide = range.side;
    for (const hunk of file.hunks) {
      const numbers = hunk.lines
        .map((line) => lineNumber(line, side))
        .filter((value): value is number => value != null);
      if (!numbers.length) continue;
      const low = numbers[0];
      const high = numbers[numbers.length - 1];
      if (range.startLineNumber < low || range.startLineNumber > high) continue;
      return { ...range, endLineNumber: Math.min(range.endLineNumber, high) };
    }
    return null;
  };
}

/** Builds a `ChangeAnchor` for a contiguous line range, deriving context text from the hunk it falls in. */
function buildAnchor(
  file: ChangeFile,
  side: ChangeSide,
  startLine: number,
  endLine: number,
): ChangeAnchor | null {
  for (const hunk of file.hunks) {
    const eligible = hunk.lines.filter((line) => lineNumber(line, side) != null);
    const selected = eligible.filter((line) => {
      const number = lineNumber(line, side)!;
      return number >= startLine && number <= endLine;
    });
    if (selected.length !== endLine - startLine + 1) continue;
    const first = eligible.indexOf(selected[0]);
    const lastIndex = eligible.indexOf(selected.at(-1)!);
    return {
      path: file.path,
      side,
      start_line: startLine,
      end_line: endLine,
      hunk_header: hunk.header,
      context_before: eligible.slice(Math.max(0, first - 2), first).map((line) => line.text),
      selected: selected.map((line) => line.text),
      context_after: eligible.slice(lastIndex + 1, lastIndex + 3).map((line) => line.text),
    };
  }
  return null;
}

function widgetStateFor(file: ChangeFile): { side: SplitSide; lineNumber: number } | undefined {
  if (!pending.value || pending.value.fileKey !== fileKey(file)) return undefined;
  return {
    side: pending.value.side === 'old' ? SplitSide.old : SplitSide.new,
    lineNumber: pending.value.lineNumber,
  };
}

function extendDataFor(file: ChangeFile): {
  oldFile: Record<string, { data: ReviewCommentEntry[] }>;
  newFile: Record<string, { data: ReviewCommentEntry[] }>;
} {
  const oldFile: Record<string, { data: ReviewCommentEntry[] }> = {};
  const newFile: Record<string, { data: ReviewCommentEntry[] }> = {};
  for (const entry of changeCommentEntries.value) {
    const anchor = entry.comment.anchor as ChangeAnchor;
    if (
      anchor.path.bytes !== file.path.bytes ||
      !inViewScope(entry.comment) ||
      !anchorPlaced(anchor)
    )
      continue;
    const bucket = anchor.side === 'old' ? oldFile : newFile;
    const key = String(anchor.end_line);
    (bucket[key] ??= { data: [] }).data.push(entry);
  }
  return { oldFile, newFile };
}

/** A '+' click in a different file, held until the composer's discard
 * confirm answers. Same-file '+' moves keep the draft's text by design, so
 * only a cross-file move is a discard. */
const deferredWidgetClick = ref<(() => void) | null>(null);

function onAddWidgetClick(
  file: ChangeFile,
  payload: { lineNumber: number; fromLineNumber?: number; side: SplitSide },
) {
  const version = changes.value?.version;
  if (!version) return;
  const key = fileKey(file);
  // A typed comment in another file asks the discard confirm first — the
  // same never-silently-destroy rule every other discard path follows.
  if (
    pending.value?.body.trim() &&
    pending.value.reanchorId == null &&
    pending.value.fileKey !== key
  ) {
    deferredScopeNav.value = null;
    deferredWidgetClick.value = () => onAddWidgetClick(file, payload);
    confirmDiscard.value = true;
    void nextTick(() => requestAnimationFrame(() => discardButton.value?.focus()));
    return;
  }
  const side = sideFromSplitSide(payload.side);
  const start = Math.min(payload.fromLineNumber ?? payload.lineNumber, payload.lineNumber);
  const end = Math.max(payload.fromLineNumber ?? payload.lineNumber, payload.lineNumber);
  const anchor = buildAnchor(file, side, start, end);
  if (!anchor) return;
  confirmDiscard.value = false;
  deferredScopeNav.value = null;
  deferredWidgetClick.value = null;
  composerError.value = '';
  pending.value = {
    anchor,
    body: reanchorComment.value == null && pending.value?.fileKey === key ? pending.value.body : '',
    version,
    fileKey: key,
    lineNumber: payload.lineNumber,
    side,
    reanchorId: reanchorComment.value ?? undefined,
  };
  // The library's "+" button focuses itself on mousedown; wait a frame past
  // that native default action so our focus call isn't immediately stolen back.
  void nextTick(() => requestAnimationFrame(() => composerInput.value?.focus()));
}

function cancelPending(onClose: () => void) {
  if (pending.value?.reanchorId != null) reanchorComment.value = null;
  confirmDiscard.value = false;
  composerError.value = '';
  pending.value = null;
  onClose();
  // A snapshot-scope switch or a held cross-file '+' was waiting on this
  // discard decision; only one can be pending at a time.
  const navigate = deferredScopeNav.value;
  const click = deferredWidgetClick.value;
  deferredScopeNav.value = null;
  deferredWidgetClick.value = null;
  navigate?.();
  click?.();
}

/** Escape or Cancel with text typed swaps the composer for a discard confirm
 * whose button takes focus, so Enter confirms and Escape returns to editing. */
function requestCancel(onClose: () => void) {
  if (!pending.value?.body.trim()) {
    cancelPending(onClose);
    return;
  }
  confirmDiscard.value = true;
  void nextTick(() => requestAnimationFrame(() => discardButton.value?.focus()));
}

function keepEditing() {
  confirmDiscard.value = false;
  // Keeping the comment also keeps its file and snapshot: the pending scope
  // switch or held cross-file '+' is abandoned, not fired off later.
  deferredScopeNav.value = null;
  deferredWidgetClick.value = null;
  void nextTick(() => requestAnimationFrame(() => composerInput.value?.focus()));
}

async function confirmPending(onClose: () => void) {
  if (!pending.value) return;
  if (pending.value.reanchorId != null) {
    // Leave re-anchor mode before the attempt: a failed move reports on the
    // comment card and must not hijack the next "+" click into another move.
    const reanchorId = pending.value.reanchorId;
    reanchorComment.value = null;
    await applyReanchor(reanchorId, pending.value.anchor);
    pending.value = null;
    onClose();
    return;
  }
  await saveComment();
  if (!pending.value) onClose();
}

async function saveComment() {
  const capture = pending.value;
  const body = capture?.body.trim();
  if (!capture || !body || savingComment.value) return;
  savingComment.value = true;
  composerError.value = '';
  trayError.value = '';
  try {
    const updated = await controller.command(async (current) => {
      const review =
        current ??
        openEnvelope.value ??
        (await createReview(props.id, {
          subject_kind: 'changes',
          subject_key: 'changes',
          subject_version: capture.version,
        }));
      return addReviewComment(review.id, {
        expected_revision: review.draft_revision,
        subject_version: capture.version,
        anchor_kind: 'change',
        anchor: capture.anchor,
        body,
      });
    });
    const saved = updated.comments.at(-1)?.id;
    if (saved != null) activeComments.add(saved);
    // A second comment may already be composing elsewhere while this save
    // was in flight: only the draft this save started from goes away. The
    // new composer keeps its text, and the caller's `onClose` check sees a
    // live pending and leaves the moved-to widget alone.
    if (pending.value === capture) {
      pending.value = null;
      // Saving answered the composer's question; any deferred scope switch
      // or held cross-file '+' is dead.
      deferredScopeNav.value = null;
      deferredWidgetClick.value = null;
    }
  } catch (cause) {
    const message = mutationMessage(cause);
    trayError.value = message;
    // Only the composer this save started from gets the inline error — a
    // newer draft may already be typing elsewhere.
    if (pending.value === capture) composerError.value = message;
  } finally {
    savingComment.value = false;
  }
}

/** Runs a pending-comment mutation against the surface that owns the
 * comment: the open draft through its controller (summary flush and
 * conflict reconciliation ride along), or — the new shape — the submitted
 * review still carrying it, whose undelivered comments are as editable as
 * draft ones. */
async function mutateComment(run: (review: Review) => Promise<Review>): Promise<void> {
  if (draft.value) {
    await controller.command((current) => {
      if (!current) throw new Error('Review draft is unavailable.');
      return run(current);
    });
    return;
  }
  const target = continuation.value;
  if (!target) throw new Error('Review draft is unavailable.');
  replaceReview(await run(target));
}

async function editComment(payload: { commentId: number; body: string }) {
  try {
    await mutateComment((review) =>
      updateReviewComment(review.id, payload.commentId, {
        expected_revision: review.draft_revision,
        body: payload.body,
      }),
    );
  } catch (cause) {
    commentErrors[payload.commentId] = mutationMessage(cause);
  }
}

/** Appends to a comment's reply thread from the operator side. */
function replyToComment(commentId: number, body: string) {
  return replyReviewComment(props.id, commentId, body);
}

async function removeComment(commentId: number) {
  try {
    await mutateComment((review) =>
      deleteReviewComment(review.id, commentId, review.draft_revision),
    );
    activeComments.delete(commentId);
  } catch (cause) {
    throw new Error(mutationMessage(cause));
  }
}

async function applyReanchor(commentId: number, anchor: ChangeAnchor) {
  const version = changes.value?.version;
  if (!version) return;
  try {
    await mutateComment((review) =>
      updateReviewComment(review.id, commentId, {
        expected_revision: review.draft_revision,
        subject_version: version,
        anchor_kind: 'change',
        anchor,
      }),
    );
    reanchorComment.value = null;
    notice.value = 'Comment re-anchored to the current changes.';
  } catch (cause) {
    commentErrors[commentId] = mutationMessage(cause);
  }
}

function editOverall(summary: string) {
  controller.editSummary(summary);
}

async function saveOverall() {
  summarySaving.value = true;
  try {
    await controller.flush();
  } catch (cause) {
    trayError.value = mutationMessage(cause);
  } finally {
    summarySaving.value = false;
  }
}

async function discardDraft() {
  discarding.value = true;
  try {
    const id = await controller.freeze(async (current) => {
      if (!current) throw new Error('Review draft is unavailable.');
      await discardReview(current.id, current.draft_revision);
      return { draft: null, result: current.id };
    });
    reviews.value = reviews.value.filter((review) => review.id !== id);
    trayOpen.value = false;
  } finally {
    discarding.value = false;
  }
}

async function retarget() {
  // The version this panel is rendering — the branch change-set, or the
  // commit a `?rev` view scopes to. Retargeting moves the empty draft onto
  // exactly the snapshot under review, so the button's "current changes" is
  // true in both spellings (and the reviewed commit's own diff becomes
  // commentable when a branch-scope draft was in the way).
  const version = changes.value?.version;
  if (!draft.value || draft.value.comments.length || !version) return;
  try {
    await controller.command((current) => {
      if (!current) throw new Error('Review draft is unavailable.');
      return updateReview(current.id, {
        expected_revision: current.draft_revision,
        subject_version: version,
      });
    });
  } catch (cause) {
    trayError.value = mutationMessage(cause);
  }
}

async function submit() {
  // A submitted review with pending comments and replies is the same
  // submission as a draft: one button sends whatever is queued.
  if (!draft.value && continuation.value) {
    const target = continuation.value;
    submitting.value = true;
    trayError.value = '';
    try {
      const updated = await submitReview(target.id, {
        expected_revision: target.draft_revision,
        // A follow-up round rides the already-delivered snapshot; the
        // outdated gate does not apply to it.
        acknowledge_outdated: false,
      });
      replaceReview(updated);
    } catch (cause) {
      trayError.value = mutationMessage(cause);
    } finally {
      submitting.value = false;
    }
    return;
  }
  submitting.value = true;
  try {
    const submitted = await controller.freeze(async (current) => {
      if (!current) throw new Error('Review draft is unavailable.');
      const result = await submitReview(current.id, {
        expected_revision: current.draft_revision,
        acknowledge_outdated: acknowledgeOutdated.value,
      });
      replaceReview(result);
      return { draft: null, result };
    });
    notice.value = `Review submitted · ${submitted.delivery_state}.`;
  } catch (cause) {
    trayError.value = mutationMessage(cause);
  } finally {
    submitting.value = false;
  }
}

async function retryDelivery(item: Review) {
  try {
    replaceReview(await retryReviewDelivery(item.id));
  } catch (cause) {
    deliveryErrors[item.id] = (cause as Error).message;
  }
}

/** Marks a comment — draft or submitted — active and reveals it: expanding
 * and force-mounting its file when needed, then scrolling its card — inline
 * or in the outdated section — into view. Shared by the tray's prev/next and
 * direct clicks. */
function focusComment(commentId: number) {
  const entry = changeCommentEntries.value.find((item) => item.comment.id === commentId);
  if (!entry) return;
  if (outdatedComments.value.some((item) => item.comment.id === commentId))
    outdatedOpen.value = true;
  if (entry.comment.anchor_kind === 'change') {
    const key = (entry.comment.anchor as ChangeAnchor).path.bytes;
    collapsed.delete(key);
    expandedByUser.add(key);
    mounted.add(key);
  }
  activeComments.add(commentId);
  lastFocusedComment.value = commentId;
  void nextTick(() =>
    document
      .querySelector(`[data-review-collapsed="${commentId}"], [data-review-card="${commentId}"]`)
      ?.scrollIntoView({ block: 'center' }),
  );
}

/** Opens (or closes) one thread without touching any other open one. */
function openComment(commentId: number) {
  activeComments.add(commentId);
  lastFocusedComment.value = commentId;
}

function closeComment(commentId: number) {
  activeComments.delete(commentId);
  closedByUser.add(commentId);
}

/** The tray's prev/next walk the draft's pending comments — the buttons say
 * pending, and submitted threads read fine from their inline pills. */
function navigate(direction: number) {
  const comments = draft.value?.comments ?? [];
  if (!comments.length) return;
  const current = comments.findIndex((comment) => comment.id === lastFocusedComment.value);
  // With nothing focused, next opens the first thread and prev the last;
  // the naive modulo would skip a comment from that state.
  const index =
    current < 0
      ? direction > 0
        ? 0
        : comments.length - 1
      : (current + direction + comments.length) % comments.length;
  focusComment(comments[index].id);
}

/** Resolve or reopen a submitted review's comment in place. Resolving
 * closes its thread — but only once the request lands, so a failed resolve
 * leaves the thread open with its error where it can be read. */
async function toggleResolved(commentId: number, resolved: boolean) {
  const entry = changeCommentEntries.value.find((item) => item.comment.id === commentId);
  if (!entry) return;
  try {
    const comment = await setReviewCommentResolution(entry.review.id, commentId, resolved);
    const index = entry.review.comments.findIndex((item) => item.id === commentId);
    if (index >= 0) entry.review.comments.splice(index, 1, comment);
    delete commentErrors[commentId];
    if (resolved) {
      activeComments.delete(commentId);
      closedByUser.add(commentId);
    } else {
      // A reopened thread is live again: it opens, and the reviewer's
      // earlier collapse no longer pins it shut.
      closedByUser.delete(commentId);
      activeComments.add(commentId);
    }
  } catch (cause) {
    commentErrors[commentId] = (cause as Error).message;
  }
}

watch(rev, () => void load());
onMounted(load);

// --- Live review updates ----------------------------------------------------
// The reviewed session answers through its own MCP tool and operators reply
// from elsewhere; both arrive as events. A burst of them coalesces into one
// reload — a draft controller reconciles what's already in flight, and an
// unchanged version keeps every diff mounted, so the composer never blinks.
const events = openSessionEvents(props.id);
let refreshTimer: ReturnType<typeof setTimeout> | null = null;
for (const kind of [
  'review_submitted',
  'review_delivery',
  'review_comment_resolved',
  'review_comment_replied',
]) {
  events.on(kind, () => {
    if (refreshTimer != null) clearTimeout(refreshTimer);
    refreshTimer = setTimeout(() => {
      refreshTimer = null;
      void load();
    }, 250);
  });
}
onBeforeUnmount(() => {
  if (refreshTimer != null) clearTimeout(refreshTimer);
  events.close();
});

/** This file's pending-comment count. */
function pendingFor(file: ChangeFile): number {
  const key = file.path.bytes;
  return (
    draft.value?.comments.filter(
      (comment) =>
        comment.anchor_kind === 'change' &&
        inViewScope(comment) &&
        (comment.anchor as ChangeAnchor).path.bytes === key,
    ).length ?? 0
  );
}

interface AsideNode {
  name: string;
  /** The full path: the segment chain down to this node. */
  path: string;
  folder?: AsideNode[];
  file?: { entry: ChangeFile; pending: number };
  /** Folder rollups: total descendant files and their pending comments. */
  count?: number;
  pending?: number;
}

/** The change set folded into a directory tree — one order with the diff
 * pane itself: the builder sorts each level so its depth-first walk lists
 * exactly the files in the served order the pane renders below. */
const asideTree = computed<AsideNode[]>(() => {
  const tree = buildChangeFileTree(changes.value?.files ?? [], (file) => file.path.display);
  const decorate = (nodes: FileTreeNode<ChangeFile>[]): AsideNode[] =>
    nodes.map((node) => {
      if (!node.folder) {
        const pending = pendingFor(node.file!);
        return {
          name: node.name,
          path: node.path,
          file: { entry: node.file!, pending },
          pending,
        };
      }
      const folder = decorate(node.folder);
      return {
        name: node.name,
        path: node.path,
        folder,
        count: folder.reduce((sum, child) => sum + (child.folder ? (child.count ?? 0) : 1), 0),
        pending: folder.reduce((sum, child) => sum + (child.pending ?? 0), 0),
      };
    });
  return decorate(tree);
});

interface AsideRow {
  depth: number;
  node: AsideNode;
}

/** The tree flattened for rendering: filter-narrowed, and collapsed folders
 * hide their children (filtering forces everything matched open). */
const asideRows = computed<AsideRow[]>(() => {
  const needle = asideFilter.value.trim().toLowerCase();
  const matches = (node: AsideNode) =>
    !needle || node.name.toLowerCase().includes(needle) || node.path.toLowerCase().includes(needle);
  const walk = (nodes: AsideNode[], depth: number): AsideRow[] => {
    const rows: AsideRow[] = [];
    for (const node of nodes) {
      if (node.file) {
        if (matches(node)) rows.push({ depth, node });
      } else {
        const children = walk(node.folder!, depth + 1);
        if (!children.length) continue;
        rows.push({ depth, node });
        if (!needle && collapsedFolders.has(node.path)) continue;
        rows.push(...children);
      }
    }
    return rows;
  };
  return walk(asideTree.value, 0);
});

/** Reveals and scrolls to a file's diff from the aside. The scroll targets
 * the file's header, not its diff body: the sticky filename bar would cover
 * the body's first lines otherwise. The rail highlights the target right
 * away rather than waiting for the scroller to catch up. */
function jumpTo(file: ChangeFile) {
  const key = fileKey(file);
  collapsed.delete(key);
  expandedByUser.add(key);
  mounted.add(key);
  activeFileKey.value = key;
  void nextTick(() =>
    document
      .querySelector(`[data-file-key="${key}"]`)
      ?.closest('article')
      ?.scrollIntoView({ block: 'start' }),
  );
}

/** A snapshot-scope switch waiting on the composer's discard confirm. */
const deferredScopeNav = ref<(() => void) | null>(null);

/** Switches review scope, unless a typed inline comment needs the discard
 * confirm first — the same never-silently-destroy rule the composer's own
 * cancel follows. Unguarded paths (a deep link, the Commits tab) still fall
 * back to the load-time drop. */
function switchScope(navigate: () => void) {
  if (pending.value?.body.trim() && pending.value.reanchorId == null) {
    // The confirm lives in the composer's widget: make sure it can render —
    // a collapsed file would hide the decision and stall the navigation
    // invisibly.
    collapsed.delete(pending.value.fileKey);
    mounted.add(pending.value.fileKey);
    deferredScopeNav.value = navigate;
    deferredWidgetClick.value = null;
    confirmDiscard.value = true;
    void nextTick(() => requestAnimationFrame(() => discardButton.value?.focus()));
    return;
  }
  navigate();
}

/** Drops the `?rev` scoping and returns to the whole branch state. */
function reviewAllChanges() {
  switchScope(() => void router.push(`/s/${props.id}/changes`));
}

// --- The commit picker: scope the review to one commit from the header -----
const pickerOpen = ref(false);
const pickerCommits = ref<SessionCommit[] | null>(null);
/** Files with unstaged edits — the row exists only when there is work to show. */
const pickerUnstaged = ref(0);
const pickerLoading = ref(false);
const pickerError = ref('');
const pickerEl = ref<HTMLElement | null>(null);
const pickerToggleEl = ref<HTMLButtonElement | null>(null);

async function loadPicker() {
  pickerLoading.value = true;
  try {
    const listing = await getSessionCommits(props.id);
    pickerCommits.value = listing.commits;
    pickerUnstaged.value = listing.unstaged_files;
    pickerError.value = '';
  } catch (cause) {
    pickerError.value = (cause as Error).message;
  } finally {
    pickerLoading.value = false;
  }
}

// The overlay sits over page content, so a pointerdown outside it (and
// outside its own toggle) minimizes it back away.
function onPickerDocPointerDown(event: PointerEvent) {
  const target = event.target as Node;
  if (pickerEl.value?.contains(target) || pickerToggleEl.value?.contains(target)) return;
  pickerOpen.value = false;
}

watch(pickerOpen, (open) => {
  document.removeEventListener('pointerdown', onPickerDocPointerDown);
  if (open) {
    document.addEventListener('pointerdown', onPickerDocPointerDown);
    void loadPicker();
  }
});
onBeforeUnmount(() => document.removeEventListener('pointerdown', onPickerDocPointerDown));

function reviewFromPicker(oid: string) {
  pickerOpen.value = false;
  switchScope(() => void router.push(`/s/${props.id}/changes?rev=${oid}`));
}

function reviewUnstagedFromPicker() {
  pickerOpen.value = false;
  switchScope(() => void router.push(`/s/${props.id}/changes?rev=unstaged`));
}

function reviewAllFromPicker() {
  pickerOpen.value = false;
  reviewAllChanges();
}
</script>

<template>
  <section
    class="relative flex h-full min-h-0 flex-col overflow-hidden"
    data-testid="changes-panel"
  >
    <header class="flex flex-wrap items-center gap-2 border-b border-line px-3 py-2">
      <!-- A fixed width keeps the row from shifting when the glyph swaps. -->
      <button
        type="button"
        class="btn-secondary w-8 px-0 py-1 text-xs"
        data-testid="changes-aside-toggle"
        :aria-pressed="asideOpen"
        title="Expand file tree"
        @click="asideOpen = !asideOpen"
      >
        {{ asideOpen ? '✕' : '◫' }}
      </button>
      <div class="relative min-w-0 flex-1">
        <h2 class="text-sm font-semibold text-fg">Changes</h2>
        <div class="flex items-center gap-2">
          <p
            v-if="changes?.base.state === 'available'"
            class="truncate font-mono text-2xs text-faint"
          >
            {{ changes.base.reference }} · {{ changes.base.oid.slice(0, 10) }}
          </p>
          <button
            v-if="changes"
            ref="pickerToggleEl"
            type="button"
            class="btn-secondary px-2 py-0.5 text-2xs"
            data-testid="changes-commit-scope-button"
            title="Pick which commit the review covers"
            @click="pickerOpen = !pickerOpen"
          >
            {{
              rev === 'unstaged'
                ? 'Unstaged changes'
                : rev
                  ? `Commit ${rev.slice(0, 10)}`
                  : 'All commits'
            }}
          </button>
        </div>

        <!-- The commit picker: scope the review to one commit's own changes.
             -->
        <div
          v-if="pickerOpen"
          ref="pickerEl"
          class="absolute left-0 top-full z-30 mt-1 w-[min(28rem,calc(100vw-1.5rem))] rounded-lg border border-line bg-surface shadow-xl"
          data-testid="changes-commit-picker"
        >
          <div class="flex items-center justify-between border-b border-line px-3 py-2">
            <button
              type="button"
              class="btn-primary px-2.5 py-1 text-xs"
              data-testid="changes-review-all"
              @click="reviewAllFromPicker"
            >
              Review all commits
            </button>
            <button
              type="button"
              class="btn-secondary px-2 py-1 text-xs"
              aria-label="Close the commit picker"
              @click="pickerOpen = false"
            >
              ✕
            </button>
          </div>
          <p class="border-b border-line px-3 py-2 text-xs font-semibold text-fg">
            Select a commit to review
          </p>
          <div class="max-h-[min(50vh,26rem)] overflow-y-auto">
            <div
              v-if="!pickerLoading && !pickerError && pickerUnstaged > 0"
              class="flex items-stretch border-b border-line bg-subtle/40"
              data-testid="changes-unstaged-row"
            >
              <button
                type="button"
                class="flex min-w-0 flex-1 items-baseline gap-3 px-3 py-2 text-left hover:bg-subtle"
                title="Review the working tree's unstaged edits"
                @click="reviewUnstagedFromPicker"
              >
                <code class="shrink-0 font-mono text-2xs text-faint">worktree</code>
                <span class="min-w-0 flex-1 truncate text-xs text-fg">Unstaged changes</span>
                <span class="shrink-0 text-2xs text-muted">
                  {{ pickerUnstaged }} file{{ pickerUnstaged === 1 ? '' : 's' }} not yet staged
                </span>
              </button>
              <button
                type="button"
                class="btn-secondary mr-1 shrink-0 self-center px-2 py-0.5 text-2xs"
                data-testid="changes-unstaged-review-button"
                title="Review unstaged changes in Code Review"
                @click="reviewUnstagedFromPicker"
              >
                Review
              </button>
            </div>
            <p v-if="pickerLoading" class="p-3 text-sm text-muted">Loading commits…</p>
            <p
              v-else-if="pickerError"
              class="m-3 rounded bg-block-soft p-2 text-xs text-block"
              role="alert"
            >
              {{ pickerError }}
            </p>
            <CommitList
              v-else-if="pickerCommits?.length"
              :commits="pickerCommits"
              @review="reviewFromPicker"
            />
            <p v-else class="p-3 text-sm text-muted">No commits on this branch beyond its base.</p>
          </div>
        </div>
      </div>
      <ReviewTray
        :floating="false"
        :reviews="reviews"
        :draft="draft"
        :open="trayOpen"
        :overall-note="overallNote"
        :summary-saving="summarySaving || summaryDirty"
        :acknowledge-outdated="acknowledgeOutdated"
        :error="trayError"
        :layout-busy="false"
        :submitting="submitting"
        :discarding="discarding"
        :delivery-errors="deliveryErrors"
        :continuation="continuation"
        subject-label="changes"
        :discard-action="discardDraft"
        @update:open="trayOpen = $event"
        @update:overall-note="editOverall"
        @update:acknowledge-outdated="acknowledgeOutdated = $event"
        @navigate="navigate"
        @focus-comment="focusComment"
        @save-overall="saveOverall"
        @retarget="retarget"
        @submit="submit"
        @retry="retryDelivery"
      />
      <span v-if="changes" class="text-xs text-muted">
        {{ changes.totals.files }} files · +{{ changes.totals.additions }} −{{
          changes.totals.deletions
        }}
      </span>
      <button type="button" class="btn-secondary px-2 py-1 text-xs" @click="load">Refresh</button>
    </header>
    <p
      v-if="rev && rev !== 'unstaged' && changes"
      class="flex flex-wrap items-center gap-2 border-b border-line bg-subtle/50 px-3 py-1.5 text-xs text-muted"
      data-testid="changes-commit-scope"
    >
      Reviewing commit <code class="font-mono text-fg">{{ rev.slice(0, 10) }}</code> — its own
      changes only.
      <button type="button" class="btn-secondary px-2 py-0.5 text-2xs" @click="reviewAllChanges">
        All changes
      </button>
    </p>
    <p
      v-if="rev === 'unstaged' && changes"
      class="flex flex-wrap items-center gap-2 border-b border-line bg-subtle/50 px-3 py-1.5 text-xs text-muted"
      data-testid="changes-unstaged-scope"
    >
      Reviewing unstaged changes — the working tree's edits that are not staged yet. Staged and
      committed work stays in the other scopes.
      <button type="button" class="btn-secondary px-2 py-0.5 text-2xs" @click="reviewAllChanges">
        All changes
      </button>
    </p>

    <p v-if="error" class="m-3 rounded bg-block-soft p-2 text-xs text-block" role="alert">
      {{ error }}
    </p>
    <p
      v-else-if="changes?.base.state === 'unavailable'"
      class="m-3 rounded border border-line p-3 text-sm text-muted"
    >
      {{ baseProblem }}
    </p>
    <div v-else class="flex min-h-0 flex-1">
      <aside
        v-if="asideOpen"
        ref="asideEl"
        class="shrink-0 overflow-y-auto border-r border-line"
        :style="{ width: `${asideWidth}px` }"
        aria-label="Change files"
        data-testid="changes-aside"
      >
        <div class="sticky top-0 z-10 border-b border-line bg-surface p-2">
          <input
            v-model="asideFilter"
            type="search"
            placeholder="Filter files…"
            class="w-full rounded border border-line bg-input px-2 py-1 text-xs text-fg outline-none focus:border-accent"
            data-testid="changes-aside-filter"
          />
        </div>
        <button
          v-for="row in asideRows"
          :key="row.node.path"
          type="button"
          class="flex w-full items-center gap-1.5 py-1 pr-2 text-left text-2xs hover:bg-subtle"
          :class="
            row.node.file && fileKey(row.node.file.entry) === activeFileKey
              ? 'bg-subtle text-fg'
              : ''
          "
          :style="{ paddingLeft: `${row.depth * 12 + 8}px` }"
          :data-aside-folder="row.node.folder ? row.node.path : undefined"
          :data-aside-file="row.node.file ? row.node.file.entry.path.display : undefined"
          :title="row.node.path"
          @click="row.node.folder ? toggleFolder(row.node.path) : jumpTo(row.node.file!.entry)"
        >
          <span v-if="row.node.folder" class="w-3 shrink-0 text-faint" aria-hidden="true">
            {{ collapsedFolders.has(row.node.path) && !asideFilter.trim() ? '▸' : '▾' }}
          </span>
          <span
            v-else
            class="shrink-0 rounded bg-subtle px-1 uppercase text-muted"
            :title="row.node.file!.entry.status"
          >
            {{ row.node.file!.entry.status.slice(0, 1) }}
          </span>
          <span class="min-w-0 flex-1 truncate" :class="row.node.folder ? 'text-muted' : 'text-fg'">
            {{ row.node.name }}
          </span>
          <span
            v-if="row.node.pending"
            class="shrink-0 rounded-full bg-accent/20 px-1.5 text-2xs text-accent"
            title="Pending comments"
          >
            {{ row.node.pending }}
          </span>
          <span v-if="row.node.file" class="shrink-0 font-mono text-2xs text-muted"
            >+{{ row.node.file.entry.additions ?? '–' }} −{{
              row.node.file.entry.deletions ?? '–'
            }}</span
          >
          <span v-else class="shrink-0 text-2xs text-faint">{{ row.node.count }}</span>
        </button>
        <p v-if="!asideRows.length" class="p-3 text-center text-2xs text-faint">No files match.</p>
      </aside>
      <div
        v-if="asideOpen"
        class="w-1 shrink-0 cursor-col-resize bg-line hover:bg-accent"
        title="Drag to resize the file rail"
        data-testid="changes-aside-resize"
        @mousedown="startAsideDrag"
      ></div>
      <div ref="diffScrollEl" class="min-h-0 min-w-0 flex-1 overflow-auto">
        <p v-if="loading && !changes" class="p-3 text-sm text-muted">Loading changes…</p>
        <p v-else-if="changes && !changes.files.length" class="p-3 text-sm text-muted">
          No branch or worktree changes.
        </p>
        <p v-if="changes?.truncated" class="m-3 rounded bg-block-soft p-2 text-xs text-block">
          This response reached its explicit display bounds. Refresh after narrowing the change set;
          the version still covers all final bytes when available.
        </p>

        <section
          v-if="outdatedComments.length"
          class="border-b border-line"
          aria-label="Outdated comments"
          data-testid="outdated-comments"
        >
          <button
            type="button"
            class="flex w-full items-center gap-2 px-3 py-2 text-left text-sm hover:bg-subtle"
            :aria-expanded="outdatedOpen"
            data-testid="outdated-comments-toggle"
            @click="outdatedOpen = !outdatedOpen"
          >
            <span class="w-4 text-faint" aria-hidden="true">{{ outdatedOpen ? '▾' : '▸' }}</span>
            <span>Outdated comments</span>
            <span
              class="rounded-full bg-accent/20 px-1.5 text-2xs text-accent"
              title="Outdated comments"
            >
              {{ outdatedComments.length }}
            </span>
          </button>
          <template v-if="outdatedOpen">
            <p class="px-3 pb-2 text-2xs text-faint">
              Comments anchored to another snapshot, or to lines this view no longer shows. Drafts
              still submit with the review; ones written against this snapshot can be re-anchored
              onto a current line, and submitted ones stay for the record and can be resolved.
            </p>
            <div
              v-for="entry in outdatedComments"
              :key="entry.comment.id"
              class="border-t border-line bg-code p-2"
            >
              <p class="mb-1 truncate font-mono text-2xs text-faint">
                {{ (entry.comment.anchor as ChangeAnchor).path.display }} ·
                {{ (entry.comment.anchor as ChangeAnchor).side }}
                {{ (entry.comment.anchor as ChangeAnchor).start_line }}–{{
                  (entry.comment.anchor as ChangeAnchor).end_line
                }}
              </p>
              <ReviewCommentCard
                :review="entry.review"
                :comment="entry.comment"
                :active="activeComments.has(entry.comment.id)"
                :reanchoring="reanchorComment === entry.comment.id"
                :error="commentErrors[entry.comment.id] ?? ''"
                :delete-action="removeComment"
                :reply-action="replyToComment"
                :reanchorable="inViewScope(entry.comment)"
                @focus="openComment"
                @close="closeComment"
                @edit="editComment"
                @reanchor="reanchorComment = $event"
                @cancel-reanchor="reanchorComment = null"
                @resolution="toggleResolved"
              />
            </div>
          </template>
        </section>

        <article
          v-for="file in changes?.files"
          :key="file.path.bytes"
          :ref="(el) => observeActiveFile(el, file)"
          class="border-b border-line"
        >
          <button
            type="button"
            class="sticky top-0 z-10 flex w-full items-center gap-2 border-b border-line bg-surface px-3 py-2 text-left hover:bg-subtle"
            :aria-expanded="isExpanded(file)"
            @click="toggleFile(file)"
          >
            <span class="w-4 text-faint">{{ isExpanded(file) ? '▾' : '▸' }}</span>
            <span class="rounded bg-subtle px-1.5 py-0.5 text-2xs uppercase text-muted">
              {{ file.status }}
            </span>
            <code class="min-w-0 flex-1 truncate text-xs">{{ file.path.display }}</code>
            <span class="text-2xs text-faint">{{ file.sources.join(' · ') }}</span>
            <span class="font-mono text-2xs text-muted"
              >+{{ file.additions ?? '–' }} −{{ file.deletions ?? '–' }}</span
            >
          </button>

          <div v-if="isExpanded(file)" class="overflow-x-auto bg-code text-xs">
            <p v-if="file.content !== 'text'" class="px-4 py-3 font-mono text-muted">
              {{ file.content }} content is not rendered.
            </p>
            <div v-else :ref="(el) => observeFile(el, file)" :data-file-key="fileKey(file)">
              <DiffViewWithMultiSelect
                v-if="mounted.has(fileKey(file)) && gitDiffData(file)"
                :key="`${fileKey(file)}:${changes?.version}`"
                :data="gitDiffData(file)!"
                :diff-view-mode="DiffModeEnum.Split"
                :diff-view-theme="theme"
                :diff-view-highlight="true"
                :diff-view-add-widget="true"
                :extend-data="extendDataFor(file)"
                :initial-widget-state="widgetStateFor(file)"
                :scope-multi-select-to-hunk="scopeToHunk(file)"
                @on-add-widget-click="(payload) => onAddWidgetClick(file, payload)"
              >
                <template #widget="{ onClose }">
                  <form
                    v-if="pending && pending.fileKey === fileKey(file)"
                    class="m-2 rounded border border-accent bg-surface p-2 text-xs shadow-xl"
                    data-testid="change-comment-composer"
                    @submit.prevent="confirmPending(onClose)"
                  >
                    <template v-if="pending.reanchorId != null">
                      <p class="mb-2 text-2xs text-muted">
                        Move comment #{{ pending.reanchorId }} to
                        {{ pending.anchor.path.display }} · {{ pending.anchor.side }}
                        {{ pending.anchor.start_line }}–{{ pending.anchor.end_line }}?
                      </p>
                      <div class="flex justify-end gap-2">
                        <button
                          type="button"
                          class="btn-secondary px-2 py-1 text-xs"
                          @click="cancelPending(onClose)"
                        >
                          Cancel
                        </button>
                        <button type="submit" class="btn-primary px-2 py-1 text-xs">
                          Move comment here
                        </button>
                      </div>
                    </template>
                    <template v-else-if="confirmDiscard">
                      <p
                        class="mb-2 text-2xs text-block"
                        data-testid="change-comment-discard-confirm"
                      >
                        Discard this pending comment?
                      </p>
                      <div class="flex justify-end gap-2" @keydown.esc.stop.prevent="keepEditing">
                        <button
                          type="button"
                          class="btn-secondary px-2 py-1 text-xs"
                          @click="keepEditing"
                        >
                          Keep editing
                        </button>
                        <button
                          :ref="setDiscardButton"
                          type="button"
                          class="rounded bg-block px-2 py-1 text-xs text-white"
                          @click="cancelPending(onClose)"
                        >
                          Discard comment
                        </button>
                      </div>
                    </template>
                    <template v-else>
                      <p class="mb-1 text-2xs font-semibold uppercase text-accent">
                        {{ pending.anchor.path.display }} · {{ pending.anchor.side }}
                        {{ pending.anchor.start_line }}–{{ pending.anchor.end_line }}
                      </p>
                      <textarea
                        :ref="setComposerInput"
                        v-model="pending.body"
                        :disabled="savingComment"
                        rows="3"
                        class="w-full rounded border border-line bg-input p-2 text-xs"
                        @keydown.ctrl.enter.prevent="confirmPending(onClose)"
                        @keydown.meta.enter.prevent="confirmPending(onClose)"
                        @keydown.esc.prevent="requestCancel(onClose)"
                      ></textarea>
                      <div class="mt-2 flex justify-end gap-2">
                        <button
                          type="button"
                          class="btn-secondary px-2 py-1 text-xs"
                          @click="requestCancel(onClose)"
                        >
                          Cancel
                        </button>
                        <button
                          type="submit"
                          class="btn-primary px-2 py-1 text-xs"
                          :disabled="!pending.body.trim() || savingComment"
                        >
                          {{ savingComment ? 'Saving…' : 'Add pending comment' }}
                        </button>
                      </div>
                      <p
                        v-if="composerError"
                        class="mt-2 rounded bg-block-soft px-2 py-1 text-2xs text-block"
                        role="alert"
                        data-testid="change-composer-error"
                      >
                        {{ composerError }}
                      </p>
                    </template>
                  </form>
                </template>
                <template #extend="{ data }">
                  <div class="space-y-1 bg-surface p-2">
                    <ReviewCommentCard
                      v-for="entry in data as ReviewCommentEntry[]"
                      :key="entry.comment.id"
                      :review="entry.review"
                      :comment="entry.comment"
                      :active="activeComments.has(entry.comment.id)"
                      :reanchoring="reanchorComment === entry.comment.id"
                      :error="commentErrors[entry.comment.id] ?? ''"
                      :delete-action="removeComment"
                      :reply-action="replyToComment"
                      :reanchorable="true"
                      @focus="openComment"
                      @close="closeComment"
                      @edit="editComment"
                      @reanchor="reanchorComment = $event"
                      @cancel-reanchor="reanchorComment = null"
                      @resolution="toggleResolved"
                    />
                  </div>
                </template>
              </DiffViewWithMultiSelect>
              <p
                v-else-if="!mounted.has(fileKey(file))"
                class="px-4 py-8 text-center text-2xs text-faint"
              >
                Loading diff…
              </p>
              <p v-else class="px-4 py-8 text-center text-2xs text-faint">No renderable diff.</p>
            </div>
          </div>
        </article>
      </div>
    </div>

    <p v-if="notice" class="absolute bottom-1 left-3 text-2xs text-accent" role="status">
      {{ notice }}
    </p>
  </section>
</template>
