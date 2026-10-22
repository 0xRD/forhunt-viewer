//! macOS: files opened from Finder ("Open With", double click, drop on the
//! Dock icon) arrive as `application:openURLs:` on the application
//! delegate, not as command-line arguments. winit installs no delegate
//! (documented in `winit::platform::macos`), so we install one that queues
//! the paths for the app to open on its next frame.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use eframe::egui;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSApplicationDelegate};
use objc2_foundation::{NSArray, NSObject, NSObjectProtocol, NSURL};

static QUEUE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static CTX: OnceLock<egui::Context> = OnceLock::new();

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FhvAppDelegate"]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(application:openURLs:))]
        fn application_open_urls(&self, _app: &NSApplication, urls: &NSArray<NSURL>) {
            let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
            for url in urls.iter() {
                if let Some(p) = url.path() {
                    q.push(PathBuf::from(p.to_string()));
                }
            }
            if let Some(ctx) = CTX.get() {
                ctx.request_repaint();
            }
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// Installs the delegate (call from the app creation callback, on the main
/// thread, after the event loop exists).
pub fn install(ctx: &egui::Context) {
    let _ = CTX.set(ctx.clone());
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let delegate = AppDelegate::new(mtm);
    let app = NSApplication::sharedApplication(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // NSApplication holds its delegate weakly: keep ours for the process.
    std::mem::forget(delegate);
}

/// Files Finder asked us to open since the last call.
pub fn take_opened() -> Vec<PathBuf> {
    std::mem::take(&mut *QUEUE.lock().unwrap_or_else(|e| e.into_inner()))
}
