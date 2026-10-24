// Development-only component gallery (kitchen.html). Not part of the app build.
import type { ComponentChildren } from 'preact';
import { useState } from 'preact/hooks';
import type { WalletInfo } from '../../backend/types';
import {
  AddressText,
  Amount,
  AssetIcon,
  Badge,
  Banner,
  BrandLockup,
  BrandMark,
  Button,
  Card,
  Combobox,
  ConfirmDialog,
  CopyButton,
  Dialog,
  Disclosure,
  EmptyState,
  Field,
  Grid,
  Heading,
  Icon,
  IconButton,
  IconTile,
  Identicon,
  KeyValue,
  KeyValueList,
  List,
  ListGroup,
  ListItem,
  Menu,
  NumberStepper,
  OptionRow,
  Page,
  PageHeader,
  PasswordInput,
  ProgressSteps,
  QRCode,
  SearchInput,
  Section,
  SegmentedControl,
  Select,
  Sheet,
  Skeleton,
  Spinner,
  Stack,
  Switch,
  Text,
  TextInput,
  ToastViewport,
  Tooltip,
  WordGrid,
  type Toast,
} from '../components';
import { iconNames } from '../icons';
import { FocusLayout, Shell, type RouteId } from '../shell';

const BECH = 'qpzry9x8gf2tvdw0s3jn54khce6mua7l';
function fakeAddress(seed: number): string {
  let s = seed * 9301 + 49297;
  let out = 'mmx1';
  for (let i = 0; i < 58; i++) {
    s = (s * 9301 + 49297) % 233280;
    out += BECH[s % 32];
  }
  return out;
}

const ADDR = fakeAddress(7);
const ADDR2 = fakeAddress(19);
const NATIVE = 'mmx1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqdgytev';
const WORDS = 'tackle pioneer orbit velvet amused saddle crystal hover giant wisdom pepper atom lunar exhibit royal fabric cradle tunnel olive spirit gadget vessel mirror cherry'.split(' ');

const WALLETS: WalletInfo[] = [
  { id: 'wallet_1.dat', name: 'Main wallet', fingerprint: '2874019362', withPassphrase: false, locked: false, addressCount: 10 },
  { id: 'wallet_2.dat', name: 'Savings', fingerprint: '913552087', withPassphrase: true, locked: true, addressCount: 3 },
  { id: 'wallet_3.dat', name: 'Farm rewards', fingerprint: '4021187745', withPassphrase: false, locked: false, addressCount: 20 },
];

function Swatch({ name }: { name: string }) {
  return (
    <div class="kitchen-swatch">
      <span class="kitchen-swatch__chip" style={{ background: `var(${name})` }} />
      <Text as="span" size="xs" mono>
        {name}
      </Text>
    </div>
  );
}

function Demo({ title, children }: { title: string; children: ComponentChildren }) {
  return (
    <Section title={title} level={3}>
      {children}
    </Section>
  );
}

