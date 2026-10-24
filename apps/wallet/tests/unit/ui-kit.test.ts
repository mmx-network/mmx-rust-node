import { describe, expect, it } from 'vitest';
import { amountDisplay } from '../../src/ui/components/Amount';
import { fnv1a, xorshift } from '../../src/ui/components/internal/hash';
import { safeLabel } from '../../src/ui/components/internal/text';
import { chunkAddress, shortenAddress } from '../../src/ui/components/AddressText';
import { identiconShapes } from '../../src/ui/components/Identicon';
import { networkLabel, networkName } from '../../src/ui/shell/NetworkStatus';
import { icons } from '../../src/ui/icons';

const ADDRESS = 'mmx1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqdgytev';

describe('amount display', () => {
  const text = (abs: bigint, decimals: number, locale: string) => {
    const p = amountDisplay(abs, decimals, locale);
    return p.integer + p.decimal + p.fraction;
  };

  it('formats atomic units without floating point', () => {
    expect(amountDisplay(1_234_567n, 6, 'en-US')).toEqual({ integer: '1', decimal: '.', fraction: '234567' });
    expect(amountDisplay(1_000_000n, 6, 'en-US')).toEqual({ integer: '1', decimal: '', fraction: '' });
    expect(text(0n, 6, 'en-US')).toBe('0');
    expect(text(1n, 18, 'en-US')).toBe('0.000000000000000001');
    expect(text(42n, 0, 'en-US')).toBe('42');
  });

  it('keeps every digit of very large values', () => {
    expect(text(123_456_789_012_345_678_901_234_567_890n, 18, 'en-US')).toBe('123,456,789,012.34567890123456789');
    expect(text(2n ** 128n - 1n, 6, 'en-US')).toBe('340,282,366,920,938,463,463,374,607,431,768.211455');
  });

  it('uses the locale separators and Latin digits', () => {
    expect(amountDisplay(1_234_500_000n, 6, 'de-DE')).toEqual({ integer: '1.234', decimal: ',', fraction: '5' });
    expect(amountDisplay(1_234_500_000n, 6, 'fr-FR').decimal).toBe(',');
    expect(text(1_234_500_000n, 6, 'ar-EG')).toMatch(/^1.234.5$/u);
  });

  it('treats invalid decimals as zero', () => {
    expect(text(15n, -1, 'en-US')).toBe('15');
    expect(text(15n, 1.5, 'en-US')).toBe('15');
    expect(text(15n, 40, 'en-US')).toBe('15');
  });
});

describe('text safety', () => {
  it('removes control and bidi override characters', () => {
    expect(safeLabel('US‮DM\u0000')).toBe('USDM');
    expect(safeLabel('⁦evil⁩‏')).toBe('evil');
  });

  it('caps length by code points', () => {
    expect(safeLabel('abcdefghij', 5)).toBe('abcd…');
    expect(Array.from(safeLabel('😀'.repeat(20), 8))).toHaveLength(8);
  });
});

describe('addresses', () => {
  it('chunks in groups of four', () => {
    const chunks = chunkAddress(ADDRESS);
    expect(chunks.join('')).toBe(ADDRESS);
    expect(chunks.every((c, i) => c.length === 4 || i === chunks.length - 1)).toBe(true);
  });

  it('shortens with prefix and suffix', () => {
    expect(shortenAddress(ADDRESS)).toBe('mmx1qqqqqq…dgytev');
    expect(shortenAddress('mmx1short')).toBe('mmx1short');
  });
});

describe('identicon', () => {
  it('is deterministic and value dependent', () => {
    expect(fnv1a('mmx')).toBe(fnv1a('mmx'));
    expect(fnv1a('a')).not.toBe(fnv1a('b'));
    const a = identiconShapes('2874019362');
    expect(identiconShapes('2874019362')).toEqual(a);
    expect(identiconShapes('913552087')).not.toEqual(a);
  });

  it('draws from a uniform generator in [0, 1)', () => {
    const r = xorshift(fnv1a('seed'));
    for (let i = 0; i < 1000; i++) {
      const v = r();
      expect(v).toBeGreaterThanOrEqual(0);
      expect(v).toBeLessThan(1);
    }
  });
});

describe('network status', () => {
  it('names networks for display', () => {
    expect(networkName('mainnet')).toBe('Mainnet');
    expect(networkName('testnet12')).toBe('Testnet 12');
    expect(networkName(undefined)).toBe('Online');
    expect(networkName('<script>')).toBe('Script');
  });

  it('labels each state', () => {
    expect(networkLabel({ state: 'online', network: 'mainnet' })).toBe('Mainnet');
    expect(networkLabel({ state: 'offline' })).toBe('Offline');
    expect(networkLabel({ state: 'connecting' })).toBe('Connecting…');
    expect(networkLabel({ state: 'syncing' })).toBe('Syncing');
  });
});

describe('icons', () => {
  it('contain only path commands and numbers', () => {
    for (const [name, d] of Object.entries(icons)) {
      expect(d, name).toMatch(/^[MmLlHhVvCcSsQqTtAaZz0-9.,\s-]+$/);
    }
  });
});
