import { h, render } from 'preact';
import { act } from 'preact/test-utils';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import type { WalletInfo } from '../../src/backend/types';
import { NO_TOASTS_PLACED, placeToasts, ToastViewport, type Toast } from '../../src/ui/components/Toasts';
import { initialFocus, MENU_ITEM_SELECTOR, type MenuItem } from '../../src/ui/components/Menu';
import { documentTitle, focusNewPage } from '../../src/ui/shell/Shell';
import { walletMenuItems } from '../../src/ui/shell/WalletSwitcher';

// ------------------------------------------------------------ minimal DOM
// Just enough of the DOM for Preact to render into, so the toast region can
// be checked commit by commit without a browser.

class FakeNode {
  parentNode: FakeElement | null = null;
  constructor(readonly nodeType: number) {}
  get nextSibling(): FakeNode | null {
    const siblings = this.parentNode?.childNodes ?? [];
    return siblings[siblings.indexOf(this) + 1] ?? null;
  }
}

class FakeText extends FakeNode {
  constructor(public data: string) {
    super(3);
  }
  get textContent(): string {
    return this.data;
  }
}

class FakeElement extends FakeNode {
  readonly childNodes: FakeNode[] = [];
  readonly attributes = new Map<string, string>();
  readonly style = { setProperty: (): void => undefined };
  constructor(
    readonly localName: string,
    readonly namespaceURI: string,
  ) {
    super(1);
  }
  get firstChild(): FakeNode | null {
    return this.childNodes[0] ?? null;
  }
  get children(): FakeElement[] {
    return this.childNodes.filter((n): n is FakeElement => n instanceof FakeElement);
  }
  get textContent(): string {
    return this.childNodes.map((n) => (n as FakeElement | FakeText).textContent).join('');
  }
  insertBefore(node: FakeNode, ref: FakeNode | null): FakeNode {
    node.parentNode?.removeChild(node);
    const at = ref ? this.childNodes.indexOf(ref) : -1;
    this.childNodes.splice(at < 0 ? this.childNodes.length : at, 0, node);
    node.parentNode = this;
    return node;
  }
  appendChild(node: FakeNode): FakeNode {
    return this.insertBefore(node, null);
  }
  removeChild(node: FakeNode): FakeNode {
    this.childNodes.splice(this.childNodes.indexOf(node), 1);
    node.parentNode = null;
    return node;
  }
  setAttribute(name: string, value: unknown): void {
    this.attributes.set(name, String(value));
  }
  getAttribute(name: string): string | null {
    return this.attributes.get(name) ?? null;
  }
  removeAttribute(name: string): void {
    this.attributes.delete(name);
  }
  addEventListener(): void {}
  removeEventListener(): void {}
}

const fakeDocument = {
  createElementNS: (ns: string, name: string) => new FakeElement(name, ns),
  createTextNode: (text: string) => new FakeText(text),
};

const g = globalThis as { document?: unknown };
const savedDocument = g.document;
beforeAll(() => {
  g.document = fakeDocument;
});
afterAll(() => {
  g.document = savedDocument;
});

// ------------------------------------------------------------------ toasts

describe('toast announcements', () => {
  const toast = (id: string, tone?: Toast['tone']): Toast => ({ id, title: `Toast ${id}`, ...(tone ? { tone } : {}) });

  it('announces errors assertively and everything else politely', () => {
    let placement = placeToasts([toast('1', 'positive')], NO_TOASTS_PLACED);
    expect(placement.live).toBe('polite');
    expect([...placement.ids]).toEqual(['1']);
    placement = placeToasts([toast('1', 'positive'), toast('2', 'negative')], placement);
    expect(placement.live).toBe('assertive');
    // An error that is already shown does not make the next toast interrupt.
    placement = placeToasts([toast('2', 'negative'), toast('3')], placement);
    expect(placement.live).toBe('polite');
    expect([...placement.ids]).toEqual(['2', '3']);
  });

  it('adds new toasts to a live region that is already on the page', async () => {
    const root = new FakeElement('div', 'http://www.w3.org/1999/xhtml');
    const draw = (toasts: Toast[]): void => {
      render(h(ToastViewport, { toasts, onDismiss: () => undefined }), root as unknown as Element);
    };
    await act(() => draw([]));
    const region = root.children[0]!;
    expect(region.getAttribute('role')).toBe('region');
    expect(region.getAttribute('aria-live')).toBe('polite');
    expect(region.children).toHaveLength(0);

    await act(() => {
      draw([toast('a', 'positive')]);
      // Same commit as the new props: the toast is not inserted yet.
      expect(region.children).toHaveLength(0);
    });
    expect(root.children[0]).toBe(region);
    expect(region.children).toHaveLength(1);
    const item = region.children[0]!;
    expect(item.textContent).toContain('Toast a');
    // The toast itself is not a separate live region.
    expect(item.getAttribute('role')).toBeNull();
    expect(item.getAttribute('aria-live')).toBeNull();

    await act(() => draw([toast('a', 'positive'), toast('b', 'negative')]));
    expect(region.getAttribute('aria-live')).toBe('assertive');
    expect(region.children.map((c) => c.textContent)).toEqual(['Toast a', 'Toast b']);

    // Dismissing removes the toast at once.
    await act(() => draw([toast('b', 'negative')]));
    expect(region.children.map((c) => c.textContent)).toEqual(['Toast b']);

    await act(() => render(null, root as unknown as Element));
  });
});

