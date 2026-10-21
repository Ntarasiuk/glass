// One display's video: fragment reassembly, loss detection, WebCodecs decode and drawing.
// The worker owns one per streamed display.

import { fletcher16, FLAG_KEYFRAME, VIDEO_HEADER_LEN } from './proto.js';

const KEYFRAME_RETRY_MS = 250;

// Safari's hardware decoder occasionally rejects valid frames (checksums match). If a
// configuration fails repeatedly, every stream steps down to the next one.
const DECODER_LADDER = [
  { name: 'hw-lowlatency', optimizeForLatency: true, hardwareAcceleration: 'prefer-hardware' },
  { name: 'hw', optimizeForLatency: false, hardwareAcceleration: 'prefer-hardware' },
  { name: 'sw', optimizeForLatency: true, hardwareAcceleration: 'prefer-software' },
  // No hint: whatever the browser has (e.g. Chromium on Linux without VA-API).
  { name: 'auto', optimizeForLatency: true },
];
const LADDER_WINDOW_MS = 120_000;
const LADDER_FAILURES = 2;
const ladder = { step: 0, failures: [] };

/**
 * Skips configurations this browser can't do at all. "prefer-hardware" fails outright
 * where there's no hardware decoder (common on Linux), rather than falling back.
 */
export async function probeDecoders(log, codec = 'avc1.640033') {
  const supported = [];
  for (const config of DECODER_LADDER) {
    const { supported: ok } = await VideoDecoder.isConfigSupported({ codec, ...config }).catch(() => ({}));
    if (ok) supported.push(config.name);
  }
  const first = DECODER_LADDER.findIndex((c) => supported.includes(c.name));
  if (first > ladder.step) ladder.step = first;
  log(first < 0 ? 'error' : 'info', 'decoder support', { supported, using: DECODER_LADDER[ladder.step]?.name });
}

/** Restores a fallback found in an earlier session (remembered per device by main.js). */
export function setDecoderStep(step) {
  ladder.step = Math.max(0, Math.min(step | 0, DECODER_LADDER.length - 1));
}

export class VideoStream {
  /**
   * @param index   stream number (matches the host's)
   * @param env     { log, post, requestKeyframe(index), hostNowUs(), stats }
   */
  constructor(index, env) {
    this.index = index;
    this.env = env;
    this.ctx = undefined;
    this.pending = new Map(); // frameId -> { parts, received, keyframe, captureUs, checksum }
    this.reset();
    this.decoder = undefined;
    this.codec = undefined;
    this.currentSps = undefined;
    this.lastChunk = undefined;
    this.sinceKeyframe = 0;
    this.lastKeyframeRequest = 0;
  }

  setCanvas(canvas) {
    this.ctx = canvas.getContext('2d', { alpha: false, desynchronized: true });
  }

  /** New session: frame ids restart, so forget reassembly state. */
  reset() {
    this.pending.clear();
    this.lastDecodedId = undefined;
    this.waitingForKeyframe = true;
  }

  /** Gives up the hardware decoder (hidden tab or stream no longer shown). */
  release() {
    try { this.decoder?.close(); } catch {}
    this.decoder = undefined;
    this.currentSps = undefined;
    this.reset();
  }

  onFragment(h, bytes) {
    this.env.stats.bytes += bytes.byteLength;
    let frame = this.pending.get(h.frameId);
    if (!frame) {
      frame = {
        parts: new Array(h.fragCount),
        received: 0,
        keyframe: (h.flags & FLAG_KEYFRAME) !== 0,
        captureUs: h.captureUs,
        checksum: h.checksum,
      };
      this.pending.set(h.frameId, frame);
    }
    if (frame.parts[h.fragIdx]) return;
    // Copy, don't view: the browser may reuse the datagram's buffer for later datagrams.
    frame.parts[h.fragIdx] = bytes.slice(VIDEO_HEADER_LEN);
    if (++frame.received === h.fragCount) this.onFrame(h.frameId, frame);
  }

  onFrame(id, frame) {
    // Anything older and still incomplete is lost.
    for (const key of this.pending.keys()) if (key <= id) this.pending.delete(key);

    const contiguous = this.lastDecodedId !== undefined && id === ((this.lastDecodedId + 1) >>> 0);
    if (!frame.keyframe && (this.waitingForKeyframe || !contiguous)) {
      this.env.stats.dropped++;
      this.requestKeyframe();
      return;
    }

    const data = join(frame.parts);
    if (fletcher16(data) !== frame.checksum) {
      // Corrupted in transit or reassembly: never feed it to the decoder.
      this.env.log('error', 'frame checksum mismatch', {
        stream: this.index, id, size: data.byteLength, parts: frame.parts.length, keyframe: frame.keyframe,
      });
      this.env.stats.dropped++;
      this.requestKeyframe();
      return;
    }
    if (frame.keyframe && !this.ensureDecoder(data)) return;
    if (!this.decoder) return;
    this.waitingForKeyframe = false;
    this.lastDecodedId = id;
    this.lastChunk = { type: frame.keyframe ? 'key' : 'delta', size: data.byteLength, id, nals: nalTypes(data) };
    this.sinceKeyframe = frame.keyframe ? 0 : this.sinceKeyframe + 1;
    this.decoder.decode(new EncodedVideoChunk({ type: frame.keyframe ? 'key' : 'delta', timestamp: frame.captureUs, data }));
  }

