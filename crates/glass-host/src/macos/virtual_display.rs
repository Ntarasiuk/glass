//! A virtual display in the viewer's shape, so the Mac's desktop can fill an iPad (4:3) or
//! another screen with no black bars. The main display mirrors the virtual one while it
//! exists, so the desktop takes its size; dropping it (or this process exiting) removes
//! it and macOS restores the displays.
//!
//! macOS has no public API for this; CGVirtualDisplay is CoreGraphics SPI (also used by
//! apps such as BetterDisplay and DeskPad), reached here through the Objective-C runtime
//! so a missing class fails cleanly instead of failing to link.

use anyhow::{Context, Result, bail};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{msg_send, sel};
use objc2_core_foundation::{CGPoint, CGSize};
use objc2_core_graphics::{
    CGBeginDisplayConfiguration, CGCompleteDisplayConfiguration, CGConfigureDisplayMirrorOfDisplay,
    CGConfigureOption, CGDirectDisplayID, CGDisplayConfigRef, CGError, CGMainDisplayID,
};
use objc2_foundation::{NSArray, NSString};

/// kCGNullDirectDisplay: "mirror nothing".
const NO_DISPLAY: CGDirectDisplayID = 0;

pub struct VirtualDisplay {
    display: Retained<AnyObject>,
    pub id: CGDirectDisplayID,
    /// The physical display mirroring this one, unmirrored on drop.
    mirrored: Option<CGDirectDisplayID>,
}

// The object is only messaged from whichever thread owns the VirtualDisplay.
unsafe impl Send for VirtualDisplay {}

fn class(name: &str) -> Result<&'static AnyClass> {
    AnyClass::get(&std::ffi::CString::new(name)?)
        .with_context(|| format!("{name} isn't available on this macOS"))
}

impl VirtualDisplay {
    /// A virtual display of exactly `width`×`height` pixels (1x, 60 Hz).
    pub fn create(width: u32, height: u32) -> Result<Self> {
        anyhow::ensure!(
            (640..=7680).contains(&width) && (480..=4320).contains(&height),
            "unsupported size {width}×{height}"
        );
        unsafe {
            let descriptor: Retained<AnyObject> =
                msg_send![class("CGVirtualDisplayDescriptor")?, new];
            let _: () = msg_send![&descriptor, setName: &*NSString::from_str("Glass")];
            let _: () = msg_send![&descriptor, setMaxPixelsWide: width];
            let _: () = msg_send![&descriptor, setMaxPixelsHigh: height];
            // A plausible physical size (~110 ppi) so macOS picks sensible UI scaling.
            let mm = CGSize {
                width: width as f64 / 110.0 * 25.4,
                height: height as f64 / 110.0 * 25.4,
            };
            let _: () = msg_send![&descriptor, setSizeInMillimeters: mm];
            let _: () = msg_send![&descriptor, setVendorID: 0xF1A5u32];
            let _: () = msg_send![&descriptor, setProductID: 0x0001u32];
            let _: () = msg_send![&descriptor, setSerialNum: 0x0001u32];
            let _: () = msg_send![&descriptor, setRedPrimary: CGPoint { x: 0.64, y: 0.33 }];
            let _: () = msg_send![&descriptor, setGreenPrimary: CGPoint { x: 0.30, y: 0.60 }];
            let _: () = msg_send![&descriptor, setBluePrimary: CGPoint { x: 0.15, y: 0.06 }];
            let _: () = msg_send![&descriptor, setWhitePoint: CGPoint { x: 0.3127, y: 0.3290 }];

            let alloc: Allocated<AnyObject> = msg_send![class("CGVirtualDisplay")?, alloc];
            let display: Option<Retained<AnyObject>> =
                msg_send![alloc, initWithDescriptor: &*descriptor];
            let display = display.context("couldn't create the virtual display")?;

            let mode_alloc: Allocated<AnyObject> = msg_send![class("CGVirtualDisplayMode")?, alloc];
            let mode: Retained<AnyObject> = msg_send![
                mode_alloc,
                initWithWidth: width as usize,
                height: height as usize,
                refreshRate: 60.0f64
            ];
            let settings: Retained<AnyObject> = msg_send![class("CGVirtualDisplaySettings")?, new];
            let _: () = msg_send![&settings, setHiDPI: 0u32];
            let modes = NSArray::from_retained_slice(&[mode]);
            let _: () = msg_send![&settings, setModes: &*modes];
            let applied: Bool = msg_send![&display, applySettings: &*settings];
            if !applied.as_bool() {
                bail!("macOS rejected the virtual display's mode ({width}×{height})");
            }
            let id: CGDirectDisplayID = msg_send![&display, displayID];
            let _ = sel!(displayID);
            Ok(Self {
                display,
                id,
                mirrored: None,
            })
        }
    }

