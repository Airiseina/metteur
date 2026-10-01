<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'

let registration: ServiceWorkerRegistration | undefined
const needRefresh = ref(false)
let reloadRequested = false
let disposed = false

function controllerChanged() {
  if (reloadRequested) window.location.reload()
  else needRefresh.value = true
}

function updateServiceWorker() {
  if (registration?.waiting) {
    reloadRequested = true
    registration.waiting.postMessage({ type: 'SKIP_WAITING' })
  } else window.location.reload()
}

onMounted(async () => {
  if (!import.meta.env.PROD || !('serviceWorker' in navigator)) return
  try {
    registration = await navigator.serviceWorker.register('/sw.js')
    if (disposed) return
    needRefresh.value = !!registration.waiting
    navigator.serviceWorker.addEventListener('controllerchange', controllerChanged)
    registration.addEventListener('updatefound', () => {
      const worker = registration?.installing
      worker?.addEventListener('statechange', () => {
        if (!disposed && worker.state === 'installed' && navigator.serviceWorker.controller) needRefresh.value = true
      })
    })
  } catch {
    // Offline/PWA support is optional; it must never block opening source.
  }
})

function checkForUpdate() {
  if (document.visibilityState === 'visible') void registration?.update().catch(() => undefined)
}
document.addEventListener('visibilitychange', checkForUpdate)
onBeforeUnmount(() => {
  disposed = true
  document.removeEventListener('visibilitychange', checkForUpdate)
  navigator.serviceWorker?.removeEventListener('controllerchange', controllerChanged)
})
</script>

<template>
  <div v-if="needRefresh" class="fixed bottom-10 right-4 z-50 max-w-sm rounded-xl border border-border bg-surface p-4 shadow-popover" role="status">
    <p class="text-[13px] font-medium">A new interface version is ready</p>
    <p class="mt-1 text-[12px] text-muted-foreground">Save any unsaved files, then reload to use the latest fixes.</p>
    <button class="btn btn-primary mt-3" type="button" @click="updateServiceWorker()">Reload application</button>
  </div>
</template>
