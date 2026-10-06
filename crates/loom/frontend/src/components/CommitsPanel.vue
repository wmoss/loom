<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { getSessionCommits } from '../api';
import type { SessionCommits } from '../types';
import { timeAgo } from '../lib/time';

const props = defineProps<{ id: string }>();
const commits = ref<SessionCommits | null>(null);
const loading = ref(false);
const error = ref('');

const baseProblem = computed(() => {
  const base = commits.value?.base;
  if (base?.state !== 'unavailable') return '';
  const reasons: Record<typeof base.reason, string> = {
    unborn_head: 'this worktree has no commits yet',
    missing_base: `no local or remote-tracking ref resolves the base ${base.reference}`,
    no_merge_base: `this branch shares no history with ${base.reference}`,
  };
  return `Commits unavailable: ${reasons[base.reason]}.`;
});

async function load() {
  loading.value = true;
  try {
    commits.value = await getSessionCommits(props.id);
    error.value = '';
  } catch (cause) {
    error.value = (cause as Error).message;
  } finally {
    loading.value = false;
  }
}

onMounted(load);
</script>

<template>
  <section
    class="relative flex h-full min-h-0 flex-col overflow-hidden"
    data-testid="commits-panel"
  >
    <header class="flex flex-wrap items-center gap-2 border-b border-line px-3 py-2">
      <div class="min-w-0 flex-1">
        <h2 class="text-sm font-semibold text-fg">Commits</h2>
        <p
          v-if="commits?.base.state === 'available'"
          class="truncate font-mono text-2xs text-faint"
        >
          {{ commits.base.reference }} · {{ commits.base.oid.slice(0, 10) }}
        </p>
      </div>
      <span v-if="commits" class="text-xs text-muted">
        {{ commits.commits.length }}{{ commits.truncated ? '+' : '' }} on this branch
      </span>
      <button type="button" class="btn-secondary px-2 py-1 text-xs" @click="load">Refresh</button>
    </header>

    <p v-if="error" class="m-3 rounded bg-block-soft p-2 text-xs text-block" role="alert">
      {{ error }}
    </p>
    <p
      v-else-if="commits?.base.state === 'unavailable'"
      class="m-3 rounded border border-line p-3 text-sm text-muted"
    >
      {{ baseProblem }}
    </p>
    <div v-else class="min-h-0 flex-1 overflow-auto">
      <p v-if="loading && !commits" class="p-3 text-sm text-muted">Loading commits…</p>
      <p v-else-if="commits && !commits.commits.length" class="p-3 text-sm text-muted">
        No commits on this branch beyond its base.
      </p>
      <p v-if="commits?.truncated" class="m-3 rounded bg-block-soft p-2 text-xs text-block">
        This branch holds more commits than the listing bound. Narrow the branch to see the rest.
      </p>

      <ul>
        <li
          v-for="commit in commits?.commits"
          :key="commit.oid"
          class="flex items-baseline gap-3 border-b border-line px-3 py-2 hover:bg-subtle"
          :data-commit="commit.oid.slice(0, 10)"
        >
          <code class="shrink-0 font-mono text-2xs text-faint">{{ commit.oid.slice(0, 8) }}</code>
          <span class="min-w-0 flex-1 truncate text-xs text-fg" :title="commit.subject">{{
            commit.subject
          }}</span>
          <span class="shrink-0 text-2xs text-muted" :title="commit.author_email">
            {{ commit.author_name }} · {{ timeAgo(commit.authored_at) }}
          </span>
        </li>
      </ul>
    </div>
  </section>
</template>