    /// Makes the main display mirror this one, so the desktop takes this display's shape.
    pub fn mirror_main(&mut self) -> Result<()> {
        let main = CGMainDisplayID();
        configure(|cfg| unsafe { CGConfigureDisplayMirrorOfDisplay(cfg, main, self.id) })?;
        self.mirrored = Some(main);
        Ok(())
    }
}

impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        if let Some(main) = self.mirrored.take()
            && let Err(e) =
                configure(|cfg| unsafe { CGConfigureDisplayMirrorOfDisplay(cfg, main, NO_DISPLAY) })
        {
            tracing::warn!("unmirroring the main display: {e:#}");
        }
        tracing::info!("virtual display {} removed", self.id);
        let _ = &self.display; // released here: macOS removes the display
    }
}

/// Applies one display configuration change for this login session.
fn configure(change: impl FnOnce(CGDisplayConfigRef) -> CGError) -> Result<()> {
    let mut cfg: CGDisplayConfigRef = std::ptr::null_mut();
    let err = unsafe { CGBeginDisplayConfiguration(&mut cfg) };
    anyhow::ensure!(
        err == CGError::Success,
        "CGBeginDisplayConfiguration: {err:?}"
    );
    let err = change(cfg);
    anyhow::ensure!(err == CGError::Success, "display configuration: {err:?}");
    let err = unsafe { CGCompleteDisplayConfiguration(cfg, CGConfigureOption::ForSession) };
    anyhow::ensure!(
        err == CGError::Success,
        "CGCompleteDisplayConfiguration: {err:?}"
    );
    Ok(())
}

/// `glass display-test WxH`: creates a virtual display, mirrors the main display onto it for
/// a few seconds, then restores, and checks the displays came back as they were.
pub fn self_test(width: u32, height: u32, seconds: u64) -> Result<()> {
    let before = super::capture::list_displays()?
        .into_iter()
        .map(|d| d.info)
        .collect::<Vec<_>>();
    println!("Displays now: {}", describe(&before));
    {
        let mut vd = VirtualDisplay::create(width, height)?;
        println!("Created virtual display {} at {width}×{height}.", vd.id);
        vd.mirror_main()?;
        println!("Main display now mirrors it; restoring in {seconds} s…");
        std::thread::sleep(std::time::Duration::from_secs(seconds));
    }
    std::thread::sleep(std::time::Duration::from_secs(2));
    let after = super::capture::list_displays()?
        .into_iter()
        .map(|d| d.info)
        .collect::<Vec<_>>();
    println!("Displays after: {}", describe(&after));
    let same = before.len() == after.len()
        && before.iter().all(|b| {
            after
                .iter()
                .any(|a| a.id == b.id && a.width == b.width && a.height == b.height)
        });
    if same {
        println!("Restored: the displays are exactly as before.");
        Ok(())
    } else {
        bail!("the displays differ from before; check System Settings › Displays")
    }
}

fn describe(list: &[crate::media::DisplayInfo]) -> String {
    list.iter()
        .map(|d| {
            format!(
                "{} {}×{}{}",
                d.id,
                d.width,
                d.height,
                if d.main { " (main)" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}