function Components() {
  const [text, setText] = useState('');
  const [amount, setAmount] = useState('12.5');
  const [memo, setMemo] = useState('Invoice 2041');
  const [pass, setPass] = useState('correct horse');
  const [asset, setAsset] = useState('mmx');
  const [seg, setSeg] = useState<'all' | 'in' | 'out' | 'rewards'>('all');
  const [on, setOn] = useState(true);
  const [count, setCount] = useState(10);
  const [query, setQuery] = useState('');
  const [dialog, setDialog] = useState(false);
  const [sheet, setSheet] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const [hidden, setHidden] = useState(true);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [word, setWord] = useState('cra');
  const [to, setTo] = useState('');
  const [autoLock, setAutoLock] = useState('15');

  const toast = (t: Omit<Toast, 'id'>): void => setToasts((list) => [...list, { ...t, id: String(Math.random()) }]);

  return (
    <Stack gap={10}>
      <Demo title="Brand">
        <Stack direction="row" gap={6} wrap>
          <BrandMark size={56} label="MMX" />
          <BrandMark size={40} variant="glyph" />
          <BrandLockup size="lg" />
          <BrandLockup />
        </Stack>
      </Demo>

      <Demo title="Tokens">
        <div class="kitchen-swatches">
          {['--bg', '--bg-elevated', '--bg-sunken', '--surface', '--surface-2', '--border', '--border-strong', '--text', '--text-muted', '--text-faint', '--accent', '--accent-soft', '--positive', '--negative', '--warning', '--focus'].map((n) => (
            <Swatch key={n} name={n} />
          ))}
        </div>
        <Stack gap={1}>
          <Heading level={1}>Heading XL</Heading>
          <Heading level={2}>Heading L</Heading>
          <Heading level={3}>Heading M</Heading>
          <Text>Body text for instructions and descriptions.</Text>
          <Text tone="muted" size="sm">
            Muted small text
          </Text>
          <Text tone="faint" size="xs">
            Faint caption
          </Text>
        </Stack>
      </Demo>

      <Demo title="Buttons">
        <Stack direction="row" gap={2} wrap>
          <Button variant="primary" icon="send">
            Send
          </Button>
          <Button icon="receive">Receive</Button>
          <Button variant="ghost">Cancel</Button>
          <Button variant="danger" icon="trash">
            Remove
          </Button>
          <Button variant="primary" loading>
            Sending
          </Button>
          <Button disabled>Disabled</Button>
        </Stack>
        <Stack direction="row" gap={2} wrap align="center">
          <Button size="sm" icon="plus">
            Add
          </Button>
          <Button size="lg" variant="primary" iconEnd="arrow-right">
            Continue
          </Button>
          <IconButton icon="copy" label="Copy" />
          <IconButton icon="edit" label="Edit" variant="secondary" />
          <IconButton icon="trash" label="Delete" variant="danger" />
          <IconButton icon="refresh" label="Refresh" loading />
          <CopyButton onCopy={() => undefined} />
          <CopyButton variant="button" onCopy={() => undefined} label="Copy address" size="sm" />
        </Stack>
      </Demo>

      <Demo title="Inputs">
        <Grid columns={2} gap={5}>
          <Field label="Recipient" hint="An MMX address or a contact name">
            <TextInput value={text} onValue={setText} placeholder="mmx1…" mono leading={<Icon name="search" />} />
          </Field>
          <Field label="Amount" error={amount.includes(',') ? 'Use a dot as the decimal separator' : undefined} labelAction={<Text as="span" size="sm" tone="muted">Available 1,204.5 MMX</Text>}>
            <TextInput
              value={amount}
              onValue={setAmount}
              inputMode="decimal"
              trailing={
                <>
                  <Text as="span" size="sm" tone="muted">
                    MMX
                  </Text>
                  <Button size="sm" variant="ghost">
                    Max
                  </Button>
                </>
              }
            />
          </Field>
          <Field label="Memo" optional counter={`${new TextEncoder().encode(memo).length}/64`}>
            <TextInput value={memo} onValue={setMemo} maxLength={64} />
          </Field>
          <Field label="Passphrase">
            <PasswordInput value={pass} onValue={setPass} autocomplete="off" data-1p-ignore="" />
          </Field>
          <Field label="Asset">
            <Select
              value={asset}
              onValue={setAsset}
              options={[
                { value: 'mmx', label: 'MMX' },
                { value: 'usdm', label: 'USDM' },
              ]}
            />
          </Field>
          <Field label="Addresses" hint="1 to 100">
            <NumberStepper value={count} onChange={setCount} min={1} max={100} />
          </Field>
          <Field label="Search" hideLabel>
            <SearchInput value={query} onValue={setQuery} label="Search contacts" placeholder="Search contacts" />
          </Field>
        </Grid>
      </Demo>

      <Demo title="Suggestions">
        <Grid columns={2} gap={5}>
          <Field label="Word 7" error={word && !WORDS.some((w) => w.startsWith(word)) ? 'Not a recovery word' : undefined}>
            <Combobox
              value={word}
              onValue={setWord}
              sensitive
              autoHighlight
              listLabel="Matching words"
              leading={<Text as="span" size="xs" tone="faint" numeric>7</Text>}
              options={word ? WORDS.filter((w) => w.startsWith(word) && w !== word).slice(0, 5).map((w) => ({ id: w, label: w })) : []}
              onSelect={(o) => setWord(o.id)}
            />
          </Field>
          <Field label="To">
            <Combobox
              value={to}
              onValue={setTo}
              mono={to.startsWith('mmx1')}
              placeholder="Address or contact"
              listLabel="Contacts"
              options={[
                { id: ADDR, label: 'Alice', description: ADDR.slice(0, 16) + '…', leading: <Identicon value={ADDR} size={28} /> },
                { id: ADDR2, label: 'Exchange deposit', description: ADDR2.slice(0, 16) + '…', leading: <Identicon value={ADDR2} size={28} /> },
              ].filter((c) => !to || (typeof c.label === 'string' && c.label.toLowerCase().includes(to.toLowerCase())))}
              onSelect={(o) => setTo(o.id)}
            />
          </Field>
        </Grid>
      </Demo>

      <Demo title="Settings rows">
        <Card>
          <OptionRow title="Lock automatically" description="After a period without activity">
            <Select
              value={autoLock}
              onValue={setAutoLock}
              size="sm"
              aria-label="Lock after"
              options={[
                { value: '5', label: '5 minutes' },
                { value: '15', label: '15 minutes' },
                { value: '60', label: '1 hour' },
              ]}
            />
          </OptionRow>
          <OptionRow title="Addresses" description="How many addresses this wallet shows">
            <NumberStepper value={count} onChange={setCount} min={1} max={100} label="Addresses" />
          </OptionRow>
          <OptionRow title="Appearance">
            <SegmentedControl
              size="sm"
              label="Appearance"
              value={'system'}
              onChange={() => undefined}
              options={[
                { value: 'system', label: 'System', icon: 'monitor' },
                { value: 'light', label: 'Light', icon: 'sun' },
                { value: 'dark', label: 'Dark', icon: 'moon' },
              ]}
            />
          </OptionRow>
          <OptionRow title="Remove wallet" description="You can restore it with its recovery phrase" tone="danger">
            <Button variant="danger" size="sm" icon="trash">
              Remove
            </Button>
          </OptionRow>
        </Card>
        <Disclosure title="Other tokens" meta="3" icon="warning">
          <List>
            <ListItem leading={<AssetIcon contract={fakeAddress(40)} symbol="MMX" size={32} />} title="MMX" subtitle={<AddressText address={fakeAddress(40)} mode="short" />} trailing={<Amount value={10_000_000n} decimals={6} />} />
          </List>
        </Disclosure>
        <Card>
          <EmptyState tone="positive" icon="check-circle" title="Transfer sent" message="It will appear in Activity shortly." action={<Button variant="primary">Done</Button>} />
        </Card>
      </Demo>

      <Demo title="Choices">
        <Stack gap={4}>
          <SegmentedControl
            label="Filter"
            value={seg}
            onChange={setSeg}
            options={[
              { value: 'all', label: 'All' },
              { value: 'in', label: 'Received' },
              { value: 'out', label: 'Sent' },
              { value: 'rewards', label: 'Rewards' },
            ]}
          />
          <Switch checked={on} onChange={setOn} label="Lock automatically" />
        </Stack>
      </Demo>

      <Demo title="Amounts & addresses">
        <Stack gap={3}>
          <Amount value={12_045_678_901n} decimals={6} symbol="MMX" size="display" />
          <Stack direction="row" gap={6} wrap>
            <Amount value={250_000_000n} decimals={6} symbol="MMX" sign="always" size="lg" />
            <Amount value={-1_500_000n} decimals={6} symbol="MMX" size="lg" />
            <Amount value={1n} decimals={18} symbol="USDM" />
            <Amount value={0n} decimals={6} symbol="MMX" tone="muted" />
          </Stack>
          <AddressText address={ADDR} onCopy={() => undefined} />
          <AddressText address={ADDR2} mode="short" onCopy={() => undefined} />
        </Stack>
      </Demo>

      <Demo title="Avatars & icons">
        <Stack direction="row" gap={3} wrap align="center">
          {[ADDR, ADDR2, 'wallet-a', 'wallet-b', '2874019362', '913552087', '4021187745', 'contact-x'].map((v) => (
            <Identicon key={v} value={v} size={40} />
          ))}
          <AssetIcon contract={NATIVE} native size={40} />
          <AssetIcon contract={ADDR2} symbol="USDM" size={40} />
          <IconTile icon="arrow-down-left" tone="positive" />
          <IconTile icon="arrow-up-right" />
          <IconTile icon="reward" tone="accent" />
          <IconTile icon="swap" />
          <IconTile icon="clock" tone="warning" />
          <IconTile icon="x-circle" tone="negative" />
        </Stack>
        <div class="kitchen-icons">
          {iconNames.map((n) => (
            <span key={n} class="kitchen-icon" title={n}>
              <Icon name={n} size="lg" />
              <Text as="span" size="xs" tone="faint">
                {n}
              </Text>
            </span>
          ))}
        </div>
      </Demo>

      <Demo title="Status">
        <Stack direction="row" gap={2} wrap>
          <Badge>Neutral</Badge>
          <Badge tone="positive" icon="check">
            Confirmed
          </Badge>
          <Badge tone="warning" icon="clock">
            Pending
          </Badge>
          <Badge tone="negative" icon="x-circle">
            Failed
          </Badge>
          <Badge tone="accent" dot>
            Active
          </Badge>
          <Spinner label="Loading" />
        </Stack>
        <Banner tone="info">Your wallet is up to date.</Banner>
        <Banner tone="warning" title="Unverified token" action={<Button size="sm">Details</Button>}>
          Anyone can create a token with this name.
        </Banner>
        <Banner tone="danger" title="We couldn’t confirm the transfer" onDismiss={() => undefined}>
          Check Activity before sending again.
        </Banner>
        <Banner tone="positive">Recovery phrase verified.</Banner>
      </Demo>

      <Demo title="Cards & lists">
        <Card variant="hero" padding="lg">
          <Stack gap={4}>
            <Text size="sm" tone="muted" weight="medium">
              Total balance
            </Text>
            <Amount value={12_045_678_901n} decimals={6} symbol="MMX" size="display" />
            <Stack direction="row" gap={2}>
              <Button variant="primary" icon="send">
                Send
              </Button>
              <Button icon="receive">Receive</Button>
            </Stack>
          </Stack>
        </Card>
        <ListGroup title="Today" aside="3 transfers">
          <ListItem
            leading={<IconTile icon="arrow-down-left" tone="positive" />}
            title="Received"
            subtitle="From Alice"
            trailing={<Amount value={250_000_000n} decimals={6} symbol="MMX" sign="always" />}
            trailingSub="14:02"
            onClick={() => undefined}
          />
          <ListItem
            leading={<IconTile icon="arrow-up-right" />}
            title="Sent"
            subtitle={<AddressText address={ADDR2} mode="short" />}
            trailing={<Amount value={-1_500_000n} decimals={6} symbol="MMX" sign="always" />}
            trailingSub={<Badge tone="warning" icon="clock">Pending</Badge>}
            onClick={() => undefined}
          />
          <ListItem
            leading={<IconTile icon="reward" tone="accent" />}
            title="Farming reward"
            subtitle="Block 1,284,410"
            trailing={<Amount value={500_000n} decimals={6} symbol="MMX" sign="always" />}
            trailingSub="09:41"
            onClick={() => undefined}
          />
        </ListGroup>
        <List inset label="Contacts">
          <ListItem
            leading={<Identicon value={ADDR} size={36} />}
            title="Alice"
            subtitle={<AddressText address={ADDR} mode="short" />}
            actions={
              <>
                <IconButton icon="send" label="Send to Alice" size="sm" />
                <IconButton icon="more" label="More" size="sm" />
              </>
            }
          />
          <ListItem leading={<Identicon value={ADDR2} size={36} />} title="Exchange deposit" subtitle={<AddressText address={ADDR2} mode="short" />} onClick={() => undefined} />
        </List>
      </Demo>

      <Demo title="Details">
        <Card>
          <KeyValueList>
            <KeyValue label="To" stacked>
              <AddressText address={ADDR} />
            </KeyValue>
            <KeyValue label="Amount">
              <Amount value={250_000_000n} decimals={6} symbol="MMX" />
            </KeyValue>
            <KeyValue label="Network fee" hint="Standard">
              <Amount value={50_000n} decimals={6} symbol="MMX" />
            </KeyValue>
            <KeyValue label="Total" emphasis>
              <Amount value={250_050_000n} decimals={6} symbol="MMX" />
            </KeyValue>
          </KeyValueList>
        </Card>
      </Demo>

      <Demo title="Navigation & overlays">
        <ProgressSteps steps={['Name', 'Passphrase', 'Recovery phrase', 'Verify']} current={2} />
        <Stack direction="row" gap={2} wrap>
          <Button onClick={() => setDialog(true)}>Dialog</Button>
          <Button onClick={() => setSheet(true)}>Sheet</Button>
          <Button variant="danger" onClick={() => setConfirm(true)}>
            Confirm
          </Button>
          <Menu
            label="Actions"
            items={[
              { label: 'Copy address', icon: 'copy', onSelect: () => undefined },
              { label: 'Edit label', icon: 'edit', onSelect: () => undefined },
              { separator: true },
              { label: 'Remove', icon: 'trash', tone: 'danger', onSelect: () => undefined },
            ]}
            trigger={(props) => <IconButton {...props} icon="more" label="Actions" variant="secondary" />}
          />
          <Tooltip content="Copies the full address">
            <Button icon="copy">Tooltip</Button>
          </Tooltip>
          <Button onClick={() => toast({ tone: 'positive', title: 'Transfer sent', message: 'It will appear in Activity shortly.' })}>Toast</Button>
          <Button onClick={() => toast({ tone: 'negative', title: 'Wrong passphrase', timeout: 0 })}>Error toast</Button>
        </Stack>
      </Demo>

      <Demo title="Recovery phrase">
        <WordGrid words={WORDS} hidden={hidden} onReveal={() => setHidden(false)} />
        <WordGrid words={WORDS} />
      </Demo>

      <Demo title="Empty & loading">
        <Card>
          <EmptyState icon="activity" title="No activity yet" message="Transfers to and from this wallet show up here." action={<Button icon="receive">Receive MMX</Button>} />
        </Card>
        <Card>
          <Stack gap={3} aria-busy="true">
            <Skeleton width="40%" height="1.5rem" />
            <Skeleton lines={3} />
            <Stack direction="row" gap={3}>
              <Skeleton shape="circle" height="2.5rem" />
              <Skeleton width="60%" />
            </Stack>
          </Stack>
        </Card>
      </Demo>

      <Dialog
        open={dialog}
        onClose={() => setDialog(false)}
        title="Label this address"
        description="Only you can see labels."
        footer={
          <>
            <Button onClick={() => setDialog(false)}>Cancel</Button>
            <Button variant="primary" onClick={() => setDialog(false)}>
              Save
            </Button>
          </>
        }
      >
        <Field label="Label">
          <TextInput value={text} onValue={setText} data-autofocus="" />
        </Field>
      </Dialog>
      <Sheet open={sheet} onClose={() => setSheet(false)} title="Received" description="Oct 24, 2026 · 14:02">
        <Stack gap={5}>
          <Amount value={250_000_000n} decimals={6} symbol="MMX" sign="always" size="xl" />
          <KeyValueList>
            <KeyValue label="Status">
              <Badge tone="positive" icon="check">
                Confirmed
              </Badge>
            </KeyValue>
            <KeyValue label="From">Alice</KeyValue>
            <KeyValue label="Memo">Invoice 2041</KeyValue>
            <KeyValue label="Block">1,284,410</KeyValue>
          </KeyValueList>
          <Button icon="external">View in explorer</Button>
        </Stack>
      </Sheet>
      <ConfirmDialog
        open={confirm}
        title="Remove this wallet?"
        message="You can restore it later with its recovery phrase."
        confirmLabel="Remove wallet"
        tone="danger"
        busy={busy}
        onCancel={() => setConfirm(false)}
        onConfirm={() => {
          setBusy(true);
          setTimeout(() => {
            setBusy(false);
            setConfirm(false);
          }, 1200);
        }}
      />
      <ToastViewport toasts={toasts} onDismiss={(id) => setToasts((l) => l.filter((t) => t.id !== id))} />
    </Stack>
  );
}

