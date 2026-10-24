import { useEffect, useState } from 'preact/hooks';
import { errorMessage, messageForCode } from '../../../lib/errors';
import { DEFAULT_RPC_URL, network, normalizeRpcUrl, notify, platform, setRpcUrl, settings } from '../../../state';
import { Button, Field, OptionRow, Section, TextInput } from '../../components';
import { NetworkStatus } from '../../shell';
import { isUnencryptedRemote } from './text';

const heightFormat = new Intl.NumberFormat(undefined, { maximumFractionDigits: 0 });

function statusDetail(): string {
  const net = network.value;
  switch (net.state) {
    case 'online':
      return net.height !== undefined ? `Block ${heightFormat.format(net.height)}` : 'Connected';
    case 'syncing':
      return 'The network server is catching up';
    case 'connecting':
      return 'Connecting…';
    case 'offline':
      return net.error ? messageForCode(net.error) : 'Cannot reach the network.';
  }
}

/** Desktop: which server the wallet uses for chain data. */
function ServerSetting() {
  const saved = settings.value.rpcUrl ?? DEFAULT_RPC_URL;
  const [text, setText] = useState(saved);
  const [error, setError] = useState<string | undefined>();
  const [busy, setBusy] = useState<'connect' | 'reset' | false>(false);

  useEffect(() => setText(saved), [saved]);

  const normalized = normalizeRpcUrl(text);
  const changed = (normalized ?? text.trim()) !== saved;
  const insecure = normalized !== null && isUnencryptedRemote(normalized);

  const connect = async (event: Event): Promise<void> => {
    event.preventDefault();
    if (busy) return;
    const url = normalizeRpcUrl(text);
    if (!url) {
      setError(`Enter a server address, like ${DEFAULT_RPC_URL}`);
      return;
    }
    if (url === saved) return;
    setBusy('connect');
    setError(undefined);
    try {
      await setRpcUrl(url);
      notify('Connected to the server.', 'success');
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const reset = async (): Promise<void> => {
    if (busy) return;
    setBusy('reset');
    setError(undefined);
    try {
      await setRpcUrl(null);
      setText(DEFAULT_RPC_URL);
      notify('Using the default server.', 'success');
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <OptionRow title="Server" description="Where the wallet gets balances and sends transfers" stacked>
      <form class="settings-server" onSubmit={connect} noValidate>
        <Field
          label="Server address"
          hideLabel
          error={error}
          hint={insecure ? 'This connection is not encrypted.' : undefined}
        >
          <TextInput
            value={text}
            onValue={(v) => {
              setText(v);
              setError(undefined);
            }}
            mono
            sensitive
            type="url"
            inputMode="url"
            placeholder={DEFAULT_RPC_URL}
            readOnly={busy !== false}
            enterkeyhint="go"
          />
        </Field>
        <div class="settings-server__actions">
          {saved !== DEFAULT_RPC_URL ? (
            <Button variant="ghost" size="sm" loading={busy === 'reset'} disabled={busy === 'connect'} onClick={() => void reset()}>
              Use default
            </Button>
          ) : null}
          <Button type="submit" variant="primary" size="sm" loading={busy === 'connect'} disabled={!changed || busy === 'reset'}>
            Connect
          </Button>
        </div>
      </form>
    </OptionRow>
  );
}

export function NetworkSettings() {
  const net = network.value;
  return (
    <Section title="Network" card>
      <OptionRow title="Status" description={statusDetail()}>
        <NetworkStatus state={net.state} height={net.height} network={net.network} />
      </OptionRow>
      {platform.value === 'desktop' ? <ServerSetting /> : null}
    </Section>
  );
}
