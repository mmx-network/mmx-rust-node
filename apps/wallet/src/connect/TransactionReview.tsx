import { NATIVE_CONTRACT, normalizeAddress } from '../lib/address';
import { sanitizeText } from '../lib/text';
import { AddressText, Amount, AssetIcon, Badge, Disclosure, Identicon, KeyValue, KeyValueList, Text } from '../ui/components';
import { object, type Account, type PreparedTransaction } from './protocol';

const rows = (v: unknown) => (Array.isArray(v) ? v.filter(object) : []);
const atomic = (v: unknown) => {
  try {
    return BigInt(String(v));
  } catch {
    return 0n;
  }
};
const exactText = (v: string) =>
  v.replace(/[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/gu, (c) => `\\u${c.charCodeAt(0).toString(16).padStart(4, '0')}`);

/** Render VM values as named fields and lists, without discarding type or precision. */
export function DetailValue({ value }: { value: unknown }) {
  if (value === null) return <span class="connection__empty">None</span>;
  if (typeof value === 'boolean') return <span>{value ? 'Yes' : 'No'}</span>;
  if (typeof value === 'number') return <span class="connection__number">{String(value)}</span>;
  if (typeof value === 'string') {
    if (normalizeAddress(value) === value) return <AddressText address={value} />;
    return <span class="connection__value">{value ? `“${exactText(value)}”` : <span class="connection__empty">Empty text</span>}</span>;
  }
  const entries = Array.isArray(value) ? value.map((v, i) => [`Item ${i + 1}`, v] as const) : object(value) ? Object.entries(value) : [];
  if (!entries.length) return <span class="connection__empty">Empty</span>;
  return (
    <dl class="connection__fields">
      {entries.map(([key, v]) => (
        <div key={key}>
          <dt>{exactText(String(key))}</dt>
          <dd>
            <DetailValue value={v} />
          </dd>
        </div>
      ))}
    </dl>
  );
}
function Money({
  value,
  contract,
  prepared,
  decimals,
}: {
  value: unknown;
  contract: string;
  prepared?: PreparedTransaction | null;
  decimals: number;
}) {
  const currency = prepared?.currencies.find((c) => c.contract === contract);
  const known = contract === NATIVE_CONTRACT || currency;
  return known ? (
    <Amount
      value={atomic(value)}
      decimals={currency?.decimals ?? decimals}
      symbol={contract === NATIVE_CONTRACT ? 'MMX' : currency?.symbol}
    />
  ) : (
    <span class="connection__number">
      {atomic(value).toLocaleString()} <span class="connection__empty">atomic units</span>
    </span>
  );
}
function Address({ address, own }: { address: string; own: boolean }) {
  return (
    <div class="connection__recipient">
      <div class="connection__recipient-title">
        <Identicon value={address} size={28} />
        <strong>{own ? 'Your address' : 'Recipient'}</strong>
      </div>
      <AddressText address={address} />
    </div>
  );
}
export function TransactionReview({
  transaction,
  account,
  prepared,
}: {
  transaction: Record<string, unknown>;
  account: Account;
  prepared: PreparedTransaction | null;
}) {
  const tx = prepared?.transaction ?? transaction;
  const outputs = rows(tx.outputs),
    calls = rows(tx.execute);
  const decimals = prepared?.decimals ?? account.decimals ?? 6;
  const own = new Set(account.addresses);
  const single = outputs.length === 1 && !calls.length && !tx.deploy ? outputs[0] : null;
  const nativeNet = prepared
    ? rows(tx.inputs)
        .filter((v) => v.contract === NATIVE_CONTRACT)
        .reduce((n, v) => n + atomic(v.amount), 0n) -
      outputs.filter((v) => v.contract === NATIVE_CONTRACT && own.has(String(v.address))).reduce((n, v) => n + atomic(v.amount), 0n)
    : null;
  const fee = prepared ? atomic(prepared.maxFee) : null;
  const other = Object.fromEntries(
    Object.entries(tx).filter(
      ([k, v]) =>
        ![
          '__type',
          'id',
          'version',
          'nonce',
          'network',
          'static_cost',
          'fee_ratio',
          'max_fee_amount',
          'expires',
          'solutions',
          'content_hash',
          'inputs',
          'outputs',
          'execute',
          'deploy',
          'exec_result',
          'sender',
          'note',
        ].includes(k) &&
        v !== null &&
        (!Array.isArray(v) || v.length),
    ),
  );
  return (
    <div class={`connection__review${single ? ' connection__review--transfer' : ''}`} role="region" aria-label="Transaction details">
      <div class="connection__amount-hero">
        {single ? (
          <>
            <span class="connection__eyebrow">You send</span>
            <div class="connection__large-amount">
              <Money value={single.amount} contract={String(single.contract ?? NATIVE_CONTRACT)} prepared={prepared} decimals={decimals} />
            </div>
          </>
        ) : (
          <>
            <span class="connection__action-icon">
              <AssetIcon contract={NATIVE_CONTRACT} native size={40} />
            </span>
            <span class="connection__eyebrow">You approve</span>
            <strong class="connection__action-title">
              {tx.deploy
                ? 'Deploy a contract'
                : calls.length === 1 && !outputs.length
                  ? 'Contract interaction'
                  : outputs.length + calls.length === 0 ? 'Network transaction' : `${outputs.length + calls.length} transaction actions`}
            </strong>
          </>
        )}
      </div>
      {outputs.map((v, i) => (
        <div class="connection__transfer" key={i}>
          {!single ? (
            <div class="connection__action-heading">
              <span>Transfer {i + 1}</span>
              <Money value={v.amount} contract={String(v.contract)} prepared={prepared} decimals={decimals} />
            </div>
          ) : null}
          <Address address={String(v.address ?? 'No recipient')} own={own.has(String(v.address))} />
          {v.contract !== NATIVE_CONTRACT ? (
            <div class="connection__token">
              <Badge tone="warning">Unverified token</Badge>
              <AddressText address={String(v.contract)} mode="short" />
            </div>
          ) : null}
          {v.memo ? (
            <KeyValueList variant="compact">
              <KeyValue label="Memo">
                <DetailValue value={v.memo} />
              </KeyValue>
            </KeyValueList>
          ) : null}
        </div>
      ))}
      {calls.map((v, i) => (
        <div class="connection__call" key={i}>
          <div class="connection__action-heading">
            <strong>
              {v.__type === 'mmx.operation.Deposit' ? 'Deposit to contract' : 'Contract interaction'}
              {calls.length > 1 ? ` ${i + 1}` : ''}
            </strong>
          </div>
          {v.__type === 'mmx.operation.Deposit' ? (
            <div class="connection__deposit">
              <Money value={v.amount} contract={String(v.currency)} prepared={prepared} decimals={decimals} />
            </div>
          ) : null}
          <KeyValueList variant="compact">
            <KeyValue label="Contract" stacked>
              <AddressText address={String(v.address)} />
            </KeyValue>
            <KeyValue label="Action">
              <span class="connection__method">{exactText(String(v.method ?? ''))}</span>
            </KeyValue>
            {v.user ? (
              <KeyValue label="Acting as">
                <AddressText address={String(v.user)} mode="short" />
              </KeyValue>
            ) : null}
          </KeyValueList>
          {Array.isArray(v.args) && v.args.length ? (
            <Disclosure title="Action details" meta={`${v.args.length} ${v.args.length === 1 ? 'argument' : 'arguments'}`} defaultOpen>
              <dl class="connection__arguments">
                {v.args.map((arg, i) => (
                  <div key={i}>
                    <dt>Argument {i + 1}</dt>
                    <dd>
                      <DetailValue value={arg} />
                    </dd>
                  </div>
                ))}
              </dl>
            </Disclosure>
          ) : null}
        </div>
      ))}
      {object(tx.deploy) ? (
        <div class="connection__call">
          <strong>
            New{' '}
            {String(tx.deploy.__type ?? 'contract')
              .split('.')
              .at(-1)}{' '}
            contract
          </strong>
          <DetailValue value={Object.fromEntries(Object.entries(tx.deploy).filter(([k]) => k !== '__type'))} />
        </div>
      ) : null}
      {calls.length || tx.deploy ? (
        <div class="connection__notice">
          <Text size="sm" tone="muted">
            Contract actions can move funds or change ownership. Check the contract and action details before sending.
          </Text>
        </div>
      ) : null}
      {fee !== null && nativeNet !== null && nativeNet >= 0n ? <div class="connection__cost">
        <KeyValueList variant="compact">
          <KeyValue label="Maximum total" emphasis>
            <Amount value={nativeNet + fee} decimals={decimals} symbol="MMX" size="lg" />
          </KeyValue>
        </KeyValueList>
      </div> : null}
      {prepared ? (
        <Disclosure title="More details">
          <KeyValueList variant="compact">
            <KeyValue label="Network">{sanitizeText(account.network,64)}</KeyValue>
            <KeyValue label="From">
              <AddressText address={account.address} mode="short" />
            </KeyValue>
            <KeyValue label="Static network fee">
              <Amount value={atomic(prepared.fee)} decimals={decimals} symbol="MMX" />
            </KeyValue>
            <KeyValue label="Valid through block">{prepared.expires.toLocaleString()}</KeyValue>
            {prepared.transaction.sender ? (
              <KeyValue label="Fee payer">
                <AddressText address={String(prepared.transaction.sender)} mode="short" />
              </KeyValue>
            ) : null}
          </KeyValueList>
          {rows(tx.inputs).length ? (
            <div class="connection__funding">
              <Text size="sm" tone="muted">
                Funds used
              </Text>
              {rows(tx.inputs).map((v, i) => (
                <div key={i}>
                  <AddressText address={String(v.address)} mode="short" />
                  <Money value={v.amount} contract={String(v.contract)} prepared={prepared} decimals={decimals} />
                </div>
              ))}
            </div>
          ) : null}
          {Object.keys(other).length ? <DetailValue value={other} /> : null}
        </Disclosure>
      ) : null}
    </div>
  );
}
