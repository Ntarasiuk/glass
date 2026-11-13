//! A thin C interface over GStreamer for the Glass host's GPU encoders (VA-API, NVENC).
//!
//! The host opens this library with dlopen at run time instead of linking GStreamer, so it
//! starts on machines without GStreamer and can say what's missing; the encoder logic
//! (pipeline descriptions, frame layout, bitrate policy) stays in the host. Bump
//! `GLASS_GST_ABI` whenever a signature here changes: the host refuses other versions.
//!
//! A panic can't unwind into the host (it would abort it, and every viewer with it), so
//! each function catches its own and returns its error value.
#![cfg(target_os = "linux")]
// A C interface: the host passes the pointers these functions were given out.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc};

pub const GLASS_GST_ABI: u32 = 1;

pub struct Pipeline {
    pipeline: gst::Pipeline,
    src: AppSrc,
    sink: AppSink,
}

/// A frame being filled by the host before it's pushed.
pub struct Buffer {
    map: gst::MappedBuffer<gst::buffer::Writable>,
}

/// Runs `f`, or gives `fallback` if it panics.
fn guard<T>(fallback: T, f: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(fallback)
}

/// Copies `message` into `out` (`len` bytes, always NUL-terminated).
fn write_error(out: *mut c_char, len: usize, message: &str) {
    if out.is_null() || len == 0 {
        return;
    }
    let n = message.len().min(len - 1);
    // SAFETY: the caller hands a writable buffer of `len` bytes.
    unsafe {
        ptr::copy_nonoverlapping(message.as_ptr(), out.cast::<u8>(), n);
        *out.add(n) = 0;
    }
}

fn str_arg<'a>(s: *const c_char) -> Option<&'a str> {
    if s.is_null() {
        return None;
    }
    // SAFETY: the caller passes a NUL-terminated string.
    unsafe { CStr::from_ptr(s) }.to_str().ok()
}

/// The interface version this library implements.
#[unsafe(no_mangle)]
pub extern "C" fn glass_gst_abi() -> u32 {
    GLASS_GST_ABI
}

/// Initializes GStreamer; 0 on success, otherwise the reason in `err`.
#[unsafe(no_mangle)]
pub extern "C" fn glass_gst_init(err: *mut c_char, err_len: usize) -> i32 {
    guard(-1, || match gst::init() {
        Ok(()) => 0,
        Err(e) => {
            write_error(err, err_len, &e.to_string());
            -1
        }
    })
}

/// GStreamer's version, e.g. "GStreamer 1.24.2", into `out`.
#[unsafe(no_mangle)]
pub extern "C" fn glass_gst_version(out: *mut c_char, len: usize) {
    guard((), || write_error(out, len, &gst::version_string()));
}

/// Whether an element factory with this name is registered (GStreamer registers the VA
/// and NVENC encoders only for devices it could open).
#[unsafe(no_mangle)]
pub extern "C" fn glass_gst_has_element(name: *const c_char) -> bool {
    guard(false, || str_arg(name).is_some_and(|n| gst::ElementFactory::find(n).is_some()))
}

/// Builds a pipeline from a gst-launch description with an appsrc named `src` and an
/// appsink named `sink`, and starts it. Null on failure, with the reason in `err`.
#[unsafe(no_mangle)]
pub extern "C" fn glass_gst_pipeline_new(
    description: *const c_char,
    err: *mut c_char,
    err_len: usize,
) -> *mut Pipeline {
    let build = || -> Result<Pipeline, String> {
        let description = str_arg(description).ok_or("bad description")?;
        let pipeline = gst::parse::launch(description)
            .map_err(|e| e.to_string())?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "not a pipeline")?;
        let src = pipeline
            .by_name("src")
            .and_then(|e| e.downcast::<AppSrc>().ok())
            .ok_or("no appsrc named src")?;
        let sink = pipeline
            .by_name("sink")
            .and_then(|e| e.downcast::<AppSink>().ok())
            .ok_or("no appsink named sink")?;
        let this = Pipeline { pipeline, src, sink };
        if this.pipeline.set_state(gst::State::Playing).is_err() {
            let detail = bus_error(&this).unwrap_or_else(|| "can't start".into());
            let _ = this.pipeline.set_state(gst::State::Null);
            return Err(detail);
        }
        Ok(this)
    };
    guard(ptr::null_mut(), || match build() {
        Ok(p) => Box::into_raw(Box::new(p)),
        Err(e) => {
            write_error(err, err_len, &e);
            ptr::null_mut()
        }
    })
}