  requestKeyframe() {
    this.waitingForKeyframe = true;
    const now = performance.now();
    if (now - this.lastKeyframeRequest < KEYFRAME_RETRY_MS) return;
    this.lastKeyframeRequest = now;
    this.env.requestKeyframe(this.index);
  }

  ensureDecoder(annexB) {
    const sps = findNal(annexB, 7);
    if (!sps) return false;
    const hex = (b) => b.toString(16).padStart(2, '0');
    // A new SPS (e.g. after a display switch changes resolution) gets a fresh decoder.
    const spsKey = Array.from(sps, hex).join('');
    if (this.decoder && this.decoder.state === 'configured' && spsKey === this.currentSps) return true;
    this.currentSps = spsKey;

    try { this.decoder?.close(); } catch {}
    this.codec = `avc1.${hex(sps[1])}${hex(sps[2])}${hex(sps[3])}`;
    this.decoder = new VideoDecoder({ output: (f) => this.draw(f), error: (e) => this.onDecodeError(e) });
    this.decoder.configure({ codec: this.codec, ...DECODER_LADDER[ladder.step] });
    this.env.post({ type: 'codec', stream: this.index, codec: `${this.codec} ${DECODER_LADDER[ladder.step].name}` });
    return true;
  }

  onDecodeError(e) {
    const now = performance.now();
    ladder.failures = ladder.failures.filter((t) => now - t < LADDER_WINDOW_MS);
    ladder.failures.push(now);
    if (ladder.failures.length >= LADDER_FAILURES) this.stepDownDecoder();
    this.env.post({ type: 'status', level: 'error', text: `decoder error: ${e.message}` });
    this.env.log('error', 'decoder error detail', {
      stream: this.index, name: e.name, lastChunk: this.lastChunk, sinceKeyframe: this.sinceKeyframe,
      queue: this.decoder?.decodeQueueSize, codec: this.codec,
      config: DECODER_LADDER[ladder.step].name, failuresInWindow: ladder.failures.length,
    });
    this.currentSps = undefined;
    this.decoder = undefined;
    this.requestKeyframe();
  }

  async stepDownDecoder() {
    for (let next = ladder.step + 1; next < DECODER_LADDER.length; next++) {
      const config = { codec: this.codec, ...DECODER_LADDER[next] };
      const { supported } = await VideoDecoder.isConfigSupported(config).catch(() => ({}));
      if (supported) {
        this.env.log('warn', 'decoder fallback', { from: DECODER_LADDER[ladder.step].name, to: DECODER_LADDER[next].name });
        ladder.step = next;
        ladder.failures = [];
        this.env.post({ type: 'decoderStep', step: next });
        // Rebuild with the new config at the next keyframe.
        this.currentSps = undefined;
        this.requestKeyframe();
        return;
      }
      this.env.log('info', `decoder config ${DECODER_LADDER[next].name} unsupported`);
    }
  }

  draw(videoFrame) {
    const ctx = this.ctx;
    if (ctx) {
      const { displayWidth: w, displayHeight: h } = videoFrame;
      if (ctx.canvas.width !== w || ctx.canvas.height !== h) {
        ctx.canvas.width = w;
        ctx.canvas.height = h;
        this.env.post({ type: 'size', stream: this.index, width: w, height: h });
      }
      ctx.drawImage(videoFrame, 0, 0);
    }
    const latencyMs = (this.env.hostNowUs() - videoFrame.timestamp) / 1000;
    videoFrame.close();

    const stats = this.env.stats;
    stats.frames++;
    stats.latencySum += latencyMs;
    stats.latencyMax = Math.max(stats.latencyMax, latencyMs);
  }
}

function join(parts) {
  let len = 0;
  for (const p of parts) len += p.byteLength;
  const out = new Uint8Array(len);
  let off = 0;
  for (const p of parts) {
    out.set(p, off);
    off += p.byteLength;
  }
  return out;
}

/** NAL unit types in an Annex B buffer, e.g. "7,8,5" or "1,1,1" (multi-slice). */
function nalTypes(buf) {
  const types = [];
  for (let i = 0; i + 4 < buf.length; i++) {
    if (buf[i] === 0 && buf[i + 1] === 0 && buf[i + 2] === 0 && buf[i + 3] === 1) {
      types.push(buf[i + 4] & 0x1f);
      i += 4;
    }
  }
  return types.join(',');
}

/** Returns the first NAL unit of `type` (header byte included) in an Annex B buffer. */
function findNal(buf, type) {
  for (let i = 0; i + 4 < buf.length; i++) {
    if (buf[i] === 0 && buf[i + 1] === 0 && buf[i + 2] === 0 && buf[i + 3] === 1) {
      if ((buf[i + 4] & 0x1f) === type) {
        let end = i + 4;
        while (end + 3 < buf.length && !(buf[end] === 0 && buf[end + 1] === 0 && buf[end + 2] === 0 && buf[end + 3] === 1)) end++;
        return buf.subarray(i + 4, end + 3 < buf.length ? end : buf.length);
      }
    }
  }
  return undefined;
}
