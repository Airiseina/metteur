import { defineStore } from 'pinia'
import { ref, watch } from 'vue'

const STORAGE_KEY = 'metteur.theme'

function initialMode(): 'light' | 'dark' {
  const saved = localStorage.getItem(STORAGE_KEY)
  if (saved === 'light' || saved === 'dark') return saved
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
}

/**
 * Light/dark appearance with runtime toggle.
 *
 * Applies the `.dark` class on `<html>` (consumed by `styles/theme.css`) and
 * persists the choice so the mode survives reloads.
 */
export const useThemeStore = defineStore('theme', () => {
  const mode = ref<'light' | 'dark'>(initialMode())

  function apply(m: 'light' | 'dark') {
    document.documentElement.classList.toggle('dark', m === 'dark')
  }

  apply(mode.value)
  watch(mode, (m) => {
    apply(m)
    localStorage.setItem(STORAGE_KEY, m)
  })

  function toggle() {
    mode.value = mode.value === 'dark' ? 'light' : 'dark'
  }

  return { mode, toggle }
})
