<script setup lang="ts">
import { computed, ref } from 'vue'
import { formatTokens } from '@/lib/chat/liveness'
import type { ChatContextStats, ChatUsage } from '@/core'

/**
 * Context capacity.
 *
 * A ring with the percentage of the window in use, and a popover breaking the
 * number down by region. The figure is what the *model* carries — the
 * conversation plus its system prompt and tool schemas — so the breakdown is
 * what makes it actionable: it says whether the growth is the conversation or
 * the toolchain.
 *
 * Percentages follow the convention providers use for caching: the share of the
 * window that is actually occupied, with the response reservation shown
 * separately. Nothing here is billed data: the token counts are the daemon's
 * estimates.
 */
const props = defineProps<{
  stats: ChatContextStats | null
  usage: ChatUsage | null
}>()

const open = ref(false)

/** Tokens the conversation occupies, or `null` when the daemon reported none. */
const used = computed(() => props.stats?.tokens ?? null)
const limit = computed(() => props.stats?.limit ?? null)
/** True when the daemon had to assume a window instead of reading one. */
const assumed = computed(() => props.stats?.assumedLimit === true)
const usedRatio = computed(() => {
  if (!used.value || !limit.value) return 0
  return Math.min(1, used.value / limit.value)
})
const percent = computed(() => Math.round(usedRatio.value * 100))

/** Colour thresholds: quiet until it matters, amber when it is close. */
const tone = computed(() => {
  if (percent.value >= 95) return 'critical'
  if (percent.value >= 80) return 'warn'
  return 'normal'
})

/** Cache hit rate of the last request, when the provider reported one. */
const cacheRate = computed(() => {
  const usage = props.usage
  if (!usage?.inputTokens || !usage.cachedInputTokens) return null
  return Math.round((usage.cachedInputTokens / usage.inputTokens) * 100)
})

const regions = computed(() =>
  (props.stats?.regions ?? []).map((region) => ({
    ...region,
    share: used.value ? Math.round((region.tokens / used.value) * 100) : 0,
  })),
)

/** Free space left in the window. */
const free = computed(() => (limit.value && used.value ? Math.max(0, limit.value - used.value) : null))

/** Stroke geometry of the ring: a circle whose dash array encodes the ratio. */
const RING = 20
const circumference = 2 * Math.PI * RING
const dash = computed(() => `${(usedRatio.value * circumference).toFixed(2)} ${circumference.toFixed(2)}`)

const REGION_LABELS: Record<string, string> = {
  system: 'System & tools',
  user: 'Your turns',
  assistant: 'Agent turns',
  tool: 'Tool results',
}
</script>

<template>
  <div class="relative">
    <button
      class="chat-meter"
      :class="`chat-meter-${tone}`"
      type="button"
      :aria-label="`Context: ${percent}% of the window in use`"
      :aria-expanded="open"
      @click="open = !open"
    >
      <svg class="chat-meter-ring" viewBox="0 0 48 48" aria-hidden="true">
        <circle class="chat-meter-track" cx="24" cy="24" :r="RING" />
        <circle class="chat-meter-fill" cx="24" cy="24" :r="RING" :stroke-dasharray="dash" />
      </svg>
      <span v-if="used !== null && limit" class="chat-meter-label tabular-nums">{{ percent }}%</span>
      <span v-else-if="used !== null" class="chat-meter-label tabular-nums">
        {{ formatTokens(used) }}
      </span>
      <span v-else class="chat-meter-label">—</span>
    </button>

    <div v-if="open" class="chat-menu chat-meter-popover">
      <p class="chat-menu-head">Context window</p>
      <dl class="chat-meter-rows">
        <div class="chat-meter-row">
          <dt>Used</dt>
          <dd class="tabular-nums">{{ used !== null ? formatTokens(used) : 'unknown' }}</dd>
        </div>
        <div v-if="limit" class="chat-meter-row">
          <dt>{{ assumed ? 'Window (assumed)' : 'Window' }}</dt>
          <dd class="tabular-nums">{{ formatTokens(limit) }}</dd>
        </div>
        <div v-if="free !== null" class="chat-meter-row chat-meter-row-muted">
          <dt>Free</dt>
          <dd class="tabular-nums">{{ formatTokens(free) }}</dd>
        </div>
        <div class="chat-meter-row">
          <dt>Cache hit</dt>
          <dd class="tabular-nums">{{ cacheRate !== null ? `${cacheRate}%` : 'not reported' }}</dd>
        </div>
      </dl>

      <template v-if="regions.length">
        <div class="my-2 border-t border-divider" />
        <p class="chat-menu-head">Composition</p>
        <ul class="chat-meter-regions">
          <li v-for="region in regions" :key="region.region">
            <span class="chat-meter-region-name">{{ REGION_LABELS[region.region] ?? region.region }}</span>
            <span class="chat-meter-region-bar" aria-hidden="true">
              <span :style="{ width: `${region.share}%` }" />
            </span>
            <span class="tabular-nums text-subtle">{{ formatTokens(region.tokens) }}</span>
          </li>
        </ul>
      </template>

      <p class="chat-meter-note">
        Estimates from the daemon; the provider reports exact counts in the usage of each turn.
        <template v-if="assumed">
          This model's window is not configured, so a 128k default is assumed —
          set it under Settings → LLM &amp; Models for an exact percentage.
        </template>
      </p>
    </div>

    <div v-if="open" class="fixed inset-0 z-30" @mousedown="open = false" />
  </div>
</template>
