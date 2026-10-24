import { useEffect, useState } from 'preact/hooks';
import { toBackendError, type WalletInfo } from '../../../backend/types';
import { errorMessage } from '../../../lib/errors';
import {
  activateWallet,
  activeWallet,
  discovery,
  navigate,
  notify,
  opening,
  platform,
  removeWallet,
  selectWallet,
  unlockWallet,
  usableWallets,
  wallets,
} from '../../../state';
import { Button, ConfirmDialog, Icon, Identicon, List, ListItem, Menu, type MenuEntry } from '../../components';
import { FocusLayout } from '../../shell';
import { DiscoveryProgress } from '../import/DiscoveryProgress';
import { FlowHeader, useMounted } from '../welcome/flow';
import { CredentialForm, DesktopPassphraseForm, PassphraseForm } from './forms';
import { useOpenWallet, type OpenWallet } from './useOpenWallet';
import './unlock.css';

export default function UnlockScreen() {
  return platform.value === 'web' ? <WebUnlock /> : <DesktopUnlock />;
}

function walletSubtitle(w: WalletInfo): string {
  if (w.error) return 'Can’t be opened';
  if (w.verified === false) return 'Setup not finished';
  return `Wallet ID ${w.fingerprint}`;
}

function onSelectError(e: unknown): void {
  notify(errorMessage(e), 'error');
}

// ---------------------------------------------------------------- desktop

function DesktopUnlock() {
  const active = activeWallet.value;
  const target = active && !active.error && active.locked ? active : undefined;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | undefined>(undefined);
  const [general, setGeneral] = useState<string | undefined>(undefined);
  const mounted = useMounted();

  useEffect(() => {
    setError(undefined);
    setGeneral(undefined);
  }, [target?.id]);

  // No locked wallet to unlock (e.g. the chosen one can't be read): pick another.
  if (!target) return <DesktopWalletList />;

  const unlock = async (passphrase: string): Promise<void> => {
    if (!passphrase) {
      setError('Enter your passphrase.');
      return;
    }
    setBusy(true);
    setError(undefined);
    setGeneral(undefined);
    try {
      await unlockWallet(target.id, { passphrase });
    } catch (e) {
      if (!mounted.current) return;
      const code = toBackendError(e).code;
      if (code === 'invalid_passphrase' || code === 'passphrase_required') setError(errorMessage(e));
      else setGeneral(errorMessage(e));
    } finally {
      if (mounted.current) setBusy(false);
    }
  };

  return (
    <FocusLayout headerAction={<DesktopSwitchMenu activeId={target.id} />}>
      <FlowHeader
        align="center"
        leading={<Identicon value={target.fingerprint || target.id} size={64} />}
        title={target.name}
        subtitle="Enter your passphrase to unlock."
      />
      <DesktopPassphraseForm key={target.id} busy={busy} error={error} generalError={general} onSubmit={(p) => void unlock(p)} />
    </FocusLayout>
  );
}

function DesktopSwitchMenu({ activeId }: { activeId: string }) {
  const list = wallets.value;
  if (list.length <= 1) {
    return (
      <Button variant="ghost" size="sm" icon="plus" onClick={() => navigate('welcome')}>
        Add wallet
      </Button>
    );
  }
  const items: MenuEntry[] = [
    ...list.map(
      (w): MenuEntry => ({
        id: w.id,
        label: w.name,
        textValue: w.name,
        description: walletSubtitle(w),
        leading: <Identicon value={w.fingerprint || w.id} size={28} />,
        trailing: w.locked && !w.error ? <Icon name="lock" size="sm" label="Locked" /> : undefined,
        checked: w.id === activeId,
        disabled: Boolean(w.error),
        onSelect: () => {
          if (w.id !== activeId) selectWallet(w.id).catch(onSelectError);
        },
      }),
    ),
    { separator: true },
    { id: '__add', label: 'Add wallet', icon: 'plus', onSelect: () => navigate('welcome') },
  ];
  return (
    <Menu
      label="Wallets"
      items={items}
      placement="bottom-end"
      trigger={(props) => (
        <Button {...props} variant="ghost" size="sm" iconEnd="chevron-down">
          Switch wallet
        </Button>
      )}
    />
  );
}

function DesktopWalletList() {
  const list = wallets.value;
  const active = activeWallet.value;
  const busy = opening.value;
  return (
    <FocusLayout>
      <FlowHeader title="Choose a wallet" />
      <List inset label="Wallets" class="unlock-list">
        {list.map((w) => (
          <ListItem
            key={w.id}
            leading={<Identicon value={w.fingerprint || w.id} size={36} />}
            title={w.name}
            subtitle={walletSubtitle(w)}
            trailing={w.locked && !w.error ? <Icon name="lock" size="sm" label="Locked" class="unlock-list__lock" /> : undefined}
            chevron={!w.error}
            selected={w.id === active?.id}
            disabled={Boolean(w.error) || busy}
            onClick={w.error ? undefined : () => activateWallet(w.id).catch(onSelectError)}
          />
        ))}
      </List>
      <Button variant="secondary" icon="plus" block onClick={() => navigate('welcome')}>
        Add wallet
      </Button>
    </FocusLayout>
  );
}

// -------------------------------------------------------------------- web

type WebView = 'wallet' | 'list' | 'saved';