// ---------------------------------------------------------- wallet switcher

describe('wallet switcher', () => {
  const wallet = (id: string, extra: Partial<WalletInfo> = {}): WalletInfo => ({
    id,
    name: `Wallet ${id}`,
    fingerprint: '123456',
    withPassphrase: false,
    locked: false,
    addressCount: 1,
    ...extra,
  });

  it('shows a wallet that cannot be opened but does not let it be chosen', () => {
    const chosen: string[] = [];
    const items = walletMenuItems(
      [wallet('ok'), wallet('broken', { error: 'unreadable key file' })],
      'ok',
      (id) => chosen.push(id),
      () => chosen.push('add'),
    ).filter((e): e is MenuItem => !('separator' in e));
    const [ok, broken, add] = items;
    expect(ok?.disabled).toBe(false);
    expect(ok?.checked).toBe(true);
    expect(broken?.disabled).toBe(true);
    expect(broken?.checked).toBe(false);
    expect(broken?.description).toBe('Can’t be opened');
    expect(add?.label).toBe('Add wallet');
    expect(add?.disabled).toBeUndefined();
    ok?.onSelect();
    add?.onSelect();
    expect(chosen).toEqual(['ok', 'add']);
  });

  it('keeps unavailable items reachable by keyboard but never focuses one first', () => {
    // Arrow keys, Home/End and type-ahead use every item, so a wallet that
    // cannot be opened is announced with its reason.
    expect(MENU_ITEM_SELECTOR).toBe('[data-menu-item]');
    const item = (name: string, attrs: Record<string, string> = {}) => ({ name, getAttribute: (a: string) => attrs[a] ?? null });
    const broken = item('broken', { 'aria-disabled': 'true', 'aria-checked': 'true' });
    const ok = item('ok', { 'aria-checked': 'false' });
    const add = item('add');
    expect(initialFocus([broken, ok, add], 'first')?.name).toBe('ok');
    expect(initialFocus([ok, add, broken], 'last')?.name).toBe('add');
    expect(initialFocus([broken, ok, add], 'checked')?.name).toBe('ok');
    expect(initialFocus([broken], 'first')).toBeUndefined();
  });
});

// -------------------------------------------------------------------- shell

describe('page changes', () => {
  it('names the page in the title', () => {
    expect(documentTitle('send')).toBe('Send · MMX Wallet');
    expect(documentTitle('overview')).toBe('Overview · MMX Wallet');
  });

  interface Focusable {
    isConnected: boolean;
    attributes: Map<string, string>;
    hasAttribute(name: string): boolean;
    setAttribute(name: string, value: string): void;
    focus(): void;
    querySelector(selector: string): Focusable | null;
  }

  const setup = (withHeading: boolean) => {
    const doc: { activeElement: Focusable | null; body: Focusable | null } = { activeElement: null, body: null };
    const el = (heading: Focusable | null = null): Focusable => {
      const self: Focusable = {
        isConnected: true,
        attributes: new Map(),
        hasAttribute: (name) => self.attributes.has(name),
        setAttribute: (name, value) => void self.attributes.set(name, value),
        focus: () => {
          doc.activeElement = self;
        },
        querySelector: (selector) => (selector === 'h1' ? heading : null),
      };
      return self;
    };
    const heading = withHeading ? el() : null;
    const main = el(heading);
    doc.body = el();
    doc.activeElement = doc.body;
    const run = () => focusNewPage(main as unknown as HTMLElement, doc as unknown as Document);
    return { doc, heading, main, run, el };
  };

  it('moves focus to the new page heading when the focused control went away', () => {
    const { doc, heading, run } = setup(true);
    expect(run()).toBe(heading);
    expect(doc.activeElement).toBe(heading);
    expect(heading?.attributes.get('tabindex')).toBe('-1');
  });

  it('falls back to the main area without a heading', () => {
    const { doc, main, run } = setup(false);
    expect(run()).toBe(main);
    expect(doc.activeElement).toBe(main);
  });

  it('treats focus on a removed element as lost', () => {
    const { doc, heading, run, el } = setup(true);
    const gone = el();
    gone.isConnected = false;
    doc.activeElement = gone;
    expect(run()).toBe(heading);
  });

  it('leaves focus on a control that is still there', () => {
    const { doc, run, el } = setup(true);
    const navButton = el();
    doc.activeElement = navButton;
    expect(run()).toBeNull();
    expect(doc.activeElement).toBe(navButton);
  });
});
