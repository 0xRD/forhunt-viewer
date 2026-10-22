//! macOS: files opened from Finder ("Open With", double click, drop on the
//! Dock icon) arrive as `application:openURLs:` sent to the application
//! delegate, not as command-line arguments.
//!
//! winit installs its own delegate (`WinitApplicationDelegate`) and casts
//! `NSApp.delegate` back to that class, so the delegate must not be
//! replaced. Instead, `application:openURLs:` is added to `NSObject` at
//! process start, before the event loop exists: winit's delegate inherits
//! it, AppKit finds it through `respondsToSelector:` (including for the
//! files a launch was started with), and winit keeps working unchanged.
//! The method only queues the paths; the app opens them on its next frame.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use eframe::egui;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{ClassType, sel};
use objc2_foundation::{NSArray, NSObject, NSURL};

static QUEUE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static CTX: OnceLock<egui::Context> = OnceLock::new();

unsafe extern "C-unwind" fn open_urls(
    _this: *mut AnyObject,
    _cmd: Sel,
    _app: *mut AnyObject,
    urls: *mut NSArray<NSURL>,
) {
    // SAFETY: AppKit passes a valid NSArray<NSURL> for this selector.
    let Some(urls) = (unsafe { urls.as_ref() }) else {
        return;
    };
    let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    for url in urls.iter() {
        if let Some(p) = url.path() {
            q.push(PathBuf::from(p.to_string()));
        }
    }
    drop(q);
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}

/// Adds `application:openURLs:` to NSObject. Call first thing in `main`.
pub fn install_open_handler() {
    let cls: &AnyClass = NSObject::class();
    // SAFETY: the function matches the selector's signature
    // (void, id self, SEL _cmd, NSApplication *app, NSArray<NSURL *> *urls),
    // described by the type encoding "v@:@@". class_addMethod does nothing
    // if the class already has the method.
    unsafe {
        let imp: Imp = std::mem::transmute::<
            unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut NSArray<NSURL>),
            Imp,
        >(open_urls);
        objc2::ffi::class_addMethod(
            cls as *const AnyClass as *mut AnyClass,
            sel!(application:openURLs:),
            imp,
            c"v@:@@".as_ptr(),
        );
    }
}

/// Lets queued opens wake the UI.
pub fn set_context(ctx: &egui::Context) {
    let _ = CTX.set(ctx.clone());
}

/// Files Finder asked us to open since the last call.
pub fn take_opened() -> Vec<PathBuf> {
    std::mem::take(&mut *QUEUE.lock().unwrap_or_else(|e| e.into_inner()))
}
