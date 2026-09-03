<script setup lang="ts">
import { computed, nextTick, ref, useId } from 'vue';
import type { AgentChoice } from '../types';

const props = withDefaults(
  defineProps<{
    choices: AgentChoice[];
    modelValue: string;
    disabled?: boolean;
    fieldClass?: string;
    testid?: string;
    id?: string;
    /** Which edge the (over-wide) option list is pinned to, so it grows into
     *  free space rather than off the panel: `'right'` grows rightward (the
     *  default), `'left'` grows leftward (the new-session drawer). */
    placement?: 'left' | 'right';
  }>(),
  {
    disabled: false,
    fieldClass: 'bg-surface',
    testid: '',
    placement: 'right',
  },
);

const popoverClass = computed(() => (props.placement === 'left' ? 'right-0' : 'left-0'));

const emit = defineEmits<{
  'update:modelValue': [string];
}>();

const uid = useId();
const open = ref(false);
const editing = ref(false);
const query = ref('');
const activeOption = ref(-1);

/** The label of the current value, or its raw text when it is a custom model. */
const currentDisplay = computed(() => {
  const value = props.modelValue;
  if (!value) return '';
  return props.choices.find((choice) => choice.id === value)?.label ?? value;
});

const matches = computed(() => {
  const q = (editing.value ? query.value : currentDisplay.value).trim().toLowerCase();
  if (!q) return props.choices;
  return props.choices.filter(
    (choice) => choice.id.toLowerCase().includes(q) || choice.label.toLowerCase().includes(q),
  );
});

/** Whether `choice.id` is just the label plus a bracketed parameter list
 *  (Cursor's `grok-4.6[effort=high,fast=true]`) — a launch detail the id
 *  badge would otherwise surface for no reason, since it adds nothing the
 *  label doesn't already say. */
function idIsRedundant(choice: AgentChoice): boolean {
  return choice.id.startsWith(`${choice.label}[`) && choice.id.endsWith(']');
}

const inputValue = computed(() => (editing.value ? query.value : currentDisplay.value));

function optionId(index: number): string {
  return `${uid}-option-${index}`;
}

function onFocus() {
  if (props.disabled) return;
  editing.value = true;
  query.value = currentDisplay.value;
  open.value = true;
  activeOption.value = -1;
}

function onInput(event: Event) {
  editing.value = true;
  query.value = (event.target as HTMLInputElement).value;
  open.value = true;
  activeOption.value = -1;
}

function onBlur() {
  commit();
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') {
    event.stopPropagation();
    editing.value = false;
    open.value = false;
    activeOption.value = -1;
    return;
  }
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    if (!open.value) {
      event.preventDefault();
      event.stopPropagation();
      editing.value = true;
      query.value = currentDisplay.value;
      open.value = true;
      return;
    }
    const count = matches.value.length;
    if (!count) return;
    event.preventDefault();
    event.stopPropagation();
    const delta = event.key === 'ArrowDown' ? 1 : -1;
    activeOption.value =
      activeOption.value < 0
        ? delta > 0
          ? 0
          : count - 1
        : (activeOption.value + delta + count) % count;
    void nextTick(() => {
      document.getElementById(optionId(activeOption.value))?.scrollIntoView({ block: 'nearest' });
    });
    return;
  }
  if (event.key !== 'Enter') return;
  event.preventDefault();
  event.stopPropagation();
  if (activeOption.value >= 0) pick(matches.value[activeOption.value]);
  else commit();
}

function pick(choice: AgentChoice) {
  emit('update:modelValue', choice.id);
  editing.value = false;
  open.value = false;
  activeOption.value = -1;
}

/** The value to commit from the current edit: an exact choice match, else the raw text. */
function commitValue(): string {
  const q = query.value.trim();
  if (!q) return '';
  const exact = props.choices.find(
    (choice) =>
      choice.id.toLowerCase() === q.toLowerCase() || choice.label.toLowerCase() === q.toLowerCase(),
  );
  return exact ? exact.id : q;
}

/** Commit the pending edit, skipping the emit when nothing changed. */
function commit() {
  if (!editing.value) return;
  const next = commitValue();
  if (next !== props.modelValue) emit('update:modelValue', next);
  editing.value = false;
  open.value = false;
  activeOption.value = -1;
}
</script>

<template>
  <div class="relative">
    <input
      :id="id"
      role="combobox"
      aria-autocomplete="list"
      :aria-expanded="open && matches.length > 0"
      :aria-controls="`${uid}-options`"
      :aria-activedescendant="activeOption >= 0 ? optionId(activeOption) : undefined"
      :value="inputValue"
      :disabled="disabled"
      :data-testid="testid || undefined"
      autocomplete="off"
      spellcheck="false"
      placeholder="Agent default"
      class="min-w-0 w-full rounded px-2 py-1.5 outline-none focus:ring-1 ring-accent disabled:opacity-60"
      :class="fieldClass"
      @focus="onFocus"
      @input="onInput"
      @blur="onBlur"
      @keydown="onKeydown"
    />
    <ul
      v-if="open && matches.length"
      :id="`${uid}-options`"
      role="listbox"
      data-testid="model-options"
      class="absolute z-20 mt-1 max-h-56 w-max min-w-full max-w-[200%] overflow-auto rounded border border-line bg-input shadow-lg"
      :class="popoverClass"
    >
      <li v-for="(choice, index) in matches" :key="choice.id">
        <button
          :id="optionId(index)"
          type="button"
          role="option"
          :aria-selected="activeOption === index"
          data-testid="model-option"
          @mousedown.prevent="pick(choice)"
          class="flex w-full min-w-0 items-baseline gap-2 px-2 py-1.5 text-left hover:bg-subtle"
          :class="{ 'bg-subtle text-fg': activeOption === index }"
        >
          <span class="whitespace-nowrap text-sm">{{ choice.label }}</span>
          <code v-if="!idIsRedundant(choice)" class="truncate font-mono text-xs text-muted">{{
            choice.id
          }}</code>
        </button>
      </li>
    </ul>
  </div>
</template>
