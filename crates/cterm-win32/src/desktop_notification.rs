//! Native Windows notification balloons with Kitty replacement and close IDs.
//!
//! Shared-model delivery for Kitty OSC 99: occasion (`o=`) filtering,
//! expiry (`w=`) auto-close, and `a=report` / `c=1` reports generated with
//! the shared `cterm_core::notifications` builders and written back through
//! the per-pane report sink (mirror terminal write path → daemon PTY).
//!
//! Platform notes (see the per-platform matrix in `docs/parity.md`):
//! - Balloons have no buttons: `p=buttons` is accepted by the core parser
//!   but has no native surface here (toast migration is the open path).
//! - Icons degrade to the application icon: transmitted bytes/names are kept
//!   in the model for other frontends.
//! - Named `s=` sounds degrade to the default balloon sound; `silent` maps
//!   to `NIIF_NOSOUND`. Unlike GTK/Cocoa, balloon hide/timeout/click events
//!   give real dismiss tracking, so no `untracked` disclaimer is sent.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_ERROR, NIIF_INFO,
    NIIF_NOSOUND, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIN_BALLOONHIDE, NIN_BALLOONTIMEOUT,
    NIN_BALLOONUSERCLICK, NOTIFYICONDATAW, NOTIFYICONDATAW_0,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, IsWindowVisible, LoadIconW, SetForegroundWindow, ShowWindow,
    IDI_APPLICATION, SW_RESTORE,
};

use crate::window::WM_APP_NATIVE_NOTIFICATION;

static NEXT_NOTIFICATION_ID: AtomicU32 = AtomicU32::new(1);
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
static ACTIVE_NOTIFICATIONS: LazyLock<Mutex<HashMap<u32, ActiveNotification>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone)]
struct ActiveNotification {
    hwnd: usize,
    protocol_id: Option<String>,
    generation: u64,
    focus: bool,
    report_activation: bool,
    report_close: bool,
    report_sink: Option<ReportSink>,
}

/// Writes one Kitty report back to the issuing PTY (mirror terminal write
/// path → daemon input channel). Provided by `window.rs` per pane.
pub type ReportSink = std::sync::Arc<dyn Fn(&[u8]) + Send + Sync>;

pub fn handle(hwnd: HWND, action: &cterm_core::DesktopNotificationAction, report: ReportSink) {
    match action {
        cterm_core::DesktopNotificationAction::Show(notification) => {
            show(hwnd, notification, report);
        }
        cterm_core::DesktopNotificationAction::Close(id) => close(hwnd, id),
    }
}