function OverviewMock({ onNavigate }: { onNavigate: (r: RouteId) => void }) {
  return (
    <Page width="wide">
      <PageHeader title="Overview" />
      <Card variant="hero" padding="lg">
        <Stack direction="row" justify="between" align="end" gap={6} wrap>
          <Stack gap={3}>
            <Text size="sm" tone="muted" weight="medium">
              Total balance
            </Text>
            <Amount value={12_045_678_901n} decimals={6} symbol="MMX" size="display" />
            <Text size="sm" tone="muted">
              <Amount value={-2_500_000n} decimals={6} symbol="MMX" size="sm" tone="muted" /> pending
            </Text>
          </Stack>
          <Stack direction="row" gap={2}>
            <Button variant="primary" icon="send" size="lg" onClick={() => onNavigate('send')}>
              Send
            </Button>
            <Button icon="receive" size="lg" onClick={() => onNavigate('receive')}>
              Receive
            </Button>
          </Stack>
        </Stack>
      </Card>
      <Grid columns={3} gap={6}>
        <div class="kitchen-span-2">
          <Section title="Balance" card>
            <Skeleton shape="rect" height="220px" />
          </Section>
        </div>
        <Section title="Assets" card>
          <List>
            <ListItem leading={<AssetIcon contract={NATIVE} native size={36} />} title="MMX" subtitle="MMX" trailing={<Amount value={12_045_678_901n} decimals={6} />} />
            <ListItem leading={<AssetIcon contract={ADDR2} symbol="USDM" size={36} />} title="USDM" subtitle={<AddressText address={ADDR2} mode="short" />} trailing={<Amount value={250_000_000n} decimals={6} />} />
          </List>
        </Section>
      </Grid>
      <Section title="Recent activity" actions={<Button variant="ghost" size="sm" iconEnd="chevron-right" onClick={() => onNavigate('activity')}>View all</Button>}>
        <List inset>
          <ListItem
            leading={<IconTile icon="arrow-down-left" tone="positive" />}
            title="Received"
            subtitle="From Alice · Today, 14:02"
            trailing={<Amount value={250_000_000n} decimals={6} symbol="MMX" sign="always" />}
            onClick={() => undefined}
          />
          <ListItem
            leading={<IconTile icon="arrow-up-right" />}
            title="Sent"
            subtitle={
              <>
                To <AddressText address={ADDR2} mode="short" /> · Today, 11:20
              </>
            }
            trailing={<Amount value={-1_500_000n} decimals={6} symbol="MMX" sign="always" />}
            trailingSub={<Badge tone="warning" icon="clock">Pending</Badge>}
            onClick={() => undefined}
          />
          <ListItem
            leading={<IconTile icon="reward" tone="accent" />}
            title="Farming reward"
            subtitle="Yesterday, 09:41"
            trailing={<Amount value={500_000n} decimals={6} symbol="MMX" sign="always" />}
            onClick={() => undefined}
          />
          <ListItem
            leading={<IconTile icon="swap" />}
            title="Moved between your addresses"
            subtitle="Oct 21, 16:05"
            trailing={<Amount value={0n} decimals={6} symbol="MMX" tone="muted" />}
            onClick={() => undefined}
          />
        </List>
      </Section>
    </Page>
  );
}

