<script setup lang="ts">
import { reactive } from 'vue'
import { ChevronRight, Plus, Trash2, X } from '@lucide/vue'
import Toggle from '@/components/settings/Toggle.vue'

/**
 * Generic "Add / Edit configuration" modal (Trae 添加模型-style): renders a
 * form from a field list, collects values into a reactive record and emits
 * `confirm` with the filled values.
 *
 * Field types:
 * - `text` / `password` / `number` / `select` / `switch` / `textarea`: simple
 *   controls bound to `values[key]`.
 * - `group`: a collapsible second-level section (collapsed by default); its
 *   `children` render normally against a nested object stored at
 *   `values[key]` (key = group name, value = `{ ...children keys }`).
 * - `prices`: four ModelPricing inputs (input / output / cache-hit /
 *   cache-write), each suffixed "$/1M"; blank = 0.
 * - `tiers`: PricingTier rows (name + max_input_tokens + prices + remove)
 *   with an "Add tier" button.
 * - `windows`: PeakWindowPrice rows (cron + duration_min + prices + remove)
 *   with an "Add window" button.
 */

export interface ModalField {
  key: string
  label: string
  type: 'text' | 'number' | 'select' | 'switch' | 'textarea' | 'password' | 'group' | 'prices' | 'tiers' | 'windows'
  options?: string[]
  placeholder?: string
  required?: boolean
  /** Sub-fields rendered inside a collapsible group (only for `type: 'group'`). */
  children?: ModalField[]
  /** Hide this field unless the predicate passes (evaluated against its values container). */
  showIf?: (values: Record<string, unknown>) => boolean
}

const PRICE_KEYS = ['input_per_mtok', 'output_per_mtok', 'cache_hit_per_mtok', 'cache_write_per_mtok'] as const
const PRICE_LABELS: Record<string, string> = {
  input_per_mtok: 'Input',
  output_per_mtok: 'Output',
  cache_hit_per_mtok: 'Cache hit',
  cache_write_per_mtok: 'Cache write',
}

const props = defineProps<{
  title: string
  fields: ModalField[]
  /** Pre-filled values when editing an existing entry. */
  initial?: Record<string, unknown>
  /** Confirm button label, e.g. `Add` / `Save`. */
  confirmLabel?: string
}>()

const emit = defineEmits<{ confirm: [values: Record<string, unknown>]; cancel: [] }>()

const values = reactive<Record<string, unknown>>({ ...(props.initial ?? {}) })

/** Groups start collapsed. */
const collapsedGroups = reactive(new Set(props.fields.filter((f) => f.type === 'group').map((f) => f.key)))

function toggleGroup(f: ModalField) {
  if (collapsedGroups.has(f.key)) collapsedGroups.delete(f.key)
  else collapsedGroups.add(f.key)
}

function groupCollapsed(f: ModalField): boolean {
  return collapsedGroups.has(f.key)
}

/** The record a field writes into: a group's nested object, or the top level. */
function container(g?: ModalField): Record<string, unknown> {
  if (!g) return values
  const v = values[g.key]
  if (v && typeof v === 'object' && !Array.isArray(v)) return v as Record<string, unknown>
  const o: Record<string, unknown> = {}
  values[g.key] = o
  return o
}

/** Children of a group, hiding fields whose `showIf` predicate fails. */
function groupChildren(f: ModalField): ModalField[] {
  const g = container(f)
  return (f.children ?? []).filter((c) => !c.showIf || c.showIf(g))
}

function fieldValue(f: ModalField, g?: ModalField): unknown {
  return container(g)[f.key]
}

function setField(f: ModalField, v: unknown, g?: ModalField) {
  container(g)[f.key] = v
}

function valueAs(f: ModalField, g?: ModalField): string {
  const v = container(g)[f.key]
  return typeof v === 'string' ? v : ''
}

function numAs(f: ModalField, g?: ModalField): string {
  const v = container(g)[f.key]
  return typeof v === 'number' ? String(v) : ''
}

function boolOf(f: ModalField, g?: ModalField): boolean {
  return container(g)[f.key] === true
}

