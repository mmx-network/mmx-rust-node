/** Receive screen: the address shown with its QR code (kept while navigating). */
import { computed, signal } from '@preact/signals';
import { activeWalletId } from '../core';
import { addresses } from '../session';

const selection = signal<{ walletId: string | undefined; index: number }>({ walletId: undefined, index: 0 });

/** Index of the selected receive address of the open wallet (the first one by default). */
export const receiveIndex = computed(() => {
  const s = selection.value;
  const count = addresses.value.length;
  if (s.walletId !== activeWalletId.value || s.index < 0 || s.index >= count) return 0;
  return s.index;
});

export function selectReceiveAddress(index: number): void {
  if (!Number.isInteger(index) || index < 0) return;
  selection.value = { walletId: activeWalletId.value, index };
}