function WebUnlock() {
  const usable = usableWallets.value;
  const active = activeWallet.value;
  const activeId = active && !active.error ? active.id : undefined;
  const [selectedId, setSelectedId] = useState<string | undefined>(activeId);
  const [view, setView] = useState<WebView>(activeId ? 'wallet' : usable.length > 0 ? 'list' : 'saved');

  useEffect(() => {
    if (activeId) {
      setSelectedId(activeId);
      setView('wallet');
    }
  }, [activeId]);

  const selected = usable.find((w) => w.id === selectedId);
  let current = view;
  if (current === 'wallet' && !selected) current = usable.length > 0 ? 'list' : 'saved';
  if (current === 'list' && usable.length === 0) current = 'saved';

  if (current === 'saved') {
    return <OpenSavedView onBack={usable.length > 0 ? () => setView('list') : () => navigate('welcome')} />;
  }
  if (current === 'list') {
    return (
      <WebWalletList
        wallets={usable}
        selectedId={selected?.id}
        onSelect={(id) => {
          setSelectedId(id);
          setView('wallet');
        }}
        onOther={() => setView('saved')}
        onBack={selected ? () => setView('wallet') : undefined}
      />
    );
  }
  return (
    <WebWalletUnlock
      key={selected!.id}
      wallet={selected!}
      onChange={usable.length > 1 ? () => setView('list') : undefined}
      onOther={() => setView('saved')}
    />
  );
}

function WebWalletList({
  wallets: list,
  selectedId,
  onSelect,
  onOther,
  onBack,
}: {
  wallets: WalletInfo[];
  selectedId?: string;
  onSelect: (id: string) => void;
  onOther: () => void;
  onBack?: () => void;
}) {
  return (
    <FocusLayout onBack={onBack}>
      <FlowHeader title="Choose a wallet" autoFocus={onBack !== undefined} />
      <List inset label="Wallets on this device" class="unlock-list">
        {list.map((w) => (
          <ListItem
            key={w.id}
            leading={<Identicon value={w.fingerprint || w.id} size={36} />}
            title={w.name}
            subtitle={walletSubtitle(w)}
            selected={w.id === selectedId}
            chevron
            onClick={() => onSelect(w.id)}
          />
        ))}
      </List>
      <div class="onboarding-actions">
        <Button variant="secondary" icon="key" block onClick={onOther}>
          Open another wallet
        </Button>
        <Button variant="ghost" icon="plus" block onClick={() => navigate('welcome')}>
          Add a new wallet
        </Button>
      </div>
    </FocusLayout>
  );
}

function PassphraseView({ open }: { open: OpenWallet }) {
  if (open.busy && discovery.value) return <DiscoveryProgress />;
  return (
    <FocusLayout onBack={open.back}>
      <FlowHeader title="Enter your passphrase" subtitle={open.pendingName} />
      <PassphraseForm open={open} />
    </FocusLayout>
  );
}

function WebWalletUnlock({ wallet, onChange, onOther }: { wallet: WalletInfo; onChange?: () => void; onOther: () => void }) {
  const unverified = wallet.verified === false;
  const open = useOpenWallet(
    { kind: 'known', wallet },
    {
      autoRequest: 'once',
      onOpened: unverified ? () => notify('Wallet created', 'success') : undefined,
    },
  );
  const [confirmRemove, setConfirmRemove] = useState(false);
  const [removing, setRemoving] = useState(false);
  const mounted = useMounted();

  if (open.busy && discovery.value) return <DiscoveryProgress />;
  if (open.step === 'passphrase') return <PassphraseView open={open} />;

  const remove = async (): Promise<void> => {
    setRemoving(true);
    try {
      await removeWallet(wallet.id);
      if (usableWallets.value.length === 0) navigate('welcome', undefined, { replace: true });
    } catch (e) {
      notify(errorMessage(e), 'error');
    } finally {
      if (mounted.current) {
        setRemoving(false);
        setConfirmRemove(false);
      }
    }
  };

  return (
    <FocusLayout>
      <FlowHeader
        title={unverified ? 'Finish setting up' : 'Unlock your wallet'}
        subtitle={unverified ? 'Open your new wallet once to confirm it was saved.' : undefined}
      />
      <CredentialForm open={open} wallet={wallet} submitLabel={unverified ? 'Open wallet' : 'Unlock'} onChangeWallet={onChange}>
        <div class="onboarding-actions__secondary">
          <Button variant="ghost" onClick={onOther} disabled={open.busy}>
            Open another wallet
          </Button>
          {unverified ? (
            <Button variant="ghost" onClick={() => setConfirmRemove(true)} disabled={open.busy}>
              Forget wallet
            </Button>
          ) : null}
        </div>
      </CredentialForm>
      <ConfirmDialog
        open={confirmRemove}
        title={`Forget ${wallet.name}?`}
        message={
          wallet.withPassphrase
            ? 'You can add it again later with its recovery phrase and passphrase.'
            : 'You can add it again later with its recovery phrase.'
        }
        confirmLabel="Forget"
        tone="danger"
        busy={removing}
        onConfirm={() => void remove()}
        onCancel={() => setConfirmRemove(false)}
      />
    </FocusLayout>
  );
}

/** "Open a saved wallet" / "Open another wallet": any wallet saved in the password manager. */
export function OpenSavedView({ onBack }: { onBack?: () => void }) {
  const open = useOpenWallet({ kind: 'saved' }, { autoRequest: 'always' });
  // A wallet new to this device is added and its used addresses are looked up.
  if (open.busy && discovery.value) return <DiscoveryProgress />;
  if (open.step === 'passphrase') return <PassphraseView open={open} />;
  return (
    <FocusLayout onBack={onBack ? () => !open.busy && onBack() : undefined}>
      <FlowHeader title="Open a saved wallet" />
      <CredentialForm open={open} submitLabel="Open wallet" />
    </FocusLayout>
  );
}
