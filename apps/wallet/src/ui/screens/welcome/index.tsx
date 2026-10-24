import { useState } from 'preact/hooks';
import { leaveOnboarding, navigate, platform, usableWallets } from '../../../state';
import { BrandMark, Icon, IconTile, type IconName } from '../../components';
import { FocusLayout } from '../../shell';
import { OpenSavedView } from '../unlock';
import { FlowHeader } from './flow';
import './welcome.css';

interface ChoiceProps {
  icon: IconName;
  title: string;
  detail: string;
  onClick: () => void;
}

function Choice({ icon, title, detail, onClick }: ChoiceProps) {
  return (
    <li class="welcome-choice">
      <button type="button" class="welcome-choice__button" onClick={onClick}>
        <IconTile icon={icon} tone="accent" size="md" class="welcome-choice__icon" />
        <span class="welcome-choice__text">
          <span class="welcome-choice__title">{title}</span>
          <span class="welcome-choice__detail">{detail}</span>
        </span>
        <Icon name="chevron-right" class="welcome-choice__chevron" />
      </button>
    </li>
  );
}

export default function WelcomeScreen() {
  const [openSaved, setOpenSaved] = useState(false);
  const web = platform.value === 'web';
  const adding = usableWallets.value.length > 0;

  if (openSaved) return <OpenSavedView onBack={() => setOpenSaved(false)} />;

  return (
    <FocusLayout onBack={adding ? leaveOnboarding : undefined} class={adding ? 'welcome' : 'welcome welcome--start'}>
      {adding ? (
        <FlowHeader title="Add a wallet" />
      ) : (
        <FlowHeader
          align="center"
          class="welcome-hero"
          leading={<BrandMark size={72} class="welcome-hero__mark" />}
          title="Welcome to MMX Wallet"
          subtitle="Send, receive and keep track of your MMX."
        />
      )}
      <ul class="welcome-choices" aria-label="Get started">
        <Choice icon="plus" title="Create a new wallet" detail="Get a new recovery phrase" onClick={() => navigate('create')} />
        <Choice icon="key" title="Import with recovery phrase" detail="Use your 24 words" onClick={() => navigate('import')} />
        {web ? (
          <Choice icon="shield" title="Open a saved wallet" detail="From your password manager" onClick={() => setOpenSaved(true)} />
        ) : null}
      </ul>
    </FocusLayout>
  );
}
