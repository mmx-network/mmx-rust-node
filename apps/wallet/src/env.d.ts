/// <reference types="vite/client" />

declare const __PLATFORM__: 'web' | 'desktop' | 'mock';
declare const __APP_VERSION__: string;

interface ImportMetaEnv {
  /** Web: chain server URL fixed at build time (default https://rpc.mmx.network). */
  readonly VITE_RPC_URL?: string;
  /** Visual style id (src/styles/themes/<id>). */
  readonly VITE_WALLET_STYLE?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
