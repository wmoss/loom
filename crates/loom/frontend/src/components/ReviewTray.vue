<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, useId, watch } from 'vue';
import type { Review } from '../types';
import InlineConfirm from './InlineConfirm.vue';

const props = defineProps<{
  reviews: Review[];
  draft: Review | null;
  open: boolean;
  overallNote: string;
  summarySaving: boolean;
  acknowledgeOutdated: boolean;
  error: string;
  layoutBusy: boolean;
  submitting: boolean;
  discarding: boolean;
  deliveryErrors: Record<number, string>;
  /** The creator's submitted review with undelivered follow-up work: its
   *  pending comments and replies queue beside a draft's — one surface, one
   *  submit. Null when a draft is open (the draft is the surface) or when
   *  nothing is pending. */
  continuation: Review | null;
  subjectLabel: string;
  discardAction: () => Promise<void>;
  /** Floats bottom-right and expands upward. Set false to sit inline (e.g.
   *  in a header) with its panel popping down below it instead; that pop-down
   *  panel overlays page content and minimizes on outside clicks, while the
   *  floating dock stays put. Pass it at every call site: an absent Boolean
   *  prop casts to false, never undefined. */
  floating?: boolean;
}>();
const emit = defineEmits<{
  'update:open': [value: boolean];
  'update:overallNote': [value: string];
  'update:acknowledgeOutdated': [value: boolean];
  navigate: [direction: number];
  focusComment: [commentId: number];
  saveOverall: [];
  retarget: [];
  submit: [];
  retry: [review: Review];
}>();
const toggleEl = ref<HTMLButtonElement | null>(null);
const rootEl = ref<HTMLElement | null>(null);
const overallInput = ref<HTMLTextAreaElement | null>(null);
const overallId = `review-overall-${useId()}`;
defineExpose({ focusToggle: () => toggleEl.value?.focus() });

const isFloating = computed(() => props.floating !== false);

// The pop-down panel overlays page content, so a pointerdown outside the tray
// minimizes it back to its toggle. The textarea's blur never fires when its
// v-if removal unfocuses it, so flush the note through the save handler first.
function closeTray() {
  emit('saveOverall');
  emit('update:open', false);
}

function onDocPointerDown(event: PointerEvent) {
  if (rootEl.value?.contains(event.target as Node)) return;
  closeTray();
}

watch(
  () => props.open,
  (open) => {
    document.removeEventListener('pointerdown', onDocPointerDown);
    if (open && !isFloating.value) document.addEventListener('pointerdown', onDocPointerDown);
    // Focus the note once the tray opens — retried over the next half-second
    // because a single nextTick+rAF attempt silently no-ops whenever the
    // textarea mounts or re-renders a frame later (the reopen path under
    // load), and nothing else ever retries.
    if (open)
      void nextTick(() => {
        const attempt = (remaining: number) => {
          const input = overallInput.value;
          if (input && document.activeElement !== input) input.focus();
          if (remaining > 0) requestAnimationFrame(() => attempt(remaining - 1));
        };
        requestAnimationFrame(() => attempt(30));
      });
  },
);
onBeforeUnmount(() => document.removeEventListener('pointerdown', onDocPointerDown));

const failed = computed(() =>
  props.reviews.filter(
    (review) =>
      review.status === 'submitted' && !review.legacy && review.delivery_state === 'failed',
  ),
);
const recent = computed(
  () =>
    [...props.reviews]
      .filter((review) => review.status === 'submitted' && !review.legacy)
      .sort((a, b) => b.id - a.id)[0] ?? null,
);

// The open surface — a draft, or a submitted review carrying undelivered
// follow-up work. Pending rows render identically for both: a new comment
// and a reply look the same because they are the same thing here, words the
// session has not received yet, queued until one submit sends them.
const surface = computed(() => props.draft ?? props.continuation);

const pendingRows = computed(() => {
  const review = surface.value;
  if (!review) return [];
  const rows = review.comments
    .filter((comment) => comment.status === 'pending')
    .map((comment) => ({
      key: `c${comment.id}`,
      body: comment.body,
      reply: false,
      focusId: comment.id,
      stale: comment.subject_version !== review.subject.current_version,
    }));
  // Replies only exist on submitted reviews; the ones past the delivered
  // cursor are still the reviewer's side of the loop.
  for (const comment of review.comments) {
    for (const reply of comment.replies) {
      if (reply.author_kind === 'operator' && reply.id > review.reply_upto) {
        rows.push({
          key: `r${reply.id}`,
          body: reply.body,
          reply: true,
          focusId: comment.id,
          stale: false,
        });
      }
    }
  }
  return rows;
});

