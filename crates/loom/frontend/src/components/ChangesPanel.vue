<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref } from 'vue';
import { DiffModeEnum, DiffViewWithMultiSelect, SplitSide } from '@git-diff-view/vue';
import type { LineRange } from '@git-diff-view/vue';
import '@git-diff-view/vue/styles/diff-view-pure.css';
import {
  addReviewComment,
  createReview,
  deleteReviewComment,
  discardReview,
  getChanges,
  listChangesReviews,
  retryReviewDelivery,
  retargetReviewToCurrent,
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
} from '../types';
import { ReviewDraftController } from '../lib/reviewDraftController';
import { toGitDiffViewData, type GitDiffViewData } from '../lib/gitDiffAdapter';
import { theme } from '../theme';
import ReviewCommentCard from './ReviewCommentCard.vue';
import ReviewTray from './ReviewTray.vue';

const props = defineProps<{ id: string }>();
const changes = ref<ChangeSet | null>(null);
const reviews = ref<Review[]>([]);
const loading = ref(false);
const error = ref('');
const notice = ref('');
// Files start expanded (opt out via `collapsed`), so a diff opens ready to
// read in full; the diff body itself only mounts once scrolled near, so a
// large change set doesn't pay full render/highlight cost up front.
const collapsed = reactive(new Set<string>());
const mounted = reactive(new Set<string>());
let fileObserver: IntersectionObserver | null = null;
const activeComment = ref<number | null>(null);
const reanchorComment = ref<number | null>(null);
const commentErrors = reactive<Record<number, string>>({});
const deliveryErrors = reactive<Record<number, string>>({});
const trayOpen = ref(false);
const trayError = ref('');
const overallNote = ref('');
const summaryDirty = ref(false);
const summarySaving = ref(false);
const acknowledgeOutdated = ref(false);
const submitting = ref(false);
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

const diffDataCache = new Map<string, GitDiffViewData | null>();

const draft = computed(() => reviews.value.find((review) => review.status === 'draft') ?? null);

