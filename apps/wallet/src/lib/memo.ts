/** Transfer memos: at most 64 UTF-8 bytes on chain. */
import { hasUnsafeCharacters } from './text';

export const MEMO_MAX_BYTES = 64;
/** The byte counter is shown from this many bytes on. */
export const MEMO_COUNTER_FROM = 48;

const encoder = new TextEncoder();

export function utf8Length(text: string): number {
  return encoder.encode(text).length;
}

export interface MemoState {
  bytes: number;
  remaining: number;
  showCounter: boolean;
  error: 'too_long' | 'invalid' | null;
}

export function memoState(text: string): MemoState {
  const bytes = utf8Length(text);
  return {
    bytes,
    remaining: MEMO_MAX_BYTES - bytes,
    showCounter: bytes >= MEMO_COUNTER_FROM,
    error: bytes > MEMO_MAX_BYTES ? 'too_long' : hasUnsafeCharacters(text) ? 'invalid' : null,
  };
}