function ReceiveMock() {
  return (
    <Page width="default">
      <PageHeader title="Receive" subtitle="Share this address to receive MMX and tokens." />
      <Card padding="lg">
        <Grid columns={2} gap={8}>
          <Stack align="center" gap={4}>
            <QRCode value={ADDR} size={220} label="QR code of your address" />
          </Stack>
          <Stack gap={4} justify="center">
            <Field label="Address">
              <Select value="0" onValue={() => undefined} options={[{ value: '0', label: '#0 · Main' }]} />
            </Field>
            <AddressText address={ADDR} />
            <Stack direction="row" gap={2} wrap>
              <CopyButton variant="button" buttonVariant="primary" label="Copy address" onCopy={() => undefined} />
              <Button icon="tag">Edit label</Button>
            </Stack>
          </Stack>
        </Grid>
      </Card>
    </Page>
  );
}

function ShellDemo() {
  const params = new URLSearchParams(location.search);
  const [route, setRoute] = useState<RouteId>((params.get('route') as RouteId) || 'overview');
  const [active, setActive] = useState('wallet_1.dat');
  const net = (params.get('net') as 'online' | 'offline' | 'connecting' | 'syncing') || 'online';
  return (
    <Shell
      route={route}
      onNavigate={setRoute}
      wallets={WALLETS}
      activeWalletId={active}
      onSelectWallet={setActive}
      onAddWallet={() => undefined}
      network={{ state: net, height: 1_284_551, network: 'mainnet' }}
      canLock
      onLock={() => undefined}
    >
      {route === 'receive' ? (
        <ReceiveMock />
      ) : route === 'overview' ? (
        <OverviewMock onNavigate={setRoute} />
      ) : (
        <Page>
          <PageHeader title={route.charAt(0).toUpperCase() + route.slice(1)} />
          <Card>
            <EmptyState icon="activity" title="Nothing here yet" message="This screen is built by the screen engineers." />
          </Card>
        </Page>
      )}
    </Shell>
  );
}

