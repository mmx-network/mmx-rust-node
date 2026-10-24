import { useEffect } from 'preact/hooks';
import { finishSend, sendFlow } from '../../../state';
import { SendForm } from './SendForm';
import { SendResult } from './SendResult';
import { SendReview } from './SendReview';
import './send.css';

export default function SendScreen() {
  // A finished transfer is not shown again after leaving the screen.
  useEffect(
    () => () => {
      const step = sendFlow.value.step;
      if (step === 'done' || step === 'uncertain') finishSend();
    },
    [],
  );

  const flow = sendFlow.value;
  switch (flow.step) {
    case 'review':
      return flow.prepared ? <SendReview /> : <SendForm />;
    case 'done':
      return <SendResult uncertain={false} />;
    case 'uncertain':
      return <SendResult uncertain />;
    default:
      return <SendForm />;
  }
}
