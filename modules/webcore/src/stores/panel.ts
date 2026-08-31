import { defineStore } from 'pinia'
import type { Component } from 'vue'
import { markRaw, ref, watch } from 'vue'

/** Activities that can own the left sidebar panel. */
export type PanelOwner = 'explorer' | 'version' | 'settings' | null

const WIDTH_KEY = 'metteur.sidebar.width'

/**
 * Generic ide-sidebar panel (VSCode-style).
 *
 * Exactly one surface owns the resizable left panel at a time: the file
 * explorer, a view's own navigation rail (e.g. the version timeline), etc.
 * A view pops its own panel in via {@link show}; the shell renders whatever
 * `component` holds. The panel width is a user preference and persists across
 * reloads.
 */
export const usePanelStore = defineStore('panel', () => {
  const component = ref<Component | null>(null)
  const title = ref('')
  const open = ref(false)
  /** Which activity owns the open panel; drives the rail highlight. */
  const owner = ref<PanelOwner>(null)
  /** User-resized panel width (px). */
  const width = ref(Number(localStorage.getItem(WIDTH_KEY)) || 232)

  watch(width, (w) => localStorage.setItem(WIDTH_KEY, String(Math.round(w))))

  /** Populate the panel for `owner`, marking it open. Pass `null` to close. */
  function show(ownerKey: PanelOwner, comp: Component | null, label = '') {
    owner.value = ownerKey
    component.value = comp ? markRaw(comp) : null
    title.value = label
    open.value = comp !== null
  }

  /** Close the panel regardless of owner. */
  function clear() {
    show(null, null, '')
  }

  return { component, title, open, owner, width, show, clear }
})