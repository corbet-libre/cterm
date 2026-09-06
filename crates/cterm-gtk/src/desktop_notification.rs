//! Native desktop notification delivery through GApplication.
//!
//! Shared-model delivery for Kitty OSC 99: user filtering (`f=`/`t=`
//! allow/deny lists from `[notifications]`), occasion (`o=`) filtering,
//! icon names/data, buttons with per-button activation reports, expiry
//! (`w=`) auto-close with `w=0` never-expire honored (no timer scheduled),
//! and `a=report` / `c=1` reports written back through the mirror
//! terminal's daemon input channel (activation always precedes its close).
//!
//! Platform notes (see the per-platform matrix in `docs/parity.md`):
//! - Sounds degrade: `GNotification` exposes no sound control, so `silent`
//!   vs named sounds are accepted by the core parser but rendered with the
//!   OS default. Muted state is preserved in the model for other frontends.
//! - User dismiss has no OS signal on this stack, so `c=1` notifications get
//!   an immediate `p=close;untracked` disclaimer (poll `p=alive` instead) plus
//!   a real `p=close` when the terminal itself closes the notification
//!   (activation, expiry, replace, or app close).
//! - `w=0` persists in the registry until replaced, explicitly closed, or
//!   app exit; the OS may still retire the banner (best effort per Kitty).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use gio::prelude::ActionMapExt;
use gio::prelude::ApplicationExt;
use gio::NotificationPriority;
use glib::prelude::Cast;
use glib::prelude::StaticVariantType;
use glib::variant::ToVariant;
use gtk4::prelude::GtkWindowExt;

type TerminalRef = std::sync::Arc<parking_lot::Mutex<cterm_core::term::Terminal>>;

/// Report preferences + delivery handle for one live protocol ID.
struct RegistryEntry {
    terminal: TerminalRef,
    generation: u64,
    focus: bool,
    report_activation: bool,
    report_close: bool,
    button_count: usize,
    protocol_id: Option<String>,
}

static REPORT_REGISTRY: LazyLock<Mutex<HashMap<String, RegistryEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
static ACTIONS_REGISTERED: std::sync::Once = std::sync::Once::new();

/// Registry key: the protocol ID, or `""` for unidentified notifications
/// (all `i=0` reports).
fn registry_key(id: Option<&str>) -> String {
    id.unwrap_or("").to_owned()
}

fn ensure_actions_registered() {
    ACTIONS_REGISTERED.call_once(|| {
        let Some(application) = gio::Application::default() else {
            return;
        };
        let activate =
            gio::SimpleAction::new("notif-activate", Some(&String::static_variant_type()));
        activate.connect_activate(|_, parameter| {
            let id = parameter
                .and_then(|variant| variant.get::<String>())
                .unwrap_or_default();
            on_notification_activated(&id, None);
        });
        application.add_action(&activate);

        let button = gio::SimpleAction::new("notif-button", Some(&String::static_variant_type()));
        button.connect_activate(|_, parameter| {
            let Some(target) = parameter.and_then(|variant| variant.get::<String>()) else {
                return;
            };
            let (id, number) = target
                .split_once('\x1f')
                .map(|(id, number)| (id, number.parse::<usize>().ok()))
                .unwrap_or((target.as_str(), None));
            on_notification_activated(id, number);
        });
        application.add_action(&button);
    });
}

fn on_notification_activated(registry_id: &str, button: Option<usize>) {
    let entry = REPORT_REGISTRY.lock().ok().and_then(|registry| {
        registry.get(registry_id).map(|entry| {
            (
                entry.terminal.clone(),
                entry.generation,
                entry.focus,
                entry.report_activation,
                entry.report_close,
                entry.protocol_id.clone(),
                entry.button_count,
            )
        })
    });
    let Some((
        terminal,
        generation,
        focus,
        report_activation,
        report_close,
        protocol_id,
        button_count,
    )) = entry
    else {
        return;
    };
    // Ignore button numbers outside the advertised set (stale actions after
    // a replace with fewer buttons).
    if let Some(number) = button {
        if number == 0 || number > button_count.max(1) {
            return;
        }
    }
    if focus {
        // Present the issuing window: prefer the active toplevel, else the
        // first one (mirrors the existing `app.focus-terminal` behavior
        // without needing the typed GtkApplication).
        let toplevels = gtk4::Window::list_toplevels();
        let active = toplevels.iter().filter_map(|window| {
            window
                .downcast_ref::<gtk4::Window>()
                .filter(|window| window.is_active())
        });
        let first = toplevels
            .iter()
            .filter_map(|window| window.downcast_ref::<gtk4::Window>());
        if let Some(window) = active.into_iter().chain(first).next() {
            window.present();
        }
    }
    if report_activation || report_close {
        // Shared-model ordering: the activation report always precedes its
        // close report, matching the Cocoa/Win32 seams.
        for report in cterm_core::notifications::activation_close_sequence(
            protocol_id.as_deref(),
            button,
            report_activation,
            report_close,
        ) {
            if let Err(error) = terminal.lock().write(&report) {
                log::warn!("Failed to send notification activation report: {error}");
            }
        }
    }
    if report_close {
        // The terminal considers an activation-closed notification closed:
        // drop the current-generation entry and withdraw the banner so a
        // pending expiry timer cannot emit a duplicate close. A replace
        // installs a newer generation first, so only the current entry is
        // ever removed here.
        if let Ok(mut registry) = REPORT_REGISTRY.lock() {
            let current = registry
                .get(registry_id)
                .is_some_and(|entry| entry.generation == generation);
            if current {
                registry.remove(registry_id);
            }
        }
        if let Some(application) = gio::Application::default() {
            if let Some(id) = protocol_id.as_deref() {
                application.withdraw_notification(id);
            }
        }
    }
}

