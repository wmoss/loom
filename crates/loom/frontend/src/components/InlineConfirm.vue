<script setup lang="ts">
import { computed, nextTick, ref } from 'vue';

const props = withDefaults(
  defineProps<{
    label: string;
    message: string;
    disabled?: boolean;
    confirmLabel?: string;
    action?: () => Promise<void>;
    danger?: boolean;
    /** Compact sizing for the comment cards' control rows, where every
     *  sibling button is `px-2 py-1 text-2xs`. */
    dense?: boolean;
  }>(),
  { disabled: false, confirmLabel: 'Confirm', action: undefined, danger: false, dense: false },
);

const emit = defineEmits<{ confirm: [] }>();
const open = ref(false);
const trigger = ref<HTMLButtonElement | null>(null);
const confirmButton = ref<HTMLButtonElement | null>(null);
const busy = ref(false);
const error = ref('');

/** One sizing for all three buttons, so the confirm row never towers over
 *  the row it opened from. */
const sizing = computed(() => (props.dense ? 'px-2 py-1 text-2xs' : 'px-3 py-1.5 text-xs'));

async function show() {
  open.value = true;
  await nextTick();
  confirmButton.value?.focus();
}

async function cancel() {
  open.value = false;
  await nextTick();
  trigger.value?.focus();
}

async function confirm() {
  if (!props.action) {
    open.value = false;
    emit('confirm');
    return;
  }
  busy.value = true;
  error.value = '';
  try {
    await props.action();
    await cancel();
  } catch (cause) {
    error.value = (cause as Error).message;
    await nextTick();
    confirmButton.value?.focus();
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <span
    class="inline-flex min-w-0 flex-wrap items-center gap-2"
    @keydown.esc.prevent.stop="!busy && cancel()"
  >
    <button
      v-if="!open"
      ref="trigger"
      type="button"
      class="btn-secondary"
      :class="sizing"
      :disabled="disabled"
      @click="show"
    >
      {{ label }}
    </button>
    <template v-else>
      <span class="text-xs text-block">{{ message }}</span>
      <button
        ref="confirmButton"
        type="button"
        class="rounded text-white"
        :class="[sizing, danger ? 'bg-block' : 'bg-accent']"
        :disabled="disabled || busy"
        @click="confirm"
      >
        {{ busy ? 'Working…' : confirmLabel }}
      </button>
      <button type="button" class="btn-secondary" :class="sizing" :disabled="busy" @click="cancel">
        Cancel
      </button>
      <span v-if="error" class="w-full text-xs text-block" role="alert">{{ error }}</span>
    </template>
  </span>
</template>
