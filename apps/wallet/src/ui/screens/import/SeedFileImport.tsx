import { useState } from 'preact/hooks';
import { errorMessage } from '../../../lib/errors';
import { DEFAULT_ADDRESS_COUNT, MAX_ADDRESSES, WALLET_NAME_MAX } from '../../../lib/appstate';
import { validateName } from '../../../lib/text';
import { importSeedFile, notify, suggestWalletName } from '../../../state';
import { Banner, Button, Field, TextInput } from '../../components';
import { FocusLayout } from '../../shell';
import { FlowHeader, useMounted } from '../welcome/flow';
import '../create/create.css';

export function SeedFileImport({ onBack }: { onBack: () => void }) {
  const [name, setName] = useState(suggestWalletName());
  const [count, setCount] = useState(String(DEFAULT_ADDRESS_COUNT));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const mounted = useMounted();
  const submit = async (event: Event): Promise<void> => {
    event.preventDefault();
    if (busy) return;
    const addressCount = Number(count);
    if (validateName(name, WALLET_NAME_MAX)) {
      setError('Enter a wallet name of 1 to 64 characters.');
      return;
    }
    if (!Number.isInteger(addressCount) || addressCount < 1 || addressCount > MAX_ADDRESSES) {
      setError(`Choose between 1 and ${MAX_ADDRESSES} addresses.`);
      return;
    }
    setBusy(true);
    setError(undefined);
    try {
      if (await importSeedFile(name, addressCount)) notify('Wallet imported', 'success');
    } catch (e) {
      if (mounted.current) setError(errorMessage(e));
    } finally {
      if (mounted.current) setBusy(false);
    }
  };
  return (
    <FocusLayout onBack={busy ? undefined : onBack}>
      <FlowHeader title="Import from seed file" subtitle="Choose an existing wallet.dat or JSON key file." />
      <form class="onboarding-form create-details" onSubmit={(event) => void submit(event)}>
        <Field label="Wallet name">
          <TextInput value={name} onValue={setName} disabled={busy} maxLength={WALLET_NAME_MAX} />
        </Field>
        <Field label="Number of addresses" hint="Use more addresses if your previous wallet used them.">
          <TextInput value={count} onValue={setCount} type="number" min={1} max={MAX_ADDRESSES} disabled={busy} />
        </Field>
        <p class="text-muted">A copy will be saved in your wallet folder. The original file and its passphrase stay unchanged.</p>
        {error ? <Banner tone="danger">{error}</Banner> : null}
        <Button type="submit" variant="primary" size="lg" block loading={busy}>Choose seed file</Button>
      </form>
    </FocusLayout>
  );
}
