<script setup lang="ts">
import { onMounted, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useWorkspaceStore } from '@/stores/workspace'
import { rootRoute } from '@/lib/workspace-url'

/**
 * Root path redirect: enter the IDE shell (`/work/{id}/explorer`) when a
 * workspace is open, otherwise land on the welcome page (`/welcome`). Reactive
 * so a workspace restored asynchronously after refresh still routes correctly.
 *
 * Also handles the `?open=<path>` deep link used by "Open in new window":
 * the folder is opened (switching workspaces if one is already active) before
 * routing into the shell.
 */
const router = useRouter()
const route = useRoute()
const workspace = useWorkspaceStore()

function reroute() {
  const target = rootRoute()
  router.replace(target || '/welcome')
}

onMounted(async () => {
  const open = route.query.open
  if (typeof open === 'string' && open.trim()) {
    const r = await workspace.open(open.trim())
    if (!r.ok) {
      router.replace('/welcome')
      return
    }
  }
  reroute()
})
watch(() => workspace.hasActive, reroute)
</script>

<template>
  <div class="h-full bg-background" />
</template>