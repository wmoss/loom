<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { useRouter } from 'vue-router';
import { getSessionCommits } from '../api';
import type { SessionCommits } from '../types';
import { timeAgo } from '../lib/time';

const props = defineProps<{ id: string }>();
const router = useRouter();
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

/** Opens the code review scoped to this commit's own changes. */
function reviewCommit(oid: string) {
  void router.push(`/s/${props.id}/changes?rev=${oid}`);
}

// Rows whose full message body is unfolded; only commits carrying a body can
// join, and the row click itself still goes to the review.
const expanded = reactive(new Set<string>());
function toggleBody(oid: string) {
  if (expanded.has(oid)) expanded.delete(oid);
  else expanded.add(oid);
}
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
        <li v-for="commit in commits?.commits" :key="commit.oid" class="border-b border-line">
          <div class="flex items-stretch">
            <button
              type="button"
              class="flex min-w-0 flex-1 items-baseline gap-3 px-3 py-2 text-left hover:bg-subtle"
              :data-commit="commit.oid.slice(0, 10)"
              :data-commit-oid="commit.oid"
              :title="`Review commit ${commit.oid.slice(0, 10)} in Code Review`"
              @click="reviewCommit(commit.oid)"
            >
              <code class="shrink-0 font-mono text-2xs text-faint">{{
                commit.oid.slice(0, 8)
              }}</code>
              <span class="min-w-0 flex-1 truncate text-xs text-fg" :title="commit.subject">{{
                commit.subject
              }}</span>
              <span class="shrink-0 text-2xs text-muted" :title="commit.author_email">
                {{ commit.author_name }} · {{ timeAgo(commit.authored_at) }}
              </span>
            </button>
            <button
              v-if="commit.body"
              type="button"
              class="shrink-0 self-stretch px-2 text-2xs text-faint hover:bg-subtle hover:text-fg"
              :aria-expanded="expanded.has(commit.oid)"
              :data-commit-body-toggle="commit.oid.slice(0, 10)"
              :title="expanded.has(commit.oid) ? 'Hide the full message' : 'Show the full message'"
              @click="toggleBody(commit.oid)"
            >
              {{ expanded.has(commit.oid) ? '▾' : '▸' }}
            </button>
          </div>
          <pre
            v-if="commit.body && expanded.has(commit.oid)"
            class="whitespace-pre-wrap border-t border-line bg-code px-3 py-2 text-2xs text-code-fg"
            :data-commit-body="commit.oid.slice(0, 10)"
            >{{ commit.body }}</pre>
        </li>
      </ul>
    </div>
  </section>
</template>
