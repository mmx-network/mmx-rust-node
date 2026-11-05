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
  setAllowWebsiteConnections,
  setWebsiteBlocked,
  setTheme,
  settings,
  usableWallets,
} from '../../../state';
import { Button, Field, TextInput, OptionRow, Page, PageHeader, Section, SegmentedControl, Select, Stack, Switch } from '../../components';
import { websiteOrigin } from '../../../lib/websites';
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
      // An explicit lock also locks the wallet in other tabs and windows (web).
      await lock({ everywhere: true });
    } catch (e) {
      notify(errorMessage(e), 'error');
    } finally {
      setLocking(false);
    }
  };

  return (
    <Section title="Security" card>
      {autoLockApplies ? (
        <OptionRow title="Lock automatically" description={web ? undefined : 'For wallets with a passphrase'}>
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
        <OptionRow title="Lock now" description={web ? 'Requires an unlock before spending again' : 'Asks for the passphrase again'}>
          <Button size="sm" icon="lock" loading={locking} onClick={() => void lockNow()}>
            Lock
          </Button>
        </OptionRow>
      ) : null}
    </Section>
  );
}

function WebsiteSettings() {
  const [saving, setSaving] = useState(false);
  const [input, setInput] = useState('');
  if (platform.value !== 'web') return null;
  const sites = settings.value.blockedWebsites ?? [];
  const save = async (action: () => Promise<void>) => {
    if (saving) return;
    setSaving(true);
    try {
      await action();
    } catch (e) {
      notify(errorMessage(e), 'error');
    } finally {
      setSaving(false);
    }
  };
  const block = async () => {
    let site: string | null = null;
    try {
      site = websiteOrigin(new URL(input.trim()).origin);
    } catch {
      /* show validation below */
    }
    if (!site) {
      notify('Enter a website URL starting with https://.', 'error');
      return;
    }
    await setWebsiteBlocked(site, true);
    setInput('');
  };
  return (
    <Section title="Website connections" card>
      <OptionRow
        title="Allow website connections"
        description="Share public addresses and review transaction requests from websites. Every transaction requires approval."
      >
        <Switch
          aria-label="Allow website connections"
          checked={settings.value.allowWebsiteConnections === true}
          disabled={saving}
          onChange={(v) => void save(() => setAllowWebsiteConnections(v))}
        />
      </OptionRow>
      <OptionRow title="Block a website" description="Blocked websites cannot connect or request transactions." stacked>
        <Stack gap={3}>
          <Field label="Website URL">
            <TextInput value={input} placeholder="https://example.com" onValue={setInput} disabled={saving} />
          </Field>
          <Button size="sm" disabled={saving || !input.trim()} onClick={() => void save(block)}>
            Block website
          </Button>
        </Stack>
      </OptionRow>
      {sites.map((site) => (
        <OptionRow key={site} title={<span class="settings__website">{site}</span>} description="Blocked">
          <Button
            size="sm"
            disabled={saving}
            aria-label={`Unblock ${site}`}
            onClick={() => void save(() => setWebsiteBlocked(site, false))}
          >
            Unblock
          </Button>
        </OptionRow>
      ))}
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
      <WebsiteSettings />
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
