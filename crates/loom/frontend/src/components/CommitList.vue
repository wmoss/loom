<script setup lang="ts">
import { reactive } from 'vue';
import type { SessionCommit } from '../types';
import { timeAgo } from '../lib/time';

defineProps<{ commits: SessionCommit[] }>();
const emit = defineEmits<{ review: [oid: string] }>();

// Rows whose full message body is unfolded; the commit line itself toggles
// too, and only commits carrying a body can join the set.
const expanded = reactive(new Set<string>());
function toggleBody(oid: string) {
  if (expanded.has(oid)) expanded.delete(oid);
  else expanded.add(oid);
}
</script>

<template>
  <ul data-testid="commit-list">
    <li v-for="commit in commits" :key="commit.oid" class="border-b border-line">
      <div class="flex items-stretch">
        <button
          type="button"
          class="flex min-w-0 flex-1 items-baseline gap-3 px-3 py-2 text-left hover:bg-subtle"
          :class="commit.body ? 'cursor-pointer' : 'cursor-default'"
          :data-commit="commit.oid.slice(0, 10)"
          :data-commit-oid="commit.oid"
          :title="commit.body ? 'Show the full message' : undefined"
          @click="commit.body && toggleBody(commit.oid)"
        >
          <code class="shrink-0 font-mono text-2xs text-faint">{{ commit.oid.slice(0, 8) }}</code>
          <span class="min-w-0 flex-1 truncate text-xs text-fg" :title="commit.subject">{{
            commit.subject
          }}</span>
          <span class="shrink-0 text-2xs text-muted" :title="commit.author_email">
            {{ commit.author_name }} · {{ timeAgo(commit.authored_at) }}
          </span>
        </button>
        <button
          type="button"
          class="btn-secondary mr-1 shrink-0 self-center px-2 py-0.5 text-2xs"
          data-testid="commit-review-button"
          :data-review-commit="commit.oid"
          :title="`Review commit ${commit.oid.slice(0, 10)} in Code Review`"
          @click="emit('review', commit.oid)"
        >
          Review
        </button>
        <!-- The toggle owns a fixed column so rows align whether or not the
             commit carries a body. -->
        <div class="flex w-8 shrink-0 items-center justify-center text-2xs text-faint">
          <button
            v-if="commit.body"
            type="button"
            class="flex h-full w-full items-center justify-center hover:bg-subtle hover:text-fg"
            :aria-expanded="expanded.has(commit.oid)"
            :data-commit-body-toggle="commit.oid.slice(0, 10)"
            :title="expanded.has(commit.oid) ? 'Hide the full message' : 'Show the full message'"
            @click="toggleBody(commit.oid)"
          >
            {{ expanded.has(commit.oid) ? '▾' : '▸' }}
          </button>
        </div>
      </div>
      <pre
        v-if="commit.body && expanded.has(commit.oid)"
        class="whitespace-pre-wrap border-t border-line bg-code px-3 py-2 text-2xs text-code-fg"
        :data-commit-body="commit.oid.slice(0, 10)"
        >{{ commit.body }}</pre>
    </li>
  </ul>
</template>
