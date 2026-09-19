<script setup lang="ts">
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { Folder, Star, X } from '@lucide/vue'
import AppLogo from '@/components/AppLogo.vue'
import { gateway } from '@/core'
import { useWorkspaceStore } from '@/stores/workspace'
import { projectName } from '@/lib/path'
import { wurl } from '@/lib/workspace-url'

const workspace = useWorkspaceStore()
const router = useRouter()
const pathInput = ref('')
const openError = ref('')

async function openPath() {
  const path = pathInput.value.trim()
  if (!path) return
  const r = await workspace.open(path)
  if (r.ok) {
    pathInput.value = ''
    openError.value = ''
    router.push(wurl('/chat'))
  } else {
    openError.value = r.message ?? 'Failed to open workspace'
  }
}

async function openRecent(path: string) {
  const r = await workspace.open(path)
  if (r.ok) {
    openError.value = ''
    router.push(wurl('/chat'))
  } else {
    openError.value = r.message ?? 'Failed to open workspace'
  }
}

/** Open a native folder dialog on the host via the Web Server Client. */
async function browse() {
  openError.value = ''
  const result = await gateway.pickDirectory()
  if (!result.ok) {
    openError.value = `Folder picker unavailable: ${result.error}`
    return
  }
  if (result.data) pathInput.value = result.data
}
</script>

<template>
  <div class="h-full overflow-auto bg-background">
    <div class="mx-auto max-w-lg p-10">
      <!-- Header -->
      <div class="flex flex-col items-center text-center">
        <span class="mb-5 block h-16 w-16 rounded-2xl p-3 shadow-card" style="background: var(--primary)">
          <AppLogo :size="40" />
        </span>
        <h1 class="text-2xl font-semibold tracking-tight">Welcome to Metteur</h1>
        <p class="mt-2 text-[13px] text-muted-foreground">
          A vibe-coding agent. Open a folder to chat, design blueprints and track versions.
        </p>
      </div>

      <!-- Open by path -->
      <form class="mt-8 flex gap-2" @submit.prevent="openPath">
        <input
          v-model="pathInput"
          class="input min-w-0 flex-1 h-9!"
          placeholder="Open a local folder, e.g. D:/playground/metrics"
          type="text"
          :aria-label="'Workspace path'"
        />
        <button
          class="btn h-9!"
          type="button"
          title="Choose a folder from the OS dialog"
          aria-label="Browse folders"
          @click="browse"
        >
          Browse…
        </button>
        <button
          class="btn btn-primary h-9!"
          type="submit"
          :disabled="workspace.busy || !pathInput.trim()"
        >
          Open
        </button>
      </form>

      <p
        v-if="openError"
        class="mt-2 rounded-md px-3 py-2 text-[12px]"
        style="background: rgba(239, 68, 68, 0.12); color: #f87171"
        role="alert"
      >
        {{ openError }}
      </p>

      <!-- Recent workspaces -->
      <section v-if="workspace.recents.length" class="mt-8">
        <div class="flex items-center gap-1.5">
          <Star class="h-3.5 w-3.5 text-subtle" />
          <p class="panel-heading">Recent</p>
        </div>
        <ul class="mt-1 space-y-1">
          <li v-for="w in workspace.recents" :key="w.path">
            <div
              class="group flex w-full items-center gap-2.5 rounded-lg px-3 py-2 transition-colors duration-150 hover:bg-hover"
              :class="workspace.active?.path === w.path ? 'bg-accent' : ''"
            >
              <button
                class="flex min-w-0 flex-1 items-center gap-2.5 text-left"
                type="button"
                @click="openRecent(w.path)"
              >
                <Folder class="h-4 w-4 shrink-0 text-muted-foreground" />
                <span class="min-w-0">
                  <span class="block truncate text-[13px] font-medium">{{ projectName(w.path) }}</span>
                  <span class="block truncate text-[11px] text-subtle">{{ w.path }}</span>
                </span>
              </button>
              <button
                class="flex h-6 w-6 shrink-0 items-center justify-center rounded text-muted-foreground opacity-0 transition-opacity duration-150 hover:bg-hover hover:text-foreground group-hover:opacity-100"
                type="button"
                title="Remove from recents"
                aria-label="Remove from recents"
                @click="workspace.forget(w.path)"
              >
                <X class="h-4 w-4" />
              </button>
            </div>
          </li>
        </ul>
      </section>

      <p v-if="!workspace.recents.length" class="mt-8 text-center text-[12px] text-subtle">
        No workspace open yet — type a folder path above to get started.
      </p>
    </div>
  </div>
</template>