/** Number input value; an empty box stays `''` so callers can map it to null/0. */
function numInput(e: Event): number | string {
  const raw = (e.target as HTMLInputElement).value
  return raw === '' ? '' : Number(raw) || 0
}

function pricesOf(f: ModalField, g?: ModalField): Record<string, unknown> {
  const c = container(g)
  const v = c[f.key]
  if (v && typeof v === 'object' && !Array.isArray(v)) return v as Record<string, unknown>
  const o: Record<string, unknown> = {}
  c[f.key] = o
  return o
}

function priceAs(f: ModalField, key: (typeof PRICE_KEYS)[number], g?: ModalField): string {
  const v = pricesOf(f, g)[key]
  return typeof v === 'number' ? String(v) : ''
}

function setPrice(f: ModalField, key: (typeof PRICE_KEYS)[number], raw: string, g?: ModalField) {
  pricesOf(f, g)[key] = raw.trim() === '' ? 0 : Number(raw) || 0
}

function listOf(f: ModalField, g?: ModalField): Array<Record<string, unknown>> {
  const v = container(g)[f.key]
  return Array.isArray(v) ? (v as Array<Record<string, unknown>>) : []
}

function tierRows(f: ModalField, g?: ModalField): Array<Record<string, unknown>> {
  return listOf(f, g)
}

function addTier(f: ModalField, g?: ModalField) {
  container(g)[f.key] = [...listOf(f, g), {}]
}

function removeTier(f: ModalField, g: ModalField | undefined, idx: number) {
  const list = [...listOf(f, g)]
  list.splice(idx, 1)
  container(g)[f.key] = list
}

function setTier(f: ModalField, g: ModalField | undefined, idx: number, field: 'name' | 'max_input_tokens', raw: string) {
  const list = [...listOf(f, g)]
  const item = { ...(list[idx] ?? {}) }
  item[field] = field === 'name' ? raw : raw.trim() === '' ? null : Number(raw) || 0
  list[idx] = item
  container(g)[f.key] = list
}

function windowRows(f: ModalField, g?: ModalField): Array<Record<string, unknown>> {
  return listOf(f, g)
}

function addWindow(f: ModalField, g?: ModalField) {
  container(g)[f.key] = [...listOf(f, g), { cron: '* * * * *', duration_min: 60 }]
}

function removeWindow(f: ModalField, g: ModalField | undefined, idx: number) {
  const list = [...listOf(f, g)]
  list.splice(idx, 1)
  container(g)[f.key] = list
}

function setWindow(f: ModalField, g: ModalField | undefined, idx: number, field: 'cron' | 'duration_min', raw: string) {
  const list = [...listOf(f, g)]
  const item = { ...(list[idx] ?? {}) }
  item[field] = field === 'cron' ? raw : Number(raw) || 0
  list[idx] = item
  container(g)[f.key] = list
}

/** The `prices` object nested inside a tier/window row. */
function rowPrices(f: ModalField, g: ModalField | undefined, idx: number): Record<string, unknown> {
  const row = listOf(f, g)[idx] ?? {}
  if (row.prices && typeof row.prices === 'object' && !Array.isArray(row.prices)) return row.prices as Record<string, unknown>
  const o: Record<string, unknown> = {}
  row.prices = o
  return o
}

function rowPriceAs(f: ModalField, g: ModalField | undefined, idx: number, key: (typeof PRICE_KEYS)[number]): string {
  const v = rowPrices(f, g, idx)[key]
  return typeof v === 'number' ? String(v) : ''
}

function setRowPrice(f: ModalField, g: ModalField | undefined, idx: number, key: (typeof PRICE_KEYS)[number], raw: string) {
  rowPrices(f, g, idx)[key] = raw.trim() === '' ? 0 : Number(raw) || 0
}

function requiredMissing(f: ModalField, g?: ModalField): boolean {
  const v = container(g)[f.key]
  return v === undefined || v === null || (typeof v === 'string' && v.trim() === '')
}

