import { describe, expect, it } from 'vitest';
import { memoryStorage } from '../../src/backend/web/memory-storage';
import { ReservationStore, type Reservation } from '../../src/backend/web/reservations';
import { NATIVE, testAddress } from '../e2e/mock-rpc';

function reservation(id = 1): Reservation {
  return { network: 'mainnet', txid: id.toString(16).toUpperCase().padStart(64, '0'), expires: 100,
    sender: testAddress('sender'), senderFee: 50_000n,
    inputs: [{address:testAddress('sender'),contract:NATIVE,amount:1_000_000n}] };
}
describe('persisted pending funds', () => {
  it('reads fresh values and isolates wallets, endpoints and networks', () => {
    const storage=memoryStorage();
    const first=new ReservationStore(storage,'https://rpc.test');
    const second=new ReservationStore(storage,'https://rpc.test');
    first.put('1',reservation(1));second.put('1',reservation(2));
    expect(first.list('1')).toHaveLength(2);
    expect(first.list('2')).toEqual([]);
    expect(new ReservationStore(storage,'https://other.test').list('1')).toEqual([]);
    first.release('1','testnet',new Set([reservation(1).txid]),101);
    expect(second.list('1')).toHaveLength(2);
    first.release('1','mainnet',new Set([reservation(1).txid]),100);
    expect(second.list('1')).toEqual([reservation(2)]);
    second.release('1','mainnet',new Set(),101);
    expect(first.list('1')).toEqual([]);
  });
  it('refuses to evict pending reservations at the capacity limit', () => {
    const storage=memoryStorage(), store=new ReservationStore(storage,'https://rpc.test');
    for(let i=0;i<256;i++) store.put('1',reservation(i));
    const before=storage.dump();
    expect(()=>store.put('1',reservation(256))).toThrowError('io_error');
    expect(storage.dump()).toEqual(before);
    expect(store.list('1')[0]).toEqual(reservation(0));
  });
  it('fails closed if stored bookkeeping is damaged or exceeds its size bound', () => {
    const storage=memoryStorage(), store=new ReservationStore(storage,'https://rpc.test');
    store.put('1',reservation());
    const key=Object.keys(storage.dump())[0]!;
    for(const raw of ['{bad', '{}', '[{}]', ' '.repeat(2_000_001)]) {
      storage.setItem(key,raw);
      expect(()=>store.list('1')).toThrowError('io_error');
    }
    store.removeWallet('1');expect(store.list('1')).toEqual([]);
  });
});
