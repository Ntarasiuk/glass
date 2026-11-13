//! Loads libglass_gst.so (crates/glass-gst), the GStreamer side of the GPU encoders, at
//! run time. The host doesn't link GStreamer, so it starts without it; when GPU encoding
//! is asked for and something is missing, `diagnose` says what and how to install it.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{Result, anyhow};

const ABI: u32 = 1;
const FILE: &str = "libglass_gst.so";

/// What a hint is for.
#[derive(Clone, Copy)]
pub enum Need {
    Gstreamer,
    Va,
    Nvenc,
}

/// The install command for `need` on this distribution (by /etc/os-release).
pub fn install_hint(need: Need) -> String {
    let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let field = |key: &str| {
        release
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim_matches('"').to_ascii_lowercase())
            .unwrap_or_default()
    };
    let ids = format!("{} {}", field("ID="), field("ID_LIKE="));
    let is = |name: &str| ids.split_whitespace().any(|id| id == name);
    let (install, gst, va, nvenc) = if is("arch") {
        (
            "sudo pacman -S",
            "gstreamer gst-plugins-base gst-plugins-bad",
            "gst-plugin-va gst-plugins-bad intel-media-driver (Intel) or libva-mesa-driver (AMD)",
            "gst-plugins-bad nvidia-utils",
        )
    } else if is("fedora") || is("rhel") {
        (
            "sudo dnf install",
            "gstreamer1 gstreamer1-plugins-base gstreamer1-plugins-bad-free",
            "gstreamer1-plugins-bad-free intel-media-driver (Intel) or mesa-va-drivers (AMD)",
            "gstreamer1-plugins-bad-freeworld and the NVIDIA driver (RPM Fusion)",
        )
    } else if is("debian") || is("ubuntu") {
        (
            "sudo apt install",
            "libgstreamer1.0-0 libgstreamer-plugins-base1.0-0 gstreamer1.0-plugins-base gstreamer1.0-plugins-bad",
            "gstreamer1.0-plugins-bad intel-media-va-driver-non-free (Intel) or mesa-va-drivers (AMD)",
            "gstreamer1.0-plugins-bad libnvidia-encode-<driver version>",
        )
    } else {
        ("install", "GStreamer 1.22+ with its base and bad plugins", "GStreamer's bad plugins (va) and a VA-API driver", "GStreamer's bad plugins (nvcodec) and the NVIDIA driver's encode library")
    };
    let what = match need {
        Need::Gstreamer => gst,
        Need::Va => va,
        Need::Nvenc => nvenc,
    };
    format!("{install} {what}")
}

pub type OutputFn = extern "C" fn(*mut c_void, *const u8, usize, bool);

#[repr(C)]
pub struct RawPipeline {
    _private: [u8; 0],
}
#[repr(C)]
pub struct RawBuffer {
    _private: [u8; 0],
}

pub struct Lib {
    _lib: libloading::Library,
    pub path: PathBuf,
    pub version: String,
    has_element: extern "C" fn(*const c_char) -> bool,
    pipeline_new: extern "C" fn(*const c_char, *mut c_char, usize) -> *mut RawPipeline,
    pipeline_free: extern "C" fn(*mut RawPipeline),
    set_uint: extern "C" fn(*mut RawPipeline, *const c_char, *const c_char, u32) -> i32,
    get_uint: extern "C" fn(*mut RawPipeline, *const c_char, *const c_char) -> u32,
    force_keyframe: extern "C" fn(*mut RawPipeline) -> i32,
    buffer_new: extern "C" fn(usize, u64, u64) -> *mut RawBuffer,
    buffer_data: extern "C" fn(*mut RawBuffer) -> *mut u8,
    buffer_free: extern "C" fn(*mut RawBuffer),
    push: extern "C" fn(*mut RawPipeline, *mut RawBuffer) -> i32,
    pull: extern "C" fn(*mut RawPipeline, u64, OutputFn, *mut c_void) -> i32,
    bus_error: extern "C" fn(*mut RawPipeline, *mut c_char, usize) -> i32,
}

