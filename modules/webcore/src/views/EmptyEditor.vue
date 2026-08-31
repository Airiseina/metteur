<script setup lang="ts">
import { FileCode2 } from '@lucide/vue'
</script>

<template>
  <div class="flex h-full items-center justify-center bg-background">
    <div class="max-w-sm px-6 text-center">
      <span
        class="mx-auto mb-4 flex h-12 w-12 items-center justify-center rounded-2xl bg-surface-muted text-muted-foreground"
      >
        <FileCode2 class="h-6 w-6" />
      </span>
      <p class="text-[13.5px] font-medium">No file open</p>
      <p class="mt-1 text-[12px] text-muted-foreground">
        Pick a file from the explorer on the left, or ask the agent to open one for you.
      </p>
    </div>
  </div>
</template>