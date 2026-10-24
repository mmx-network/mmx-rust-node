import { describe, expect, it } from 'vitest';
import { availableFeeLevels, FEE_LEVELS, FEE_OPTIONS, feeOption, isFeeLevel } from '../../src/lib/fees';
import { memoState, utf8Length } from '../../src/lib/memo';

describe('memo', () => {
  it('counts UTF-8 bytes', () => {
    expect(utf8Length('')).toBe(0);
    expect(utf8Length('abc')).toBe(3);
    expect(utf8Length('é')).toBe(2);
    expect(utf8Length('☕')).toBe(3);
    expect(utf8Length('😀')).toBe(4);
  });

  it('validates the 64-byte limit and shows the counter near it', () => {
    expect(memoState('')).toEqual({ bytes: 0, remaining: 64, showCounter: false, error: null });
    expect(memoState('x'.repeat(47)).showCounter).toBe(false);
    expect(memoState('x'.repeat(48)).showCounter).toBe(true);
    expect(memoState('x'.repeat(64)).error).toBeNull();
    expect(memoState('x'.repeat(65))).toMatchObject({ remaining: -1, error: 'too_long' });
    expect(memoState('😀'.repeat(16)).error).toBeNull();
    expect(memoState('😀'.repeat(16) + 'a').error).toBe('too_long');
    expect(memoState('a\nb').error).toBe('invalid');
    expect(memoState('a\u{202e}b').error).toBe('invalid');
  });
});

describe('fees', () => {
  it('defines the five levels', () => {
    expect(FEE_LEVELS).toEqual([1, 2, 3, 5, 10]);
    expect(FEE_OPTIONS.map((o) => o.label)).toEqual(['Standard', 'Faster', 'Priority', 'High priority', 'Fastest']);
    expect(feeOption(10).multiplier).toBe('10\u{d7}');
    expect(isFeeLevel(3)).toBe(true);
    expect(isFeeLevel(4)).toBe(false);
    expect(isFeeLevel('1')).toBe(false);
  });

  it('limits levels to what the chain allows', () => {
    expect(availableFeeLevels([1, 2, 3, 5, 10])).toEqual([1, 2, 3, 5, 10]);
    expect(availableFeeLevels([1, 2])).toEqual([1, 2]);
    expect(availableFeeLevels([])).toEqual([1, 2, 3, 5, 10]);
    expect(availableFeeLevels(undefined)).toEqual([1, 2, 3, 5, 10]);
    expect(availableFeeLevels([7])).toEqual([1, 2, 3, 5, 10]);
  });
});