function FocusDemo() {
  const [name, setName] = useState('Main wallet');
  return (
    <FocusLayout footer="MMX Wallet">
      <Stack gap={2}>
        <ProgressSteps steps={['Name', 'Passphrase', 'Recovery phrase', 'Verify']} current={0} />
      </Stack>
      <Stack gap={2}>
        <Heading level={1} size="lg">
          Name your wallet
        </Heading>
        <Text tone="muted">You can change it later.</Text>
      </Stack>
      <Field label="Wallet name">
        <TextInput value={name} onValue={setName} size="lg" />
      </Field>
      <Button variant="primary" size="lg" block>
        Continue
      </Button>
    </FocusLayout>
  );
}

export function Kitchen() {
  const params = new URLSearchParams(location.search);
  const view = params.get('view') ?? 'components';
  if (view === 'shell') return <ShellDemo />;
  if (view === 'focus') return <FocusDemo />;
  if (view === 'avatars') {
    return (
      <div class="kitchen-pane" data-scheme={params.get('scheme') === 'dark' ? 'dark' : 'light'}>
        <Stack direction="row" gap={4} wrap>
          {Array.from({ length: 40 }, (_, i) => (
            <Identicon key={i} value={i % 2 ? fakeAddress(i * 31 + 5) : String(1_000_000_000 + i * 7_919_133)} size={64} />
          ))}
        </Stack>
      </div>
    );
  }
  const single = params.get('single');
  if (single) {
    return (
      <div class="kitchen-pane" data-scheme={single === 'dark' ? 'dark' : 'light'}>
        <Components />
      </div>
    );
  }
  return (
    <div class="kitchen">
      <div class="kitchen-pane" data-scheme="light">
        <Components />
      </div>
      <div class="kitchen-pane" data-scheme="dark">
        <Components />
      </div>
    </div>
  );
}

