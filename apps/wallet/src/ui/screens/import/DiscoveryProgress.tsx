/** Progress of the used-address scan after a wallet was added to this device. */
import { discovery } from '../../../state';
import { FocusLayout } from '../../shell';
import { FlowHeader } from '../welcome/flow';
import './import.css';

export function DiscoveryProgress() {
  const progress = discovery.value;
  const total = progress?.total ?? 0;
  const checked = progress ? Math.min(progress.checked, total) : 0;
  const percent = total > 0 ? Math.round((checked / total) * 100) : 0;
  return (
    <FocusLayout class="import">
      <div class="import-progress" aria-busy="true">
        <FlowHeader
          autoFocus
          align="center"
          title="Finding your addresses"
          subtitle={total > 0 ? `Checked ${checked} of ${total}` : 'Adding your wallet'}
        />
        <div
          class={total > 0 ? 'import-progress__bar' : 'import-progress__bar import-progress__bar--waiting'}
          role="progressbar"
          aria-label="Finding your addresses"
          aria-valuemin={0}
          aria-valuemax={total || undefined}
          aria-valuenow={total > 0 ? checked : undefined}
        >
          <span class="import-progress__fill" style={{ width: total > 0 ? `${percent}%` : undefined }} />
        </div>
      </div>
    </FocusLayout>
  );
}
