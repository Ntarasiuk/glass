//! Game controllers from the viewer (browser Gamepad API, standard mapping), as virtual
//! controllers on the host.
//!
//! TODO(gamepad agent): Linux uinput pads. Interface is fixed.

pub fn supported() -> bool {
    crate::testmode::enabled()
}

pub fn handle(ev: glass_proto::InputEvent) {
    let _ = ev;
}

/// A session ended: let go of its pads.
pub fn release_all() {}
