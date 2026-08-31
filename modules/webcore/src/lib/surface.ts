import { useRouter } from 'vue-router'
import VersionSidebar from '@/components/VersionSidebar.vue'
import SettingsNav from '@/components/SettingsNav.vue'
import { useTabsStore } from '@/stores/tabs'
import { usePanelStore } from '@/stores/panel'
import { wurl } from '@/lib/workspace-url'

/**
 * Surface navigation with its owning sidebar panel asserted.
 *
 * Surface ownership is a single source of truth so that re-opening a surface
 * (from the activity rail *or* from its tab) always restores the correct left
 * rail — fixing the "Version sidebar vanishes after toggling the Explorer and
 * returning" bug where a tab click only re-navigated the route and never
 * re-asserted the panel.
 *
 * This must be used as a composable (called during a component's `setup()`):
 * `useRouter()` and the Pinia stores rely on active-injection, which does not
 * exist at event-handler time. Each consumer resolves them once in setup and
 * reuses the returned `openSurface`.
 */

type SurfaceKey = 'chat' | 'execution' | 'audit' | 'version' | 'settings'
type PanelSurface = Exclude<SurfaceKey, 'execution'>

interface SurfacePanel {
  owner: 'version' | 'settings'
  comp: typeof VersionSidebar | typeof SettingsNav
  title: string
}

/** Sidebar each surface should show; `null` means full-width (no rail). */
function surfacePanel(surface: PanelSurface): SurfacePanel | null {
  if (surface === 'version') return { owner: 'version', comp: VersionSidebar, title: 'Version Flow' }
  if (surface === 'settings') return { owner: 'settings', comp: SettingsNav, title: 'Settings' }
  return null
}

export function useSurfaceNavigation() {
  const router = useRouter()
  const panel = usePanelStore()
  const tabs = useTabsStore()

  /** Open (or focus) a surface tab, navigate to it, and assert its sidebar. */
  function openSurface(key: SurfaceKey) {
    const surface = key === 'execution' ? 'audit' : key
    tabs.openSurface(surface as 'chat' | 'audit' | 'version' | 'settings')
    const target = surface === 'settings' ? '/settings' : wurl(`/${surface === 'audit' ? 'execution' : surface}`)
    router.push(target)
    const p = surfacePanel(surface)
    if (p) panel.show(p.owner, p.comp, p.title)
    // chat / audit are full-page surfaces: they own no sidebar, so the current
    // panel (e.g. the Explorer) stays open instead of being cleared.
  }

  return { openSurface }
}

export type { SurfaceKey }