/// Where the library may be: `GLASS_GST_LIB`, the installed layout
/// (`<prefix>/bin/glass` + `<prefix>/share/glass/libglass_gst.so`), next to the binary
/// (a checkout's target/release), then the system library path.
fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = std::env::var_os("GLASS_GST_LIB") {
        out.push(PathBuf::from(p));
    }
    if let Some(exe) = std::env::current_exe().ok().and_then(|e| e.canonicalize().ok()) {
        if let Some(prefix) = exe.parent().and_then(|bin| bin.parent()) {
            out.push(prefix.join("share/glass").join(FILE));
        }
        if let Some(dir) = exe.parent() {
            out.push(dir.join(FILE));
        }
    }
    out
}

/// The text of a C error buffer.
fn text(buf: &[c_char]) -> String {
    // SAFETY: the library NUL-terminates what it writes; the buffer starts zeroed.
    unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

fn open() -> Result<Lib, String> {
    let paths = candidates();
    let path = paths.iter().find(|p| p.is_file()).cloned();
    // SAFETY: loading our own library; its initializers only register with GStreamer.
    let (lib, path) = match path {
        Some(p) => (unsafe { libloading::Library::new(&p) }, p),
        None => (unsafe { libloading::Library::new(FILE) }, PathBuf::from(FILE)),
    };
    let lib = lib.map_err(|e| {
        let e = e.to_string();
        if e.contains("libgst") || e.contains("libglib") || e.contains("libgobject") {
            format!("GStreamer isn't installed ({e}). Install it: {}", install_hint(Need::Gstreamer))
        } else if path.is_file() {
            format!("can't load {}: {e}", path.display())
        } else {
            let looked: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
            format!(
                "this Glass has no {FILE} (looked in {}). Build it with `cargo build --release -p glass-gst` and put it in <prefix>/share/glass/, or set GLASS_GST_LIB",
                looked.join(", ")
            )
        }
    })?;
    macro_rules! sym {
        ($name:literal) => {
            // SAFETY: the symbol has this signature in ABI version `ABI` (checked below
            // before anything else is called).
            *unsafe { lib.get($name) }.map_err(|e| format!("{}: {e}", path.display()))?
        };
    }
    let abi: extern "C" fn() -> u32 = sym!(b"glass_gst_abi\0");
    if abi() != ABI {
        return Err(format!(
            "{} is version {} of the interface, this Glass needs {ABI}: rebuild it from the same checkout",
            path.display(),
            abi()
        ));
    }
    let init: extern "C" fn(*mut c_char, usize) -> i32 = sym!(b"glass_gst_init\0");
    let mut err = [0 as c_char; 512];
    if init(err.as_mut_ptr(), err.len()) != 0 {
        return Err(format!("GStreamer doesn't start: {}", text(&err)));
    }
    let version_fn: extern "C" fn(*mut c_char, usize) = sym!(b"glass_gst_version\0");
    let mut version = [0 as c_char; 128];
    version_fn(version.as_mut_ptr(), version.len());
    Ok(Lib {
        has_element: sym!(b"glass_gst_has_element\0"),
        pipeline_new: sym!(b"glass_gst_pipeline_new\0"),
        pipeline_free: sym!(b"glass_gst_pipeline_free\0"),
        set_uint: sym!(b"glass_gst_set_uint\0"),
        get_uint: sym!(b"glass_gst_get_uint\0"),
        force_keyframe: sym!(b"glass_gst_force_keyframe\0"),
        buffer_new: sym!(b"glass_gst_buffer_new\0"),
        buffer_data: sym!(b"glass_gst_buffer_data\0"),
        buffer_free: sym!(b"glass_gst_buffer_free\0"),
        push: sym!(b"glass_gst_push\0"),
        pull: sym!(b"glass_gst_pull\0"),
        bus_error: sym!(b"glass_gst_bus_error\0"),
        version: text(&version),
        path,
        _lib: lib,
    })
}

/// The loaded library, or why it can't be used (with what to install).
pub fn lib() -> Result<&'static Lib> {
    static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();
    LIB.get_or_init(open).as_ref().map_err(|e| anyhow!("{e}"))
}

fn cstr(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}

