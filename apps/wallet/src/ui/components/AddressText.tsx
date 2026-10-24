import { CopyButton } from './CopyButton';
import { cx } from './internal/cx';

export interface AddressTextProps {
  address: string;
  /** full: complete address (chunked in groups of 4); short: start…end. */
  mode?: 'full' | 'short';
  /** Group the full address in blocks of 4 characters (default true). */
  chunked?: boolean;
  /** Shows a copy button that calls this. */
  onCopy?: () => void | Promise<unknown>;
  copyLabel?: string;
  class?: string;
}

/** "mmx1qy9z…k7xg4" — keeps the prefix plus enough characters at both ends to recognise an address. */
export function shortenAddress(address: string, head = 10, tail = 6): string {
  return address.length <= head + tail + 1 ? address : `${address.slice(0, head)}…${address.slice(-tail)}`;
}

export function chunkAddress(address: string, size = 4): string[] {
  const out: string[] = [];
  for (let i = 0; i < address.length; i += size) out.push(address.slice(i, i + size));
  return out;
}

export function AddressText({ address, mode = 'full', chunked = true, onCopy, copyLabel = 'Copy address', class: className }: AddressTextProps) {
  const text =
    mode === 'short' ? (
      <span class="address__text" title={address}>
        {shortenAddress(address)}
      </span>
    ) : chunked ? (
      <span class="address__text address__chunks" translate={false}>
        {chunkAddress(address).map((chunk, i) => (
          <span key={i} class="address__chunk">
            {chunk}
          </span>
        ))}
      </span>
    ) : (
      <span class="address__text address__plain">{address}</span>
    );
  return (
    <span class={cx('address', `address--${mode}`, className)} translate={false}>
      {text}
      {onCopy ? <CopyButton onCopy={onCopy} label={copyLabel} size="sm" class="address__copy" /> : null}
    </span>
  );
}
