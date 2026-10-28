<script setup lang="ts">
// The unresolved-comment wayfinding pill: ‹ 2/5 › pinned to the review
// surface's lower-center while any unresolved comment exists. The host
// surface owns the list — document order — and the jump itself (open the
// card, scroll to it); the pill owns the cursor and the count, so the reader
// always knows how many threads are left and where they are. Following a
// reader's own click keeps the counter honest: the cursor tracks the open
// comment, not just the arrows.
import { ref, watch } from 'vue';

const props = defineProps<{
  /** Unresolved comment ids, in the order the surface reads them. */
  targets: number[];
  /** The comment the surface has open right now — the cursor follows it
   *  whenever it is one of the targets. Absent targets are ignored, so a
   *  resolved or stale id never drags the counter away. */
  activeId: number | null;
}>();
const emit = defineEmits<{ jump: [commentId: number] }>();

const cursor = ref(0);

watch(
  () => [props.targets, props.activeId] as const,
  () => {
    if (props.activeId != null) {
      const index = props.targets.indexOf(props.activeId);
      if (index >= 0) cursor.value = index;
    }
    // A list that shrank — someone resolved — can leave the cursor past the
    // end; the nearest thread keeps the reader's place.
    if (cursor.value >= props.targets.length) cursor.value = 0;
  },
  { immediate: true },
);

function jump(step: 1 | -1) {
  if (!props.targets.length) return;
  // The ends wrap: reading past the last thread finds the first again.
  cursor.value = (cursor.value + step + props.targets.length) % props.targets.length;
  emit('jump', props.targets[cursor.value]);
}
</script>

<template>
  <nav
    v-if="targets.length"
    class="review-nav"
    data-testid="review-nav"
    aria-label="Unresolved comments"
  >
    <button
      type="button"
      class="review-nav-arrow"
      :disabled="targets.length < 2"
      aria-label="Previous unresolved comment"
      title="Previous unresolved comment"
      @click="jump(-1)"
    >
      ‹
    </button>
    <span class="review-nav-count" title="Unresolved comments">
      {{ cursor + 1 }} / {{ targets.length }}
    </span>
    <button
      type="button"
      class="review-nav-arrow"
      :disabled="targets.length < 2"
      aria-label="Next unresolved comment"
      title="Next unresolved comment"
      @click="jump(1)"
    >
      ›
    </button>
  </nav>
</template>

<style scoped>
.review-nav {
  /* Absolute in the panel root — both host roots are positioned — so the
     pill pins to the surface's lower-center regardless of how long or short
     the document under it is. The bottom band itself belongs to the review
     tray's floating form on the artifact surface, so the pill hovers just
     above the tray's collapsed bar instead of fighting it for the corner. */
  position: absolute;
  bottom: 3.25rem;
  left: 50%;
  transform: translateX(-50%);
  z-index: 20;
  display: flex;
  align-items: center;
  gap: 0.25rem;
  border: 1px solid var(--line);
  border-radius: 999px;
  background: var(--surface);
  box-shadow: 0 2px 8px rgb(0 0 0 / 0.16);
}

.review-nav-arrow {
  display: grid;
  width: 1.5rem;
  height: 1.5rem;
  place-items: center;
  border-radius: 999px;
  color: var(--muted);
  cursor: pointer;
  font-size: 0.875rem;
  line-height: 1;
  transition:
    color 0.12s ease,
    background-color 0.12s ease;
}

.review-nav-arrow:hover:not(:disabled) {
  background: var(--subtle-hover);
  color: var(--fg);
}

.review-nav-arrow:focus-visible {
  outline: 2px solid color-mix(in srgb, var(--accent) 55%, transparent);
  outline-offset: 1px;
}

.review-nav-arrow:disabled {
  cursor: default;
  opacity: 0.45;
}

.review-nav-count {
  padding-right: 0.25rem;
  padding-left: 0.25rem;
  color: var(--muted);
  font-size: 0.6875rem;
  font-variant-numeric: tabular-nums;
  line-height: 1;
  user-select: none;
  white-space: nowrap;
}
</style>
