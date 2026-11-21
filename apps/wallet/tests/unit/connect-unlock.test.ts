import { describe, expect, it } from 'vitest';
import { openUnlock, sealUnlock, unlockKeyPair, unlockPublicKey } from '../../src/connect/unlock';

const context={host:'host',bridge:'bridge',walletId:'123',origin:'https://shop.example',nonce:'nonce'};
describe('one-use unlock handoff encryption', () => {
  it('only opens in the recipient wallet, preserving exact credentials without exposing plaintext', async () => {
    const pair=await unlockKeyPair();
    expect(pair.privateKey.extractable).toBe(false);
    const request={mnemonic:'example recovery words',passphrase:'🔒 exact passphrase\nwith whitespace'};
    const sealed=await sealUnlock(request,await unlockPublicKey(pair),context);
    expect(JSON.stringify(sealed)).not.toContain('example');
    expect(JSON.stringify(sealed)).not.toContain('passphrase');
    expect(await openUnlock(pair,sealed,context)).toEqual(request);
    await expect(openUnlock(await unlockKeyPair(),sealed,context)).rejects.toThrow();
    const again=await sealUnlock(request,await unlockPublicKey(pair),context);
    expect(again.ciphertext).not.toBe(sealed.ciphertext);
  });
  it('rejects altered ciphertext and changes to recipient, bridge, wallet, site or challenge', async () => {
    const pair=await unlockKeyPair();
    const sealed=await sealUnlock({passphrase:'secret'},await unlockPublicKey(pair),context);
    for(const field of Object.keys(context))
      await expect(openUnlock(pair,sealed,{...context,[field]:'different'})).rejects.toThrow();
    const corrupt={...sealed,ciphertext:(sealed.ciphertext[0]==='A'?'B':'A')+sealed.ciphertext.slice(1)};
    await expect(openUnlock(pair,corrupt,context)).rejects.toThrow();
    await expect(openUnlock(pair,{...sealed,iv:'AA=='},context)).rejects.toThrow();
  });
});
