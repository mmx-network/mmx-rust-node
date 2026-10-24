import { useEffect, useRef, useState } from 'preact/hooks';
import { normalizeAddress } from '../../../lib/address';
import { formatAmount, parseAtomic } from '../../../lib/amount';
import { shortenHash } from '../../../lib/text';
import { copyToClipboard, describeAddress, finishSend, navigate, openTransaction, preparedSymbol, sendFlow } from '../../../state';
import { Button, Card, CopyButton, EmptyState, KeyValue, KeyValueList, Page, shortenAddress } from '../../components';
import { ContactDialog } from '../contacts/ContactDialog';

async function copyTxid(txid: string): Promise<void> {
  if (!(await copyToClipboard(txid, { message: 'Transaction ID copied' }))) throw new Error('copy failed');
}

/** Result of a broadcast: sent, or unknown when the connection failed while sending. */
export function SendResult({ uncertain }: { uncertain: boolean }) {
  const flow = sendFlow.value;
  const prepared = flow.prepared;
  const [saveOpen, setSaveOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    window.scrollTo({ top: 0 });
    rootRef.current?.focus({ preventScroll: true });
  }, []);

  if (!prepared) return null;
  const to = normalizeAddress(prepared.to) ?? prepared.to;
  const who = describeAddress(to);
  const symbol = preparedSymbol(prepared);
  const amount = parseAtomic(prepared.amount) ?? 0n;
  const summary = `${formatAmount(amount, prepared.decimals)} ${symbol} to ${who.name ?? shortenAddress(to)}`;

  const finish = (route: 'overview' | 'activity'): void => {
    finishSend();
    navigate(route);
  };

  return (
    <Page width="narrow" class="send">
      <div ref={rootRef} tabIndex={-1} class="send-result" role="region" aria-label={uncertain ? 'Transfer status unknown' : 'Transfer sent'}>
        <Card padding="lg" class="send-result__card">
          {uncertain ? (
            <EmptyState
              tone="warning"
              icon="warning"
              title="Transfer status unknown"
              message="We couldn't confirm the transfer. Check Activity before sending again."
            />
          ) : (
            <EmptyState tone="positive" icon="check-circle" title="Transfer sent" message={summary} />
          )}

          <KeyValueList class="send-result__details">
            <KeyValue label="Transaction">
              <span class="send-result__txid">
                <span title={prepared.txid}>{shortenHash(prepared.txid, 8, 8)}</span>
                <CopyButton size="sm" label="Copy transaction ID" onCopy={() => copyTxid(prepared.txid)} />
              </span>
            </KeyValue>
          </KeyValueList>

          <div class="send-result__links">
            <Button variant="ghost" icon="external" onClick={() => void openTransaction(prepared.txid)}>
              View in explorer
            </Button>
            {!uncertain && who.source === 'address' ? (
              <Button variant="ghost" icon="user-plus" onClick={() => setSaveOpen(true)}>
                Save to contacts
              </Button>
            ) : null}
          </div>

          <div class="send-result__actions">
            {uncertain ? (
              <>
                <Button variant="secondary" size="lg" onClick={() => finish('overview')}>
                  Close
                </Button>
                <Button variant="primary" size="lg" onClick={() => finish('activity')}>
                  Open Activity
                </Button>
              </>
            ) : (
              <Button variant="primary" size="lg" onClick={() => finish('overview')}>
                Done
              </Button>
            )}
          </div>
        </Card>
      </div>
      <ContactDialog open={saveOpen} address={to} onClose={() => setSaveOpen(false)} />
    </Page>
  );
}
