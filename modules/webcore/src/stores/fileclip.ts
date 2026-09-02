import { defineStore } from 'pinia'
import { computed, ref } from 'vue'

/**
 * VSCode-style file clipboard for the explorer: holds the selected paths and
 * whether they will be moved (cut) or copied. Consumed by the context-menu
 * Copy / Cut / Paste actions and cleared after a paste.
 */

export const useFileClipboardStore = defineStore('fileclip', () => {
  const op = ref<'copy' | 'cut' | null>(null)
  const sources = ref<string[]>([])

  /** Whether a paste can currently be performed. */
  const active = computed(() => op.value !== null && sources.value.length > 0)

  /** Stage paths for the given operation. */
  function set(operation: 'copy' | 'cut', paths: string[]) {
    op.value = operation
    sources.value = [...paths]
  }

  function clear() {
    op.value = null
    sources.value = []
  }

  return { op, sources, active, set, clear }
})