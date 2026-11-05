/** One-use, recipient-bound unlock handoff between same-origin wallet pages.
 * Only the selected wallet tab can decrypt it. Nothing is persisted. */
import type { UnlockRequest } from '../backend/types';
import { ConnectionError, object } from './protocol';

const CURVE = {name:'ECDH',namedCurve:'P-256'};
const LIMIT = 32_768;
const encoder = new TextEncoder();
export interface UnlockContext { host: string; bridge: string; walletId: string; origin: string; nonce: string }
export interface SealedUnlock { key: string; iv: string; ciphertext: string }
function encode(bytes: Uint8Array): string { return btoa(String.fromCharCode(...bytes)); }
function decode(text: unknown, limit: number): Uint8Array<ArrayBuffer> {
  if (typeof text !== 'string' || text.length > limit || !/^[A-Za-z0-9+/]*={0,2}$/u.test(text)) throw new ConnectionError('invalid_request');
  return Uint8Array.from(atob(text),(c)=>c.charCodeAt(0));
}
function binding(c: UnlockContext) {
  return encoder.encode(JSON.stringify(['mmx-wallet-unlock/v1',c.host,c.bridge,c.walletId,c.origin,c.nonce]));
}
export function unlockKeyPair(): Promise<CryptoKeyPair> {
  return crypto.subtle.generateKey(CURVE,false,['deriveKey']);
}
export async function unlockPublicKey(pair: CryptoKeyPair): Promise<string> {
  return encode(new Uint8Array(await crypto.subtle.exportKey('raw',pair.publicKey)));
}
async function sharedKey(pair: CryptoKeyPair, remote: string, usage: 'encrypt'|'decrypt') {
  const bytes=decode(remote,128);
  if(bytes.length!==65) throw new ConnectionError('invalid_request');
  const key=await crypto.subtle.importKey('raw',bytes,CURVE,false,[]);
  return crypto.subtle.deriveKey({name:'ECDH',public:key},pair.privateKey,{name:'AES-GCM',length:256},false,[usage]);
}
export async function sealUnlock(request: UnlockRequest, remote: string, context: UnlockContext): Promise<SealedUnlock> {
  const plain=encoder.encode(JSON.stringify(request));
  try {
    if(plain.length>LIMIT) throw new ConnectionError('invalid_request');
    const pair=await unlockKeyPair(), key=await sharedKey(pair,remote,'encrypt');
    const iv=crypto.getRandomValues(new Uint8Array(12));
    const encrypted=await crypto.subtle.encrypt({name:'AES-GCM',iv,additionalData:binding(context),tagLength:128},key,plain);
    return {key:await unlockPublicKey(pair),iv:encode(iv),ciphertext:encode(new Uint8Array(encrypted))};
  } finally { plain.fill(0); }
}
export async function openUnlock(pair: CryptoKeyPair, sealed: unknown, context: UnlockContext): Promise<UnlockRequest> {
  if(!object(sealed) || typeof sealed.key!=='string') throw new ConnectionError('invalid_request');
  const iv=decode(sealed.iv,32), encrypted=decode(sealed.ciphertext,45_000);
  if(iv.length!==12 || encrypted.length>LIMIT+16) throw new ConnectionError('invalid_request');
  const key=await sharedKey(pair,sealed.key,'decrypt');
  const plain=new Uint8Array(await crypto.subtle.decrypt({name:'AES-GCM',iv,additionalData:binding(context),tagLength:128},key,encrypted));
  try {
    const v: unknown=JSON.parse(new TextDecoder('utf-8',{fatal:true}).decode(plain));
    if(!object(v) || Object.keys(v).some(k=>!['mnemonic','passphrase'].includes(k)) ||
      (v.mnemonic!==undefined && typeof v.mnemonic!=='string') || (v.passphrase!==undefined && typeof v.passphrase!=='string'))
      throw new ConnectionError('invalid_request');
    return v;
  } finally { plain.fill(0); }
}
