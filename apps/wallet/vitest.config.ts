import { defineConfig } from 'vitest/config';

export default defineConfig({
  define: { __PLATFORM__: JSON.stringify('mock'), __APP_VERSION__: JSON.stringify('test') },
  esbuild: { jsx: 'automatic', jsxImportSource: 'preact' },
  test: { include: ['tests/unit/**/*.test.ts'], environment: 'node' },
});
