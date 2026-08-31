<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { ChevronDown, ChevronRight, Search } from '@lucide/vue'

/**
 * Generic Unreal-style context menu.
 *
 * Rendered as a floating popover at a fixed screen position, optionally with a
 * search box. Groups are collapsible (click the header to fold them) and items
 * may carry `children` to open a nested fly-out submenu on hover. Emits
 * `select` with the clicked leaf id and closes on outside click / Escape.
 */

export interface SubMenu {
  label: string
  /** Nested items; items here may also nest for multi-level submenus. */
  items: MenuItem[]
}

export interface MenuItem {
  id: string
  label: string
  /** Right-hand hint, e.g. a shortcut ("Del", "⌫"). */
  hint?: string
  /** Nested fly-out submenu. */
  children?: SubMenu | MenuItem[]
  /** Shown as a chevron ▸ at the right (set for items with children). */
  sublabel?: string
}

export interface MenuGroup {
  label: string
  color?: string
  items: MenuItem[]
  /** Whether the group header shows a collapse chevron (default: true). */
  collapsible?: boolean
}

const props = withDefaults(
  defineProps<{
    x: number
    y: number
    groups: MenuGroup[]
    searchable?: boolean
  }>(),
  { searchable: false },
)

const emit = defineEmits<{
  (e: 'select', id: string): void
  (e: 'close'): void
}>()

const MENU_W = 248
const MENU_H = 360
const PAD = 8

const x = Math.min(props.x, window.innerWidth - MENU_W - PAD)
const y = Math.min(props.y, window.innerHeight - MENU_H - PAD)

const query = ref('')
/** Group labels folded away by the user. */
const collapsed = ref<Set<string>>(new Set())

function toggleGroup(label: string) {
  const next = new Set(collapsed.value)
  if (next.has(label)) next.delete(label)
  else next.add(label)
  collapsed.value = next
}
function isCollapsed(label: string): boolean {
  return collapsed.value.has(label)
}

const filtered = computed<MenuGroup[]>(() => {
  const q = query.value.trim().toLowerCase()
  if (!q) return props.groups
  return props.groups
    .map((g) => ({ ...g, items: g.items.filter((it) => it.label.toLowerCase().includes(q)) }))
    .filter((g) => g.items.length > 0)
})

function isSubmenu(children: SubMenu | MenuItem[] | undefined): children is SubMenu {
  return children !== undefined && 'label' in children
}

function onPick(id: string) {
  emit('select', id)
  emit('close')
}

function onKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape') emit('close')
}

onMounted(() => {
  window.addEventListener('mousedown', onGlobalDown)
  window.addEventListener('keydown', onKeydown)
})

onBeforeUnmount(() => {
  window.removeEventListener('mousedown', onGlobalDown)
  window.removeEventListener('keydown', onKeydown)
})

const menuRoot = ref<HTMLElement | null>(null)
function onGlobalDown(e: MouseEvent) {
  if (menuRoot.value && !menuRoot.value.contains(e.target as Node)) emit('close')
}
</script>

<template>
  <Teleport to="body">
    <div
      ref="menuRoot"
      class="glass fixed z-50 flex flex-col overflow-hidden rounded-lg py-1"
      :style="{ left: x + 'px', top: y + 'px', width: MENU_W + 'px', maxWidth: MENU_W + 'px' }"
      role="menu"
    >
      <div v-if="searchable" class="relative px-2 pb-1 pt-1">
        <Search class="pointer-events-none absolute left-3.5 top-2.5 h-3.5 w-3.5 text-subtle" />
        <input
          v-model="query"
          class="input h-7! w-full pl-7 text-[12px]"
          placeholder="Filter…"
          type="text"
          autofocus
        />
      </div>

      <div class="min-h-0 overflow-y-auto px-1 pb-1">
        <div v-for="g in filtered" :key="g.label" class="mb-0.5">
          <!-- Collapsible group header -->
          <button
            type="button"
            class="flex w-full items-center gap-1 rounded-md px-1.5 py-0.5 text-left text-[10px] font-semibold uppercase tracking-wider text-subtle transition-colors hover:bg-hover hover:text-foreground"
            @click="g.collapsible !== false && toggleGroup(g.label)"
          >
            <span class="flex min-w-0 shrink-0 items-center gap-1.5">
              <span v-if="g.color" class="h-2 w-2 shrink-0 rounded-sm" :style="{ background: g.color }" />
              <span class="truncate">{{ g.label }}</span>
            </span>
            <ChevronDown
              v-if="g.collapsible !== false"
              class="ml-auto h-3 w-3 shrink-0 text-subtle transition-transform duration-150"
              :class="isCollapsed(g.label) ? '-rotate-90' : ''"
            />
          </button>

          <div
            v-show="query.trim() !== '' || !isCollapsed(g.label)"
            v-for="item in g.items"
            :key="item.id"
            class="group/sub relative"
          >
            <button
              class="flex w-full items-center justify-between gap-2 rounded-md px-2.5 py-1.5 text-left text-[12.5px] text-foreground transition-colors duration-100 hover:bg-accent hover:text-accent-foreground"
              type="button"
              role="menuitem"
              @click="item.children ? undefined : onPick(item.id)"
            >
              <span class="flex min-w-0 items-center gap-2">
                <span class="truncate font-medium">{{ item.label }}</span>
              </span>
              <span v-if="item.children || item.sublabel" class="flex shrink-0 items-center gap-1">
                <span v-if="item.sublabel" class="text-[10px] text-subtle">{{ item.sublabel }}</span>
                <ChevronRight class="h-3 w-3 text-subtle" />
              </span>
              <span v-else-if="item.hint" class="shrink-0 text-[10px] text-subtle">{{ item.hint }}</span>
            </button>

            <!-- Nested submenu, shown on hover of the parent item -->
            <div
              v-if="item.children"
              class="glass invisible absolute left-full top-0 z-50 ml-0.5 w-52 rounded-lg py-1 opacity-0 shadow-lg transition-opacity duration-100 group-hover/sub:visible group-hover/sub:opacity-100"
            >
              <template v-for="sub in isSubmenu(item.children) ? item.children.items : item.children" :key="sub.id">
                <button
                  class="flex w-full items-center justify-between gap-2 rounded-md px-2.5 py-1.5 text-left text-[12.5px] text-foreground transition-colors duration-100 hover:bg-accent hover:text-accent-foreground"
                  type="button"
                  role="menuitem"
                  @click="sub.children ? undefined : onPick(sub.id)"
                >
                  <span class="truncate font-medium">{{ sub.label }}</span>
                  <ChevronRight v-if="sub.children" class="h-3 w-3 shrink-0 text-subtle" />
                  <span v-else-if="sub.hint" class="shrink-0 text-[10px] text-subtle">{{ sub.hint }}</span>
                </button>
              </template>
            </div>
          </div>
        </div>
      </div>
    </div>
  </Teleport>
</template>