/// Deliver one parsed notification action, sending Kitty reports back through
/// `terminal` (mirror write path → daemon PTY). The user filter comes from
/// the operator `[notifications]` config; a missing/unreadable config fails
/// open to allow-all (today's behavior).
pub fn handle(action: &cterm_core::DesktopNotificationAction, terminal: &TerminalRef) {
    let filter = match cterm_app::config::load_config() {
        Ok(config) => config.notifications.to_filter(),
        Err(error) => {
            log::debug!("Notification filter unavailable ({error}); allowing all");
            cterm_core::NotificationFilter::default()
        }
    };
    handle_with_filter(action, terminal, &filter);
}

/// Deliver with an explicit user filter. Filtered `Show` actions return
/// before any GTK side effects: never shown, never registered for
/// activation, never reported (the daemon's optimistic `p=alive` set still
/// answers polls). App-initiated `Close` actions always pass through so a
/// banner can still be withdrawn.
pub fn handle_with_filter(
    action: &cterm_core::DesktopNotificationAction,
    terminal: &TerminalRef,
    filter: &cterm_core::NotificationFilter,
) {
    if let cterm_core::DesktopNotificationAction::Show(notification) = action {
        if !cterm_core::notifications::notification_allowed_full(
            filter,
            notification.app_name.as_deref(),
            &notification.notif_types,
            &notification.title,
            &notification.body,
        ) {
            log::debug!(
                "Suppressing desktop notification {:?} by user filter (app={:?}, types={:?})",
                notification.id,
                notification.app_name,
                notification.notif_types,
            );
            return;
        }
    }
    ensure_actions_registered();
    let Some(application) = gio::Application::default() else {
        log::warn!("Cannot deliver desktop notification before GApplication startup");
        return;
    };

    match action {
        cterm_core::DesktopNotificationAction::Show(notification) => {
            show(&application, terminal, notification);
        }
        cterm_core::DesktopNotificationAction::Close(id) => {
            close(&application, id);
        }
    }
}