const baseProblem = computed(() => {
  const base = changes.value?.base;
  if (base?.state !== 'unavailable') return '';
  const reasons: Record<typeof base.reason, string> = {
    unborn_head: 'this worktree has no commits yet',
    missing_base: `no local or remote-tracking ref resolves the base ${base.reference}`,
    no_merge_base: `this branch shares no history with ${base.reference}`,
  };
  return `Changes unavailable: ${reasons[base.reason]}.`;
});

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
    activeComment.value = null;
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
      getChanges(props.id),
      listChangesReviews(props.id),
    ]);
    const nextDraft = nextReviews.find((review) => review.status === 'draft') ?? null;
    if (!controller.acceptRefresh(epoch, nextDraft)) return;
    changes.value = nextChanges;
    reviews.value = nextReviews;
    error.value = '';
  } catch (cause) {
    error.value = (cause as Error).message;
  } finally {
    loading.value = false;
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
  if (collapsed.has(key)) collapsed.delete(key);
  else collapsed.add(key);
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

function observeFile(el: unknown, file: ChangeFile) {
  if (!(el instanceof Element) || mounted.has(fileKey(file))) return;
  ensureFileObserver().observe(el);
}

onBeforeUnmount(() => fileObserver?.disconnect());

function gitDiffData(file: ChangeFile): GitDiffViewData | null {
  const key = `${changes.value?.version ?? ''}:${fileKey(file)}`;
  if (!diffDataCache.has(key)) diffDataCache.set(key, toGitDiffViewData(file));
  return diffDataCache.get(key) ?? null;
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
    const side = range.side as ChangeSide;
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
  oldFile: Record<string, { data: ReviewComment[] }>;
  newFile: Record<string, { data: ReviewComment[] }>;
} {
  const oldFile: Record<string, { data: ReviewComment[] }> = {};
  const newFile: Record<string, { data: ReviewComment[] }> = {};
  for (const comment of draft.value?.comments ?? []) {
    if (comment.anchor_kind !== 'change') continue;
    const anchor = comment.anchor as ChangeAnchor;
    if (anchor.path.bytes !== file.path.bytes) continue;
    const bucket = anchor.side === 'old' ? oldFile : newFile;
    const key = String(anchor.end_line);
    (bucket[key] ??= { data: [] }).data.push(comment);
  }
  return { oldFile, newFile };
}

function onAddWidgetClick(
  file: ChangeFile,
  payload: { lineNumber: number; fromLineNumber?: number; side: SplitSide },
) {
  const version = changes.value?.version;
  if (!version) return;
  const side = sideFromSplitSide(payload.side);
  const start = Math.min(payload.fromLineNumber ?? payload.lineNumber, payload.lineNumber);
  const end = Math.max(payload.fromLineNumber ?? payload.lineNumber, payload.lineNumber);
  const anchor = buildAnchor(file, side, start, end);
  if (!anchor) return;
  const key = fileKey(file);
  pending.value = {
    anchor,
    body: reanchorComment.value == null && pending.value?.fileKey === key ? pending.value.body : '',
    version,
    fileKey: key,
    lineNumber: payload.lineNumber,
    side,
    reanchorId: reanchorComment.value ?? undefined,
  };
  void nextTick(() => composerInput.value?.focus());
}

function cancelPending(onClose: () => void) {
  if (pending.value?.reanchorId != null) reanchorComment.value = null;
  pending.value = null;
  onClose();
}

async function confirmPending(onClose: () => void) {
  if (!pending.value) return;
  if (pending.value.reanchorId != null) {
    await applyReanchor(pending.value.reanchorId, pending.value.anchor);
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
  trayError.value = '';
  try {
    const updated = await controller.command(async (current) => {
      const review =
        current ??
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
    activeComment.value = updated.comments.at(-1)?.id ?? null;
    pending.value = null;
    trayOpen.value = true;
    notice.value = 'Pending comment saved.';
  } catch (cause) {
    trayError.value = mutationMessage(cause);
  } finally {
    savingComment.value = false;
  }
}

async function editComment(payload: { commentId: number; body: string }) {
  try {
    await controller.command((current) => {
      if (!current) throw new Error('Review draft is unavailable.');
      return updateReviewComment(current.id, payload.commentId, {
        expected_revision: current.draft_revision,
        body: payload.body,
      });
    });
  } catch (cause) {
    commentErrors[payload.commentId] = mutationMessage(cause);
  }
}

async function removeComment(commentId: number) {
  try {
    await controller.command((current) => {
      if (!current) throw new Error('Review draft is unavailable.');
      return deleteReviewComment(current.id, commentId, current.draft_revision);
    });
    activeComment.value = null;
  } catch (cause) {
    throw new Error(mutationMessage(cause));
  }
}

async function applyReanchor(commentId: number, anchor: ChangeAnchor) {
  const version = changes.value?.version;
  if (!version) return;
  try {
    await controller.command((current) => {
      if (!current) throw new Error('Review draft is unavailable.');
      return updateReviewComment(current.id, commentId, {
        expected_revision: current.draft_revision,
        subject_version: version,
        anchor_kind: 'change',
        anchor,
      });
    });
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
  if (!draft.value || draft.value.comments.length) return;
  try {
    await controller.command((current) => {
      if (!current) throw new Error('Review draft is unavailable.');
      return retargetReviewToCurrent(current.id, current.draft_revision);
    });
  } catch (cause) {
    trayError.value = mutationMessage(cause);
  }
}

async function submit() {
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

function navigate(direction: number) {
  const comments = draft.value?.comments ?? [];
  if (!comments.length) return;
  const current = comments.findIndex((comment) => comment.id === activeComment.value);
  const next = comments[(current + direction + comments.length) % comments.length];
  activeComment.value = next.id;
  if (next.anchor_kind === 'change') {
    const key = (next.anchor as ChangeAnchor).path.bytes;
    collapsed.delete(key);
    mounted.add(key);
  }
  void nextTick(() =>
    document
      .querySelector(`[data-review-collapsed="${next.id}"], [data-review-card="${next.id}"]`)
      ?.scrollIntoView({ block: 'center' }),
  );
}

onMounted(load);
</script>

<template>
  <section
    class="relative flex h-full min-h-0 flex-col overflow-hidden"
    data-testid="changes-panel"
  >
    <header class="flex flex-wrap items-center gap-2 border-b border-line px-3 py-2">
      <div class="min-w-0 flex-1">
        <h2 class="text-sm font-semibold text-fg">Changes</h2>
        <p
          v-if="changes?.base.state === 'available'"
          class="truncate font-mono text-2xs text-faint"
        >
          {{ changes.base.reference }} · {{ changes.base.oid.slice(0, 10) }}
        </p>
      </div>
      <span v-if="changes" class="text-xs text-muted">
        {{ changes.totals.files }} files · +{{ changes.totals.additions }} −{{
          changes.totals.deletions
        }}
      </span>
      <button type="button" class="btn-secondary px-2 py-1 text-xs" @click="load">Refresh</button>
    </header>

    <p v-if="error" class="m-3 rounded bg-block-soft p-2 text-xs text-block" role="alert">
      {{ error }}
    </p>
    <p
      v-else-if="changes?.base.state === 'unavailable'"
      class="m-3 rounded border border-line p-3 text-sm text-muted"
    >
      {{ baseProblem }}
    </p>
    <div v-else class="min-h-0 flex-1 overflow-auto">
      <p v-if="loading && !changes" class="p-3 text-sm text-muted">Loading changes…</p>
      <p v-else-if="changes && !changes.files.length" class="p-3 text-sm text-muted">
        No branch or worktree changes.
      </p>
      <p v-if="changes?.truncated" class="m-3 rounded bg-block-soft p-2 text-xs text-block">
        This response reached its explicit display bounds. Refresh after narrowing the change set;
        the version still covers all final bytes when available.
      </p>

      <article v-for="file in changes?.files" :key="file.path.bytes" class="border-b border-line">
        <button
          type="button"
          class="flex w-full items-center gap-2 px-3 py-2 text-left hover:bg-subtle"
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
                    Move comment #{{ pending.reanchorId }} to {{ pending.anchor.path.display }} ·
                    {{ pending.anchor.side }} {{ pending.anchor.start_line }}–{{
                      pending.anchor.end_line
                    }}?
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
                <template v-else>
                  <p class="mb-1 text-2xs font-semibold uppercase text-accent">
                    {{ pending.anchor.path.display }} · {{ pending.anchor.side }}
                    {{ pending.anchor.start_line }}–{{ pending.anchor.end_line }}
                  </p>
                  <textarea
                    ref="composerInput"
                    v-model="pending.body"
                    rows="3"
                    class="w-full rounded border border-line bg-input p-2 text-xs"
                  ></textarea>
                  <div class="mt-2 flex justify-end gap-2">
                    <button
                      type="button"
                      class="btn-secondary px-2 py-1 text-xs"
                      @click="cancelPending(onClose)"
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
                </template>
              </form>
            </template>
            <template #extend="{ data }">
              <div class="space-y-1 bg-surface p-2">
                <ReviewCommentCard
                  v-for="comment in data as ReviewComment[]"
                  :key="comment.id"
                  :review="draft!"
                  :comment="comment"
                  :active="activeComment === comment.id"
                  :reanchoring="reanchorComment === comment.id"
                  :error="commentErrors[comment.id] ?? ''"
                  :delete-action="removeComment"
                  @focus="activeComment = $event"
                  @close="activeComment = null"
                  @edit="editComment"
                  @reanchor="reanchorComment = $event"
                  @cancel-reanchor="reanchorComment = null"
                />
              </div>
            </template>
          </DiffViewWithMultiSelect>
          <p v-else class="px-4 py-8 text-center text-2xs text-faint">Loading diff…</p>
          </div>
        </div>
      </article>
    </div>

    <ReviewTray
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
      subject-label="changes"
      :discard-action="discardDraft"
      @update:open="trayOpen = $event"
      @update:overall-note="editOverall"
      @update:acknowledge-outdated="acknowledgeOutdated = $event"
      @navigate="navigate"
      @focus-comment="activeComment = $event"
      @save-overall="saveOverall"
      @retarget="retarget"
      @submit="submit"
      @retry="retryDelivery"
    />
    <p v-if="notice" class="absolute bottom-1 left-3 text-2xs text-accent" role="status">
      {{ notice }}
    </p>
  </section>
</template>
