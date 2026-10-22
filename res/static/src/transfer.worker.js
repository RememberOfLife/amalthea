import '../vendor/sha256.js';
const sha256 = self.sha256;
import { CHUNK, hex, checkRange, throttle } from './transfer-utils.js';

let controller;
let rate = 0;
self.onmessage = async ({ data }) => {
  if (data.type === 'pause') { controller?.abort(); return; }
  if (data.type === 'rate') { rate = data.rate; return; }
  controller = new AbortController();
  rate = data.rate || 0;
  try {
    if (data.type === 'hash') {
      const hash = sha256.create();
      for (let offset = 0; offset < data.file.size; offset += CHUNK) {
        controller.signal.throwIfAborted();
        hash.update(new Uint8Array(await data.file.slice(offset, offset + CHUNK).arrayBuffer()));
        postMessage({ type: 'progress', offset: Math.min(offset + CHUNK, data.file.size) });
      }
      postMessage({ type: 'hash', hash: hex(hash.digest()) });
    } else if (data.type === 'download') {
      await download(data);
    }
  } catch (error) {
    postMessage({ type: 'error', message: error.name === 'AbortError' ? 'Paused' : error.message });
  }
};

async function download({ directory, key, record }) {
  const root = await (await navigator.storage.getDirectory()).getDirectoryHandle(directory, { create: true });
  let handle;
  let journal;
  try {
    handle = await (await root.getFileHandle(`${key}.data`, { create: true })).createSyncAccessHandle();
    journal = await (await root.getFileHandle(`${key}.progress`, { create: true })).createSyncAccessHandle();
    const physicalSize = handle.getSize();
    let offset = 0;
    let checkpoint = Math.floor(journal.getSize() / 16) * 16;
    const entry = new Uint8Array(16);
    const view = new DataView(entry.buffer);
    while (checkpoint > 0) {
      journal.read(entry, { at: checkpoint - 16 });
      const value = view.getBigUint64(0, true);
      const inverse = view.getBigUint64(8, true);
      const candidate = Number(value);
      if ((value ^ inverse) === 0xffffffffffffffffn && candidate <= physicalSize && candidate <= record.size && (candidate % CHUNK === 0 || candidate === record.size)) {
        offset = candidate; break;
      }
      checkpoint -= 16;
    }
    // Data flush precedes the checkpoint. Drop any interrupted, uncommitted write.
    journal.truncate(checkpoint);
    handle.truncate(offset);
    if (offset > record.size) throw new Error('Local partial file is larger than the source; discard it and restart.');
    const estimate = await navigator.storage.estimate();
    if (estimate.quota != null && estimate.usage != null && record.size - offset > estimate.quota - estimate.usage) {
      throw new Error('Not enough browser storage for this file. Free browser storage or use Direct download.');
    }
    const etag = `"${record.sha256}"`;
    // Authenticate/check expiry even if the bytes were already complete before a restart.
    const head = await fetch(record.url, { method: 'HEAD', headers: { 'If-Match': etag }, cache: 'no-store', signal: controller.signal });
    if (!head.ok) throw new Error(`Download is unavailable (${head.status}); sign in or ask for a fresh link.`);
    if (head.headers.get('ETag') !== etag || Number(head.headers.get('Content-Length')) !== record.size) throw new Error('Source file changed; discard this partial download.');
    while (offset < record.size) {
      controller.signal.throwIfAborted();
      const started = performance.now();
      const end = Math.min(offset + CHUNK, record.size) - 1;
      const response = await fetch(record.url, {
        headers: { Range: `bytes=${offset}-${end}`, 'If-Match': etag },
        signal: controller.signal, cache: 'no-store',
      });
      if (!response.ok) throw new Error(`Download interrupted (${response.status}). Sign in again if needed, then resume.`);
      checkRange(response, offset, end, record.size, etag);
      const bytes = new Uint8Array(await response.arrayBuffer());
      if (bytes.length !== end - offset + 1) throw new Error('Incomplete response; resume to retry this chunk.');
      let written = 0;
      while (written < bytes.length) {
        const count = handle.write(bytes.subarray(written), { at: offset + written });
        if (!count) throw new Error('Browser storage is full. Free space and resume.');
        written += count;
      }
      handle.flush();
      offset += bytes.length;
      view.setBigUint64(0, BigInt(offset), true);
      view.setBigUint64(8, BigInt(offset) ^ 0xffffffffffffffffn, true);
      let committed = 0;
      while (committed < entry.length) {
        const count = journal.write(entry.subarray(committed), { at: checkpoint + committed });
        if (!count) throw new Error('Cannot save transfer checkpoint; free browser storage and resume.');
        committed += count;
      }
      journal.flush();
      checkpoint += 16;
      postMessage({ type: 'progress', offset });
      await throttle(started, bytes.length, rate, controller.signal);
    }
    postMessage({ type: 'verifying', offset });
    const hasher = sha256.create();
    const buffer = new Uint8Array(CHUNK);
    for (let cursor = 0; cursor < record.size;) {
      controller.signal.throwIfAborted();
      const count = handle.read(buffer.subarray(0, Math.min(CHUNK, record.size - cursor)), { at: cursor });
      if (!count) throw new Error('Local file is incomplete.');
      hasher.update(buffer.subarray(0, count));
      cursor += count;
      // Yield so pause/rate messages are processed during long verification.
      await new Promise(resolve => setTimeout(resolve, 0));
    }
    if (hex(hasher.digest()) !== record.sha256) throw new Error('Downloaded checksum does not match. Discard this download and retry.');
    handle.close();
    handle = null;
    journal.close();
    journal = null;
    postMessage({ type: 'complete', offset });
  } finally {
    handle?.close();
    journal?.close();
  }
}
