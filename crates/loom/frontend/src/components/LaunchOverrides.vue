<script setup lang="ts">
import { computed, ref, useId, watch } from 'vue';
import type { AgentChoice, AgentMetadata, LaunchOverrides, ResolvedLaunch } from '../types';
import { getModelEfforts } from '../api';
import {
  agentOptionsWithCurrent,
  availableAgents,
  isAgentAvailable,
} from '../lib/agentAvailability';
import ModelCombobox from './ModelCombobox.vue';

const props = withDefaults(
  defineProps<{
    agents: AgentMetadata[];
    modelValue: LaunchOverrides;
    resolved: ResolvedLaunch | null;
    fallback?: ResolvedLaunch | null;
    disabled?: boolean;
    /** Which way the model option list grows; see `ModelCombobox`. */
    modelDropdownPlacement?: 'left' | 'right';
  }>(),
  { modelDropdownPlacement: 'right' },
);

const emit = defineEmits<{
  'update:modelValue': [LaunchOverrides];
}>();
const uid = useId();
const settings = computed(() => props.resolved ?? props.fallback ?? null);
const effectiveAgent = computed(
  () =>
    props.modelValue.agent ?? settings.value?.agent ?? availableAgents(props.agents)[0]?.kind ?? '',
);
// The effective agent is always an option, marked when its harness is missing,
// so the control can never display a different agent than the one in effect.
const agentOptions = computed(() => agentOptionsWithCurrent(props.agents, effectiveAgent.value));
const metadata = computed(() => props.agents.find((agent) => agent.kind === effectiveAgent.value));
// Effort choices track the selected model when the harness scopes them per
// model (codex, antigravity): a known model uses its own list — even when that
// list is empty — and only a raw-typed or unset model falls back to the global
// superset.
//
// A harness flagged `effort_lookup` (cursor-agent) carries no per-model
// efforts in its catalogue at all — probing every model live to populate it
// up front is too slow — so they're fetched on demand for whichever model is
// selected, catalogue entry or raw-typed id alike (`getModelEfforts` handles
// both), and that fetched list is authoritative.
const lookedUpEfforts = ref<AgentChoice[]>([]);
const effortsLoading = ref(false);
watch(
  () => [metadata.value?.kind, metadata.value?.effort_lookup, value('model')] as const,
  ([kind, needsLookup, model], _old, onCleanup) => {
    lookedUpEfforts.value = [];
    effortsLoading.value = false;
    if (!needsLookup || !kind || !model) return;
    let stale = false;
    onCleanup(() => {
      stale = true;
    });
    effortsLoading.value = true;
    getModelEfforts(kind, model)
      .then((efforts) => {
        if (!stale) lookedUpEfforts.value = efforts;
      })
      .finally(() => {
        if (!stale) effortsLoading.value = false;
      });
  },
  { immediate: true },
);

const effortChoices = computed(() => {
  if (metadata.value?.effort_lookup) {
    return lookedUpEfforts.value;
  }
  const known = metadata.value?.models.find((m) => m.id === value('model'));
  return known ? (known.efforts ?? []) : (metadata.value?.efforts ?? []);
});

function value(field: keyof LaunchOverrides): string {
  return props.modelValue[field] ?? (settings.value?.[field] as string | undefined) ?? '';
}

function changed(field: keyof LaunchOverrides): boolean {
  return Object.prototype.hasOwnProperty.call(props.modelValue, field);
}

function set(field: keyof LaunchOverrides, nextValue: string) {
  const next = { ...props.modelValue, [field]: nextValue };
  if (field === 'agent') {
    // A new agent has its own models/efforts — reset both to "Agent default"
    // rather than carry a selection that doesn't apply.
    next.model = '';
    next.effort = '';
  }
  emit('update:modelValue', next);
}

function locked(field: keyof LaunchOverrides): boolean {
  const lockedFields = props.resolved?.locked_fields ?? props.fallback?.locked_fields ?? [];
  return Boolean(props.disabled || lockedFields.includes(field));
}
</script>

