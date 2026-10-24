// Defensive clean-up for short labels that may originate from the network
// (token symbols, names): the app's sanitizer (control and bidirectional
// formatting characters removed) plus a length cap in code points with an ellipsis.
import { sanitizeText } from '../../../lib/text';

export function safeLabel(value: string, max = 64): string {
  const cleaned = sanitizeText(value, Number.POSITIVE_INFINITY).trim();
  const chars = Array.from(cleaned);
  return chars.length > max ? `${chars.slice(0, max - 1).join('')}…` : cleaned;
}
