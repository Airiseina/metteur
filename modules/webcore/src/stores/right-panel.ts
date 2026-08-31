import { defineStore } from 'pinia'
import type { Component } from 'vue'
import { markRaw, ref, watch } from 'vue'

const WIDTH_KEY = 'metteur.right-panel.width'

/**
 * Generic right-hand side panel (VSCode-style secondary sidebar).
 *
 * Mirrors the left {@link usePanelStore}: exactly one surface owns the right
 * rail at a time, its body is whatever component the owner supplies, and the
 * width is a persisted user preference. It is always closable from its own
 * header. The editor opens it (e.g. per-file version history); other features
 * can reuse it the same way by calling {@link show}.
 */
export const useRightPanelStore = defineStore('right-panel', () => {
  const component = ref<Component | null>(null)
  const title = ref('')
  const open = ref(false)
  /** Props forwarded to the mounted panel component, e.g. the file's path. */
  const props = ref<Record<string, unknown>>({})
  const width = ref(Number(localStorage.getItem(WIDTH_KEY)) || 300)

  watch(width, (w) => localStorage.setItem(WIDTH_KEY, String(Math.round(w))))

  /** Populate the right panel, marking it open. Pass `null` to close it. */
  function show(comp: Component | null, label = '', panelProps: Record<string, unknown> = {}) {
    component.value = comp ? markRaw(comp) : null
    title.value = label
    props.value = panelProps
    open.value = comp !== null
  }

  function close() {
    show(null, '')
  }

  return { component, title, open, props, width, show, close }
})