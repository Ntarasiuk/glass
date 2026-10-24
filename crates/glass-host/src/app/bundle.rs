//! Glass.app in ~/Applications, so the app is in Spotlight, Launchpad and the Dock. The
//! bundle's executable is a link to the installed `glass`, which starts the app when it's
//! a two-line script that runs `glass app`; there's one binary, and updating it updates
//! the app.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub fn path() -> PathBuf {
    crate::home().join("Applications/Glass.app")
}

/// Writes (or refreshes) the bundle. Cheap when it's already current.
pub fn install() -> Result<PathBuf> {
    let app = path();
    let contents = app.join("Contents");
    let plist = info_plist();
    let plist_path = contents.join("Info.plist");
    let exe = contents.join("MacOS/Glass");
    let script = format!("#!/bin/sh\nexec \"{}\" app\n", crate::home().join(".glass/bin/glass").display());
    let current = std::fs::read_to_string(&plist_path).is_ok_and(|p| p == plist)
        && std::fs::read_to_string(&exe).is_ok_and(|t| t == script)
        && contents.join("Resources/Glass.icns").exists();
    if current {
        return Ok(app);
    }
    std::fs::create_dir_all(contents.join("MacOS"))?;
    std::fs::create_dir_all(contents.join("Resources"))?;
    std::fs::write(&plist_path, plist)?;
    let _ = std::fs::remove_file(&exe);
    std::fs::write(&exe, script).context("write the app's launcher")?;
    std::fs::set_permissions(&exe, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    icon(&contents.join("Resources/Glass.icns"))?;
    // Tell Launch Services (and so Spotlight and the Dock) about it.
    let _ = std::process::Command::new(
        "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister",
    )
    .args(["-f"])
    .arg(&app)
    .status();
    Ok(app)
}

fn info_plist() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Glass</string>
  <key>CFBundleDisplayName</key><string>Glass</string>
  <key>CFBundleIdentifier</key><string>dev.glassdesk.app</string>
  <key>CFBundleExecutable</key><string>Glass</string>
  <key>CFBundleIconFile</key><string>Glass</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>{v}</string>
  <key>CFBundleVersion</key><string>{v}</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
"#,
        v = crate::update::CURRENT
    )
}

/// The app icon, made from the embedded PNG with the system's own tools.
fn icon(out: &Path) -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("glass-icon-{}", std::process::id()));
    let set = tmp.join("Glass.iconset");
    std::fs::create_dir_all(&set)?;
    let src = tmp.join("icon.png");
    std::fs::write(&src, include_bytes!("../../assets/icon.png"))?;
    for (name, px) in [
        ("icon_16x16", 16),
        ("icon_16x16@2x", 32),
        ("icon_32x32", 32),
        ("icon_32x32@2x", 64),
        ("icon_128x128", 128),
        ("icon_128x128@2x", 256),
        ("icon_256x256", 256),
        ("icon_256x256@2x", 512),
        ("icon_512x512", 512),
    ] {
        let status = std::process::Command::new("sips")
            .args(["-z", &px.to_string(), &px.to_string()])
            .arg(&src)
            .arg("--out")
            .arg(set.join(format!("{name}.png")))
            .stdout(std::process::Stdio::null())
            .status()?;
        anyhow::ensure!(status.success(), "sips failed");
    }
    let status = std::process::Command::new("iconutil").args(["-c", "icns"]).arg(&set).arg("-o").arg(out).status()?;
    let _ = std::fs::remove_dir_all(&tmp);
    anyhow::ensure!(status.success(), "iconutil failed");
    Ok(())
}

/// Opens the app: the bundle when it's there (so it gets its Dock icon), else this binary.
pub fn open() {
    let app = path();
    if app.exists() {
        let _ = std::process::Command::new("open").arg(app).spawn();
    } else if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg("app").spawn();
    }
}