/// Stops and frees a pipeline.
#[unsafe(no_mangle)]
pub extern "C" fn glass_gst_pipeline_free(p: *mut Pipeline) {
    if p.is_null() {
        return;
    }
    // SAFETY: `p` came from glass_gst_pipeline_new and is freed once.
    let p = unsafe { Box::from_raw(p) };
    guard((), move || {
        let _ = p.pipeline.set_state(gst::State::Null);
    });
}

fn pipeline<'a>(p: *mut Pipeline) -> Option<&'a Pipeline> {
    // SAFETY: the host passes a live pipeline from glass_gst_pipeline_new.
    unsafe { p.as_ref() }
}

/// Sets an unsigned integer property (e.g. an encoder's `bitrate`) on the element with
/// this name, within the property's range. 0 on success; -1 if there's no such writable
/// guint property (set_property panics on another type or an out-of-range value).
#[unsafe(no_mangle)]
pub extern "C" fn glass_gst_set_uint(
    p: *mut Pipeline,
    element: *const c_char,
    property: *const c_char,
    value: u32,
) -> i32 {
    guard(-1, || {
        let (Some(p), Some(element), Some(property)) = (pipeline(p), str_arg(element), str_arg(property))
        else {
            return -1;
        };
        let Some(element) = p.pipeline.by_name(element) else {
            return -1;
        };
        let Some(spec) = element.find_property(property) else {
            return -1;
        };
        let Some(spec) = spec.downcast_ref::<gst::glib::ParamSpecUInt>() else {
            return -1;
        };
        if !spec.flags().contains(gst::glib::ParamFlags::WRITABLE) {
            return -1;
        }
        element.set_property(property, value.clamp(spec.minimum(), spec.maximum()));
        0
    })
}

/// Reads an unsigned integer property; 0 if there's none (or it isn't a guint).
#[unsafe(no_mangle)]
pub extern "C" fn glass_gst_get_uint(
    p: *mut Pipeline,
    element: *const c_char,
    property: *const c_char,
) -> u32 {
    guard(0, || {
        let (Some(p), Some(element), Some(property)) = (pipeline(p), str_arg(element), str_arg(property))
        else {
            return 0;
        };
        p.pipeline
            .by_name(element)
            .filter(|e| e.find_property(property).is_some())
            .and_then(|e| e.property_value(property).get::<u32>().ok())
            .unwrap_or(0)
    })
}

/// Asks the encoder for a keyframe with SPS/PPS on the next frame (force-key-unit).
#[unsafe(no_mangle)]
pub extern "C" fn glass_gst_force_keyframe(p: *mut Pipeline) -> i32 {
    guard(-1, || {
        let Some(p) = pipeline(p) else { return -1 };
        let event = gstreamer_video::DownstreamForceKeyUnitEvent::builder()
            .all_headers(true)
            .build();
        if p.src.send_event(event) { 0 } else { -1 }
    })
}

/// A writable buffer of `size` bytes with these timestamps (nanoseconds), for the host
/// to fill (glass_gst_buffer_data) and push (glass_gst_push). Null on failure.
#[unsafe(no_mangle)]
pub extern "C" fn glass_gst_buffer_new(size: usize, pts_ns: u64, duration_ns: u64) -> *mut Buffer {
    guard(ptr::null_mut(), || {
        let Ok(mut buffer) = gst::Buffer::with_size(size) else {
            return ptr::null_mut();
        };
        {
            let Some(b) = buffer.get_mut() else {
                return ptr::null_mut();
            };
            b.set_pts(gst::ClockTime::from_nseconds(pts_ns));
            b.set_duration(gst::ClockTime::from_nseconds(duration_ns));
        }
        match buffer.into_mapped_buffer_writable() {
            Ok(map) => Box::into_raw(Box::new(Buffer { map })),
            Err(_) => ptr::null_mut(),
        }
    })
}

/// The buffer's bytes (as many as asked for in glass_gst_buffer_new).
#[unsafe(no_mangle)]
pub extern "C" fn glass_gst_buffer_data(b: *mut Buffer) -> *mut u8 {
    // SAFETY: `b` is a live buffer from glass_gst_buffer_new.
    guard(ptr::null_mut(), || match unsafe { b.as_mut() } {
        Some(b) => b.map.as_mut_slice().as_mut_ptr(),
        None => ptr::null_mut(),
    })
}

