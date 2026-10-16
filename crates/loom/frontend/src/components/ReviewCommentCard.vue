<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue';
import type { Anchor, ChangeAnchor, Review, ReviewComment } from '../types';
import { timeAgo } from '../lib/time';
import InlineConfirm from './InlineConfirm.vue';
import MarkdownView from './MarkdownView.vue';

const props = defineProps<{
  review: Review;
  comment: ReviewComment;
  active: boolean;
  reanchoring: boolean;
  error: string;
  deleteAction: (commentId: number) => Promise<void>;
  /** Appends one entry to the comment's reply thread and returns the updated
   *  comment; the card splices it into the review it already holds. */
  replyAction: (commentId: number, body: string) => Promise<ReviewComment>;
}>();
const emit = defineEmits<{
  focus: [commentId: number];
  close: [commentId: number];
  edit: [payload: { commentId: number; body: string }];
  reanchor: [commentId: number];
  cancelReanchor: [];
  resolution: [commentId: number, resolved: boolean];
  editing: [payload: { commentId: number; editing: boolean }];
}>();

const body = ref(props.comment.body);
const editing = ref(false);
const saving = ref(false);
const cardEl = ref<HTMLElement | null>(null);
const editEl = ref<HTMLButtonElement | null>(null);
const resolutionEl = ref<HTMLButtonElement | null>(null);
const closeEl = ref<HTMLButtonElement | null>(null);
const textareaEl = ref<HTMLTextAreaElement | null>(null);
watch(editing, (editing) => emit('editing', { commentId: props.comment.id, editing }));
onBeforeUnmount(() => {
  if (editing.value) emit('editing', { commentId: props.comment.id, editing: false });
});

watch(
  () => props.review.status,
  (status) => {
    if (status !== 'draft') {
      saving.value = false;
      editing.value = false;
    }
  },
);

const anchorLabel = computed(() => {
  if (props.comment.anchor_kind === 'change') {
    const anchor = props.comment.anchor as ChangeAnchor;
    const range =
      anchor.start_line === anchor.end_line
        ? `${anchor.side} ${anchor.start_line}`
        : `${anchor.side} ${anchor.start_line}–${anchor.end_line}`;
    return `${anchor.path.display}:${range}`;
  }
  return (props.comment.anchor as Anchor).quote;
});

watch(
  () => props.comment.body,
  (next) => {
    body.value = next;
    if (saving.value) {
      saving.value = false;
      editing.value = false;
      void nextTick(() => editEl.value?.focus());
    }
  },
);

watch(
  () => props.active,
  (active) => {
    if (active) {
      void nextTick(() => {
        (editEl.value ?? resolutionEl.value ?? closeEl.value ?? cardEl.value)?.focus();
      });
    } else {
      editing.value = false;
      saving.value = false;
    }
  },
  { immediate: true },
);

watch(
  () => props.reanchoring,
  (reanchoring, previous) => {
    if (!reanchoring && previous) {
      void nextTick(() => (editEl.value ?? closeEl.value ?? cardEl.value)?.focus());
    }
  },
);

watch(
  () => props.error,
  (error) => {
    if (error && saving.value) {
      saving.value = false;
      void nextTick(() => textareaEl.value?.focus());
    }
  },
);

function beginEdit() {
  editing.value = true;
  void nextTick(() => textareaEl.value?.focus());
}

function save() {
  const next = body.value.trim();
  if (!next) return;
  if (next === props.comment.body) {
    cancelEdit();
    return;
  }
  saving.value = true;
  emit('edit', { commentId: props.comment.id, body: next });
}

function cancelEdit() {
  body.value = props.comment.body;
  saving.value = false;
  editing.value = false;
  void nextTick(() => editEl.value?.focus());
}

// --- The reply thread: an operator joins the conversation the session may
// already have started. One composer, appended oldest-first. ---
const replying = ref(false);
const replyBody = ref('');
const replySaving = ref(false);
const replyError = ref('');
const replyInput = ref<HTMLTextAreaElement | null>(null);

function beginReply() {
  replying.value = true;
  replyBody.value = '';
  replyError.value = '';
  void nextTick(() => replyInput.value?.focus());
}

function cancelReply() {
  replying.value = false;
  replyBody.value = '';
  replyError.value = '';
}

async function submitReply() {
  const text = replyBody.value.trim();
  if (!text || replySaving.value) return;
  replySaving.value = true;
  replyError.value = '';
  try {
    const updated = await props.replyAction(props.comment.id, text);
    const index = props.review.comments.findIndex((item) => item.id === props.comment.id);
    if (index >= 0) props.review.comments.splice(index, 1, updated);
    replying.value = false;
    replyBody.value = '';
  } catch (cause) {
    replyError.value = (cause as Error).message;
  } finally {
    replySaving.value = false;
  }
}