function submit() {
  // Validate required fields at the top level and inside every group.
  const missing = props.fields.find(
    (f) =>
      (f.required && requiredMissing(f)) ||
      (f.type === 'group' && !!f.children && f.children.some((c) => c.required && requiredMissing(c, f))),
  )
  if (missing) return
  emit('confirm', { ...values })
}
</script>

<template>
  <Teleport to="body">
    <div
      class="fixed inset-0 z-1100 flex items-center justify-center p-4"
      style="background: var(--overlay)"
      @mousedown.self="emit('cancel')"
    >
      <div
        class="max-h-full w-full max-w-md overflow-auto rounded-xl p-5"
        style="background: var(--popover); box-shadow: var(--shadow-popover); border: 1px solid var(--divider)"
      >
        <div class="flex items-center gap-2">
          <h2 class="flex-1 text-[13.5px] font-semibold text-foreground">{{ props.title }}</h2>
          <button class="btn-icon h-6! w-6!" type="button" aria-label="Cancel" @click="emit('cancel')">
            <X class="h-3.5 w-3.5" />
          </button>
        </div>

        <div class="mt-4 space-y-3">
          <template v-for="f in fields" :key="f.key">
            <!-- Collapsible second-level group -->
            <div v-if="f.type === 'group'" class="overflow-hidden rounded-lg" style="border: 1px solid var(--divider)">
              <button
                type="button"
                class="flex w-full items-center gap-1.5 px-3 py-2 text-left text-[12px] font-medium text-foreground hover:bg-accent"
                :aria-expanded="!groupCollapsed(f)"
                @click="toggleGroup(f)"
              >
                <ChevronRight class="h-3.5 w-3.5 transition-transform duration-150" :class="groupCollapsed(f) ? '' : 'rotate-90'" />
                <span class="flex-1">{{ f.label }}</span>
              </button>
              <div v-if="!groupCollapsed(f)" class="space-y-3 border-t px-3 py-2.5" style="border-color: var(--divider)">
                <!-- Children render normally, against the group's nested object -->
                <label v-for="c in groupChildren(f)" :key="c.key" class="block">
                  <span class="mb-1 flex items-center gap-1 text-[12px] font-medium text-foreground">
                    {{ c.label }}
                    <span v-if="c.required" class="text-[11px] text-danger">*</span>
                  </span>
                  <select
                    v-if="c.type === 'select'"
                    class="input w-full! text-[12.5px]!"
                    :value="(fieldValue(c, f) as string | undefined) ?? ''"
                    @change="setField(c, ($event.target as HTMLSelectElement).value, f)"
                  >
                    <option v-for="o in c.options" :key="o" :value="o">{{ o }}</option>
                  </select>
                  <textarea
                    v-else-if="c.type === 'textarea'"
                    :value="valueAs(c, f)"
                    class="input w-full! resize-none text-[12px]! leading-relaxed!"
                    rows="2"
                    :placeholder="c.placeholder"
                    @input="setField(c, ($event.target as HTMLTextAreaElement).value, f)"
                  />
                  <Toggle
                    v-else-if="c.type === 'switch'"
                    :model-value="boolOf(c, f)"
                    @update:model-value="setField(c, $event, f)"
                  />
                  <input
                    v-else-if="c.type === 'number'"
                    :value="numAs(c, f)"
                    class="input w-full! text-[12.5px]!"
                    type="number"
                    step="any"
                    :placeholder="c.placeholder"
                    @input="setField(c, numInput($event), f)"
                  />
                  <div v-else-if="c.type === 'prices'" class="space-y-1.5">
                    <div v-for="k in PRICE_KEYS" :key="k" class="flex items-center gap-1.5">
                      <span class="w-20 shrink-0 text-[11px] text-subtle">{{ PRICE_LABELS[k] }}</span>
                      <input
                        class="input h-7! min-w-0 flex-1! text-[12px]!"
                        type="number"
                        step="any"
                        :value="priceAs(c, k, f)"
                        placeholder="0"
                        @input="setPrice(c, k, ($event.target as HTMLInputElement).value, f)"
                      />
                      <span class="shrink-0 text-[11px] text-subtle">$/1M</span>
                    </div>
                  </div>
                  <div v-else-if="c.type === 'tiers'" class="space-y-2">
                    <div v-for="(t, idx) in tierRows(c, f)" :key="idx" class="rounded-md p-2" style="background: var(--surface-muted)">
                      <div class="flex items-center gap-1.5">
                        <input
                          class="input h-7! min-w-0 flex-1! text-[11.5px]!"
                          :value="(t.name as string | undefined) ?? ''"
                          placeholder="Tier name"
                          @input="setTier(c, f, idx, 'name', ($event.target as HTMLInputElement).value)"
                        />
                        <div class="flex items-center gap-1">
                          <input
                            class="input h-7! w-16! text-[11.5px]!"
                            type="number"
                            :value="(t.max_input_tokens as number | null | undefined) ?? ''"
                            placeholder="∞"
                            title="Max input tokens (blank = no limit)"
                            @input="setTier(c, f, idx, 'max_input_tokens', ($event.target as HTMLInputElement).value)"
                          />
                          <span class="text-[10.5px] text-subtle">tok</span>
                        </div>
                        <button class="btn-icon h-6! w-6!" type="button" aria-label="Remove tier" @click="removeTier(c, f, idx)">
                          <Trash2 class="h-3.5 w-3.5" />
                        </button>
                      </div>
                      <div class="mt-1.5 grid grid-cols-2 gap-1.5">
                        <div v-for="k in PRICE_KEYS" :key="k" class="flex items-center gap-1">
                          <span class="w-16 shrink-0 text-[10.5px] text-subtle">{{ PRICE_LABELS[k] }}</span>
                          <input
                            class="input h-6! min-w-0 flex-1! text-[11px]!"
                            type="number"
                            step="any"
                            :value="rowPriceAs(c, f, idx, k)"
                            placeholder="0"
                            @input="setRowPrice(c, f, idx, k, ($event.target as HTMLInputElement).value)"
                          />
                          <span class="shrink-0 text-[10.5px] text-subtle">$/1M</span>
                        </div>
                      </div>
                    </div>
                    <button class="btn btn-outline h-7! px-2.5! text-[12px]!" type="button" @click="addTier(c, f)">
                      <Plus class="h-3.5 w-3.5" /> Add tier
                    </button>
                  </div>
                  <div v-else-if="c.type === 'windows'" class="space-y-2">
                    <p class="text-[11px] text-subtle">
                      5-field cron (min hour day month weekday) marks the window start; the duration extends it to the minute.
                    </p>
                    <div v-for="(w, idx) in windowRows(c, f)" :key="idx" class="rounded-md p-2" style="background: var(--surface-muted)">
                      <div class="flex items-center gap-1.5">
                        <input
                          class="input h-7! min-w-0 flex-1! font-mono text-[11.5px]!"
                          :value="(w.cron as string | undefined) ?? ''"
                          placeholder="* * * * *"
                          @input="setWindow(c, f, idx, 'cron', ($event.target as HTMLInputElement).value)"
                        />
                        <div class="flex items-center gap-1">
                          <input
                            class="input h-7! w-16! text-[11.5px]!"
                            type="number"
                            min="1"
                            :value="(w.duration_min as number | undefined) ?? ''"
                            title="Duration in minutes"
                            @input="setWindow(c, f, idx, 'duration_min', ($event.target as HTMLInputElement).value)"
                          />
                          <span class="text-[10.5px] text-subtle">min</span>
                        </div>
                        <button class="btn-icon h-6! w-6!" type="button" aria-label="Remove window" @click="removeWindow(c, f, idx)">
                          <Trash2 class="h-3.5 w-3.5" />
                        </button>
                      </div>
                      <div class="mt-1.5 grid grid-cols-2 gap-1.5">
                        <div v-for="k in PRICE_KEYS" :key="k" class="flex items-center gap-1">
                          <span class="w-16 shrink-0 text-[10.5px] text-subtle">{{ PRICE_LABELS[k] }}</span>
                          <input
                            class="input h-6! min-w-0 flex-1! text-[11px]!"
                            type="number"
                            step="any"
                            :value="rowPriceAs(c, f, idx, k)"
                            placeholder="0"
                            @input="setRowPrice(c, f, idx, k, ($event.target as HTMLInputElement).value)"
                          />
                          <span class="shrink-0 text-[10.5px] text-subtle">$/1M</span>
                        </div>
                      </div>
                    </div>
                    <button class="btn btn-outline h-7! px-2.5! text-[12px]!" type="button" @click="addWindow(c, f)">
                      <Plus class="h-3.5 w-3.5" /> Add window
                    </button>
                  </div>
                  <input
                    v-else
                    :type="c.type === 'password' ? 'password' : 'text'"
                    :value="valueAs(c, f)"
                    class="input w-full! text-[12.5px]!"
                    :placeholder="c.placeholder"
                    @input="setField(c, ($event.target as HTMLInputElement).value, f)"
                  />
                </label>
              </div>
            </div>

            <!-- Regular top-level field -->
            <label v-else class="block">
              <span class="mb-1 flex items-center gap-1 text-[12px] font-medium text-foreground">
                {{ f.label }}
                <span v-if="f.required" class="text-[11px] text-danger">*</span>
              </span>
              <select
                v-if="f.type === 'select'"
                class="input w-full! text-[12.5px]!"
                :value="(fieldValue(f) as string | undefined) ?? ''"
                @change="setField(f, ($event.target as HTMLSelectElement).value)"
              >
                <option v-for="o in f.options" :key="o" :value="o">{{ o }}</option>
              </select>
              <textarea
                v-else-if="f.type === 'textarea'"
                :value="valueAs(f)"
                class="input w-full! resize-none text-[12px]! leading-relaxed!"
                rows="2"
                :placeholder="f.placeholder"
                @input="setField(f, ($event.target as HTMLTextAreaElement).value)"
              />
              <Toggle
                v-else-if="f.type === 'switch'"
                :model-value="boolOf(f)"
                @update:model-value="setField(f, $event)"
              />
              <input
                v-else-if="f.type === 'number'"
                :value="numAs(f)"
                class="input w-full! text-[12.5px]!"
                type="number"
                step="any"
                :placeholder="f.placeholder"
                @input="setField(f, numInput($event))"
              />
              <div v-else-if="f.type === 'prices'" class="space-y-1.5">
                <div v-for="k in PRICE_KEYS" :key="k" class="flex items-center gap-1.5">
                  <span class="w-20 shrink-0 text-[11px] text-subtle">{{ PRICE_LABELS[k] }}</span>
                  <input
                    class="input h-7! min-w-0 flex-1! text-[12px]!"
                    type="number"
                    step="any"
                    :value="priceAs(f, k)"
                    placeholder="0"
                    @input="setPrice(f, k, ($event.target as HTMLInputElement).value)"
                  />
                  <span class="shrink-0 text-[11px] text-subtle">$/1M</span>
                </div>
              </div>
              <div v-else-if="f.type === 'tiers'" class="space-y-2">
                <div v-for="(t, idx) in tierRows(f)" :key="idx" class="rounded-md p-2" style="background: var(--surface-muted)">
                  <div class="flex items-center gap-1.5">
                    <input
                      class="input h-7! min-w-0 flex-1! text-[11.5px]!"
                      :value="(t.name as string | undefined) ?? ''"
                      placeholder="Tier name"
                      @input="setTier(f, undefined, idx, 'name', ($event.target as HTMLInputElement).value)"
                    />
                    <div class="flex items-center gap-1">
                      <input
                        class="input h-7! w-16! text-[11.5px]!"
                        type="number"
                        :value="(t.max_input_tokens as number | null | undefined) ?? ''"
                        placeholder="∞"
                        title="Max input tokens (blank = no limit)"
                        @input="setTier(f, undefined, idx, 'max_input_tokens', ($event.target as HTMLInputElement).value)"
                      />
                      <span class="text-[10.5px] text-subtle">tok</span>
                    </div>
                    <button class="btn-icon h-6! w-6!" type="button" aria-label="Remove tier" @click="removeTier(f, undefined, idx)">
                      <Trash2 class="h-3.5 w-3.5" />
                    </button>
                  </div>
                  <div class="mt-1.5 grid grid-cols-2 gap-1.5">
                    <div v-for="k in PRICE_KEYS" :key="k" class="flex items-center gap-1">
                      <span class="w-16 shrink-0 text-[10.5px] text-subtle">{{ PRICE_LABELS[k] }}</span>
                      <input
                        class="input h-6! min-w-0 flex-1! text-[11px]!"
                        type="number"
                        step="any"
                        :value="rowPriceAs(f, undefined, idx, k)"
                        placeholder="0"
                        @input="setRowPrice(f, undefined, idx, k, ($event.target as HTMLInputElement).value)"
                      />
                      <span class="shrink-0 text-[10.5px] text-subtle">$/1M</span>
                    </div>
                  </div>
                </div>
                <button class="btn btn-outline h-7! px-2.5! text-[12px]!" type="button" @click="addTier(f)">
                  <Plus class="h-3.5 w-3.5" /> Add tier
                </button>
              </div>
              <div v-else-if="f.type === 'windows'" class="space-y-2">
                <p class="text-[11px] text-subtle">
                  5-field cron (min hour day month weekday) marks the window start; the duration extends it to the minute.
                </p>
                <div v-for="(w, idx) in windowRows(f)" :key="idx" class="rounded-md p-2" style="background: var(--surface-muted)">
                  <div class="flex items-center gap-1.5">
                    <input
                      class="input h-7! min-w-0 flex-1! font-mono text-[11.5px]!"
                      :value="(w.cron as string | undefined) ?? ''"
                      placeholder="* * * * *"
                      @input="setWindow(f, undefined, idx, 'cron', ($event.target as HTMLInputElement).value)"
                    />
                    <div class="flex items-center gap-1">
                      <input
                        class="input h-7! w-16! text-[11.5px]!"
                        type="number"
                        min="1"
                        :value="(w.duration_min as number | undefined) ?? ''"
                        title="Duration in minutes"
                        @input="setWindow(f, undefined, idx, 'duration_min', ($event.target as HTMLInputElement).value)"
                      />
                      <span class="text-[10.5px] text-subtle">min</span>
                    </div>
                    <button class="btn-icon h-6! w-6!" type="button" aria-label="Remove window" @click="removeWindow(f, undefined, idx)">
                      <Trash2 class="h-3.5 w-3.5" />
                    </button>
                  </div>
                  <div class="mt-1.5 grid grid-cols-2 gap-1.5">
                    <div v-for="k in PRICE_KEYS" :key="k" class="flex items-center gap-1">
                      <span class="w-16 shrink-0 text-[10.5px] text-subtle">{{ PRICE_LABELS[k] }}</span>
                      <input
                        class="input h-6! min-w-0 flex-1! text-[11px]!"
                        type="number"
                        step="any"
                        :value="rowPriceAs(f, undefined, idx, k)"
                        placeholder="0"
                        @input="setRowPrice(f, undefined, idx, k, ($event.target as HTMLInputElement).value)"
                      />
                      <span class="shrink-0 text-[10.5px] text-subtle">$/1M</span>
                    </div>
                  </div>
                </div>
                <button class="btn btn-outline h-7! px-2.5! text-[12px]!" type="button" @click="addWindow(f)">
                  <Plus class="h-3.5 w-3.5" /> Add window
                </button>
              </div>
              <input
                v-else
                :type="f.type === 'password' ? 'password' : 'text'"
                :value="valueAs(f)"
                class="input w-full! text-[12.5px]!"
                :placeholder="f.placeholder"
                @input="setField(f, ($event.target as HTMLInputElement).value)"
              />
            </label>
          </template>
        </div>

        <div class="mt-5 flex justify-end gap-2">
          <button class="btn" type="button" @click="emit('cancel')">Cancel</button>
          <button class="btn btn-primary" type="button" @click="submit">
            {{ props.confirmLabel ?? 'Add' }}
          </button>
        </div>
      </div>
    </div>
  </Teleport>
</template>