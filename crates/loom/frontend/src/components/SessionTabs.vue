<script setup lang="ts">
import { computed } from 'vue';

// Work-area sub-nav. The local tabs are a flip the parent (SessionDetail) acts
// on: the panes v-show their kept-alive selves. Artifacts and Code Review are
// route-backed and deep-linkable — real hrefs, so they stay bookmarkable and
// open correctly on a fresh load — but a plain in-app click is intercepted
// into a `select` emit so the parent's docking/layout logic owns the
// navigation (a raw link to the already-current route would be a no-op and
// leave a popped panel stuck in the rail). Modifier and middle clicks keep
// the native href.
// Neutral underline indicator — no loud fills; only the active tab gets
// text-fg + an accent underline.
//
// The local set depends on the execution backend. A terminal session leads
// with its live Agent surface; an ACP session leads with Conversation.
// Overview was a duplicate of these operational surfaces and is deliberately
// absent.
type LocalTab = 'terminal' | 'conversation' | 'shells';
type Tab = LocalTab | 'artifacts' | 'commits' | 'changes';

const props = defineProps<{
  tab: Tab;
  /** Session id — Artifacts/Code Review link to `/s/<id>/…`. */
  id: string;
  /** Artifacts is open in the rail (popped out) rather than the work area. */
  artifactsPopped?: boolean;
  /** Execution backend — selects the local tab set + order. */
  protocol?: string;
}>();
const emit = defineEmits<{ select: [Tab] }>();

/** Only a plain left-click takes the guarded in-app path; everything else
 * (modifier combos, middle click) keeps the raw href. */
function onRouteTabClick(event: MouseEvent, tab: 'artifacts' | 'commits' | 'changes') {
  if (event.button !== 0 || event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) {
    return;
  }
  event.preventDefault();
  emit('select', tab);
}

const TERMINAL_TABS: { key: LocalTab; label: string }[] = [
  { key: 'terminal', label: 'Agent' },
  { key: 'conversation', label: 'Conversation' },
];
const ACP_TABS: { key: LocalTab; label: string }[] = [
  { key: 'conversation', label: 'Conversation' },
  { key: 'shells', label: 'Shells' },
];
const localTabs = computed(() => (props.protocol === 'acp' ? ACP_TABS : TERMINAL_TABS));

const tabClass = (active: boolean) =>
  active ? 'border-accent text-fg font-medium' : 'border-transparent text-muted hover:text-fg';
</script>

<template>
  <!-- pl-0.5 mirrors the header's 2px left wash border so tab labels align
       with the title above. -->
  <nav
    class="mb-1.5 flex items-center gap-0.5 border-b border-line pl-0.5 text-xs"
    aria-label="Session surfaces"
    data-testid="session-tabs"
  >
    <button
      v-for="t in localTabs"
      :key="t.key"
      type="button"
      role="tab"
      :data-tab="t.key"
      :aria-selected="tab === t.key"
      class="-mb-px shrink-0 border-b-2 px-1.5 py-1 sm:px-2"
      :class="tabClass(tab === t.key)"
      @click="$emit('select', t.key)"
    >
      {{ t.label }}
    </button>
    <a
      :href="`/s/${id}/artifacts`"
      role="tab"
      data-tab="artifacts"
      :aria-selected="tab === 'artifacts'"
      class="-mb-px shrink-0 border-b-2 px-1.5 py-1 sm:px-2"
      :class="tabClass(tab === 'artifacts' || Boolean(artifactsPopped))"
      @click="onRouteTabClick($event, 'artifacts')"
    >
      Artifacts
      <!-- When popped out, the Artifacts surface lives in the rail, not here —
           a small glyph marks it open without claiming the work area. -->
      <span v-if="artifactsPopped" class="ml-1 text-faint" title="Open in the split panel">⤢</span>
    </a>
    <a
      :href="`/s/${id}/commits`"
      role="tab"
      data-tab="commits"
      :aria-selected="tab === 'commits'"
      class="-mb-px shrink-0 border-b-2 px-1.5 py-1 sm:px-2"
      :class="tabClass(tab === 'commits')"
      @click="onRouteTabClick($event, 'commits')"
    >
      Commits
    </a>
    <a
      :href="`/s/${id}/changes`"
      role="tab"
      data-tab="changes"
      :aria-selected="tab === 'changes'"
      class="-mb-px shrink-0 border-b-2 px-1.5 py-1 sm:px-2"
      :class="tabClass(tab === 'changes')"
      @click="onRouteTabClick($event, 'changes')"
    >
      Code Review
    </a>
    <!-- The tab row's right side is otherwise dead space — hosts compact,
         always-relevant extras (the scratch attach strip on the detail page). -->
    <div class="ml-auto flex min-w-0 items-center">
      <slot name="right" />
    </div>
  </nav>
</template>
