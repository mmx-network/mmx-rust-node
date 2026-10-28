import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import { QrPhotoError, QrReader, qrPhoto, qrPixels } from '../../../lib/qr-reader';
import { lockVersion } from '../../../state';
import { recipientFromQr } from '../../../state/actions/send';
import { Banner, Button, Dialog, Icon, Stack } from '../../components';

interface ScanProps {
  onClose: () => void;
  onRecipient: (address: string) => void;
}

export function ScanDialog(props: ScanProps) {
  const latest = useRef(props);
  latest.current = props;
  const lockedAt = lockVersion.value;
  const openedAt = useRef(lockedAt);
  const video = useRef<HTMLVideoElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const controller = useRef<{ close: () => void; camera: () => void; photo: (file: File) => void }>();
  const [camera, setCamera] = useState<'off' | 'starting' | 'on'>('off');
  const [reading, setReading] = useState(false);
  const [error, setError] = useState<string>();
  // Camera capture requires a secure browser context.
  const hasCamera = __PLATFORM__ === 'web' && window.isSecureContext && typeof navigator.mediaDevices?.getUserMedia === 'function';

  useLayoutEffect(() => {
    if (lockedAt !== openedAt.current) controller.current?.close();
  }, [lockedAt]);

  useEffect(() => {
    let alive = true;
    let operation = 0;
    let stream: MediaStream | null = null;
    let decoder: QrReader | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const cancel = () => {
      operation++;
      clearTimeout(timer);
      decoder?.close();
      decoder = null;
      stream?.getTracks().forEach((track) => track.stop());
      stream = null;
      if (video.current) video.current.srcObject = null;
    };
    const close = () => { cancel(); latest.current.onClose(); };
    const current = (id: number) => alive && operation === id && lockVersion.peek() === openedAt.current;
    const accept = (text: string | null, photo = false) => {
      const address = text === null ? null : recipientFromQr(text);
      if (address) {
        cancel();
        latest.current.onRecipient(address);
        return true;
      }
      if (text !== null) setError('This QR code does not contain a valid MMX address.');
      else if (photo) setError('No QR code found. Try a clearer photo with the whole code visible.');
      return false;
    };
    const startCamera = async () => {
      cancel();
      const id = operation;
      setError(undefined);
      setReading(false);
      setCamera('starting');
      try {
        const media = await navigator.mediaDevices.getUserMedia({
          audio: false,
          video: { facingMode: { ideal: 'environment' }, width: { ideal: 1280 }, height: { ideal: 720 }, frameRate: { ideal: 10 } },
        });
        if (!current(id)) { media.getTracks().forEach((track) => track.stop()); return; }
        stream = media;
        for (const track of media.getVideoTracks()) track.addEventListener('ended', () => {
          if (current(id)) { cancel(); setCamera('off'); setError('Camera stopped. Try again or choose a photo.'); }
        }, { once: true });
        const preview = video.current!;
        preview.srcObject = media;
        await preview.play();
        if (!current(id)) return;
        const reader = new QrReader();
        decoder = reader;
        setCamera('on');
        const scan = async () => {
          if (!current(id)) return;
          try {
            if (preview.readyState >= 2 && preview.videoWidth > 0) {
              const text = await reader.read(qrPixels(preview, preview.videoWidth, preview.videoHeight, 640));
              if (!current(id) || accept(text)) return;
            }
            timer = setTimeout(() => void scan(), 300);
          } catch {
            if (current(id)) { cancel(); setCamera('off'); setError('Could not read the camera. Try again or choose a photo.'); }
          }
        };
        void scan();
      } catch (cause) {
        if (!current(id)) return;
        cancel();
        setCamera('off');
        setError(cause instanceof DOMException && cause.name === 'NotAllowedError'
          ? 'Camera access was denied. Allow it in your browser, or choose a photo.'
          : 'Camera unavailable. Try again or choose a photo.');
      }
    };
    const readPhoto = async (file: File) => {
      cancel();
      const id = operation;
      setCamera('off');
      setError(undefined);
      setReading(true);
      try {
        const image = await qrPhoto(file);
        if (!current(id)) return;
        const reader = new QrReader();
        decoder = reader;
        const text = await reader.read(qrPixels(image, image.naturalWidth, image.naturalHeight, 1600));
        if (current(id)) accept(text, true);
      } catch (cause) {
        if (current(id)) setError(cause instanceof QrPhotoError ? cause.message : 'Could not read this photo. Try another image.');
      } finally {
        if (current(id)) { decoder?.close(); decoder = null; setReading(false); }
      }
    };
    const onHidden = () => { if (document.visibilityState === 'hidden') close(); };
    controller.current = { close, camera: () => void startCamera(), photo: (file) => void readPhoto(file) };
    document.addEventListener('visibilitychange', onHidden);
    window.addEventListener('pagehide', close);
    if (hasCamera) void startCamera();
    return () => {
      alive = false;
      cancel();
      controller.current = undefined;
      document.removeEventListener('visibilitychange', onHidden);
      window.removeEventListener('pagehide', close);
    };
  }, []);

  return (
    <Dialog open size="sm" title="Scan QR code" description="Scan an MMX address to fill the recipient. Photos stay on this device."
      onClose={() => controller.current?.close()}
      footer={<>
        {hasCamera ? <Button disabled={camera !== 'off' || reading} onClick={() => controller.current?.camera()}>Use camera</Button> : null}
        <Button variant="primary" icon="upload" loading={reading} onClick={() => input.current?.click()}>Choose photo</Button>
      </>}
    >
      <Stack gap={4}>
        {hasCamera ? <div class="qr-scan__preview">
          <video ref={video} autoPlay muted playsInline aria-label="Camera preview" />
          {camera !== 'on' ? <span class="qr-scan__placeholder"><Icon name="qr" size="xl" />{camera === 'starting' ? 'Starting camera…' : 'Camera off'}</span> : null}
        </div> : <p>Choose a photo or screenshot containing the QR code.</p>}
        <p class="qr-scan__hint" role="status">{camera === 'on' ? 'Point the camera at the whole QR code.' : reading ? 'Reading photo…' : 'You can also choose a photo of the code.'}</p>
        {error ? <Banner tone="warning">{error}</Banner> : null}
        <input ref={input} type="file" accept="image/png,image/jpeg,image/webp,image/gif,image/avif,image/heic,image/heif" hidden
          aria-label="QR code photo" onChange={(event) => {
            const file = event.currentTarget.files?.[0];
            event.currentTarget.value = '';
            if (file) controller.current?.photo(file);
          }} />
      </Stack>
    </Dialog>
  );
}
