/** QR decoding stays off the UI thread; one bounded frame is sent at a time. */
import jsQR from 'jsqr';

self.onmessage = (event: MessageEvent<{ pixels: ArrayBuffer; width: number; height: number }>) => {
  const { pixels, width, height } = event.data;
  const code = jsQR(new Uint8ClampedArray(pixels), width, height, { inversionAttempts: 'attemptBoth' });
  self.postMessage(code?.data ?? null);
};
