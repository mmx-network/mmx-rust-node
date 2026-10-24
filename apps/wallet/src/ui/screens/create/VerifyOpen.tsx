/**
 * Web: completes a new wallet by opening it once through the normal unlock
 * form, filled from the password manager. This proves the credential was saved.
 */
import type { WalletInfo } from '../../../backend/types';
import { notify } from '../../../state';
import { Button } from '../../components';
import { CredentialForm, PassphraseForm } from '../unlock/forms';
import { useOpenWallet } from '../unlock/useOpenWallet';

export interface VerifyOpenProps {
  wallet: WalletInfo;
  /** The passphrase chosen a moment ago ('' = none). */
  passphrase: string;
  onShowAgain: () => void;
}

export function VerifyOpen({ wallet, passphrase, onShowAgain }: VerifyOpenProps) {
  const open = useOpenWallet(
    { kind: 'known', wallet },
    {
      knownPassphrase: passphrase || undefined,
      // No automatic prompt: it would replace the browser's "save password" prompt shown a moment ago.
      allowOtherWallet: false,
      onOpened: () => notify('Wallet created', 'success'),
    },
  );
  if (open.step === 'passphrase') return <PassphraseForm open={open} />;
  return (
    <CredentialForm open={open} wallet={wallet} submitLabel="Open wallet">
      <div class="onboarding-actions__secondary">
        <Button variant="ghost" onClick={onShowAgain} disabled={open.busy}>
          Show recovery phrase again
        </Button>
      </div>
    </CredentialForm>
  );
}
