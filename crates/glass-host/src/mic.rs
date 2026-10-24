//! The viewer's microphone, as a microphone on the host (KIND_MIC datagrams).
//!
//! Linux: while any viewer's mic is on, a virtual source "Glass microphone" exists
//! (PulseAudio / PipeWire `module-pipe-source`), fed 48 kHz mono s16 through a FIFO. Apps
//! on the host pick it like any other microphone. Needs only `pactl` (`glass mic install`
//! adds it with the system's package manager when it's missing).
//!
//! macOS: through Glass Microphone, our own virtual audio driver (native/glass-mic, shipped
//! with Glass; `glass mic install` puts it in /Library/Audio/Plug-Ins/HAL, which asks for
//! the Mac's password once), or BlackHole when that's what's installed. A microphone other
//! apps can choose needs a driver installed system-wide; Glass plays the viewer's voice
//! into the driver's output, which comes out of its input, and makes that the Mac's
//! default input while a mic is on (put back after). Without a driver the mic isn't
//! offered: playing the voice through the speakers would feed it back to the viewer
//! through the host's own audio capture, an echo rather than a microphone.

use glass_proto::{AUDIO_HEADER_LEN, KIND_MIC};

pub fn supported() -> bool {
    if crate::testmode::enabled() {
        return true;
    }
    #[cfg(target_os = "macos")]
    return mac::device().is_some();
    #[cfg(target_os = "linux")]
    return linux::pactl();
    #[allow(unreachable_code)]
    false
}

/// What a viewer's mic comes in as: "Glass Microphone", "BlackHole 2ch", or on Linux
/// "Glass microphone" (made while a mic is on); `None` when it can't yet.
pub fn device_name() -> Option<String> {
    #[cfg(target_os = "macos")]
    return mac::device().map(|(_, name)| name);
    #[cfg(target_os = "linux")]
    return linux::pactl().then(|| "Glass microphone".to_string());
    #[allow(unreachable_code)]
    None
}

/// How `install` asks for the admin password: in this terminal (`sudo`), or with the
/// system's own password dialog (the app, or a viewer asking remotely).
#[derive(Clone, Copy, PartialEq)]
pub enum Ask {
    Terminal,
    Dialog,
}

/// Installs what a viewer's microphone needs. macOS: the Glass Microphone driver shipped
/// with Glass (or BlackHole's official installer when this build has no driver). Linux:
/// `pactl`, with the system's package manager.
pub fn install(ask: Ask) -> anyhow::Result<String> {
    #[cfg(target_os = "macos")]
    return mac_install(ask);
    #[cfg(target_os = "linux")]
    return linux::install(ask);
    #[allow(unreachable_code)]
    {
        let _ = ask;
        anyhow::bail!("not supported on this system")
    }
}

/// Removes the Glass Microphone driver (macOS).
pub fn uninstall(ask: Ask) -> anyhow::Result<String> {
    #[cfg(target_os = "macos")]
    {
        let dest = std::path::Path::new(HAL_DIR).join(DRIVER);
        if !dest.exists() {
            return Ok("Glass Microphone isn't installed.".into());
        }
        as_admin(&format!("rm -rf {} && killall -9 coreaudiod 2>/dev/null; true", sh_quote(&dest.display().to_string())), ask, "remove Glass Microphone")?;
        restart_for_audio();
        return Ok("Removed Glass Microphone.".into());
    }
    #[allow(unreachable_code)]
    {
        let _ = ask;
        Ok("Nothing to remove here.".into())
    }
}

