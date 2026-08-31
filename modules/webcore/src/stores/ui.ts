import { defineStore } from 'pinia'
import { ref } from 'vue'

/**
 * Transient UI state for the IDE shell.
 *
 * Owns the left activity bar selection and the workspace-gated surfaces that
 * should survive route changes. Persistent preferences (theme, etc.) live in
 * their own stores.
 */
export const useUiStore = defineStore('ui', () => {
  /** Which activity is focused; drives the highlighted icon and the title. */
  const activity = ref<'workspace' | 'blueprint' | 'execution' | 'version' | 'settings' | null>(null)

  /** Whether the contextual side panel (e.g. the blueprint palette) is open. */
  const sidePanelOpen = ref(true)

  function setActivity(a: typeof activity.value) {
    activity.value = a
  }

  function toggleSidePanel() {
    sidePanelOpen.value = !sidePanelOpen.value
  }

  return { activity, sidePanelOpen, setActivity, toggleSidePanel }
})