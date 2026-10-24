import type { WalletInfo } from '../../backend/types';
import { Icon } from '../components/Icon';
import { Identicon } from '../components/Identicon';
import { Menu, type MenuEntry } from '../components/Menu';
import { cx } from '../components/internal/cx';
import { safeLabel } from '../components/internal/text';
import type { Placement } from '../components/internal/floating';

export interface WalletSwitcherProps {
  wallets: WalletInfo[];
  activeWalletId?: string;
  onSelectWallet: (id: string) => void;
  onAddWallet: () => void;
  /** Avatar and chevron only. */
  compact?: boolean;
  placement?: Placement;
  class?: string;
}

const avatarSeed = (w: WalletInfo): string => w.fingerprint || w.id;

/**
 * Menu entries for the wallet list. A wallet that can't be opened (for example
 * an unreadable key file) is shown with the reason but can't be chosen.
 */
export function walletMenuItems(
  wallets: readonly WalletInfo[],
  activeWalletId: string | undefined,
  onSelectWallet: (id: string) => void,
  onAddWallet: () => void,
): MenuEntry[] {
  return [
    ...wallets.map(
      (w): MenuEntry => ({
        id: w.id,
        label: safeLabel(w.name, 64) || 'Wallet',
        textValue: safeLabel(w.name, 64),
        description: w.error ? 'Can’t be opened' : w.fingerprint ? `Wallet ID ${w.fingerprint}` : undefined,
        leading: <Identicon value={avatarSeed(w)} size={28} />,
        trailing: w.error ? (
          <Icon name="warning" size="sm" label="Unavailable" class="switcher__state switcher__state--error" />
        ) : w.locked ? (
          <Icon name="lock" size="sm" label="Locked" class="switcher__state" />
        ) : undefined,
        checked: w.id === activeWalletId,
        disabled: Boolean(w.error),
        onSelect: () => onSelectWallet(w.id),
      }),
    ),
    ...(wallets.length > 0 ? [{ separator: true } as const] : []),
    { id: '__add', label: 'Add wallet', icon: 'plus', onSelect: onAddWallet },
  ];
}

export function WalletSwitcher({
  wallets,
  activeWalletId,
  onSelectWallet,
  onAddWallet,
  compact = false,
  placement = 'bottom-start',
  class: className,
}: WalletSwitcherProps) {
  const active = wallets.find((w) => w.id === activeWalletId);
  const items = walletMenuItems(wallets, activeWalletId, onSelectWallet, onAddWallet);
  const name = active ? safeLabel(active.name, 64) || 'Wallet' : 'Choose wallet';
  return (
    <Menu
      label="Wallets"
      items={items}
      placement={placement}
      matchWidth={!compact}
      class={cx('switcher-root', className)}
      trigger={(props, open) => (
        <button
          {...props}
          type="button"
          class={cx('switcher', compact && 'switcher--compact', open && 'switcher--open')}
          aria-label={compact ? `Wallet: ${name}` : undefined}
          title={compact ? name : undefined}
        >
          {active ? (
            <Identicon value={avatarSeed(active)} size={compact ? 30 : 34} class="switcher__avatar" />
          ) : (
            <span class="switcher__avatar switcher__avatar--empty" aria-hidden="true">
              <Icon name="wallet" size="sm" />
            </span>
          )}
          {compact ? null : (
            <span class="switcher__text">
              <span class="switcher__name">{name}</span>
              {active ? (
                <span class="switcher__meta">
                  {active.locked ? (
                    <>
                      <Icon name="lock" size="sm" /> Locked
                    </>
                  ) : active.fingerprint ? (
                    `Wallet ID ${active.fingerprint}`
                  ) : null}
                </span>
              ) : null}
            </span>
          )}
          <Icon name="chevron-down" size="sm" class="switcher__chevron" />
        </button>
      )}
    />
  );
}