fn show(
    application: &gio::Application,
    terminal: &TerminalRef,
    notification: &cterm_core::DesktopNotification,
) {
    // Occasion filtering (`o=`): skip delivery when the issuing window is
    // focused/visible. Filtered notifications are never shown and never
    // tracked for activation (the daemon's optimistic `p=alive` set still
    // answers polls).
    let is_focused = gtk4::Window::list_toplevels().iter().any(|window| {
        window
            .downcast_ref::<gtk4::Window>()
            .is_some_and(|window| window.is_active())
    });
    let is_visible = terminal.lock().screen().frontend_state().visibility
        == cterm_core::WindowVisibility::Visible;
    if !cterm_core::notifications::should_deliver(notification.occasion, is_focused, is_visible) {
        log::debug!(
            "Suppressing desktop notification {:?} for occasion {:?} (focused={is_focused}, visible={is_visible})",
            notification.id,
            notification.occasion,
        );
        return;
    }

    let native = gio::Notification::new(&notification.title);
    if !notification.body.is_empty() {
        native.set_body(Some(&notification.body));
    }
    native.set_priority(match notification.urgency {
        cterm_core::NotificationUrgency::Low => NotificationPriority::Low,
        cterm_core::NotificationUrgency::Normal => NotificationPriority::Normal,
        cterm_core::NotificationUrgency::Critical => NotificationPriority::Urgent,
    });
    // Icons: first available name wins per the Kitty spec, falling back to
    // transmitted bytes. Theme misses degrade to the default icon.
    if let Some(name) = notification.icon_names.iter().find(|name| !name.is_empty()) {
        native.set_icon(&gio::ThemedIcon::new(name));
    } else if let Some(data) = notification.icon_data.as_deref() {
        native.set_icon(&gio::BytesIcon::new(&glib::Bytes::from(data)));
    }
    // Note: `GNotification` has no sound control; `muted`/`sound_name` are
    // accepted by the core parser and honored on Cocoa/Win32.

    let key = registry_key(notification.id.as_deref());
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    let button_count = notification.buttons.len();
    // Activation: a targeted default action carries the protocol id so the
    // global handler can attribute the click (and focus) correctly.
    native.set_default_action_and_target_value("app.notif-activate", Some(&key.to_variant()));
    for (index, label) in notification.buttons.iter().enumerate() {
        let target = format!("{key}\x1f{}", index + 1);
        native.add_button_with_target_value(label, "app.notif-button", Some(&target.to_variant()));
    }

    if let Ok(mut registry) = REPORT_REGISTRY.lock() {
        registry.insert(
            key.clone(),
            RegistryEntry {
                terminal: terminal.clone(),
                generation,
                focus: notification.focus,
                report_activation: notification.report_activation,
                report_close: notification.report_close,
                button_count,
                protocol_id: notification.id.clone(),
            },
        );
    }
    application.send_notification(notification.id.as_deref(), &native);

    // Close reporting: GTK gets no user-dismiss signal, so tell the app up
    // front that dismissal is untracked (it should poll `p=alive`). A real
    // `p=close` still follows terminal-initiated closes below.
    if notification.report_close {
        let report = cterm_core::notifications::close_report(notification.id.as_deref(), true);
        if let Err(error) = terminal.lock().write(&report) {
            log::warn!("Failed to send notification untracked report: {error}");
        }
    }

    // Expiry (`w=`): terminal-enforced auto-close with a real close report.
    // `Never` (w=0) schedules no timer: the entry persists until replaced,
    // explicitly closed, or app exit (the OS may still retire the banner —
    // best effort per the Kitty spec). `OsDefault` (absent/negative)
    // likewise leaves the lifetime to the notification service.
    if let cterm_core::notifications::ExpiryPolicy::After(timeout_ms) =
        cterm_core::notifications::expiry_policy(notification.expire_time)
    {
        let protocol_id = notification.id.clone();
        let native_id = notification.id.clone();
        let terminal = terminal.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(timeout_ms), move || {
            let should_close = REPORT_REGISTRY
                .lock()
                .map(|registry| {
                    registry
                        .get(&registry_key(protocol_id.as_deref()))
                        .is_some_and(|entry| entry.generation == generation)
                })
                .unwrap_or(false);
            if !should_close {
                return;
            }
            if let Some(application) = gio::Application::default() {
                if let Some(id) = native_id.as_deref() {
                    application.withdraw_notification(id);
                } else {
                    // Unidentified notifications cannot be withdrawn by id;
                    // they age out of the registry only.
                }
            }
            let report_close = REPORT_REGISTRY
                .lock()
                .map(|mut registry| {
                    registry
                        .remove(&registry_key(protocol_id.as_deref()))
                        .map(|entry| entry.report_close)
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            if report_close {
                let report = cterm_core::notifications::close_report(protocol_id.as_deref(), false);
                if let Err(error) = terminal.lock().write(&report) {
                    log::warn!("Failed to send notification expiry report: {error}");
                }
            }
        });
    }
}

fn close(application: &gio::Application, id: &str) {
    application.withdraw_notification(id);
    if let Ok(mut registry) = REPORT_REGISTRY.lock() {
        registry.remove(id);
    }
    // App-initiated closes need no report (the app already knows). Terminal
    // replaces route through `show`, which sends `untracked` again when the
    // replacement requests close reports.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_terminal() -> TerminalRef {
        std::sync::Arc::new(parking_lot::Mutex::new(cterm_core::term::Terminal::new(
            80,
            24,
            cterm_core::screen::ScreenConfig::default(),
        )))
    }

    #[test]
    fn user_filter_denies_before_any_side_effect() {
        let terminal = test_terminal();
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = std::sync::Arc::clone(&written);
        terminal.lock().set_write_fn(Box::new(move |data: &[u8]| {
            sink.lock().unwrap().push(data.to_vec());
            Ok(())
        }));
        let filter = cterm_core::NotificationFilter {
            deny_apps: vec!["spammy".to_owned()],
            ..Default::default()
        };
        let denied = cterm_core::DesktopNotificationAction::Show(cterm_core::DesktopNotification {
            id: Some("gtk-filter-denied-1".to_owned()),
            title: "Hi".into(),
            app_name: Some("spammy".to_owned()),
            // Even with close reports requested, a filtered notification
            // must not emit the `untracked` disclaimer.
            report_close: true,
            ..Default::default()
        });
        handle_with_filter(&denied, &terminal, &filter);
        assert!(written.lock().unwrap().is_empty());
        assert!(!REPORT_REGISTRY
            .lock()
            .unwrap()
            .contains_key("gtk-filter-denied-1"));
    }

    #[test]
    fn user_filter_allow_all_passes_untagged_without_display() {
        // Without a GApplication (headless tests) delivery stops at the
        // application lookup, but the filter gate must not stop it first:
        // no panic, no registry write either way.
        let terminal = test_terminal();
        let allowed =
            cterm_core::DesktopNotificationAction::Show(cterm_core::DesktopNotification {
                id: Some("gtk-filter-allowed-1".to_owned()),
                title: "Hi".into(),
                ..Default::default()
            });
        handle_with_filter(
            &allowed,
            &terminal,
            &cterm_core::NotificationFilter::default(),
        );
        assert!(!REPORT_REGISTRY
            .lock()
            .unwrap()
            .contains_key("gtk-filter-allowed-1"));
    }
}
