/** Recovery phrase display with reveal, hide and copy (copied words are cleared from the clipboard as soon as possible). */
import { copyToClipboard } from '../../../state';
import { Button, CopyButton, WordGrid } from '../../components';
import './create.css';

export interface PhraseWordsProps {
  words: string[];
  revealed: boolean;
  onRevealedChange: (revealed: boolean) => void;
  /** Shows a Copy button (default true); off where another control already copies the words. */
  copyable?: boolean;
}

export function PhraseWords({ words, revealed, onRevealedChange, copyable = true }: PhraseWordsProps) {
  const copy = async (): Promise<void> => {
    const ok = await copyToClipboard(words.join(' '), { sensitive: true });
    if (!ok) throw new Error('copy failed');
  };
  return (
    <div class="create-phrase">
      <WordGrid words={words} hidden={!revealed} onReveal={() => onRevealedChange(true)} />
      {revealed ? (
        <div class="create-phrase__tools">
          <Button variant="ghost" size="sm" icon="eye-off" onClick={() => onRevealedChange(false)}>
            Hide
          </Button>
          {copyable ? <CopyButton variant="button" buttonVariant="ghost" size="sm" label="Copy" onCopy={copy} /> : null}
        </div>
      ) : null}
    </div>
  );
}
