// A WebTransport-shaped connection over WebRTC, for remote access. The peer connection
// itself lives on the main thread (rtc.js); this side speaks to it over a MessagePort and
// offers just what worker.js uses: datagrams, bidirectional and unidirectional streams,
// and incoming unidirectional streams.

const MAX_IN_FLIGHT = 1024 * 1024; // bytes per stream handed over but not yet drained
let generations = 0;

export class RtcTransport {
  constructor(port) {
    this.port = port;
    this.gen = ++generations;
    const send = (msg, transfer) => port.postMessage({ ...msg, gen: this.gen }, transfer ?? []);
    this.send = send;
    this.streams = new Map(); // id -> { controller (readable side), credit waiters }
    this.nextId = 1;
    let incoming;
    this.incomingUnidirectionalStreams = new ReadableStream({ start: (c) => { incoming = c; } });
    this.incoming = incoming;

    let dgramIn;
    const readable = new ReadableStream({ start: (c) => { dgramIn = c; } }, { highWaterMark: 256 });
    const writable = new WritableStream({
      write: (chunk) => {
        const copy = chunk.slice().buffer;
        send({ t: 'dgram', data: copy }, [copy]);
      },
    });
    this.datagrams = { readable, writable, createWritable: () => writable, maxDatagramSize: 1200 };
    this.dgramIn = dgramIn;

    this.ready = new Promise((resolve, reject) => { this.resolveReady = resolve; this.rejectReady = reject; });
    this.closed = new Promise((resolve) => { this.resolveClosed = resolve; });
    this.ready.catch(() => {});
    port.onmessage = ({ data }) => { if (data.gen === this.gen) this.onMessage(data); };
    send({ t: 'connect' });
  }

  onMessage(m) {
    switch (m.t) {
      case 'open':
        this.resolveReady();
        break;
      case 'failed':
        this.rejectReady(new Error(m.msg));
        this.end(m.msg);
        break;
      case 'closed':
        this.end(m.reason);
        break;
      case 'dgram':
        // A full queue means the reader is behind: drop, as the network would.
        if (this.dgramIn.desiredSize > 0) this.dgramIn.enqueue(new Uint8Array(m.data));
        break;
      case 'incoming': {
        const readable = this.readableFor(m.id);
        this.incoming.enqueue(readable);
        break;
      }
      case 'data':
        this.streams.get(m.id)?.controller?.enqueue(new Uint8Array(m.data));
        break;
      case 'end': {
        const s = this.streams.get(m.id);
        try { s?.controller?.close(); } catch {}
        break;
      }
      case 'ack': {
        const s = this.streams.get(m.id);
        if (!s) break;
        s.inFlight -= m.bytes;
        for (const wake of s.waiters.splice(0)) wake();
        break;
      }
    }
  }

  end(reason) {
    for (const s of this.streams.values()) {
      try { s.controller?.error(new Error(reason)); } catch {}
    }
    this.streams.clear();
    try { this.dgramIn.close(); } catch {}
    try { this.incoming.close(); } catch {}
    this.resolveClosed({ reason });
  }

  stream(id) {
    let s = this.streams.get(id);
    if (!s) this.streams.set(id, (s = { inFlight: 0, waiters: [] }));
    return s;
  }

  readableFor(id) {
    const s = this.stream(id);
    return new ReadableStream({ start: (c) => { s.controller = c; } });
  }

  writableFor(id) {
    const s = this.stream(id);
    return new WritableStream({
      write: async (chunk) => {
        while (s.inFlight > MAX_IN_FLIGHT) await new Promise((r) => s.waiters.push(r));
        const copy = chunk.slice().buffer;
        s.inFlight += copy.byteLength;
        this.send({ t: 'data', id, data: copy }, [copy]);
      },
      close: () => this.send({ t: 'end', id }),
    });
  }

  async createBidirectionalStream() {
    const id = this.nextId++;
    this.send({ t: 'open-stream', id, kind: 'bi' });
    return { readable: this.readableFor(id), writable: this.writableFor(id) };
  }

  async createUnidirectionalStream() {
    const id = this.nextId++;
    this.send({ t: 'open-stream', id, kind: 'uni' });
    return this.writableFor(id);
  }

  close() {
    this.send({ t: 'close' });
    this.end('closed');
  }
}
