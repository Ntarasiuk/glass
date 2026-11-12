# Linux hardware encoding

The default Linux build uses OpenH264. Optional GStreamer builds add hardware
H.264 encoding while keeping portal/PipeWire capture and the existing viewers.

```sh
# NVIDIA, independently of VA-API:
cargo build --release -p glass-host --features nvenc
# Intel/AMD:
cargo build --release -p glass-host --features vaapi
# Both backends:
cargo build --release -p glass-host --features vaapi,nvenc
```

For the container build, use `GLASS_BUILD_FEATURES=nvenc scripts/linux/build.sh`.
These optional binaries require GStreamer libraries at startup. Standard release
artifacts remain software builds; building the feature does not install plugins
or change the running service.

Set the environment of the Glass service, then restart it when ready:

| `GLASS_ENCODER` | Behavior |
|---|---|
| unset / `software` | OpenH264 |
| `nvenc` | NVIDIA NVENC, then OpenH264 on failure; never tries VA-API |
| `vaapi` | VA-API, then OpenH264 on failure |
| `auto` | NVENC, VA-API, OpenH264, trying only compiled backends |

Failures are tracked per monitor. A failed backend is not retried for every frame;
changing source dimensions or restarting Glass permits another attempt. Fallback
immediately re-encodes the retained picture as a keyframe, even on a static screen.
Logs identify the selected encoder and explain failures. Invalid encoder names
produce a warning and use software.

## NVIDIA dependencies

Use GStreamer 1.24 or later with the `nvcodec` plugin's `nvh264enc` element,
`h264parse`, and the app/video libraries. The NVIDIA driver must expose CUDA and
NVENC to the service user. A GPU with NVENC hardware is required. No VA-API plugin
or driver is involved in explicit NVENC mode. Package names vary by distribution;
some builds of the GStreamer Bad Plug-ins package omit nvcodec.

Check `gst-inspect-1.0 nvh264enc` and `gst-inspect-1.0 h264parse` in the service's
environment. Factory discovery is only an installation check: successful encoding
still depends on driver/API compatibility, GPU permissions, and available sessions.
An unsupported plugin or failed session falls back to software.

## Performance

Both hardware backends currently use this path:

```text
PipeWire CPU RGB -> CPU scaling/I420 -> padded NV12 -> GPU upload/H.264
    -> Annex-B access unit -> existing transport/viewer
```

This shares conversion and upload costs with the existing VA-API implementation.
It is not a zero-copy path. Screens over the existing software pixel budget are
halved; a 3840x2160 source currently produces 1920x1080 output at scale 1.0.
Changing that policy or adding DMA-BUF capture requires separate performance work.

NVENC uses its fastest P1 preset, ultra-low-latency tuning, no B-frames/lookahead,
and repeated SPS/PPS on IDRs. One input is drained before the next is submitted;
obsolete raw pictures are replaced by the existing mailbox. Output timeout is
500 ms for failure recovery, not a latency target. Conversion remains BT.601
limited range. Bitrate cuts apply promptly; increases/small changes are coalesced
as with VA-API. NVENC VBV changes can cause reconfiguration and an extra IDR.

The ignored comparison test below uses the same synthetic frames, fps, scale,
and bitrate target for OpenH264 and NVENC. It reports whole encode-call latency
(conversion/upload/encode/output copy), percentiles, and output bitrate. It runs
without capture/display or real-time pacing and does not measure end-to-end
latency or establish equal visual quality. Compare quality separately before
drawing efficiency conclusions.

An initial run on 2026-11-13 used an RTX 2080 Ti, NVIDIA driver 595.112.07,
GStreamer 1.28.8, and Xeon E7-8895 v2 CPUs, with VA plugins excluded. Both
encoders used 60 fps timestamps, an 8 Mbit/s target, 125 synthetic frames (first
five excluded), and two requested IDRs. The host was shared, with no CPU/GPU
affinity or clock controls. These are observations from one run:

| Source → output | Encoder | Mean call | p95 call | Encoded Mbit/s at 60 fps |
|---|---|---:|---:|---:|
| 1920x1080 → 1920x1080 | OpenH264 | 40.94 ms | 46.47 ms | 1.46 |
| 1920x1080 → 1920x1080 | NVENC | 5.68 ms | 6.40 ms | 7.77 |
| 2560x1440 → 2560x1440 | OpenH264 | 80.12 ms | 92.68 ms | 3.29 |
| 2560x1440 → 2560x1440 | NVENC | 10.50 ms | 14.04 ms | 7.72 |
| 3840x2160 → 1920x1080 | OpenH264 | 45.49 ms | 51.84 ms | 1.63 |
| 3840x2160 → 1920x1080 | NVENC | 6.74 ms | 7.61 ms | 7.33 |

The actual output bitrates differ substantially; quality was not scored. There
was no usable VA-API encoder in this test, so there is no measured NVENC/VA-API
speed comparison. The two hardware paths share their CPU conversion code.

## Validation

GPU-less checks:

```sh
cargo test --profile ci --workspace
cargo test --profile ci -p glass-host --features vaapi
cargo test --profile ci -p glass-host --features nvenc
cargo test --profile ci -p glass-host --features vaapi,nvenc
```

Explicit NVIDIA tests (fail if NVENC cannot initialize; no software substitution):

```sh
cargo test --profile ci -p glass-host --features nvenc nvenc_roundtrip -- --ignored --nocapture
cargo test --profile ci -p glass-host --features nvenc nvenc_compare_software -- --ignored --nocapture
```

Roundtrip coverage includes single-frame output without EOS, forced IDRs and
standalone SPS/PPS decoding, bitrate changes, padded rows, RGBx/BGRx, odd sizes,
portrait frames, and scaling. Ordinary tests cover backend selection, per-monitor
failure isolation, and recovery using the retained picture.

Before claiming complete desktop support, also run a portal-to-browser session
as the service user with VA plugins absent: reconnect, loss recovery, resize,
multiple monitors, congestion, and both WebTransport/WebRTC. Encoder tests alone
do not validate capture permissions, input/audio, or viewer latency.
