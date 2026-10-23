//! The viewer's microphone, as a microphone on the host (KIND_MIC datagrams).
//!
//! Linux: while any viewer's mic is on, a virtual source "Glass microphone" exists
//! (PulseAudio / PipeWire `module-pipe-source`), fed 48 kHz mono s16 through a FIFO. Apps
//! on the host pick it like any other microphone.
//!
//! macOS: not offered. A microphone other apps can choose needs an audio driver installed
//! system-wide (a HAL plug-in such as BlackHole); playing the viewer's voice through the
//! speakers instead would also feed it back to the viewer through the host's audio
//! capture, which is an echo, not a microphone.

use glass_proto::{AUDIO_HEADER_LEN, KIND_MIC};

pub fn supported() -> bool {
    crate::testmode::enabled() || cfg!(target_os = "linux")
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
}

/// A session started (true) or stopped (false) sending.
pub fn active(on: bool) {
    if crate::testmode::enabled() {
        return;
    }
    #[cfg(target_os = "linux")]
    linux::active(on);
    #[cfg(not(target_os = "linux"))]
    let _ = on;
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
