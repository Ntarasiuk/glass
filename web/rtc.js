// Remote access over WebRTC (glassdesk.dev/app/#r=<host key>). RTCPeerConnection exists
// only on the main thread, so the connection lives here and is relayed to the worker over
// a MessagePort; the worker sees a WebTransport-shaped object (rtc-transport.js).
//
// The introducer (glassdesk.dev) only passes the offer and answer. The host signs its
// answer together with our offer, and we check that against the key in the link, so a
// compromised introducer can't put anyone else in the middle: DTLS then pins the host's
// certificate fingerprint from that signed answer.

const STUN = 'stun:stun.cloudflare.com:3478';
const CHUNK = 16 * 1024;
const BUFFER_HIGH = 1024 * 1024;
const BUFFER_LOW = 256 * 1024;
const GATHER_TIMEOUT_MS = 3000;
const CONNECT_TIMEOUT_MS = 20_000;

export function toB64url(bytes) {
  let s = '';
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

export function fromB64url(text) {
  return Uint8Array.from(atob(text.replace(/-/g, '+').replace(/_/g, '/')), (c) => c.charCodeAt(0));
}

/** The introducer's id for a host: the first 16 bytes of SHA-256 of its public key. */
export async function hostIdFor(hostKey) {
  const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', fromB64url(hostKey)));
  return toB64url(digest.slice(0, 16));
}

/** Displays a connection can carry as video tracks (the host's MAX_STREAMS). */
const VIDEO_TRACKS = 4;

/**
 * Whether to ask for video as RTP tracks: needs H.264 (packetization-mode 1) on the
 * receiving side, and can be switched off (`glass.rtp` = 0) if tracks failed before.
 */
function wantRtpVideo() {
  try { if (sessionStorage.getItem('glass.rtp') === '0') return false; } catch {}
  const codecs = RTCRtpReceiver.getCapabilities?.('video')?.codecs ?? [];
  return codecs.some((c) => /h264/i.test(c.mimeType) && /packetization-mode=1/.test(c.sdpFmtpLine ?? ''));
}

/** H.264 first, High profile before Baseline (the host sends High), then its RTX. */
function codecPreferences() {
  const codecs = RTCRtpReceiver.getCapabilities('video').codecs;
  const profile = (c) => (/profile-level-id=(\w{4})/i.exec(c.sdpFmtpLine ?? '')?.[1] ?? '').toLowerCase();
  const rank = (c) => ['6400', '640c', '4d00', '42e0', '4200'].indexOf(profile(c)) >>> 0;
  const h264 = codecs
    .filter((c) => /h264/i.test(c.mimeType) && /packetization-mode=1/.test(c.sdpFmtpLine ?? ''))
    .sort((a, b) => rank(a) - rank(b));
  const rtx = codecs.filter((c) => /rtx/i.test(c.mimeType));
  return [...h264, ...rtx];
}

/**
 * Serves the worker's RtcTransport over `port`, connecting on request. Video arrives as
 * RTP tracks when both sides support it: `onVideo(stream, track)` for each, and
 * `onMode(rtp)` once the answer is in (false: video comes over the data channel).
 */
export function serveRtc({ port, hostKey, signalUrl, log, onVideo = () => {}, onMode = () => {} }) {
  // Each connection attempt has a generation (from the worker); everything posted carries
  // it, so a late message from an old connection can't reach a new one.
  let session; // { gen, pc, channels: Map<id, channel>, senders, dgram }
  const api = {};
  let nextIncoming = -1;

  const post = (gen, msg, transfer) => port.postMessage({ ...msg, gen }, transfer ?? []);

  /** `notify`: tell the worker (only when the connection dropped by itself). */
  function teardown(reason, notify) {
    if (!session) return;
    const s = session;
    session = undefined;
    for (const ch of s.channels.values()) try { ch.close(); } catch {}
    try { s.pc.close(); } catch {}
    if (notify) post(s.gen, { t: 'closed', reason });
  }

  function wireChannel(gen, id, ch) {
    ch.binaryType = 'arraybuffer';
    ch.bufferedAmountLowThreshold = BUFFER_LOW;
    ch.onmessage = ({ data }) => {
      if (typeof data === 'string') {
        if (data === 'end') post(gen, { t: 'end', id });
        return;
      }
      post(gen, { t: 'data', id, data }, [data]);
    };
    ch.onclose = () => post(gen, { t: 'end', id });
  }

  /** Sends on a channel once it's open, acking bytes back to the worker as they drain. */
  function sender(gen, id, ch) {
    const queue = [];
    let unacked = 0;
    const flush = () => {
      while (queue.length && ch.readyState === 'open' && ch.bufferedAmount < BUFFER_HIGH) {
        const item = queue.shift();
        if (item === 'end') ch.send('end');
        else ch.send(item);
      }
      if (ch.bufferedAmount < BUFFER_HIGH && unacked) {
        post(gen, { t: 'ack', id, bytes: unacked });
        unacked = 0;
      }
    };
    ch.addEventListener('open', flush);
    ch.addEventListener('bufferedamountlow', flush);
    return {
      data(bytes) {
        for (let i = 0; i < bytes.byteLength; i += CHUNK) queue.push(bytes.subarray(i, i + CHUNK));
        unacked += bytes.byteLength;
        flush();
      },
      end() { queue.push('end'); flush(); },
    };
  }

  async function connect(gen) {
    teardown('reconnecting', false);
    const ws = new WebSocket(signalUrl);
    const inbox = [];
    let wake;
    ws.onmessage = ({ data }) => { inbox.push(JSON.parse(data)); wake?.(); };
    const closed = new Promise((resolve) => { ws.onclose = ws.onerror = () => { resolve(); wake?.(); }; });
    const next = async (timeoutMs) => {
      const deadline = Date.now() + timeoutMs;
      while (!inbox.length) {
        if (ws.readyState >= WebSocket.CLOSING) throw new Error("couldn't reach glassdesk.dev");
        if (Date.now() > deadline) throw new Error('the computer didn’t answer');
        await Promise.race([new Promise((r) => { wake = r; }), closed, sleep(deadline - Date.now())]);
      }
      return inbox.shift();
    };
    try {
      const hello = await next(10_000);
      if (hello.t !== 'hello') throw new Error('unexpected reply from glassdesk.dev');
      if (!hello.online) throw new Error('This computer is offline, or remote access is turned off on it.');
      const pc = new RTCPeerConnection({ iceServers: [{ urls: STUN }, ...(hello.iceServers ?? [])] });
      session = { gen, pc, channels: new Map(), senders: new Map() };
      const s = session;
      const dgram = pc.createDataChannel('d', { ordered: false, maxRetransmits: 0 });
      dgram.binaryType = 'arraybuffer';
      dgram.onmessage = ({ data }) => { if (typeof data !== 'string') post(gen, { t: 'dgram', data }, [data]); };
      s.dgram = dgram;
      // Video as RTP tracks: one recvonly transceiver per possible display, in stream order.
      const transceivers = [];
      if (wantRtpVideo()) {
        const prefs = codecPreferences();
        for (let i = 0; i < VIDEO_TRACKS; i++) {
          const t = pc.addTransceiver('video', { direction: 'recvonly' });
          try { t.setCodecPreferences(prefs); } catch {}
          transceivers.push(t);
        }
      }
      pc.ontrack = (e) => {
        const i = transceivers.indexOf(e.transceiver);
        // A video m-line the host left unbound fires without a stream: ignore it.
        if (i < 0 || !e.streams.length) return;
        // Show frames as they arrive: a remote desktop wants latency, not smoothing.
        try {
          if ('jitterBufferTarget' in e.receiver) e.receiver.jitterBufferTarget = 0;
          else e.receiver.playoutDelayHint = 0;
        } catch {}
        onVideo(i, e.track);
      };
      pc.ondatachannel = ({ channel }) => {
        const id = nextIncoming--;
        s.channels.set(id, channel);
        wireChannel(gen, id, channel);
        post(gen, { t: 'incoming', id });
      };
      pc.onconnectionstatechange = () => {
        if (['failed', 'closed'].includes(pc.connectionState) && session === s) teardown(`connection ${pc.connectionState}`, true);
      };

      const t0 = performance.now();
      await pc.setLocalDescription(await pc.createOffer());
      await gathered(pc);
      const tGathered = performance.now();
      const offer = pc.localDescription.sdp;
      ws.send(JSON.stringify({ t: 'offer', sdp: offer }));
      const reply = await next(15_000);
      const tAnswered = performance.now();
      if (reply.t === 'error') throw new Error(reply.msg);
      if (reply.t !== 'answer') throw new Error('unexpected reply from glassdesk.dev');
      if (!(await verifyAnswer(hostKey, offer, reply.sdp, reply.sig))) {
        throw new Error("Couldn't verify this computer's identity; not connecting.");
      }
      await pc.setRemoteDescription({ type: 'answer', sdp: reply.sdp });
      onMode(transceivers.some((t) => t.currentDirection === 'recvonly'));
      ws.close();
      await opened(pc, dgram);
      log('info', 'remote: connected', {
        relay: await usesRelay(pc),
        gatherMs: Math.round(tGathered - t0),
        answerMs: Math.round(tAnswered - tGathered),
        connectMs: Math.round(performance.now() - tAnswered),
      });
      post(gen, { t: 'open' });
    } catch (e) {
      try { ws.close(); } catch {}
      if (session?.gen === gen) teardown('failed', false);
      post(gen, { t: 'failed', msg: e.message ?? String(e) });
    }
  }

  port.onmessage = ({ data: m }) => {
    if (m.t === 'connect') { connect(m.gen); return; }
    const s = session;
    if (!s || m.gen !== s.gen) return;
    if (m.t === 'close') { teardown('closed by client', false); return; }
    if (m.t === 'dgram') {
      if (s.dgram.readyState === 'open' && s.dgram.bufferedAmount < BUFFER_LOW) s.dgram.send(m.data);
    } else if (m.t === 'open-stream') {
      const ch = s.pc.createDataChannel(m.kind, { ordered: true });
      s.channels.set(m.id, ch);
      wireChannel(s.gen, m.id, ch);
      s.senders.set(m.id, sender(s.gen, m.id, ch));
    } else if (m.t === 'data') {
      senderFor(s, m.id)?.data(new Uint8Array(m.data));
    } else if (m.t === 'end') {
      senderFor(s, m.id)?.end();
    }
  };

  /** The live connection's stats (for the HUD), if connected. */
  api.stats = () => session?.pc.getStats();

  function senderFor(s, id) {
    let snd = s.senders.get(id);
    if (!snd && s.channels.has(id)) s.senders.set(id, (snd = sender(s.gen, id, s.channels.get(id))));
    return snd;
  }
  return api;
}

export async function verifyAnswer(hostKey, offerSdp, answerSdp, sig) {
  try {
    const key = await crypto.subtle.importKey('raw', fromB64url(hostKey), { name: 'ECDSA', namedCurve: 'P-256' }, false, ['verify']);
    const data = new TextEncoder().encode(`glass-rtc-v1\n${offerSdp}\n--\n${answerSdp}`);
    return await crypto.subtle.verify({ name: 'ECDSA', hash: 'SHA-256' }, key, fromB64url(sig), data);
  } catch {
    return false;
  }
}

function gathered(pc) {
  if (pc.iceGatheringState === 'complete') return Promise.resolve();
  return Promise.race([
    new Promise((resolve) => pc.addEventListener('icegatheringstatechange', () => { if (pc.iceGatheringState === 'complete') resolve(); })),
    sleep(GATHER_TIMEOUT_MS),
  ]);
}

function opened(pc, channel) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('Couldn’t make a direct connection to this computer.')), CONNECT_TIMEOUT_MS);
    const check = () => {
      if (channel.readyState === 'open') { clearTimeout(timer); resolve(); }
      else if (['failed', 'closed'].includes(pc.connectionState)) { clearTimeout(timer); reject(new Error('Couldn’t make a direct connection to this computer.')); }
    };
    channel.addEventListener('open', check);
    pc.addEventListener('connectionstatechange', check);
    check();
  });
}

/** Whether traffic goes through a TURN relay (for the logs: direct is the goal). */
async function usesRelay(pc) {
  try {
    const stats = await pc.getStats();
    for (const s of stats.values()) {
      if (s.type === 'candidate-pair' && s.nominated && s.state === 'succeeded') {
        return stats.get(s.localCandidateId)?.candidateType === 'relay' || stats.get(s.remoteCandidateId)?.candidateType === 'relay';
      }
    }
  } catch {}
  return undefined;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, Math.max(0, ms)));
