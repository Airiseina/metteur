/// <reference types="vite/client" />
/// <reference types="vite-plugin-pwa/client" />

interface ImportMetaEnv {
  /** Set to `1` to use the demo (mock) gateway instead of grpc-web. */
  readonly VITE_MOCK?: string
  /** Base URL of the grpc-web endpoint (default: same origin). */
  readonly VITE_GW_URL?: string
}

declare module '*.vue' {
  import type { DefineComponent } from 'vue'
  const component: DefineComponent<object, object, unknown>
  export default component
}