/// Runs a shell command as root: sudo in the terminal, or the system's password dialog.
fn as_admin(script: &str, ask: Ask, why: &str) -> anyhow::Result<()> {
    let ok = match ask {
        Ask::Terminal => std::process::Command::new("sudo").args(["sh", "-c", script]).status()?.success(),
        #[cfg(target_os = "macos")]
        Ask::Dialog => {
            let apple = format!(
                "do shell script \"{}\" with prompt \"Glass needs your password to {why}.\" with administrator privileges",
                script.replace('\\', "\\\\").replace('"', "\\\"")
            );
            let out = std::process::Command::new("osascript").args(["-e", &apple]).output()?;
            anyhow::ensure!(!String::from_utf8_lossy(&out.stderr).contains("-128"), "cancelled");
            out.status.success()
        }
        #[cfg(not(target_os = "macos"))]
        Ask::Dialog => std::process::Command::new("pkexec").args(["sh", "-c", script]).status()?.success(),
    };
    anyhow::ensure!(ok, "couldn't {why}");
    Ok(())
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Restarting the Mac's audio server (to load or unload the driver) ends the system-audio
/// capture of a running Glass: restart Glass so sound comes back. In the service itself
/// (a viewer asked), after a moment so the answer reaches the viewer; elsewhere (the CLI,
/// the app), the background service.
#[cfg(target_os = "macos")]
fn restart_for_audio() {
    if crate::testmode::enabled() {
        return;
    }
    if crate::server::shared().is_some() {
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            let _ = crate::power::restart_glass();
        });
    } else if crate::agent::is_installed() {
        if let Err(e) = crate::restart_and_wait() {
            tracing::warn!("microphone: restarting Glass after the driver change: {e:#}");
        }
    }
}

#[cfg(target_os = "macos")]
const HAL_DIR: &str = "/Library/Audio/Plug-Ins/HAL";
#[cfg(target_os = "macos")]
const DRIVER: &str = "GlassMicrophone.driver";

/// The driver shipped with this Glass: `<prefix>/share/glass/GlassMicrophone.driver`
/// (installed), or the CMake build in a checkout; `GLASS_MIC_DRIVER` overrides.
#[cfg(target_os = "macos")]
fn bundled_driver() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("GLASS_MIC_DRIVER") {
        return Some(p.into());
    }
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let mut candidates = vec![exe.parent()?.parent()?.join("share/glass").join(DRIVER)];
    // A checkout: target/<profile>/glass-host -> target/glass-mic/
    candidates.push(exe.parent()?.parent()?.join("glass-mic").join(DRIVER));
    candidates.into_iter().find(|p| p.join("Contents/MacOS/GlassMicrophone").exists())
}

