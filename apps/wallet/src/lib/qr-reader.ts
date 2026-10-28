import workerAsset from './qr-worker?worker&url';

type ScriptPolicy = { createScriptURL: (value: string) => unknown };
type TrustedTypes = { createPolicy: (name: string, rules: { createScriptURL: (value: string) => string }) => ScriptPolicy };
let policy: ScriptPolicy | undefined;

function workerUrl(): string {
  const url = new URL(workerAsset, window.location.href).href;
  const trusted = (window as Window & { trustedTypes?: TrustedTypes }).trustedTypes;
  if (!trusted) return url;
  // No default policy: this one accepts exactly the bundled decoder asset.
  policy ??= trusted.createPolicy('mmx-qr-worker', {
    createScriptURL: (value) => {
      if (value !== url) throw new TypeError('invalid QR worker URL');
      return value;
    },
  });
  return policy.createScriptURL(url) as string;
}

/** A decoder owned by one scan operation; close cancels an outstanding frame. */
export class QrReader {
  // Vite serves modules in development and bundles a classic worker for release.
  private readonly worker = new Worker(workerUrl(), { type: import.meta.env.DEV ? 'module' : 'classic' });
  private pending: { resolve: (text: string | null) => void; reject: (reason: Error) => void } | null = null;

  constructor() {
    this.worker.onmessage = (event: MessageEvent<string | null>) => {
      const request = this.pending;
      this.pending = null;
      request?.resolve(event.data);
    };
    this.worker.onerror = (event) => {
      event.preventDefault();
      const request = this.pending;
      this.pending = null;
      request?.reject(new Error('QR decoder unavailable'));
    };
  }

  read(image: ImageData): Promise<string | null> {
    if (this.pending) return Promise.reject(new Error('QR decoder busy'));
    return new Promise((resolve, reject) => {
      this.pending = { resolve, reject };
      this.worker.postMessage({ pixels: image.data.buffer, width: image.width, height: image.height }, [image.data.buffer]);
    });
  }

  close(): void {
    this.worker.terminate();
    this.pending?.reject(new Error('QR scan cancelled'));
    this.pending = null;
  }
}

/** Keep decoding memory and work bounded for both photos and camera frames. */
export function qrPixels(source: CanvasImageSource, width: number, height: number, maxSide: number): ImageData {
  const scale = Math.min(1, maxSide / Math.max(width, height));
  const canvas = document.createElement('canvas');
  canvas.width = Math.max(1, Math.round(width * scale));
  canvas.height = Math.max(1, Math.round(height * scale));
  const context = canvas.getContext('2d', { willReadFrequently: true });
  if (!context) throw new Error('Image reader unavailable');
  context.drawImage(source, 0, 0, canvas.width, canvas.height);
  return context.getImageData(0, 0, canvas.width, canvas.height);
}

/** Data URLs keep the existing image CSP; no remote image or blob URL is loaded. */
export class QrPhotoError extends Error {}

export function qrPhoto(file: File): Promise<HTMLImageElement> {
  if (!/^image\/(png|jpeg|webp|gif|avif|heic|heif)$/i.test(file.type) || file.size > 10 * 1024 * 1024) {
    return Promise.reject(new QrPhotoError('Choose a photo or screenshot smaller than 10 MB.'));
  }
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new QrPhotoError('Could not open this photo.'));
    reader.onload = () => {
      const image = new Image();
      image.onerror = () => reject(new QrPhotoError('Could not open this photo. Try a PNG or JPEG image.'));
      image.onload = () => resolve(image);
      image.src = reader.result as string;
    };
    reader.readAsDataURL(file);
  });
}
