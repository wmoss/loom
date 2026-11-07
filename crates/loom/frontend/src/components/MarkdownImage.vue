<script setup lang="ts">
import { ref } from 'vue';

// An inline markdown image that degrades in place. Agent prose embeds host
// paths that loom may not be able to serve — the file can sit in a mount the
// server cannot see, or be gone by the time the reader looks — and the
// browser's broken-image icon explains none of that, so a failed load becomes
// a card naming the source.
const props = defineProps<{
  /** Resolved, fetchable URL; empty when the token carried no src. */
  src: string;
  /** The original markdown source, shown when the load fails. */
  raw: string;
  alt?: string;
}>();

const failed = ref(false);
</script>

<template>
  <img
    v-if="src && !failed"
    data-testid="markdown-image"
    :src="src"
    :alt="alt ?? ''"
    @error="failed = true"
  />
  <span v-else data-testid="markdown-image-fallback" class="markdown-image-fallback" :title="raw">
    <span aria-hidden="true">◧</span>
    <span>{{ alt || 'image' }} unavailable</span>
    <code v-if="raw">{{ raw }}</code>
  </span>
</template>
