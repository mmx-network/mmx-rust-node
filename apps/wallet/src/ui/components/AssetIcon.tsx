import { BrandMark } from './Brand';
import { Identicon } from './Identicon';
import { cx } from './internal/cx';
import { safeLabel } from './internal/text';

export interface AssetIconProps {
  /** Contract address of the token (identicon seed). */
  contract: string;
  symbol?: string;
  /** The native MMX currency (decide by the zero contract address, never by symbol). */
  native?: boolean;
  size?: number;
  class?: string;
}

/** Round asset avatar: the MMX mark for the native currency, an identicon with the initial otherwise. */
export function AssetIcon({ contract, symbol, native = false, size = 32, class: className }: AssetIconProps) {
  if (native) {
    return (
      <span class={cx('asset-icon', 'asset-icon--native', className)} style={{ '--asset-size': `${size}px` }}>
        <BrandMark size={size} />
      </span>
    );
  }
  const initial = symbol ? Array.from(safeLabel(symbol, 8))[0]?.toUpperCase() ?? '' : '';
  return (
    <span class={cx('asset-icon', className)} style={{ '--asset-size': `${size}px` }} aria-hidden="true">
      <Identicon value={contract} size={size} />
      {initial ? <span class="asset-icon__initial">{initial}</span> : null}
    </span>
  );
}
