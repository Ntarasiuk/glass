// Captures the microphone for the host: downmixes to mono, resamples to 48 kHz and posts
// 240-frame (5 ms) Int16 packets through this node's port, the size the host expects.

const TARGET_RATE = 48000;
const FRAMES_PER_PACKET = 240;

class GlassMic extends AudioWorkletProcessor {
  constructor() {
    super();
    this.step = sampleRate / TARGET_RATE; // input frames per output frame
    this.pos = 0; // fractional read position into `carry` + the current block
    this.carry = 0; // last input sample of the previous block, for interpolation
    this.packet = new Int16Array(FRAMES_PER_PACKET);
    this.filled = 0;
  }

  process(inputs) {
    const input = inputs[0];
    if (!input?.length || !input[0].length) return true;
    const n = input[0].length;
    const mono = new Float32Array(n);
    for (const ch of input) for (let i = 0; i < n; i++) mono[i] += ch[i] / input.length;
    // Linear interpolation; index -1 is the previous block's last sample.
    const at = (i) => (i < 0 ? this.carry : mono[i]);
    while (this.pos < n - 1) {
      const i = Math.floor(this.pos);
      const f = this.pos - i;
      const s = at(i) + (at(i + 1) - at(i)) * f;
      this.packet[this.filled++] = Math.max(-32768, Math.min(32767, Math.round(s * 32767)));
      if (this.filled === FRAMES_PER_PACKET) {
        this.port.postMessage(this.packet.buffer, [this.packet.buffer]);
        this.packet = new Int16Array(FRAMES_PER_PACKET);
        this.filled = 0;
      }
      this.pos += this.step;
    }
    this.pos -= n;
    this.carry = mono[n - 1];
    return true;
  }
}

registerProcessor('glass-mic', GlassMic);
