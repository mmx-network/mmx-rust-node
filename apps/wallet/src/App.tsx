import { useSignalEffect } from '@preact/signals';
import type { ComponentType } from 'preact';
import { useEffect } from 'preact/hooks';
import {
  activeWalletId,
  canLock,
  dismissToast,
  init,
  initError,
  isShellRoute,
  lock,
  navigate,
  network,
  notify,
  phase,
  route,
  selectWallet,
  settings,
  startRouter,
  toasts,
  wallets,
  type FlowRouteId,
  type RouteId,
  type Toast,
} from './state';
import { errorMessage } from './lib/errors';
import { applyAppearance } from './styles';
import { Button } from './ui/components/Button';
import { Stack } from './ui/components/Layout';
import { Spinner } from './ui/components/Spinner';
import { Heading, Text } from './ui/components/Text';
import { ToastViewport, type Toast as ViewToast } from './ui/components/Toasts';
import { FocusLayout } from './ui/shell/FocusLayout';
import { Shell } from './ui/shell/Shell';
import ActivityScreen from './ui/screens/activity';
import ContactsScreen from './ui/screens/contacts';
import CreateScreen from './ui/screens/create';
import ImportScreen from './ui/screens/import';
import OverviewScreen from './ui/screens/overview';
import ReceiveScreen from './ui/screens/receive';
import SendScreen from './ui/screens/send';
import SettingsScreen from './ui/screens/settings';
import UnlockScreen from './ui/screens/unlock';
import WelcomeScreen from './ui/screens/welcome';

const SHELL_SCREENS: Record<RouteId, ComponentType> = {
  overview: OverviewScreen,
  send: SendScreen,
  receive: ReceiveScreen,
  activity: ActivityScreen,
  contacts: ContactsScreen,
  settings: SettingsScreen,
};

const FLOW_SCREENS: Record<FlowRouteId, ComponentType> = {
  welcome: WelcomeScreen,
  create: CreateScreen,
  import: ImportScreen,
  unlock: UnlockScreen,
};

const TONES: Record<Toast['kind'], NonNullable<ViewToast['tone']>> = {
  success: 'positive',
  error: 'negative',
  warning: 'warning',
  info: 'info',
};

function viewToasts(list: readonly Toast[]): ViewToast[] {
  return list.map((t) => {
    const v: ViewToast = { id: String(t.id), tone: TONES[t.kind], title: t.message, timeout: t.timeout };
    if (t.action) v.action = t.action;
    return v;
  });
}

function onSelectWallet(id: string): void {
  selectWallet(id).catch((e: unknown) => notify(errorMessage(e), 'error'));
}

function onLock(): void {
  lock().catch((e: unknown) => notify(errorMessage(e), 'error'));
}

function StartError({ message }: { message: string }) {
  return (
    <FocusLayout>
      <Stack gap={6}>
        <Stack gap={2}>
          <Heading level={1}>MMX Wallet could not start</Heading>
          <Text tone="muted">{message}</Text>
        </Stack>
        <Button variant="primary" onClick={() => window.location.reload()}>
          Try again
        </Button>
      </Stack>
    </FocusLayout>
  );
}

export function App() {
  useEffect(() => {
    const stop = startRouter();
    void init();
    return stop;
  }, []);

  useSignalEffect(() => {
    applyAppearance(settings.value.theme);
  });

  const toastLayer = <ToastViewport toasts={viewToasts(toasts.value)} onDismiss={(id) => dismissToast(Number(id))} />;
  const startError = initError.value;
  if (startError) return <StartError message={startError} />;

  const current = phase.value;
  if (current === 'loading') {
    return (
      <div class="app-loading">
        <Spinner size="lg" label="Loading" />
      </div>
    );
  }

  const r = route.value;
  if (current === 'ready' && isShellRoute(r.name)) {
    const Screen = SHELL_SCREENS[r.name];
    return (
      <>
        <Shell
          route={r.name}
          onNavigate={(id) => navigate(id)}
          wallets={[...wallets.value]}
          activeWalletId={activeWalletId.value}
          onSelectWallet={onSelectWallet}
          onAddWallet={() => navigate('welcome')}
          network={network.value}
          canLock={canLock.value}
          onLock={onLock}
        >
          <Screen />
        </Shell>
        {toastLayer}
      </>
    );
  }

  // Flow screens; a shell route outside 'ready' is redirected by the route guard.
  const Screen = isShellRoute(r.name) ? null : FLOW_SCREENS[r.name];
  return (
    <>
      {Screen ? <Screen /> : null}
      {toastLayer}
    </>
  );
}
