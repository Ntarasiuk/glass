use objc2_core_media::{CMClock, CMTime};

/// Microseconds on the host time clock (mach absolute time), the same clock
/// ScreenCaptureKit stamps sample buffers with.
pub fn now_us() -> u64 {
    cmtime_us(unsafe { CMClock::host_time_clock().time() })
}

pub fn cmtime_us(t: CMTime) -> u64 {
    if t.timescale <= 0 {
        return 0;
    }
    (t.value as i128 * 1_000_000 / t.timescale as i128) as u64
}
