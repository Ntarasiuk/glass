//! The Glass menu bar icon: lit (with a count) while anyone is viewing this Mac, and a
//! menu listing who, with controls to end it. Runs the app's main thread; the host itself
//! runs on a background tokio runtime.

use std::cell::RefCell;

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSColor, NSImage, NSMenu, NSMenuItem,
    NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSString, ns_string};

use crate::viewers::{self, Viewer};

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GlassMenuTarget"]
    struct MenuTarget;

    unsafe impl NSObjectProtocol for MenuTarget {}

    impl MenuTarget {
        #[unsafe(method(disconnectAll:))]
        fn disconnect_all(&self, _sender: Option<&AnyObject>) {
            viewers::disconnect_all();
        }

        #[unsafe(method(toggleRemote:))]
        fn toggle_remote(&self, _sender: Option<&AnyObject>) {
            let _ = crate::remote::set_enabled(!crate::remote::enabled());
            refresh_now();
        }

        #[unsafe(method(revokeLinks:))]
        fn revoke_links(&self, _sender: Option<&AnyObject>) {
            let file = crate::access::file_for(&crate::default_token_file());
            match crate::access::revoke_all(&file) {
                Ok(n) => tracing::info!("revoked links from the menu bar ({n} guest passes)"),
                Err(e) => tracing::warn!("revoking links: {e:#}"),
            }
        }

        #[unsafe(method(quitGlass:))]
        fn quit_glass(&self, _sender: Option<&AnyObject>) {
            // A clean exit: the login item restarts Glass only after a crash, so it stays
            // quit until the next login (or `glass setup`). Viewers see it disconnect.
            tracing::info!("quit from the menu bar");
            viewers::disconnect_all();
            std::thread::sleep(std::time::Duration::from_millis(200));
            std::process::exit(0);
        }

        #[unsafe(method(openLogs:))]
        fn open_logs(&self, _sender: Option<&AnyObject>) {
            let _ = std::process::Command::new("open").arg(super::log_dir()).spawn();
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

struct Ui {
    item: Retained<NSStatusItem>,
    target: Retained<MenuTarget>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

/// Shows the icon and runs the app on this (the main) thread. Never returns.
pub fn run(rt: tokio::runtime::Handle) -> ! {
    let mtm = MainThreadMarker::new().expect("the menu bar runs on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    // A menu bar item only: no Dock icon, no menu bar of its own.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    let target = MenuTarget::new(mtm);
    UI.with(|ui| *ui.borrow_mut() = Some(Ui { item, target }));
    refresh(&viewers::subscribe().borrow().clone());

    // Redraw when viewers come and go, and each minute (for the "3 min" labels).
    rt.spawn(async {
        let mut rx = viewers::subscribe();
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            tokio::select! {
                changed = rx.changed() => if changed.is_err() { return },
                _ = tick.tick() => {}
            }
            refresh_now();
        }
    });
    app.run();
    std::process::exit(0)
}

/// Redraws on the main thread with the current viewers.
fn refresh_now() {
    let list = viewers::subscribe().borrow().clone();
    DispatchQueue::main().exec_async(move || refresh(&list));
}

fn refresh(list: &[Viewer]) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    UI.with(|ui| {
        let ui = ui.borrow();
        let Some(ui) = ui.as_ref() else { return };
        let viewing = !list.is_empty();
        if let Some(button) = ui.item.button(mtm) {
            let symbol = ns_string!("display");
            let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                symbol,
                Some(ns_string!("Glass")),
            );
            if let Some(image) = &image {
                image.setTemplate(true);
            }
            button.setImage(image.as_deref());
            // Lit, with a count, while anyone is viewing.
            let tint = viewing.then(NSColor::systemBlueColor);
            button.setContentTintColor(tint.as_deref());
            let title = if viewing {
                format!(" {}", list.len())
            } else {
                String::new()
            };
            button.setTitle(&NSString::from_str(&title));
            let tip = if viewing {
                format!("Glass: {} viewing this Mac", list.len())
            } else {
                "Glass: no one is viewing this Mac".to_string()
            };
            button.setToolTip(Some(&NSString::from_str(&tip)));
        }
        ui.item.setMenu(Some(&menu(mtm, &ui.target, list)));
    });
}

fn menu(mtm: MainThreadMarker, target: &MenuTarget, list: &[Viewer]) -> Retained<NSMenu> {
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    let label = |text: &str| {
        let item = item(mtm, text, None, target);
        item.setEnabled(false);
        item
    };
    if list.is_empty() {
        menu.addItem(&label("No one is viewing this Mac"));
    } else {
        let heading = match list.len() {
            1 => "1 viewer".to_string(),
            n => format!("{n} viewers"),
        };
        menu.addItem(&label(&heading));
        for v in list {
            menu.addItem(&label(&format!("   {}", v.describe())));
        }
    }
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    let disconnect = item(
        mtm,
        "Disconnect everyone",
        Some(sel!(disconnectAll:)),
        target,
    );
    disconnect.setEnabled(!list.is_empty());
    menu.addItem(&disconnect);
    let remote = item(
        mtm,
        "Remote access from anywhere",
        Some(sel!(toggleRemote:)),
        target,
    );
    remote.setState(if crate::remote::enabled() { 1 } else { 0 });
    menu.addItem(&remote);
    menu.addItem(&item(
        mtm,
        "Revoke all guest links",
        Some(sel!(revokeLinks:)),
        target,
    ));
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu.addItem(&item(mtm, "Open logs", Some(sel!(openLogs:)), target));
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    let quit = item(mtm, "Quit Glass", Some(sel!(quitGlass:)), target);
    quit.setKeyEquivalent(ns_string!("q"));
    quit.setToolTip(Some(ns_string!(
        "Stops Glass until you next log in (or run glass setup)"
    )));
    menu.addItem(&quit);
    menu
}

fn item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<objc2::runtime::Sel>,
    target: &MenuTarget,
) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            ns_string!(""),
        )
    };
    if action.is_some() {
        unsafe { item.setTarget(Some(target)) };
    }
    item
}
