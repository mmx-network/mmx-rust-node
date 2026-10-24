import { useState } from 'preact/hooks';
import type { ThemeSetting } from '../../../backend/types';
import { AUTO_LOCK_CHOICES } from '../../../lib/appstate';
import { errorMessage } from '../../../lib/errors';
import {
  activeWallet,
  canLock,
  lock,
  notify,
  platform,
  setAutoLockMinutes,
  setTheme,
  settings,
  usableWallets,
} from '../../../state';
import { Button, OptionRow, Page, PageHeader, Section, SegmentedControl, Select } from '../../components';
import { NetworkSettings } from './NetworkSettings';
import { autoLockLabel } from './text';
import { WalletSettings } from './WalletSettings';
import './settings.css';

const THEMES: Array<{ value: ThemeSetting; label: string; icon: 'monitor' | 'sun' | 'moon' }> = [
  { value: 'system', label: 'System', icon: 'monitor' },
  { value: 'light', label: 'Light', icon: 'sun' },
  { value: 'dark', label: 'Dark', icon: 'moon' },
];

function SecuritySettings() {
  const web = platform.value === 'web';
  const minutes = settings.value.autoLockMinutes;
  const [locking, setLocking] = useState(false);
  // Desktop wallets without a passphrase are always open; auto-lock only affects passphrase wallets.
  const autoLockApplies = web || usableWallets.value.some((w) => w.withPassphrase);
  const lockable = canLock.value;
  if (!autoLockApplies && !lockable) return null;

  const choices = AUTO_LOCK_CHOICES[platform.value].slice();
  if (!choices.includes(minutes)) choices.unshift(minutes);

  const lockNow = async (): Promise<void> => {
    if (locking) return;
    setLocking(true);
    try {
      await lock();
    } catch (e) {
      notify(errorMessage(e), 'error');
    } finally {
      setLocking(false);
    }
  };

  return (
    <Section title="Security" card>
      {autoLockApplies ? (
        <OptionRow
          title="Lock automatically"
          description={web ? undefined : 'For wallets with a passphrase'}
        >
          <Select
            size="sm"
            aria-label="Lock automatically after"
            value={String(minutes)}
            onValue={(v) => void setAutoLockMinutes(Number(v)).catch((e: unknown) => notify(errorMessage(e), 'error'))}
            options={choices.map((m) => ({ value: String(m), label: autoLockLabel(m) }))}
          />
        </OptionRow>
      ) : null}
      {lockable ? (
        <OptionRow title="Lock now" description={web ? 'Closes all wallets on this device' : 'Asks for the passphrase again'}>
          <Button size="sm" icon="lock" loading={locking} onClick={() => void lockNow()}>
            Lock
          </Button>
        </OptionRow>
      ) : null}
    </Section>
  );
}

export default function SettingsScreen() {
  const wallet = activeWallet.value;
  const theme = settings.value.theme;

  return (
    <Page class="settings">
      <PageHeader title="Settings" />
      {wallet ? <WalletSettings key={wallet.id} wallet={wallet} /> : null}
      <SecuritySettings />
      <NetworkSettings />
      <Section title="Appearance" card>
        <OptionRow title="Theme" class="settings-theme">
          <SegmentedControl
            size="sm"
            label="Theme"
            value={theme}
            onChange={(t) => void setTheme(t).catch((e: unknown) => notify(errorMessage(e), 'error'))}
            options={THEMES}
          />
        </OptionRow>
      </Section>
      <Section title="About" card>
        <OptionRow title="MMX Wallet" description={`Version ${__APP_VERSION__}`} />
      </Section>
    </Page>
  );
}
