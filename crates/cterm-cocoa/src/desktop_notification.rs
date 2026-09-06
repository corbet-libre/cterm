//! Native macOS desktop notifications through UserNotifications.framework.
//!
//! Shared-model delivery for Kitty OSC 99: occasion (`o=`) filtering,
//! expiry (`w=`) auto-close, and `a=report` / `c=1` reports generated with
//! the shared `cterm_core::notifications` builders and written back through
//! the per-view report sink (mirror terminal write path → daemon PTY).
//!
//! Platform notes (see the per-platform matrix in `docs/parity.md`):
//! - UserNotifications has no dismiss signal, so `c=1` notifications get an
//!   immediate `p=close;untracked` disclaimer (poll `p=alive` instead) plus
//!   a real `p=close` when the terminal itself closes the notification
//!   (activation, expiry, or app close).
//! - Buttons degrade: macOS surfaces notification actions as a hover menu,
//!   so button labels are kept in the shared model and activation reports
//!   without a button number.
//! - Custom icons degrade: transmitted bytes/names are kept in the shared
//!   model (a `UNNotificationAttachment` file-URL hop is the open path).
//! - Named `s=` sounds degrade to the default sound; `silent` suppresses it.
//!
//! NOTE (seam-reuse, uncompiled on Linux): this module follows the same
//! registry + report-sink pattern as the GTK delivery so all three
//! frontends share one core state machine. macOS builds must verify.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSRunningApplication};
use objc2_foundation::{MainThreadMarker, NSArray, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
    UNNotificationInterruptionLevel, UNNotificationPresentationOptions, UNNotificationRequest,
    UNNotificationResponse, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

static NEXT_NOTIFICATION_ID: AtomicU64 = AtomicU64::new(1);
static NOTIFICATIONS_UNAVAILABLE: AtomicBool = AtomicBool::new(false);
static ACTIVE_NOTIFICATION_IDS: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static FOCUS_NOTIFICATION_IDS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
/// Report preferences + sink per native notification ID, mirroring the GTK
/// delivery registry so activation/close reports share one core model.
static REPORT_REGISTRY: LazyLock<Mutex<HashMap<String, ReportEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Writes one Kitty report back to the issuing PTY (mirror terminal write
/// path → daemon input channel). Provided by `terminal_view.rs` per view.
pub type ReportSink = std::sync::Arc<dyn Fn(&[u8]) + Send + Sync>;

#[derive(Clone)]
struct ReportEntry {
    protocol_id: Option<String>,
    report_activation: bool,
    report_close: bool,
    sink: ReportSink,
}

struct NotificationDelegateIvars;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "CtermNotificationDelegate"]
    #[ivars = NotificationDelegateIvars]
    struct NotificationDelegate;

    unsafe impl NSObjectProtocol for NotificationDelegate {}

    unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &block2::DynBlock<dyn Fn()>,
        ) {
            let native_id = response.notification().request().identifier().to_string();
            let focus = FOCUS_NOTIFICATION_IDS
                .lock()
                .map(|mut ids| ids.remove(&native_id))
                .unwrap_or(false);
            if let Ok(mut active) = ACTIVE_NOTIFICATION_IDS.lock() {
                active.retain(|_, current| current != &native_id);
            }
            let report = REPORT_REGISTRY
                .lock()
                .ok()
                .and_then(|mut registry| registry.remove(&native_id));
            if focus {
                let app = NSApplication::sharedApplication(MainThreadMarker::from(self));
                app.activateIgnoringOtherApps(true);
            }
            // Activation first, then close when both were requested — the
            // Kitty `a=report` + `c=1` contract.
            if let Some(entry) = report {
                if entry.report_activation {
                    (entry.sink)(&cterm_core::notifications::activation_report(
                        entry.protocol_id.as_deref(),
                        None,
                    ));
                }
                if entry.report_close {
                    (entry.sink)(&cterm_core::notifications::close_report(
                        entry.protocol_id.as_deref(),
                        false,
                    ));
                }
            }
            completion.call(());
        }
    }
);

impl NotificationDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc::<Self>();
        let this = this.set_ivars(NotificationDelegateIvars);
        unsafe { msg_send![super(this), init] }
    }
}

thread_local! {
    static NOTIFICATION_DELEGATE: RefCell<Option<Retained<NotificationDelegate>>> =
        const { RefCell::new(None) };
}

fn is_app_bundle_executable(path: &Path) -> bool {
    let Some(macos_directory) = path.parent() else {
        return false;
    };
    if macos_directory.file_name().and_then(|name| name.to_str()) != Some("MacOS") {
        return false;
    }

    let Some(contents_directory) = macos_directory.parent() else {
        return false;
    };
    if contents_directory
        .file_name()
        .and_then(|name| name.to_str())
        != Some("Contents")
    {
        return false;
    }

    contents_directory
        .parent()
        .and_then(Path::extension)
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
}

