<script setup lang="ts">
import { X } from '@lucide/vue'
import { DATA_COLORS } from '@/lib/blueprint'
import type { BlueprintPin } from '@/core'

/** The pin data of a selected flow node (its `data` payload). */
interface InspectorNodeData {
  title?: string
  category?: string
  inputs?: BlueprintPin[]
  outputs?: BlueprintPin[]
  values?: Record<string, string>
}

/** Inspector shows the selected node's pins; data inputs that are not wired
 *  can have their default value edited here (mirrors the inline editors on the
 *  node itself). Emits `change` so the caller can mark the graph dirty. */
const props = defineProps<{ node: InspectorNodeData }>()
const emit = defineEmits<{
  (e: 'close'): void
  (e: 'change'): void
}>()

const ROOT = (type?: string): string => (type ?? 'any').split(/[<{]/)[0]

function pinColor(pin: BlueprintPin): string {
  return DATA_COLORS[ROOT(pin.type)] ?? DATA_COLORS.any
}

function valueOf(pin: BlueprintPin): string {
  return props.node.values?.[pin.id] ?? ''
}

/** The inspector edits data inputs only; exec pins carry no parameters. */
const dataInputs = (): BlueprintPin[] => (props.node.inputs ?? []).filter((p) => p.kind === 'data-in')
const dataOutputs = (): BlueprintPin[] => (props.node.outputs ?? []).filter((p) => p.kind === 'data-out')

/** Whether the pin's value is edited through a checkbox. */
function isBool(pin: BlueprintPin): boolean {
  return ROOT(pin.type) === 'bool'
}

/** Whether the pin takes values from a pick list instead of free text. */
function isChoice(pin: BlueprintPin): boolean {
  return !!(pin.choices && pin.choices.length > 0)
}

/** Whether free-text default values make sense for this input. */
function editable(pin: BlueprintPin): boolean {
  return pin.choices && pin.choices.length > 0 ? true : ROOT(pin.type) !== 'context'
}

function setValue(pin: BlueprintPin, raw: string) {
  const values = props.node.values ?? (props.node.values = {})
  if (raw === '') delete values[pin.id]
  else values[pin.id] = raw
  emit('change')
}
</script>

<template>
  <aside
    class="glass absolute right-3 top-3 z-30 flex max-h-[calc(100%-1.5rem)] w-72 flex-col overflow-hidden rounded-xl border border-border shadow-xl"
  >
    <header class="flex items-center gap-2 border-b border-border px-3 py-2">
      <span class="truncate text-[13px] font-semibold text-foreground">{{ node.title ?? 'Node' }}</span>
      <span class="truncate text-[10px] text-subtle mono">{{ node.category ?? '' }}</span>
      <button
        class="ml-auto grid h-6 w-6 place-items-center rounded-md text-subtle transition-colors hover:bg-hover hover:text-foreground"
        type="button"
        title="Close"
        @click="emit('close')"
      >
        <X class="h-3.5 w-3.5" />
      </button>
    </header>

    <div class="min-h-0 flex-1 overflow-y-auto px-3 py-2">
      <section v-if="dataInputs().length" class="mb-2">
        <h4 class="mb-1 text-[10px] font-semibold uppercase tracking-wider text-subtle">Inputs</h4>
        <div v-for="pin in dataInputs()" :key="pin.id" class="mb-2">
          <div class="flex items-center gap-1.5">
            <span class="h-2 w-2 shrink-0 rounded-full" :style="{ background: pinColor(pin) }" />
            <span class="truncate text-[12px] text-foreground">{{ pin.name }}</span>
            <span class="ml-auto shrink-0 text-[10px] text-subtle">{{ pin.type }}</span>
          </div>
          <p v-if="pin.description" class="mt-0.5 text-[10px] leading-snug text-subtle">
            {{ pin.description }}
          </p>
          <!-- Data inputs that are not wired carry an inline default editor. -->
          <select
            v-if="isChoice(pin)"
            class="input mt-1 h-7! w-full text-[12px]"
            :value="valueOf(pin)"
            @change="setValue(pin, ($event.target as HTMLSelectElement).value)"
          >
            <option value="">—</option>
            <option v-for="c in pin.choices" :key="c" :value="c">{{ c }}</option>
          </select>
          <input
            v-else-if="editable(pin)"
            :type="isBool(pin) ? 'checkbox' : 'text'"
            :value="isBool(pin) ? valueOf(pin) === 'true' : valueOf(pin)"
            :checked="isBool(pin) ? valueOf(pin) === 'true' : undefined"
            class="input mt-1 h-7! w-full text-[12px]"
            :class="isBool(pin) ? 'h-4! w-4' : ''"
            :placeholder="isBool(pin) ? '' : pin.optional ? 'optional' : 'default value'"
            @input="
              setValue(
                pin,
                isBool(pin) ? (($event.target as HTMLInputElement).checked ? 'true' : 'false') : ($event.target as HTMLInputElement).value,
              )
            "
          />
        </div>
      </section>

      <section v-if="dataOutputs().length">
        <h4 class="mb-1 text-[10px] font-semibold uppercase tracking-wider text-subtle">Outputs</h4>
        <div v-for="pin in dataOutputs()" :key="pin.id" class="mb-1 flex items-center gap-1.5">
          <span class="h-2 w-2 shrink-0 rounded-full" :style="{ background: pinColor(pin) }" />
          <span class="truncate text-[12px] text-foreground">{{ pin.name }}</span>
          <span class="ml-auto shrink-0 text-[10px] text-subtle">{{ pin.type }}</span>
        </div>
      </section>
    </div>
  </aside>
</template>