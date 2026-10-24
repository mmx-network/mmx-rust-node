import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

const SOURCE = readFileSync(fileURLToPath(new URL('../../src-tauri/src/page_guard.js', import.meta.url)), 'utf8');

type Listener = (event: Record<string, unknown>) => void;

/** Runs the desktop page guard against a fake window; returns its keydown handler. */
function keydown(macos: boolean): (event: Partial<KeyboardEvent>) => boolean {
  const listeners = new Map<string, Listener>();
  const fakeWindow = { addEventListener: (type: string, fn: Listener) => listeners.set(type, fn) };
  class FakeElement {
    closest(): null {
      return null;
    }
  }
  new Function('window', 'Element', SOURCE.replace('__MACOS__', String(macos)))(fakeWindow, FakeElement);
  const handler = listeners.get('keydown')!;
  return (init) => {
    let prevented = false;
    handler({ ctrlKey: false, metaKey: false, altKey: false, code: '', target: null, ...init, preventDefault: () => (prevented = true) });
    return prevented;
  };
}

describe('desktop page guard', () => {
  it('blocks print, save, reload and view source with the platform modifier only', () => {
    const other = keydown(false);
    const mac = keydown(true);
    for (const key of ['r', 'p', 's', 'u']) {
      expect(other({ key, ctrlKey: true }), key).toBe(true);
      expect(mac({ key, metaKey: true }), key).toBe(true);
      // macOS text fields use Ctrl+letter (Ctrl+P: up a line).
      expect(mac({ key, ctrlKey: true }), key).toBe(false);
    }
    expect(other({ key: 'a', ctrlKey: true })).toBe(false);
    expect(other({ key: 'F5' })).toBe(true);
  });
});