<template>
  <div class="space-y-3" data-testid="launch-settings">
    <div class="grid gap-3 sm:grid-cols-2">
      <label class="rounded border border-line bg-input p-2 text-xs">
        <span class="mb-1 flex items-center justify-between gap-2">
          <span class="font-medium text-fg">Agent</span>
          <span :class="changed('agent') ? 'text-accent' : 'text-faint'">
            {{ changed('agent') ? 'changed' : 'from profile' }}
          </span>
        </span>
        <select
          :id="`${uid}-agent`"
          aria-label="Agent"
          :value="value('agent')"
          :disabled="locked('agent')"
          data-testid="override-agent"
          class="w-full rounded bg-surface px-2 py-1.5 disabled:opacity-60"
          @change="set('agent', ($event.target as HTMLSelectElement).value)"
        >
          <option v-for="agent in agentOptions" :key="agent.kind" :value="agent.kind">
            {{ agent.label }}{{ isAgentAvailable(agent) ? '' : ' — unavailable' }}
          </option>
        </select>
      </label>

      <label class="rounded border border-line bg-input p-2 text-xs">
        <span class="mb-1 flex items-center justify-between gap-2">
          <span class="font-medium text-fg">Model</span>
          <span :class="changed('model') ? 'text-accent' : 'text-faint'">
            {{ changed('model') ? 'changed' : 'from profile' }}
          </span>
        </span>
        <ModelCombobox
          v-if="metadata?.accepts_raw_model"
          :id="`${uid}-model`"
          :choices="metadata.models"
          :model-value="value('model')"
          :disabled="locked('model')"
          :placement="modelDropdownPlacement"
          field-class="bg-surface"
          testid="override-model"
          @update:model-value="set('model', $event)"
        />
        <select
          v-else
          :id="`${uid}-model`"
          aria-label="Model"
          :value="value('model')"
          :disabled="locked('model')"
          data-testid="override-model"
          class="w-full rounded bg-surface px-2 py-1.5 disabled:opacity-60"
          @change="set('model', ($event.target as HTMLSelectElement).value)"
        >
          <option value="">Agent default</option>
          <option v-for="choice in metadata?.models ?? []" :key="choice.id" :value="choice.id">
            {{ choice.label }}
          </option>
        </select>
      </label>

      <label class="rounded border border-line bg-input p-2 text-xs">
        <span class="mb-1 flex items-center justify-between gap-2">
          <span class="flex items-center gap-1.5 font-medium text-fg">
            Effort
            <span
              v-if="effortsLoading"
              class="inline-block h-3 w-3 animate-spin rounded-full border border-current border-t-transparent"
              aria-label="Loading effort levels"
            />
          </span>
          <span :class="changed('effort') ? 'text-accent' : 'text-faint'">
            {{ changed('effort') ? 'changed' : 'from profile' }}
          </span>
        </span>
        <select
          :id="`${uid}-effort`"
          aria-label="Effort"
          :value="value('effort')"
          :disabled="locked('effort')"
          data-testid="override-effort"
          class="w-full rounded bg-surface px-2 py-1.5 disabled:opacity-60"
          @change="set('effort', ($event.target as HTMLSelectElement).value)"
        >
          <option value="">Agent default</option>
          <option v-for="choice in effortChoices" :key="choice.id" :value="choice.id">
            {{ choice.label }}
          </option>
        </select>
      </label>

      <label class="rounded border border-line bg-input p-2 text-xs">
        <span class="mb-1 flex items-center justify-between gap-2">
          <span class="font-medium text-fg">Permission mode</span>
          <span :class="changed('mode') ? 'text-accent' : 'text-faint'">
            {{ changed('mode') ? 'changed' : 'from profile' }}
          </span>
        </span>
        <select
          :id="`${uid}-mode`"
          aria-label="Permission mode"
          :value="value('mode')"
          :disabled="locked('mode')"
          data-testid="override-mode"
          class="w-full rounded bg-surface px-2 py-1.5 disabled:opacity-60"
          @change="set('mode', ($event.target as HTMLSelectElement).value)"
        >
          <option
            v-for="mode in ['auto', 'default', 'acceptEdits', 'plan', 'bypassPermissions']"
            :key="mode"
            :value="mode"
          >
            {{ mode }}
          </option>
        </select>
      </label>
    </div>

    <details class="rounded border border-line bg-input">
      <summary class="cursor-pointer px-2 py-1.5 text-xs text-muted">Advanced runtime</summary>
      <div class="grid gap-3 border-t border-line p-2 sm:grid-cols-2">
        <label class="text-xs">
          <span class="mb-1 flex items-center justify-between gap-2">
            <span class="font-medium text-fg">Connection</span>
            <span :class="changed('protocol') ? 'text-accent' : 'text-faint'">
              {{ changed('protocol') ? 'changed' : 'from profile' }}
            </span>
          </span>
          <select
            :id="`${uid}-protocol`"
            aria-label="Connection"
            :value="value('protocol')"
            :disabled="locked('protocol')"
            data-testid="override-protocol"
            class="w-full rounded bg-surface px-2 py-1.5 disabled:opacity-60"
            @change="set('protocol', ($event.target as HTMLSelectElement).value)"
          >
            <option v-if="metadata?.supports_acp !== false" value="acp">ACP</option>
            <option value="terminal">Terminal</option>
          </select>
        </label>

        <label class="text-xs">
          <span class="mb-1 flex items-center justify-between gap-2">
            <span class="font-medium text-fg">Session type</span>
            <span :class="changed('class') ? 'text-accent' : 'text-faint'">
              {{ changed('class') ? 'changed' : 'from profile' }}
            </span>
          </span>
          <select
            :id="`${uid}-class`"
            aria-label="Session type"
            :value="value('class')"
            :disabled="locked('class')"
            data-testid="override-class"
            class="w-full rounded bg-surface px-2 py-1.5 disabled:opacity-60"
            @change="set('class', ($event.target as HTMLSelectElement).value)"
          >
            <option value="interactive">Interactive</option>
            <option value="automation">Automation</option>
          </select>
        </label>
      </div>
    </details>

    <p
      v-if="(resolved ?? fallback)?.locked_fields.length"
      class="text-xs text-faint"
      data-testid="launch-settings-policy"
    >
      This profile’s policy locks some session settings.
    </p>
  </div>
</template>
