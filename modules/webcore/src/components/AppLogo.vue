<script setup lang="ts">
/** Brand mark rendered from the favicon artwork (shared with /favicon.svg). */
withDefaults(defineProps<{ size?: number }>(), { size: 24 })
</script>

<template>
  <img
    :width="size"
    :height="size"
    :style="{ borderRadius: Math.round(size * 0.22) + 'px' }"
    src="/favicon.svg"
    alt="Metteur"
    class="shrink-0"
  />
</template>