// Cmd/Ctrl+Enter in the overall note submits without requiring a blur first —
// `submit` (via the draft controller's freeze) already flushes any dirty note
// before it actually submits, so this only guards the conditions that would
// make the request invalid outright, not "unsaved" itself.
function submitShortcut() {
  if (props.layoutBusy || props.submitting || props.discarding) return;
  if (props.draft) {
    if (
      (!props.draft.comments.length && !props.overallNote.trim()) ||
      (props.draft.outdated && !props.acknowledgeOutdated)
    ) {
      return;
    }
  } else if (!props.continuation || !pendingRows.value.length) {
    if (!props.overallNote.trim()) return;
  }
  emit('submit');
}
</script>

<template>
  <aside
    ref="rootEl"
    :class="
      isFloating
        ? 'absolute bottom-3 right-3 z-20 w-[min(28rem,calc(100%-1.5rem))] rounded-lg border border-line bg-surface shadow-xl'
        : 'relative shrink-0'
    "
    data-testid="review-tray"
    aria-label="Review tray"
  >
    <div
      class="flex min-h-8 items-center gap-1.5"
      :class="isFloating ? 'min-h-10 px-2' : 'rounded border border-line bg-surface px-1.5'"
    >
      <button
        ref="toggleEl"
        type="button"
        class="min-w-0 flex-1 px-1 py-1.5 text-left text-xs font-semibold text-fg"
        data-testid="review-tray-toggle"
        :aria-expanded="open"
        @click="emit('update:open', !open)"
      >
        <template v-if="surface">
          Review · {{ pendingRows.length }} pending
          <span v-if="draft?.outdated" class="ml-1 text-block">· stale</span>
          <span v-if="failed.length" class="ml-1 text-block">
            · {{ failed.length }} delivery failed
          </span>
        </template>
        <template v-else-if="failed.length">Review delivery · {{ failed.length }} failed</template>
        <template v-else-if="recent">Review submitted · {{ recent.delivery_state }}</template>
        <template v-else>Review {{ subjectLabel }}</template>
      </button>
      <template v-if="draft?.comments.length">
        <button
          type="button"
          class="btn-secondary px-2 py-1 text-xs"
          aria-label="Previous pending comment"
          @click="emit('navigate', -1)"
        >
          ↑
        </button>
        <button
          type="button"
          class="btn-secondary px-2 py-1 text-xs"
          aria-label="Next pending comment"
          @click="emit('navigate', 1)"
        >
          ↓
        </button>
      </template>
      <button
        type="button"
        class="btn-secondary px-2 py-1 text-xs"
        :aria-label="open ? 'Collapse review tray' : 'Expand review tray'"
        @click="emit('update:open', !open)"
      >
        {{ open ? '▾' : '▴' }}
      </button>
    </div>

    <div
      v-if="open"
      class="max-h-[min(60vh,34rem)] overflow-auto p-3"
      :class="
        isFloating
          ? 'border-t border-line'
          : 'absolute right-0 top-full z-30 mt-1 w-[min(28rem,calc(100vw-1.5rem))] rounded-lg border border-line bg-surface shadow-xl'
      "
    >
      <div v-if="failed.length" class="mb-3 space-y-2" aria-label="Failed review deliveries">
        <div
          v-for="item in failed"
          :key="item.id"
          class="rounded border border-block-line bg-block-soft p-2 text-xs"
          :data-testid="`failed-review-delivery-${item.id}`"
        >
          <p class="font-medium text-block">Review {{ item.id }} delivery failed</p>
          <p v-if="item.delivery_error" class="mt-1 text-block">{{ item.delivery_error }}</p>
          <button
            type="button"
            class="btn-primary mt-2 px-2 py-1 text-xs"
            @click="emit('retry', item)"
          >
            Retry delivery
          </button>
          <p
            v-if="deliveryErrors[item.id]"
            class="mt-2 rounded bg-surface/70 px-2 py-1 text-2xs text-block"
            role="alert"
          >
            {{ deliveryErrors[item.id] }}
          </p>
        </div>
      </div>

      <template v-if="surface">
        <p
          v-if="draft?.outdated"
          class="mb-3 rounded border border-block-line bg-block-soft p-2 text-xs text-block"
          data-testid="review-stale-warning"
        >
          This {{ subjectLabel }} changed to {{ draft.subject.current_version }}. Captured anchors
          are preserved; re-anchor them, or acknowledge the older context before submitting.
        </p>
        <button
          v-if="draft?.outdated && !draft.comments.length"
          type="button"
          class="btn-secondary mb-3 px-2 py-1 text-xs"
          data-testid="review-retarget-current"
          :disabled="summarySaving"
          @click="emit('retarget')"
        >
          Move review to current {{ subjectLabel }}
        </button>

        <div class="space-y-1.5" aria-label="Pending comments">
          <button
            v-for="(row, index) in pendingRows"
            :key="row.key"
            type="button"
            class="flex w-full items-center gap-2 rounded border border-line px-2 py-1.5 text-left text-xs hover:border-accent"
            :data-testid="
              row.reply ? `pending-reply-row-${row.key}` : `pending-comment-row-${row.key}`
            "
            @click="emit('focusComment', row.focusId)"
          >
            <span class="shrink-0 font-mono text-2xs text-faint">{{ index + 1 }}</span>
            <span class="min-w-0 flex-1 truncate text-fg">
              <span v-if="row.reply" class="mr-1 text-faint" aria-hidden="true">↳</span
              >{{ row.body }}
            </span>
            <span v-if="row.stale" class="shrink-0 text-2xs text-block"> stale </span>
          </button>
        </div>

        <label v-if="draft" class="mt-3 block text-xs font-medium text-muted" :for="overallId">
          Overall note <span class="font-normal text-faint">(optional)</span>
        </label>
        <textarea
          :id="overallId"
          ref="overallInput"
          :value="overallNote"
          rows="3"
          class="mt-1 w-full resize-y rounded border border-line bg-input p-2 text-xs text-fg outline-none focus:border-accent"
          :placeholder="`Feedback that applies to the ${subjectLabel} as a whole…`"
          data-testid="review-overall-note"
          :disabled="layoutBusy || submitting || discarding"
          @input="emit('update:overallNote', ($event.target as HTMLTextAreaElement).value)"
          @blur="emit('saveOverall')"
          @keydown.esc.prevent="closeTray"
          @keydown.ctrl.enter.prevent="submitShortcut"
          @keydown.meta.enter.prevent="submitShortcut"
        ></textarea>
        <p v-if="summarySaving" class="mt-1 text-2xs text-faint" aria-live="polite">
          Saving overall note…
        </p>

        <label
          v-if="draft?.outdated"
          class="mt-2 flex cursor-pointer items-start gap-2 rounded bg-subtle/50 p-2 text-xs text-muted"
        >
          <input
            :checked="acknowledgeOutdated"
            type="checkbox"
            class="mt-0.5"
            data-testid="review-stale-ack"
            @change="
              emit('update:acknowledgeOutdated', ($event.target as HTMLInputElement).checked)
            "
          />
          Submit against the captured version intentionally.
        </label>

        <div v-if="draft" class="mt-3 border-t border-line pt-3">
          <p class="mb-2 text-2xs font-semibold uppercase tracking-wide text-faint">
            Conversation feedback preview
          </p>
          <pre
            class="max-h-32 overflow-auto whitespace-pre-wrap rounded bg-code p-2 text-xs text-code-fg"
            >{{ draft.message }}</pre>
        </div>

        <div class="mt-3 flex flex-wrap items-center gap-2">
          <p
            v-if="error"
            class="w-full rounded bg-block-soft px-2 py-1 text-2xs text-block"
            role="alert"
          >
            {{ error }}
          </p>
          <InlineConfirm
            v-if="draft"
            label="Discard draft"
            :message="`Discard the overall note and all ${draft.comments.length} pending comments?`"
            confirm-label="Discard draft"
            danger
            :disabled="layoutBusy || submitting || discarding"
            :action="discardAction"
          />
          <button
            type="button"
            class="btn-primary ml-auto px-3 py-1.5 text-xs"
            data-testid="submit-review"
            :disabled="
              submitting ||
              layoutBusy ||
              discarding ||
              (!pendingRows.length && !(draft && overallNote.trim())) ||
              (draft?.outdated && !acknowledgeOutdated)
            "
            @click="emit('submit')"
          >
            {{ submitting ? 'Submitting…' : 'Submit review' }}
          </button>
        </div>
      </template>

      <template v-else-if="!continuation">
        <div
          v-if="recent && !failed.some((review) => review.id === recent?.id)"
          class="mb-3 text-xs text-muted"
        >
          <p class="font-medium text-fg">Review {{ recent.id }} submitted</p>
          <p class="mt-1">
            Delivery: <span class="text-accent">{{ recent.delivery_state }}</span>
          </p>
        </div>
        <label class="block text-xs font-medium text-muted" :for="overallId">
          Start a new review with an overall note
        </label>
        <textarea
          :id="overallId"
          ref="overallInput"
          :value="overallNote"
          rows="3"
          class="mt-1 w-full resize-y rounded border border-line bg-input p-2 text-xs text-fg outline-none focus:border-accent"
          :placeholder="`Feedback that applies to the ${subjectLabel} as a whole…`"
          data-testid="review-overall-note"
          :disabled="layoutBusy || submitting || discarding"
          @input="emit('update:overallNote', ($event.target as HTMLTextAreaElement).value)"
          @blur="emit('saveOverall')"
          @keydown.esc.prevent="closeTray"
          @keydown.ctrl.enter.prevent="submitShortcut"
          @keydown.meta.enter.prevent="submitShortcut"
        ></textarea>
        <p v-if="summarySaving" class="mt-1 text-2xs text-faint" aria-live="polite">
          Saving overall note…
        </p>
        <p
          v-if="error"
          class="mt-2 rounded bg-block-soft px-2 py-1 text-2xs text-block"
          role="alert"
        >
          {{ error }}
        </p>
      </template>
    </div>
  </aside>
</template>