/// UserNotifications requires a LaunchServices-backed application bundle.
/// Calling `currentNotificationCenter` from a raw executable raises an
/// Objective-C exception on recent macOS releases instead of returning an
/// error, so reject such processes before entering the framework.
fn notification_center() -> Option<Retained<UNUserNotificationCenter>> {
    if NOTIFICATIONS_UNAVAILABLE.load(Ordering::Relaxed) {
        return None;
    }

    let executable_is_bundled = std::env::current_exe()
        .ok()
        .as_deref()
        .is_some_and(is_app_bundle_executable);
    if !executable_is_bundled {
        NOTIFICATIONS_UNAVAILABLE.store(true, Ordering::Relaxed);
        log::debug!("macOS desktop notifications are unavailable outside a .app bundle");
        return None;
    }

    let running_application = NSRunningApplication::currentApplication();
    let launch_services_recognizes_bundle = running_application.bundleIdentifier().is_some()
        && running_application.bundleURL().is_some();
    if !launch_services_recognizes_bundle {
        NOTIFICATIONS_UNAVAILABLE.store(true, Ordering::Relaxed);
        log::debug!(
            "macOS desktop notifications are unavailable because LaunchServices did not register the .app bundle"
        );
        return None;
    }

    Some(UNUserNotificationCenter::currentNotificationCenter())
}

/// Ask once during application startup so the first terminal notification is
/// not silently dropped while macOS is still presenting its permission sheet.
pub fn request_authorization() {
    let Some(center) = notification_center() else {
        return;
    };
    NOTIFICATION_DELEGATE.with(|stored| {
        let mut stored = stored.borrow_mut();
        let delegate = stored.get_or_insert_with(|| {
            NotificationDelegate::new(MainThreadMarker::new().expect("main thread"))
        });
        center.setDelegate(Some(ProtocolObject::from_ref(&**delegate)));
    });
    let completion = RcBlock::new(
        |granted: objc2::runtime::Bool, _error: *mut objc2_foundation::NSError| {
            if !granted.as_bool() {
                log::info!("macOS desktop notification permission was not granted");
            }
        },
    );
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
        &completion,
    );
}

pub fn handle(action: &cterm_core::DesktopNotificationAction, report: ReportSink) {
    match action {
        cterm_core::DesktopNotificationAction::Show(notification) => show(notification, report),
        cterm_core::DesktopNotificationAction::Close(id) => close(id),
    }
}