/** A reply's speaker: the reviewed session, or the operator who joined. */
function replySpeaker(reply: ReviewComment['replies'][number]): string {
  return reply.author_kind === 'operator' ? reply.author : 'Session';
}
</script>

<template>
  <button
    v-if="!active"
    type="button"
    class="my-1.5 flex w-full items-center gap-2 rounded border px-2 py-1 text-left text-xs"
    :class="
      review.status === 'draft'
        ? 'border-accent/60 bg-subtle text-fg hover:border-accent'
        : 'border-line bg-subtle/50 text-muted hover:border-accent/60'
    "
    :data-testid="`review-comment-${comment.id}`"
    :data-review-collapsed="comment.id"
    @click.stop="emit('focus', comment.id)"
  >
    <span
      class="inline-flex shrink-0 items-center rounded-full px-1.5 py-0.5 text-2xs font-semibold uppercase tracking-wide"
      :class="review.status === 'draft' ? 'bg-accent text-accent-fg' : 'bg-surface text-faint'"
    >
      {{
        review.status === 'draft'
          ? 'Pending'
          : comment.status === 'resolved'
            ? 'Resolved'
            : review.legacy
              ? 'Earlier thread'
              : 'Submitted'
      }}
    </span>
    <span class="min-w-0 flex-1 truncate">{{ comment.body }}</span>
    <span
      v-if="comment.replies?.length"
      class="shrink-0 rounded-full bg-accent/20 px-1.5 text-2xs text-accent"
      title="Session replies"
    >
      {{ comment.replies.length }}
    </span>
    <span
      v-if="comment.subject_version !== review.subject.current_version"
      class="shrink-0 text-2xs text-block"
    >
      Stale
    </span>
  </button>

  <div
    v-else
    ref="cardEl"
    tabindex="-1"
    class="my-2 rounded border p-2 text-xs ring-1"
    :class="
      review.status === 'draft'
        ? 'border-accent bg-subtle/40 ring-accent/40'
        : 'border-accent bg-subtle/40 ring-accent/30'
    "
    :data-testid="`review-comment-${comment.id}`"
    :data-review-card="comment.id"
    @click.stop
    @keydown.esc.stop.prevent="reanchoring ? emit('cancelReanchor') : emit('close', comment.id)"
  >
    <div class="mb-1.5 flex items-start gap-2">
      <div class="min-w-0 flex-1 truncate italic text-muted" :title="anchorLabel">
        {{ comment.anchor_kind === 'text' ? `“${anchorLabel}”` : anchorLabel }}
      </div>
      <span
        v-if="comment.subject_version !== review.subject.current_version"
        class="shrink-0 rounded bg-block-soft px-1.5 py-0.5 text-2xs font-medium text-block"
      >
        Revision {{ comment.subject_version }} · stale
      </span>
      <span
        v-else-if="comment.status === 'resolved'"
        class="shrink-0 rounded bg-subtle px-1.5 py-0.5 text-2xs font-medium text-muted"
      >
        Resolved
      </span>
    </div>

    <template v-if="review.legacy">
      <div
        v-for="legacyComment in review.comments"
        :key="legacyComment.id"
        class="mt-1 whitespace-pre-wrap text-fg"
      >
        {{ legacyComment.body }}
      </div>
    </template>
    <template v-else-if="review.status === 'draft'">
      <textarea
        v-if="editing"
        ref="textareaEl"
        v-model="body"
        rows="3"
        class="w-full resize-y rounded border border-line bg-input p-1.5 text-xs text-fg outline-none focus:border-accent"
        data-testid="review-comment-edit"
        @keydown.ctrl.enter.prevent="save"
        @keydown.meta.enter.prevent="save"
        @keydown.esc.stop.prevent="cancelEdit"
      ></textarea>
      <!-- A pending comment previews exactly as it will read when the review
           is submitted, so the markdown a reviewer writes is no surprise. -->
      <div v-else class="comment-md chat-prose">
        <MarkdownView :id="review.session_id" path="" :source="comment.body" />
      </div>
    </template>
    <div v-else class="comment-md chat-prose">
      <MarkdownView :id="review.session_id" path="" :source="comment.body" />
    </div>

    <!-- The comment's reply thread — the conversation between the reviewed
         session and the operators, oldest first. Resolving stays with the
         reviewer; replying is everyone's. -->
    <div
      v-if="comment.replies?.length"
      class="mt-2 space-y-1 border-l-2 border-line pl-2"
      data-testid="review-comment-replies"
    >
      <div v-for="reply in comment.replies" :key="reply.id" class="rounded bg-code/60 px-2 py-1">
        <p class="mb-0.5 flex items-center gap-1.5 text-2xs text-faint">
          <span class="rounded bg-subtle px-1 py-0.5 font-semibold uppercase tracking-wide">
            {{ replySpeaker(reply) }}
          </span>
          <span :title="reply.created_at">{{ timeAgo(reply.created_at) }}</span>
        </p>
        <div class="comment-md chat-prose">
          <MarkdownView :id="review.session_id" path="" :source="reply.body" />
        </div>
      </div>
    </div>

    <form
      v-if="replying"
      class="mt-2"
      data-testid="review-reply-composer"
      @submit.prevent="submitReply"
    >
      <textarea
        ref="replyInput"
        v-model="replyBody"
        rows="2"
        class="w-full resize-y rounded border border-line bg-input p-1.5 text-xs text-fg outline-none focus:border-accent"
        @keydown.ctrl.enter.prevent="submitReply"
        @keydown.meta.enter.prevent="submitReply"
        @keydown.esc.stop.prevent="cancelReply"
      ></textarea>
      <div class="mt-1 flex justify-end gap-1.5">
        <button type="button" class="btn-secondary px-2 py-1 text-2xs" @click.stop="cancelReply">
          Cancel
        </button>
        <button
          type="submit"
          class="btn-primary px-2 py-1 text-2xs"
          :disabled="!replyBody.trim() || replySaving"
        >
          {{ replySaving ? 'Sending…' : 'Reply' }}
        </button>
      </div>
      <p
        v-if="replyError"
        class="mt-1 rounded bg-block-soft px-2 py-1 text-2xs text-block"
        role="alert"
      >
        {{ replyError }}
      </p>
    </form>

    <div class="mt-2 flex flex-wrap items-center gap-1.5">
      <template v-if="review.status === 'draft'">
        <button
          v-if="editing"
          type="button"
          class="btn-primary px-2 py-1 text-2xs"
          :disabled="saving"
          @click.stop="save"
        >
          {{ saving ? 'Saving…' : 'Save' }}
        </button>
        <button
          v-if="editing"
          type="button"
          class="btn-secondary px-2 py-1 text-2xs"
          @click.stop="cancelEdit"
        >
          Cancel
        </button>
        <button
          v-if="!editing"
          ref="editEl"
          type="button"
          class="btn-secondary px-2 py-1 text-2xs"
          @click.stop="beginEdit"
        >
          Edit
        </button>
        <button
          type="button"
          class="btn-secondary px-2 py-1 text-2xs"
          @click.stop="reanchoring ? emit('cancelReanchor') : emit('reanchor', comment.id)"
        >
          {{ reanchoring ? 'Cancel re-anchor' : 'Re-anchor' }}
        </button>
        <InlineConfirm
          label="Delete"
          message="Delete this pending comment?"
          confirm-label="Delete comment"
          danger
          :action="() => deleteAction(comment.id)"
          @click.stop
        />
      </template>
      <button
        v-if="!review.legacy && review.status === 'submitted'"
        type="button"
        class="btn-secondary px-2 py-1 text-2xs"
        @click.stop="replying ? cancelReply() : beginReply()"
      >
        {{ replying ? 'Cancel reply' : 'Reply' }}
      </button>
      <button
        v-if="!review.legacy && review.status === 'submitted'"
        ref="resolutionEl"
        type="button"
        class="btn-secondary px-2 py-1 text-2xs"
        @click.stop="emit('resolution', comment.id, comment.status !== 'resolved')"
      >
        {{ comment.status === 'resolved' ? 'Reopen' : 'Resolve' }}
      </button>
      <button
        v-if="review.legacy && comment.status === 'open'"
        ref="resolutionEl"
        type="button"
        class="btn-secondary px-2 py-1 text-2xs"
        @click.stop="emit('resolution', comment.id, true)"
      >
        Resolve
      </button>
      <button
        ref="closeEl"
        type="button"
        class="btn-secondary ml-auto px-2 py-1 text-2xs"
        @click.stop="emit('close', comment.id)"
      >
        Close
      </button>
    </div>
    <p v-if="error" class="mt-2 rounded bg-block-soft px-2 py-1 text-2xs text-block" role="alert">
      {{ error }}
    </p>
    <p v-if="reanchoring" class="mt-2 rounded bg-subtle px-2 py-1 text-2xs text-accent">
      {{
        comment.anchor_kind === 'change'
          ? 'Choose the replacement line in the current changes.'
          : 'Select the replacement text in this artifact, then choose Re-anchor selection.'
      }}
    </p>
  </div>
</template>

<style scoped>
/* MarkdownView's document frame flattened into a comment card: the markdown
   pipeline is shared with the chat surfaces, `chat-prose` supplies their
   density, and only the geometry is tightened here. */
.comment-md :deep(div:has(> .markdown-body)) {
  background: transparent;
  overflow: visible;
  height: auto;
  width: auto;
}
.comment-md :deep(.markdown-body) {
  max-width: none;
  margin: 0;
  padding: 0.125rem 0;
  font-size: 0.8125rem;
}
</style>