/// Frees a buffer that won't be pushed.
#[unsafe(no_mangle)]
pub extern "C" fn glass_gst_buffer_free(b: *mut Buffer) {
    if !b.is_null() {
        // SAFETY: `b` came from glass_gst_buffer_new and is freed once.
        let b = unsafe { Box::from_raw(b) };
        guard((), move || drop(b));
    }
}

/// Pushes a filled buffer into the pipeline (taking it). 0 on success.
#[unsafe(no_mangle)]
pub extern "C" fn glass_gst_push(p: *mut Pipeline, b: *mut Buffer) -> i32 {
    if b.is_null() {
        return -1;
    }
    // SAFETY: `b` came from glass_gst_buffer_new; pushing takes it.
    let b = unsafe { Box::from_raw(b) };
    guard(-1, move || {
        let Some(p) = pipeline(p) else { return -1 };
        match p.src.push_buffer(b.map.into_buffer()) {
            Ok(_) => 0,
            Err(_) => -1,
        }
    })
}

/// Called with each encoded frame: the bytes and whether it's a keyframe.
pub type OutputFn = extern "C" fn(ctx: *mut c_void, data: *const u8, len: usize, keyframe: bool);

/// Waits up to `timeout_ns` for the next encoded frame and hands it to `out`. 1 if a
/// frame came, 0 on timeout, -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn glass_gst_pull(
    p: *mut Pipeline,
    timeout_ns: u64,
    out: OutputFn,
    ctx: *mut c_void,
) -> i32 {
    guard(-1, || {
        let Some(p) = pipeline(p) else { return -1 };
        let Some(sample) = p.sink.try_pull_sample(gst::ClockTime::from_nseconds(timeout_ns)) else {
            return 0;
        };
        let Some(buffer) = sample.buffer() else { return -1 };
        let keyframe = !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT);
        let Ok(map) = buffer.map_readable() else { return -1 };
        out(ctx, map.as_ptr(), map.len(), keyframe);
        1
    })
}

fn bus_error(p: &Pipeline) -> Option<String> {
    let bus = p.pipeline.bus()?;
    let mut first = None;
    // Empties the bus (nothing else reads it, so tags and other messages would pile up).
    while let Some(message) = bus.pop() {
        if let gst::MessageView::Error(e) = message.view()
            && first.is_none()
        {
            first = Some(format!("{} ({:?})", e.error(), e.debug()));
        }
    }
    first
}

/// Empties the pipeline's bus; 1 with the first error in `err` if there was one.
#[unsafe(no_mangle)]
pub extern "C" fn glass_gst_bus_error(p: *mut Pipeline, err: *mut c_char, err_len: usize) -> i32 {
    guard(-1, || {
        let Some(p) = pipeline(p) else { return -1 };
        match bus_error(p) {
            Some(e) => {
                write_error(err, err_len, &e);
                1
            }
            None => 0,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Properties that aren't writable guints are refused, not set (which would panic),
    /// and values outside the range are clamped.
    #[test]
    fn uint_properties_never_panic() {
        assert_eq!(glass_gst_init(ptr::null_mut(), 0), 0);
        if gst::ElementFactory::find("appsrc").is_none() || gst::ElementFactory::find("appsink").is_none() {
            eprintln!("no app plugin here; skipping");
            return;
        }
        let mut err = [0 as c_char; 256];
        let p = glass_gst_pipeline_new(c"appsrc name=src ! appsink name=sink".as_ptr(), err.as_mut_ptr(), err.len());
        assert!(!p.is_null());
        let src = c"src".as_ptr();
        let set = |name: &CStr, v| glass_gst_set_uint(p, src, name.as_ptr(), v);
        // min-percent is a guint from 0 to 100.
        assert_eq!(set(c"min-percent", 1000), 0);
        assert_eq!(glass_gst_get_uint(p, src, c"min-percent".as_ptr()), 100);
        // A guint64, a boolean, nothing at all.
        assert_eq!(set(c"max-buffers", 1), -1);
        assert_eq!(set(c"is-live", 1), -1);
        assert_eq!(set(c"no-such-property", 1), -1);
        assert_eq!(glass_gst_get_uint(p, src, c"is-live".as_ptr()), 0);
        assert_eq!(glass_gst_set_uint(p, c"nothing".as_ptr(), c"min-percent".as_ptr(), 1), -1);
        glass_gst_pipeline_free(p);
    }
}