impl Lib {
    /// Whether GStreamer is at least `major.minor` (from its version string, e.g.
    /// "GStreamer 1.24.2"); false if that can't be read.
    pub fn version_at_least(&self, major: u32, minor: u32) -> bool {
        let mut numbers = self
            .version
            .rsplit(' ')
            .next()
            .unwrap_or_default()
            .split('.')
            .map(|n| n.parse::<u32>().ok());
        match (numbers.next().flatten(), numbers.next().flatten()) {
            (Some(a), Some(b)) => (a, b) >= (major, minor),
            _ => false,
        }
    }

    pub fn has_element(&self, name: &str) -> bool {
        (self.has_element)(cstr(name).as_ptr())
    }

    pub fn pipeline(&'static self, description: &str) -> Result<Pipeline> {
        let mut err = [0 as c_char; 1024];
        let raw = (self.pipeline_new)(cstr(description).as_ptr(), err.as_mut_ptr(), err.len());
        if raw.is_null() {
            let why = text(&err);
            return Err(anyhow!("{}", if why.is_empty() { "GStreamer failed (no reason given)" } else { &why }));
        }
        Ok(Pipeline { lib: self, raw })
    }
}

/// A running pipeline with an appsrc `src` and an appsink `sink`.
pub struct Pipeline {
    lib: &'static Lib,
    raw: *mut RawPipeline,
}

// The pipeline is used from the one encoder thread that owns it; GStreamer objects are
// thread-safe to move.
unsafe impl Send for Pipeline {}

impl Pipeline {
    pub fn set_uint(&self, element: &str, property: &str, value: u32) -> Result<()> {
        let r = (self.lib.set_uint)(self.raw, cstr(element).as_ptr(), cstr(property).as_ptr(), value);
        if r == 0 { Ok(()) } else { Err(anyhow!("can't set {element}.{property}")) }
    }

    pub fn get_uint(&self, element: &str, property: &str) -> u32 {
        (self.lib.get_uint)(self.raw, cstr(element).as_ptr(), cstr(property).as_ptr())
    }

    pub fn force_keyframe(&self) {
        (self.lib.force_keyframe)(self.raw);
    }

    /// Pushes a frame of `size` bytes that `fill` writes, with these timestamps.
    pub fn push(&self, size: usize, pts_ns: u64, duration_ns: u64, fill: impl FnOnce(&mut [u8])) -> Result<()> {
        let buffer = (self.lib.buffer_new)(size, pts_ns, duration_ns);
        if buffer.is_null() {
            return Err(anyhow!("no buffer of {size} bytes"));
        }
        let data = (self.lib.buffer_data)(buffer);
        if data.is_null() {
            (self.lib.buffer_free)(buffer);
            return Err(anyhow!("can't map the buffer"));
        }
        // SAFETY: the library mapped `size` writable bytes, ours until pushed.
        fill(unsafe { std::slice::from_raw_parts_mut(data, size) });
        if (self.lib.push)(self.raw, buffer) == 0 { Ok(()) } else { Err(anyhow!("push failed")) }
    }

    /// The next encoded frame and whether it's a keyframe; None on timeout.
    pub fn pull(&self, timeout_ns: u64) -> Result<Option<(Vec<u8>, bool)>> {
        extern "C" fn take(ctx: *mut c_void, data: *const u8, len: usize, keyframe: bool) {
            // SAFETY: ctx is the Option below; data is `len` readable bytes for this call.
            let out = unsafe { &mut *ctx.cast::<Option<(Vec<u8>, bool)>>() };
            *out = Some((unsafe { std::slice::from_raw_parts(data, len) }.to_vec(), keyframe));
        }
        let mut out: Option<(Vec<u8>, bool)> = None;
        match (self.lib.pull)(self.raw, timeout_ns, take, (&raw mut out).cast()) {
            1 => Ok(out),
            0 => Ok(None),
            _ => Err(anyhow!("pull failed")),
        }
    }

    /// Empties the bus; the first error on it, if any.
    pub fn bus_error(&self) -> Option<String> {
        let mut err = [0 as c_char; 1024];
        ((self.lib.bus_error)(self.raw, err.as_mut_ptr(), err.len()) == 1).then(|| text(&err))
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        (self.lib.pipeline_free)(self.raw);
    }
}
