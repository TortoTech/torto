#![deny(unsafe_code)]

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos {
    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    use objc2::sel;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::{MainThreadMarker, NSArray, NSURL};
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    type Callback = Rc<dyn Fn(PathBuf)>;
    thread_local! {
        static CALLBACK: RefCell<Option<Callback>> = const { RefCell::new(None) };
    }
    unsafe extern "C" {
        fn class_addMethod(
            class: *const AnyClass,
            selector: Sel,
            implementation: Imp,
            types: *const std::ffi::c_char,
        ) -> Bool;
    }

    // AppKit invokes this on its existing delegate. No ivars, object class,
    // delegate identity or lifecycle methods are changed.
    unsafe extern "C" fn open_urls(
        _delegate: &AnyObject,
        _selector: Sel,
        _application: &NSApplication,
        urls: &NSArray<NSURL>,
    ) {
        let callback = CALLBACK.with(|slot| slot.borrow().clone());
        let Some(callback) = callback else {
            return;
        };
        for url in urls {
            if !unsafe { url.isFileURL() } {
                continue;
            }
            if let Some(path) = unsafe { url.path() } {
                callback(path.to_string().into());
            }
        }
    }

    /// Keeps the file-open callback alive on AppKit's main thread.
    pub struct OpenFileHandler {
        _main_thread: MainThreadMarker,
    }
    impl Drop for OpenFileHandler {
        fn drop(&mut self) {
            CALLBACK.with(|slot| slot.borrow_mut().take());
        }
    }

    /// Extends winit 0.30's delegate without replacing it. Call after building
    /// the event loop and before entering it.
    pub fn install(on_open: impl Fn(PathBuf) + 'static) -> Result<OpenFileHandler, std::io::Error> {
        let mtm = MainThreadMarker::new().ok_or_else(|| {
            std::io::Error::other("macOS application must start on the main thread")
        })?;
        if CALLBACK.with(|slot| slot.borrow().is_some()) {
            return Err(std::io::Error::other(
                "file-open callback is already installed",
            ));
        }
        let application = NSApplication::sharedApplication(mtm);
        // SAFETY: the main-thread marker above guarantees AppKit access here.
        let delegate = unsafe { application.delegate() }.ok_or_else(|| {
            std::io::Error::other(
                "create the window event loop before installing file-open handling",
            )
        })?;
        // SAFETY: every protocol object is an Objective-C object. Erasing the
        // protocol bound leaves its identity, ownership and runtime class intact.
        let delegate: objc2::rc::Retained<AnyObject> =
            unsafe { objc2::rc::Retained::cast(delegate) };
        let class = delegate.class();
        let selector = sel!(application:openURLs:);
        // SAFETY: void(id, SEL, NSApplication*, NSArray*) matches v@:@@ and the
        // AppKit protocol. The erased IMP is called only through that selector.
        let implementation: Imp = unsafe {
            std::mem::transmute::<
                unsafe extern "C" fn(&AnyObject, Sel, &NSApplication, &NSArray<NSURL>),
                Imp,
            >(open_urls)
        };
        if let Some(method) = class.instance_method(selector) {
            if !std::ptr::fn_addr_eq(method.implementation(), implementation) {
                return Err(std::io::Error::other(
                    "application delegate already handles file URLs",
                ));
            }
        } else {
            // SAFETY: main-thread registration of an absent method; never
            // replaces winit's delegate or any of its existing methods.
            let added =
                unsafe { class_addMethod(class, selector, implementation, c"v@:@@".as_ptr()) };
            if !added.as_bool() {
                return Err(std::io::Error::other(
                    "could not register file-open handling",
                ));
            }
        }
        CALLBACK.with(|slot| *slot.borrow_mut() = Some(Rc::new(on_open)));
        Ok(OpenFileHandler { _main_thread: mtm })
    }
}

#[cfg(target_os = "macos")]
pub use macos::{OpenFileHandler, install};