#[cfg(target_os = "macos")]
fn mac_install(ask: Ask) -> anyhow::Result<String> {
    if let Some(src) = bundled_driver() {
        let dest = std::path::Path::new(HAL_DIR).join(DRIVER);
        // Copy, owned by root, then restart the audio server (launchd starts it again) so
        // it loads the driver.
        let script = format!(
            "mkdir -p {hal} && rm -rf {dest} && cp -R {src} {hal}/ && chown -R root:wheel {dest} && xattr -cr {dest} && killall -9 coreaudiod 2>/dev/null; true",
            hal = sh_quote(HAL_DIR),
            dest = sh_quote(&dest.display().to_string()),
            src = sh_quote(&src.display().to_string()),
        );
        as_admin(&script, ask, "install its microphone (Glass Microphone)")?;
        // The audio server takes a moment to come back with the new device.
        for _ in 0..40 {
            if mac::device().is_some_and(|(_, name)| name == "Glass Microphone") {
                restart_for_audio();
                return Ok("Glass Microphone is installed. Glass restarts to pick it up; turn on the mic in the viewer once it's back.".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        anyhow::bail!("installed, but the Mac's audio hasn't picked it up yet; try again in a minute, or restart the Mac");
    }
    blackhole_install()
}

/// Without a bundled driver (a build without one): BlackHole's official installer, as
/// Homebrew lists it, checked against its SHA-256, opened in Installer. Falls back to
/// BlackHole's website.
#[cfg(target_os = "macos")]
fn blackhole_install() -> anyhow::Result<String> {

    let opened = (|| -> anyhow::Result<()> {
        let info: serde_json::Value = ureq::get("https://formulae.brew.sh/api/cask/blackhole-2ch.json")
            .call()?
            .body_mut()
            .read_json()?;
        let url = info["url"].as_str().filter(|u| u.starts_with("https://existential.audio/")).ok_or_else(|| anyhow::anyhow!("no download link"))?;
        let want = info["sha256"].as_str().ok_or_else(|| anyhow::anyhow!("no checksum"))?;
        let pkg = ureq::get(url).call()?.body_mut().with_config().limit(64 << 20).read_to_vec()?;
        let got: String = ring::digest::digest(&ring::digest::SHA256, &pkg).as_ref().iter().map(|b| format!("{b:02x}")).collect();
        anyhow::ensure!(got == want, "the download didn't match its checksum");
        let path = std::env::temp_dir().join(url.rsplit('/').next().unwrap_or("BlackHole.pkg"));
        std::fs::write(&path, pkg)?;
        anyhow::ensure!(std::process::Command::new("open").arg(&path).status()?.success(), "couldn't open the installer");
        Ok(())
    })();
    match opened {
        Ok(()) => Ok("The BlackHole installer is open: follow it (it asks for your Mac's password once). Then turn on the mic in the viewer.".into()),
        Err(e) => {
            tracing::warn!("microphone: BlackHole download: {e:#}");
            let _ = std::process::Command::new("open").arg("https://existential.audio/blackhole/").status();
            Ok("Opened BlackHole's website: download BlackHole 2ch and install it.".into())
        }
    }
}

/// macOS: a mic left on by a host that didn't exit cleanly left BlackHole as the default
/// input; put the previous one back. Call once at startup.
pub fn recover() {
    #[cfg(target_os = "macos")]
    if !crate::testmode::enabled() {
        mac::recover();
    }
}

/// A KIND_MIC packet's samples as mono s16 LE bytes (stereo is averaged down).
pub fn mono_pcm(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < AUDIO_HEADER_LEN || data[0] != KIND_MIC {
        return None;
    }
    let channels = data[1] as usize;
    let pcm = &data[AUDIO_HEADER_LEN..];
    if !(1..=2).contains(&channels) || pcm.len() % (2 * channels) != 0 {
        return None;
    }
    if channels == 1 {
        return Some(pcm.to_vec());
    }
    let mut out = Vec::with_capacity(pcm.len() / 2);
    for frame in pcm.chunks_exact(4) {
        let l = i16::from_le_bytes([frame[0], frame[1]]) as i32;
        let r = i16::from_le_bytes([frame[2], frame[3]]) as i32;
        out.extend_from_slice(&(((l + r) / 2) as i16).to_le_bytes());
    }
    Some(out)
}

/// One KIND_MIC datagram from a session that turned its mic on.
pub fn packet(data: &[u8]) {
    if crate::testmode::enabled() {
        crate::testmode::record_mic(data.len());
        return;
    }
    #[cfg(target_os = "linux")]
    if let Some(pcm) = mono_pcm(data) {
        linux::write(&pcm);
    }
    #[cfg(target_os = "macos")]
    if let Some(pcm) = mono_pcm(data) {
        mac::write(&pcm);
    }
}

/// A session started (true) or stopped (false) sending.
pub fn active(on: bool) {
    if crate::testmode::enabled() {
        return;
    }
    #[cfg(target_os = "linux")]
    linux::active(on);
    #[cfg(target_os = "macos")]
    mac::active(on);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = on;
}

/// A short buffer of mono 48 kHz samples: the newest win when the viewer sends faster
/// than the device plays, so delay never builds up.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
struct Ring {
    samples: std::collections::VecDeque<i16>,
}

#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
impl Ring {
    /// Past this much buffered audio, trim back to `KEEP`.
    const MAX: usize = 48_000 * 120 / 1000;
    const KEEP: usize = 48_000 * 40 / 1000;

    fn push(&mut self, pcm: &[u8]) {
        self.samples.extend(pcm.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])));
        if self.samples.len() > Self::MAX {
            let drop = self.samples.len() - Self::KEEP;
            self.samples.drain(..drop);
        }
    }
}

#[cfg(target_os = "macos")]
mod mac {
    //! BlackHole through the CoreAudio HAL: an IOProc on its output, and the system's
    //! default input device switched to it (and back).

    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::Mutex;