fn show(notification: &cterm_core::DesktopNotification, report: ReportSink) {
    // Occasion (`o=`) filtering against best-effort native state. Tab
    // visibility is owned by the view layer; here `invisible` degrades to
    // `unfocused` (documented in the parity matrix).
    let is_focused = NSRunningApplication::currentApplication().isActive();
    if !cterm_core::notifications::should_deliver(notification.occasion, is_focused, true) {
        log::debug!(
            "Suppressing desktop notification {:?} for occasion {:?} (focused={is_focused})",
            notification.id,
            notification.occasion,
        );
        return;
    }
    if !notification.buttons.is_empty() {
        // Button labels stay in the shared model; activation reports without
        // a button number (hover-menu actions are the open path).
        log::debug!(
            "Ignoring {} notification button(s) on UserNotifications",
            notification.buttons.len()
        );
    }
    if notification.icon_names.iter().any(|name| !name.is_empty())
        || notification.icon_data.is_some()
    {
        // Custom icons degrade to the application icon (attachment file-URL
        // hop is the open path).
        log::debug!("Ignoring custom notification icon on UserNotifications");
    }
    if let Some(sound) = notification.sound_name.as_deref() {
        // Named sounds degrade to the default sound.
        log::debug!("Ignoring named notification sound {sound:?} on UserNotifications");
    }
    let Some(center) = notification_center() else {
        return;
    };

    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(&notification.title));
    content.setBody(&NSString::from_str(&notification.body));
    content.setInterruptionLevel(match notification.urgency {
        cterm_core::NotificationUrgency::Low => UNNotificationInterruptionLevel::Passive,
        cterm_core::NotificationUrgency::Normal => UNNotificationInterruptionLevel::Active,
        cterm_core::NotificationUrgency::Critical => UNNotificationInterruptionLevel::TimeSensitive,
    });
    if !notification.muted {
        let sound = UNNotificationSound::defaultSound();
        content.setSound(Some(&sound));
    }

    let sequence = NEXT_NOTIFICATION_ID.fetch_add(1, Ordering::Relaxed);
    let native_id = format!("cterm-{}-{sequence}", std::process::id());
    if notification.focus {
        if let Ok(mut ids) = FOCUS_NOTIFICATION_IDS.lock() {
            if ids.len() >= 128 {
                if let Some(oldest) = ids.iter().next().cloned() {
                    ids.remove(&oldest);
                }
            }
            ids.insert(native_id.clone());
        }
    }
    if let Some(protocol_id) = notification.id.as_ref() {
        let obsolete = ACTIVE_NOTIFICATION_IDS
            .lock()
            .map(|mut active| {
                let mut obsolete = active
                    .insert(protocol_id.clone(), native_id.clone())
                    .into_iter()
                    .collect::<Vec<_>>();
                if active.len() > 64 {
                    if let Some(evicted) = active.keys().find(|id| *id != protocol_id).cloned() {
                        if let Some(native_id) = active.remove(&evicted) {
                            obsolete.push(native_id);
                        }
                    }
                }
                obsolete
            })
            .unwrap_or_default();
        for obsolete_id in obsolete {
            close_native(&obsolete_id);
        }
    }

    let identifier = NSString::from_str(&native_id);
    let request =
        UNNotificationRequest::requestWithIdentifier_content_trigger(&identifier, &content, None);
    center.addNotificationRequest_withCompletionHandler(&request, None);

    // Track reporting preferences for the delegate callback.
    if let Ok(mut registry) = REPORT_REGISTRY.lock() {
        if registry.len() >= 128 {
            if let Some(oldest) = registry.keys().next().cloned() {
                registry.remove(&oldest);
            }
        }
        registry.insert(
            native_id.clone(),
            ReportEntry {
                protocol_id: notification.id.clone(),
                report_activation: notification.report_activation,
                report_close: notification.report_close,
                sink: report.clone(),
            },
        );
    }
    // No OS dismiss signal exists here: tell the app up front that dismissal
    // is untracked (it should poll `p=alive`). A real `p=close` still follows
    // terminal-initiated closes (activation, expiry) below.
    if notification.report_close {
        report(&cterm_core::notifications::close_report(
            notification.id.as_deref(),
            true,
        ));
    }

    if let Some(milliseconds) = notification.expire_time.filter(|value| *value > 0) {
        let protocol_id = notification.id.clone();
        let native_id_owned = native_id.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(milliseconds as u64));
            if let Some(protocol_id) = protocol_id {
                let should_close = ACTIVE_NOTIFICATION_IDS
                    .lock()
                    .map(|mut active| {
                        if active.get(&protocol_id) == Some(&native_id_owned) {
                            active.remove(&protocol_id);
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                if !should_close {
                    return;
                }
            }
            let report_close = REPORT_REGISTRY
                .lock()
                .ok()
                .and_then(|mut registry| registry.remove(&native_id_owned))
                .map(|entry| (entry.report_close, entry.protocol_id, entry.sink));
            close_native(&native_id_owned);
            // Terminal-enforced expiry is an observed close.
            if let Some((true, protocol_id, sink)) = report_close {
                sink(&cterm_core::notifications::close_report(
                    protocol_id.as_deref(),
                    false,
                ));
            }
        });
    }
}

fn close(protocol_id: &str) {
    let native_id = ACTIVE_NOTIFICATION_IDS
        .lock()
        .ok()
        .and_then(|mut active| active.remove(protocol_id));
    if let Some(native_id) = native_id {
        if let Ok(mut registry) = REPORT_REGISTRY.lock() {
            registry.remove(&native_id);
        }
        close_native(&native_id);
    }
}

fn close_native(native_id: &str) {
    if let Ok(mut ids) = FOCUS_NOTIFICATION_IDS.lock() {
        ids.remove(native_id);
    }
    if let Ok(mut registry) = REPORT_REGISTRY.lock() {
        registry.remove(native_id);
    }
    let identifier = NSString::from_str(native_id);
    let identifiers = NSArray::from_slice(&[&*identifier]);
    let Some(center) = notification_center() else {
        return;
    };
    center.removePendingNotificationRequestsWithIdentifiers(&identifiers);
    center.removeDeliveredNotificationsWithIdentifiers(&identifiers);
}

#[cfg(test)]
mod tests {
    use super::is_app_bundle_executable;
    use std::path::Path;

    #[test]
    fn recognizes_app_bundle_executable_paths() {
        assert!(is_app_bundle_executable(Path::new(
            "/Applications/cterm.app/Contents/MacOS/cterm"
        )));
        assert!(is_app_bundle_executable(Path::new(
            "/tmp/Test.APP/Contents/MacOS/cterm"
        )));
    }

    #[test]
    fn rejects_raw_and_malformed_executable_paths() {
        assert!(!is_app_bundle_executable(Path::new(
            "/tmp/cterm/target/debug/cterm"
        )));
        assert!(!is_app_bundle_executable(Path::new(
            "/tmp/cterm.app/MacOS/cterm"
        )));
        assert!(!is_app_bundle_executable(Path::new(
            "/tmp/cterm.app/Contents/cterm"
        )));
    }
}
