import { defineConfig } from 'vitest/config';
import { fileURLToPath } from 'node:url';

export default defineConfig({
  define: { __PLATFORM__: JSON.stringify('web') },
  resolve: { alias: { '@wallet': fileURLToPath(new URL('../wallet/src', import.meta.url)) } },
  test: { include: ['tests/unit/**/*.test.ts'], environment: 'node', maxWorkers: 2 },
});