    use objc2_core_audio::*;
    use objc2_core_audio_types::{AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp, kAudioFormatFlagIsFloat};
    use objc2_core_foundation::{CFRetained, CFString};

    use super::Ring;

    const SYSTEM: AudioObjectID = kAudioObjectSystemObject as AudioObjectID;

    struct Playing {
        device: AudioObjectID,
        proc_id: AudioDeviceIOProcID,
        /// The default input before ours, put back when the last mic turns off.
        previous: Option<AudioObjectID>,
        users: usize,
    }

    // SAFETY: the IOProc id is an opaque handle from the HAL, only used under the lock.
    unsafe impl Send for Playing {}

    static PLAYING: Mutex<Option<Playing>> = Mutex::new(None);
    static RING: Mutex<Ring> = Mutex::new(Ring { samples: std::collections::VecDeque::new() });
    /// The device's sample rate (viewers send 48 kHz) and the read position in the ring.
    static FORMAT: Mutex<(f64, f64)> = Mutex::new((48_000.0, 0.0));

    fn address(selector: AudioObjectPropertySelector, scope: AudioObjectPropertyScope) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress { mSelector: selector, mScope: scope, mElement: kAudioObjectPropertyElementMain }
    }

    fn size_of(object: AudioObjectID, addr: AudioObjectPropertyAddress) -> Option<u32> {
        let mut size = 0u32;
        // SAFETY: valid address and out pointer.
        let status = unsafe {
            AudioObjectGetPropertyDataSize(object, NonNull::from(&addr), 0, std::ptr::null(), NonNull::from(&mut size))
        };
        (status == 0).then_some(size)
    }

    fn get<T: Copy>(object: AudioObjectID, addr: AudioObjectPropertyAddress, mut value: T) -> Option<T> {
        let mut size = std::mem::size_of::<T>() as u32;
        // SAFETY: `value` has room for `size` bytes.
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                NonNull::from(&addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::from(&mut value).cast(),
            )
        };
        (status == 0).then_some(value)
    }

    fn set<T: Copy>(object: AudioObjectID, addr: AudioObjectPropertyAddress, value: T) -> bool {
        // SAFETY: `value` is `size_of::<T>()` bytes and outlives the call.
        let status = unsafe {
            AudioObjectSetPropertyData(
                object,
                NonNull::from(&addr),
                0,
                std::ptr::null(),
                std::mem::size_of::<T>() as u32,
                NonNull::from(&value).cast(),
            )
        };
        status == 0
    }

    fn string(object: AudioObjectID, selector: AudioObjectPropertySelector) -> Option<String> {
        let raw: *const CFString = get(object, address(selector, kAudioObjectPropertyScopeGlobal), std::ptr::null())?;
        // SAFETY: the HAL returns a +1 CFString we now own.
        let s = unsafe { CFRetained::from_raw(NonNull::new(raw.cast_mut())?) };
        Some(s.to_string())
    }

    fn devices() -> Vec<AudioObjectID> {
        let addr = address(kAudioHardwarePropertyDevices, kAudioObjectPropertyScopeGlobal);
        let Some(mut size) = size_of(SYSTEM, addr) else { return Vec::new() };
        let mut ids = vec![0 as AudioObjectID; size as usize / std::mem::size_of::<AudioObjectID>()];
        if ids.is_empty() {
            return ids;
        }
        // SAFETY: `ids` has room for `size` bytes.
        let status = unsafe {
            AudioObjectGetPropertyData(
                SYSTEM,
                NonNull::from(&addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::new(ids.as_mut_ptr()).unwrap().cast(),
            )
        };
        if status != 0 {
            return Vec::new();
        }
        ids.truncate(size as usize / std::mem::size_of::<AudioObjectID>());
        ids
    }

    fn has_streams(device: AudioObjectID, scope: AudioObjectPropertyScope) -> bool {
        size_of(device, address(kAudioDevicePropertyStreams, scope)).is_some_and(|s| s > 0)
    }

    /// BlackHole, if installed (the 2-channel one first): its id and name.
    /// `GLASS_MIC_DEVICE` picks another loopback driver by name instead.
    pub fn device() -> Option<(AudioObjectID, String)> {
        // Ours first, then BlackHole (2ch over its bigger variants); GLASS_MIC_DEVICE picks
        // another loopback driver by name.
        let wanted: Vec<String> = match std::env::var("GLASS_MIC_DEVICE") {
            Ok(name) => vec![name],
            Err(_) => vec!["Glass Microphone".into(), "BlackHole".into()],
        };
        let mut found: Vec<(usize, AudioObjectID, String)> = devices()
            .into_iter()
            .filter_map(|d| Some((d, string(d, kAudioObjectPropertyName)?)))
            .filter_map(|(d, name)| {
                let rank = wanted.iter().position(|w| name.contains(w.as_str()))?;
                (has_streams(d, kAudioObjectPropertyScopeOutput) && has_streams(d, kAudioObjectPropertyScopeInput)).then_some((rank, d, name))
            })
            .collect();
        found.sort_by_key(|(rank, _, name)| (*rank, !name.contains("2ch")));
        found.into_iter().next().map(|(_, d, name)| (d, name))
    }

    fn default_input() -> Option<AudioObjectID> {
        get(SYSTEM, address(kAudioHardwarePropertyDefaultInputDevice, kAudioObjectPropertyScopeGlobal), 0 as AudioObjectID)
    }

    fn set_default_input(device: AudioObjectID) -> bool {
        set(SYSTEM, address(kAudioHardwarePropertyDefaultInputDevice, kAudioObjectPropertyScopeGlobal), device)
    }

    fn by_uid(uid: &str) -> Option<AudioObjectID> {
        devices().into_iter().find(|d| string(*d, kAudioDevicePropertyDeviceUID).as_deref() == Some(uid))
    }

    /// The input to put back, on disk in case the host never gets to.
    fn saved_file() -> std::path::PathBuf {
        crate::config_dir().join("mic-default-input")
    }

    pub fn recover() {
        let Ok(uid) = std::fs::read_to_string(saved_file()) else { return };
        let _ = std::fs::remove_file(saved_file());
        let (Some((blackhole, _)), Some(previous)) = (device(), by_uid(uid.trim())) else { return };
        if default_input() == Some(blackhole) && set_default_input(previous) {
            tracing::info!("microphone: put the default input back after an unclean exit");
        }
    }

    pub fn active(on: bool) {
        let mut playing = PLAYING.lock().unwrap();
        if on {
            if let Some(p) = playing.as_mut() {
                p.users += 1;
                return;
            }
            match start() {
                Ok(p) => *playing = Some(p),
                Err(e) => tracing::warn!("microphone: {e:#}"),
            }
        } else if let Some(p) = playing.as_mut() {
            p.users = p.users.saturating_sub(1);
            if p.users == 0 {
                stop(playing.take().unwrap());
            }
        }
    }

    fn start() -> anyhow::Result<Playing> {
        let (device, name) = device().ok_or_else(|| anyhow::anyhow!("BlackHole isn't installed (glass mic install)"))?;
        // Ask for 48 kHz (what viewers send); if it won't, the callback resamples.
        set(device, address(kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal), 48_000.0f64);
        // SAFETY: a plain C struct; zero is a valid starting value.
        let blank: AudioStreamBasicDescription = unsafe { std::mem::zeroed() };
        let format = get(device, address(kAudioDevicePropertyStreamFormat, kAudioObjectPropertyScopeOutput), blank)
            .ok_or_else(|| anyhow::anyhow!("couldn't read {name}'s format"))?;
        anyhow::ensure!(
            format.mFormatFlags & kAudioFormatFlagIsFloat != 0 && format.mBitsPerChannel == 32,
            "{name} doesn't play 32-bit float"
        );
        *FORMAT.lock().unwrap() = (format.mSampleRate, 0.0);
        RING.lock().unwrap().samples.clear();

        let mut proc_id: AudioDeviceIOProcID = None;
        // SAFETY: `render` matches AudioDeviceIOProc and uses no client data.
        let status = unsafe { AudioDeviceCreateIOProcID(device, Some(render), std::ptr::null_mut(), NonNull::from(&mut proc_id)) };
        anyhow::ensure!(status == 0 && proc_id.is_some(), "couldn't open {name} (CoreAudio {status})");
        // SAFETY: the proc was just created on this device.
        let status = unsafe { AudioDeviceStart(device, proc_id) };
        if status != 0 {
            // SAFETY: as above.
            unsafe { AudioDeviceDestroyIOProcID(device, proc_id) };
            anyhow::bail!("couldn't start {name} (CoreAudio {status})");
        }

        // Apps listening to "the microphone" now hear the viewer.
        let previous = default_input().filter(|d| *d != device);
        if let Some(prev) = previous
            && let Some(uid) = string(prev, kAudioDevicePropertyDeviceUID)
        {
            let _ = crate::settings::write_private(&saved_file(), uid.as_bytes());
        }
        if !set_default_input(device) {
            tracing::warn!("microphone: couldn't make {name} the default input; choose it in the app instead");
        }
        tracing::info!("microphone: playing into {name} ({} Hz), now the default input", format.mSampleRate);
        Ok(Playing { device, proc_id, previous, users: 1 })
    }

    fn stop(p: Playing) {
        // SAFETY: the proc was created and started on this device in `start`.
        unsafe {
            AudioDeviceStop(p.device, p.proc_id);
            AudioDeviceDestroyIOProcID(p.device, p.proc_id);
        }
        // Put the previous input back, unless someone chose another one meanwhile.
        if let Some(prev) = p.previous
            && default_input() == Some(p.device)
        {
            set_default_input(prev);
        }
        let _ = std::fs::remove_file(saved_file());
        tracing::info!("microphone: off, default input put back");
    }

    pub fn write(pcm: &[u8]) {
        if PLAYING.lock().unwrap().is_some() {
            RING.lock().unwrap().push(pcm);
        }
    }

    /// The HAL's callback: fills BlackHole's output from the ring, or silence when it runs
    /// dry or is busy. Never blocks.
    unsafe extern "C-unwind" fn render(
        _device: AudioObjectID,
        _now: NonNull<AudioTimeStamp>,
        _input: NonNull<AudioBufferList>,
        _input_time: NonNull<AudioTimeStamp>,
        output: NonNull<AudioBufferList>,
        _output_time: NonNull<AudioTimeStamp>,
        _client: *mut c_void,
    ) -> i32 {
        // SAFETY: the HAL hands us a valid buffer list for this cycle.
        let list = unsafe { &mut *output.as_ptr() };
        // SAFETY: the list holds `mNumberBuffers` buffers.
        let buffers = unsafe { std::slice::from_raw_parts_mut(list.mBuffers.as_mut_ptr(), list.mNumberBuffers as usize) };
        let (Ok(mut ring), Ok(mut format)) = (RING.try_lock(), FORMAT.try_lock()) else {
            for b in buffers.iter_mut() {
                // SAFETY: each buffer is `mDataByteSize` bytes.
                unsafe { std::ptr::write_bytes(b.mData.cast::<u8>(), 0, b.mDataByteSize as usize) };
            }
            return 0;
        };
        let step = 48_000.0 / format.0.max(1.0);
        // Every buffer carries the same frames (interleaved, or one buffer per channel).
        let frames = buffers.first().map_or(0, |b| b.mDataByteSize as usize / 4 / b.mNumberChannels.max(1) as usize);
        let mut mono = Vec::with_capacity(frames);
        let mut pos = format.1;
        for _ in 0..frames {
            mono.push(ring.samples.get(pos as usize).map_or(0.0, |s| *s as f32 / 32768.0));
            pos += step;
        }
        let used = (pos as usize).min(ring.samples.len());
        ring.samples.drain(..used);
        format.1 = if used < pos as usize { 0.0 } else { pos.fract() };
        for b in buffers.iter_mut() {
            let channels = b.mNumberChannels.max(1) as usize;
            // SAFETY: 32-bit float samples (checked in `start`), `mDataByteSize` bytes.
            let out = unsafe { std::slice::from_raw_parts_mut(b.mData.cast::<f32>(), b.mDataByteSize as usize / 4) };
            for (frame, sample) in out.chunks_exact_mut(channels).zip(&mono) {
                frame.fill(*sample);
            }
        }
        RENDERED.fetch_add(frames as u64, std::sync::atomic::Ordering::Relaxed);
        0
    }

    /// Frames handed to the device so far (for tests).
    pub static RENDERED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Plays a tone into a real loopback device and checks the default input moves to it
        /// and back. Changes this Mac's default input for a moment, so it's opt-in:
        /// `GLASS_MIC_DEVICE=BlackHole cargo test -- --ignored plays_into`.
        #[test]
        #[ignore]
        fn plays_into_the_device_and_puts_the_input_back() {
            let (device, name) = device().expect("no loopback device (set GLASS_MIC_DEVICE)");
            let before = default_input();
            active(true);
            let switched = default_input() == Some(device);
            let tone: Vec<u8> = (0..4800).flat_map(|i| (((i as f32 / 48.0).sin() * 8000.0) as i16).to_le_bytes()).collect();
            for _ in 0..10 {
                write(&tone);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let rendered = RENDERED.load(std::sync::atomic::Ordering::Relaxed);
            active(false);
            assert!(rendered > 0, "{name} pulled audio");
            // Some virtual devices (Zoom's) refuse to be a default; BlackHole doesn't.
            let can_default = get(device, address(kAudioDevicePropertyDeviceCanBeDefaultDevice, kAudioObjectPropertyScopeInput), 0u32) == Some(1);
            assert_eq!(switched, can_default, "{name} is the default input while on (when it can be)");
            assert_eq!(default_input(), before, "the previous input is back");
            assert!(!saved_file().exists());
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Mutex;

    /// O_NONBLOCK on Linux: a full FIFO drops audio instead of stalling the session.
    const O_NONBLOCK: i32 = 0o4000;

    struct Source {
        module: String,
        fifo: Option<std::fs::File>,
        /// Sessions with their mic on.
        users: usize,
    }

    static SOURCE: Mutex<Option<Source>> = Mutex::new(None);

    /// `pactl` works here: it's installed and reaches a PulseAudio or PipeWire server.
    pub fn pactl() -> bool {
        Command::new("pactl")
            .arg("info")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Adds `pactl` with this system's package manager (it's in PulseAudio's utilities,
    /// which PipeWire systems use too).
    pub fn install(ask: super::Ask) -> anyhow::Result<String> {
        if pactl() {
            return Ok("The microphone is ready: nothing to install.".into());
        }
        let has = |cmd: &str| Command::new("sh").args(["-c", &format!("command -v {cmd}")]).output().is_ok_and(|o| o.status.success());
        let script = if has("apt-get") {
            "apt-get install -y pulseaudio-utils"
        } else if has("dnf") {
            "dnf install -y pulseaudio-utils"
        } else if has("pacman") {
            "pacman -S --noconfirm --needed libpulse"
        } else if has("zypper") {
            "zypper --non-interactive install pulseaudio-utils"
        } else if has("apk") {
            "apk add pulseaudio-utils"
        } else {
            anyhow::bail!("install pactl (PulseAudio's utilities) with your package manager, then try again");
        };
        super::as_admin(script, ask, "install pactl for the microphone")?;
        anyhow::ensure!(
            pactl(),
            "pactl is installed but can't reach a sound server; make sure PipeWire (with pipewire-pulse) or PulseAudio is running"
        );
        Ok("The microphone is ready. Turn it on in the viewer.".into())
    }

    fn fifo_path() -> PathBuf {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        dir.join("glass-mic.fifo")
    }

    pub fn active(on: bool) {
        let mut source = SOURCE.lock().unwrap();
        if on {
            if let Some(s) = source.as_mut() {
                s.users += 1;
                return;
            }
            match load() {
                Ok(module) => {
                    tracing::info!("microphone: \"Glass microphone\" is available on this computer");
                    *source = Some(Source {
                        module,
                        fifo: None,
                        users: 1,
                    });
                }
                Err(e) => tracing::warn!("microphone: {e:#}"),
            }
        } else if let Some(s) = source.as_mut() {
            s.users = s.users.saturating_sub(1);
            if s.users == 0 {
                let s = source.take().unwrap();
                drop(s.fifo);
                let _ = Command::new("pactl")
                    .args(["unload-module", &s.module])
                    .status();
                tracing::info!("microphone: removed");
            }
        }
    }

    fn load() -> anyhow::Result<String> {
        // One left over from a host that didn't exit cleanly would take the name.
        if let Ok(out) = Command::new("pactl").args(["list", "short", "modules"]).output() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if line.contains("source_name=glass_mic")
                    && let Some(id) = line.split_whitespace().next()
                {
                    let _ = Command::new("pactl").args(["unload-module", id]).status();
                }
            }
        }
        let fifo = fifo_path();
        let _ = std::fs::remove_file(&fifo);
        let out = Command::new("pactl")
            .args([
                "load-module",
                "module-pipe-source",
                "source_name=glass_mic",
                &format!("file={}", fifo.display()),
                "format=s16le",
                "rate=48000",
                "channels=1",
                "source_properties=device.description=\"Glass microphone\"",
            ])
            .output()
            .map_err(|e| anyhow::anyhow!("pactl isn't available ({e})"))?;
        anyhow::ensure!(
            out.status.success(),
            "pactl couldn't add the microphone: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn write(pcm: &[u8]) {
        let mut source = SOURCE.lock().unwrap();
        let Some(s) = source.as_mut() else { return };
        if s.fifo.is_none() {
            // The module creates the FIFO; it opens its read end first.
            s.fifo = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(O_NONBLOCK)
                .open(fifo_path())
                .ok();
        }
        if let Some(f) = s.fifo.as_mut()
            && let Err(e) = f.write_all(pcm)
            && e.kind() != std::io::ErrorKind::WouldBlock
        {
            tracing::warn!("microphone: {e}");
            s.fifo = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(channels: u8, samples: &[i16]) -> Vec<u8> {
        let mut p = glass_proto::audio_packet(1, 2, channels, samples);
        p[0] = KIND_MIC;
        p
    }

    #[test]
    fn mono_passes_through_and_stereo_is_averaged() {
        let mono = mono_pcm(&packet(1, &[1, -2, 300])).unwrap();
        assert_eq!(mono, [1i16, -2, 300].iter().flat_map(|s| s.to_le_bytes()).collect::<Vec<_>>());
        let stereo = mono_pcm(&packet(2, &[100, 200, -100, -300])).unwrap();
        assert_eq!(stereo, [150i16, -200].iter().flat_map(|s| s.to_le_bytes()).collect::<Vec<_>>());
    }

    #[test]
    fn the_buffer_never_holds_more_than_a_moment() {
        let mut ring = Ring { samples: Default::default() };
        let pcm: Vec<u8> = (0..Ring::MAX as i16 + 100).flat_map(|s| s.to_le_bytes()).collect();
        ring.push(&pcm);
        assert_eq!(ring.samples.len(), Ring::KEEP);
        // The newest samples are the ones kept.
        assert_eq!(*ring.samples.back().unwrap(), Ring::MAX as i16 + 99);
    }

    #[test]
    fn rejects_other_packets() {
        let mut audio = packet(1, &[1]);
        audio[0] = glass_proto::KIND_AUDIO;
        assert!(mono_pcm(&audio).is_none());
        assert!(mono_pcm(&packet(3, &[1, 2, 3])).is_none());
        assert!(mono_pcm(&[KIND_MIC, 1]).is_none());
        let mut odd = packet(2, &[1, 2]);
        odd.push(0);
        assert!(mono_pcm(&odd).is_none());
    }
}