pub fn native_event(hwnd: HWND, native_id: u32, event: u32) {
    if !matches!(
        event,
        NIN_BALLOONUSERCLICK | NIN_BALLOONHIDE | NIN_BALLOONTIMEOUT
    ) {
        return;
    }

    let notification = ACTIVE_NOTIFICATIONS
        .lock()
        .ok()
        .and_then(|mut active| active.remove(&native_id));
    let Some(notification) = notification else {
        return;
    };
    if notification.hwnd != hwnd.0 as usize {
        return;
    }

    if event == NIN_BALLOONUSERCLICK {
        if notification.focus {
            unsafe {
                let _ = ShowWindow(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
            }
        }
        if notification.report_activation {
            let report = cterm_core::notifications::activation_report(
                notification.protocol_id.as_deref(),
                None,
            );
            if let Some(sink) = notification.report_sink.as_ref() {
                sink(&report);
            }
            // Per the Kitty spec, an activated notification with `c=1` sends
            // both the activation and the close report.
            if notification.report_close {
                let closed = cterm_core::notifications::close_report(
                    notification.protocol_id.as_deref(),
                    false,
                );
                if let Some(sink) = notification.report_sink.as_ref() {
                    sink(&closed);
                }
            }
        } else if notification.report_close {
            // Dismiss/timeout without activation still closes the report loop.
            let closed =
                cterm_core::notifications::close_report(notification.protocol_id.as_deref(), false);
            if let Some(sink) = notification.report_sink.as_ref() {
                sink(&closed);
            }
        }
    } else if notification.report_close {
        let closed =
            cterm_core::notifications::close_report(notification.protocol_id.as_deref(), false);
        if let Some(sink) = notification.report_sink.as_ref() {
            sink(&closed);
        }
    }
    remove_native(hwnd, native_id);
}

fn show(hwnd: HWND, notification: &cterm_core::DesktopNotification, report: ReportSink) {
    // Occasion (`o=`) filtering against best-effort native state.
    let (is_focused, is_visible) = unsafe {
        (
            GetForegroundWindow() == hwnd,
            IsWindowVisible(hwnd).as_bool(),
        )
    };
    if !cterm_core::notifications::should_deliver(notification.occasion, is_focused, is_visible) {
        log::debug!(
            "Suppressing desktop notification {:?} for occasion {:?} (focused={is_focused}, visible={is_visible})",
            notification.id,
            notification.occasion,
        );
        return;
    }
    if !notification.buttons.is_empty() {
        // Balloons expose no button surface; the request stays in the shared
        // model so other frontends (and future toast migration) honor it.
        log::debug!(
            "Ignoring {} notification button(s) on Win32 balloons",
            notification.buttons.len()
        );
    }
    if notification.icon_names.iter().any(|name| !name.is_empty())
        || notification.icon_data.is_some()
    {
        // Custom icons degrade to the application icon on balloons.
        log::debug!("Ignoring custom notification icon on Win32 balloons");
    }
    if let Some(sound) = notification.sound_name.as_deref() {
        // Named sounds degrade to the default balloon sound.
        log::debug!("Ignoring named notification sound {sound:?} on Win32 balloons");
    }
    let hwnd_value = hwnd.0 as usize;
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    let (native_id, replace) = {
        let mut active = ACTIVE_NOTIFICATIONS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let existing = notification.id.as_ref().and_then(|protocol_id| {
            active.iter().find_map(|(native_id, current)| {
                (current.hwnd == hwnd_value && current.protocol_id.as_ref() == Some(protocol_id))
                    .then_some(*native_id)
            })
        });
        let native_id = existing.unwrap_or_else(next_native_id);
        active.insert(
            native_id,
            ActiveNotification {
                hwnd: hwnd_value,
                protocol_id: notification.id.clone(),
                generation,
                focus: notification.focus,
                report_activation: notification.report_activation,
                report_close: notification.report_close,
                report_sink: Some(report.clone()),
            },
        );
        (native_id, existing.is_some())
    };

    let mut info_flags = match notification.urgency {
        cterm_core::NotificationUrgency::Low | cterm_core::NotificationUrgency::Normal => NIIF_INFO,
        cterm_core::NotificationUrgency::Critical => NIIF_ERROR,
    };
    if notification.muted {
        info_flags |= NIIF_NOSOUND;
    }

    let timeout = notification
        .expire_time
        .filter(|milliseconds| *milliseconds > 0)
        .map_or(12_000, |milliseconds| milliseconds as u32);
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: native_id,
        uFlags: NIF_ICON | NIF_TIP | NIF_INFO | NIF_MESSAGE,
        uCallbackMessage: WM_APP_NATIVE_NOTIFICATION,
        hIcon: unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default(),
        Anonymous: NOTIFYICONDATAW_0 { uTimeout: timeout },
        dwInfoFlags: info_flags,
        ..Default::default()
    };
    copy_utf16("cterm", &mut data.szTip);
    copy_utf16(&notification.title, &mut data.szInfoTitle);
    copy_utf16(&notification.body, &mut data.szInfo);

    let message = if replace { NIM_MODIFY } else { NIM_ADD };
    if !unsafe { Shell_NotifyIconW(message, &data) }.as_bool() {
        log::warn!("Windows rejected a terminal desktop notification");
        remove_if_current(native_id, generation);
        return;
    }

    let report_sink = report.clone();
    let protocol_id = notification.id.clone();
    let report_close = notification.report_close;
    // `w=0` never expires: schedule no terminal timer, so the balloon
    // persists until replaced, closed, evicted, or frontend exit — matching
    // GTK/Cocoa (`never_expires`). All other cases keep the timer above.
    if cterm_core::notifications::never_expires(notification.expire_time) {
        return;
    }
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(timeout as u64));
        if remove_if_current(native_id, generation) {
            remove_native(HWND(hwnd_value as *mut _), native_id);
            // Terminal-enforced expiry is a close the terminal observes, so
            // it gets a real (tracked) close report, unlike user dismissals
            // on stacks without OS signals.
            if report_close {
                report_sink(&cterm_core::notifications::close_report(
                    protocol_id.as_deref(),
                    false,
                ));
            }
        }
    });
}

fn close(hwnd: HWND, protocol_id: &str) {
    let hwnd_value = hwnd.0 as usize;
    let native_ids = ACTIVE_NOTIFICATIONS
        .lock()
        .map(|mut active| {
            let ids: Vec<_> = active
                .iter()
                .filter_map(|(native_id, notification)| {
                    (notification.hwnd == hwnd_value
                        && notification.protocol_id.as_deref() == Some(protocol_id))
                    .then_some(*native_id)
                })
                .collect();
            for native_id in &ids {
                active.remove(native_id);
            }
            ids
        })
        .unwrap_or_default();
    for native_id in native_ids {
        remove_native(hwnd, native_id);
    }
}

fn remove_if_current(native_id: u32, generation: u64) -> bool {
    ACTIVE_NOTIFICATIONS
        .lock()
        .map(|mut active| {
            if active.get(&native_id).map(|entry| entry.generation) == Some(generation) {
                active.remove(&native_id);
                true
            } else {
                false
            }
        })
        .unwrap_or(false)
}

fn remove_native(hwnd: HWND, native_id: u32) {
    let remove = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: native_id,
        ..Default::default()
    };
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &remove);
    }
}

fn next_native_id() -> u32 {
    NEXT_NOTIFICATION_ID.fetch_add(1, Ordering::Relaxed).max(1)
}

fn copy_utf16<const N: usize>(value: &str, destination: &mut [u16; N]) {
    let max = destination.len().saturating_sub(1);
    for (target, unit) in destination.iter_mut().take(max).zip(value.encode_utf16()) {
        *target = unit;
    }
}
