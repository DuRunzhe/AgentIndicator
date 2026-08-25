//! Platform notification delivery with a single, asynchronous click-action path.
//!
//! Native platform callbacks must never touch the tray or winit directly: AppKit
//! can invoke them while it is already processing an event. Each backend only
//! sends a `NotificationAction` through this channel; `App` performs the actual
//! focus work on a worker thread.

use crate::notifications::{NotificationAction, NotificationRequest};
use crossbeam_channel::Sender;

#[cfg(target_os = "macos")]
fn debug_event(event: &str) {
    use std::io::Write;
    let Some(path) =
        dirs::data_dir().map(|root| root.join("agent-status-indicator/notification-debug.log"))
    else {
        return;
    };
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_ok() {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{:?} {event}", std::time::SystemTime::now());
        }
    }
}

pub struct NotificationService {
    #[cfg(target_os = "macos")]
    macos: macos::MacNotificationService,
    #[cfg(target_os = "windows")]
    windows: windows::WindowsNotificationService,
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    action_tx: Sender<NotificationAction>,
}

impl NotificationService {
    pub fn new(action_tx: Sender<NotificationAction>) -> Self {
        Self {
            #[cfg(target_os = "macos")]
            macos: macos::MacNotificationService::new(action_tx),
            #[cfg(target_os = "windows")]
            windows: windows::WindowsNotificationService::new(action_tx),
            #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
            action_tx,
        }
    }

    pub fn send(&mut self, request: NotificationRequest) {
        #[cfg(target_os = "macos")]
        self.macos.send(request);
        #[cfg(target_os = "windows")]
        self.windows.send(request);
        #[cfg(target_os = "linux")]
        linux::send(request, self.action_tx.clone());
        #[cfg(all(
            not(target_os = "macos"),
            not(target_os = "linux"),
            not(target_os = "windows")
        ))]
        fallback::send(request, self.action_tx.clone());
    }

    pub fn request_authorization(&self, result_tx: Sender<bool>) {
        #[cfg(target_os = "macos")]
        debug_event("authorization requested");
        #[cfg(target_os = "macos")]
        self.macos.request_authorization(result_tx);
        #[cfg(not(target_os = "macos"))]
        {
            let _ = result_tx.send(true);
        }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use super::*;
    use windows::{
        core::{IInspectable, HSTRING},
        Data::Xml::Dom::XmlDocument,
        Foundation::TypedEventHandler,
        UI::Notifications::{ToastNotification, ToastNotificationManager, ToastNotifier},
    };

    pub struct WindowsNotificationService {
        notifier: Option<ToastNotifier>,
        action_tx: Sender<NotificationAction>,
        // Toast owns the activation handler. Keep a small bounded set alive
        // until the user clicks or the toast expires.
        active: Vec<ToastNotification>,
    }

    impl WindowsNotificationService {
        pub fn new(action_tx: Sender<NotificationAction>) -> Self {
            Self {
                notifier: ToastNotificationManager::CreateToastNotifier().ok(),
                action_tx,
                active: vec![],
            }
        }

        pub fn send(&mut self, request: NotificationRequest) {
            let Some(notifier) = &self.notifier else {
                fallback_notification(&request);
                return;
            };
            let xml = format!(
                "<toast launch=\"focus\"><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual><actions><action content=\"打开会话\" arguments=\"focus\" activationType=\"foreground\"/></actions><audio src=\"ms-winsoundevent:Notification.Default\"/></toast>",
                escape_xml(&request.title), escape_xml(&request.body),
            );
            let document = match XmlDocument::new().and_then(|document| {
                document.LoadXml(&HSTRING::from(xml))?;
                Ok(document)
            }) {
                Ok(document) => document,
                Err(_) => {
                    fallback_notification(&request);
                    return;
                }
            };
            let toast = match ToastNotification::CreateToastNotification(&document) {
                Ok(toast) => toast,
                Err(_) => {
                    fallback_notification(&request);
                    return;
                }
            };
            let action_tx = self.action_tx.clone();
            let action = request.action.clone();
            let handler = TypedEventHandler::<ToastNotification, IInspectable>::new(move |_, _| {
                let _ = action_tx.send(action.clone());
                Ok(())
            });
            if toast.Activated(&handler).is_err() || notifier.Show(&toast).is_err() {
                fallback_notification(&request);
                return;
            }
            if self.active.len() >= 32 {
                self.active.remove(0);
            }
            self.active.push(toast);
        }
    }

    fn fallback_notification(request: &NotificationRequest) {
        let _ = notify_rust::Notification::new()
            .appname("AgentStatusIndicator")
            .summary(&request.title)
            .body(&request.body)
            .show();
    }

    fn escape_xml(input: &str) -> String {
        input
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('\"', "&quot;")
            .replace('\'', "&apos;")
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use block2::{DynBlock, RcBlock};
    use objc2::{
        define_class, msg_send, rc::Retained, runtime::ProtocolObject, DefinedClass, MainThreadOnly,
    };
    use objc2_foundation::{ns_string, MainThreadMarker, NSObject, NSObjectProtocol, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
        UNNotificationRequest, UNNotificationResponse, UNNotificationSound,
        UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
        time::{SystemTime, UNIX_EPOCH},
    };

    #[derive(Clone)]
    struct DelegateIvars {
        actions: Arc<Mutex<HashMap<String, NotificationAction>>>,
        action_tx: Sender<NotificationAction>,
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements and Delegate has no Drop.
        #[unsafe(super = NSObject)]
        #[thread_kind = MainThreadOnly]
        #[ivars = DelegateIvars]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UNUserNotificationCenterDelegate for Delegate {
            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn user_notification_center_did_receive_notification_response_with_completion_handler(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                completion_handler: &DynBlock<dyn Fn()>,
            ) {
                let identifier = response.notification().request().identifier().to_string();
                if let Some(action) = self
                    .ivars()
                    .actions
                    .lock()
                    .ok()
                    .and_then(|mut actions| actions.remove(&identifier))
                {
                    let _ = self.ivars().action_tx.send(action);
                }
                completion_handler.call(());
            }

            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn user_notification_center_will_present_notification_with_completion_handler(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &UNNotification,
                completion_handler: &DynBlock<
                    dyn Fn(objc2_user_notifications::UNNotificationPresentationOptions),
                >,
            ) {
                debug_event("willPresent callback received");
                // A menu-bar app is always foregrounded. Banner alone can be
                // suppressed by macOS in that state, so request the full
                // foreground presentation set explicitly.
                let options = objc2_user_notifications::UNNotificationPresentationOptions::Banner
                    | objc2_user_notifications::UNNotificationPresentationOptions::List
                    | objc2_user_notifications::UNNotificationPresentationOptions::Sound;
                completion_handler.call((options,));
            }
        }
    );

    impl Delegate {
        fn new(mtm: MainThreadMarker, ivars: DelegateIvars) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(ivars);
            // SAFETY: NSObject init has the expected Objective-C signature.
            unsafe { msg_send![super(this), init] }
        }
    }

    pub struct MacNotificationService {
        center: Retained<UNUserNotificationCenter>,
        // The system center retains its delegate weakly; holding this strong
        // reference is essential for click actions to survive tray refreshes.
        _delegate: Retained<Delegate>,
        actions: Arc<Mutex<HashMap<String, NotificationAction>>>,
        next_id: u64,
    }

    impl MacNotificationService {
        pub fn new(action_tx: Sender<NotificationAction>) -> Self {
            debug_event("notification service initialized");
            let mtm =
                MainThreadMarker::new().expect("notification service must start on main thread");
            let actions = Arc::new(Mutex::new(HashMap::new()));
            let delegate = Delegate::new(
                mtm,
                DelegateIvars {
                    actions: Arc::clone(&actions),
                    action_tx,
                },
            );
            let center = UNUserNotificationCenter::currentNotificationCenter();
            center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            Self {
                center,
                _delegate: delegate,
                actions,
                next_id: 0,
            }
        }

        pub fn send(&mut self, request: NotificationRequest) {
            debug_event("notification submitted");
            self.next_id = self.next_id.wrapping_add(1);
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis());
            let identifier = format!("agent-status-indicator-{stamp}-{}", self.next_id);
            let content = UNMutableNotificationContent::new();
            content.setTitle(ns_string!("AgentStatusIndicator"));
            let subtitle = NSString::from_str(&request.title);
            let body = NSString::from_str(&request.body);
            let identifier_string = NSString::from_str(&identifier);
            content.setSubtitle(&subtitle);
            content.setBody(&body);
            content.setSound(Some(&UNNotificationSound::defaultSound()));
            let notification = UNNotificationRequest::requestWithIdentifier_content_trigger(
                &identifier_string,
                &content,
                None,
            );
            if let Ok(mut actions) = self.actions.lock() {
                // Cap stale entries in case the OS never reports a dismissal.
                if actions.len() >= 64 {
                    actions.clear();
                }
                actions.insert(identifier, request.action);
            }
            let completion = RcBlock::new(|error: *mut objc2_foundation::NSError| {
                if !error.is_null() {
                    debug_event("notification delivery completion: error");
                    eprintln!("AgentStatusIndicator notification delivery failed");
                } else {
                    debug_event("notification delivery completion: accepted");
                }
            });
            self.center
                .addNotificationRequest_withCompletionHandler(&notification, Some(&completion));
        }

        pub fn request_authorization(&self, result_tx: Sender<bool>) {
            let completion = RcBlock::new(
                move |granted: objc2::runtime::Bool, _error: *mut objc2_foundation::NSError| {
                    debug_event(if granted.as_bool() {
                        "authorization callback: granted"
                    } else {
                        "authorization callback: denied or unavailable"
                    });
                    let _ = result_tx.send(granted.as_bool());
                },
            );
            self.center
                .requestAuthorizationWithOptions_completionHandler(
                    UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                    &completion,
                );
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;

    pub fn send(request: NotificationRequest, action_tx: Sender<NotificationAction>) {
        let shown = notify_rust::Notification::new()
            .appname("AgentStatusIndicator")
            .summary(&request.title)
            .body(&request.body)
            .action("focus", "打开会话")
            .show();
        if let Ok(handle) = shown {
            std::thread::spawn(move || {
                handle.wait_for_action(move |action| {
                    if action == "focus" || action == "default" {
                        let _ = action_tx.send(request.action.clone());
                    }
                });
            });
        }
    }
}

#[cfg(all(
    not(target_os = "macos"),
    not(target_os = "linux"),
    not(target_os = "windows")
))]
mod fallback {
    use super::*;

    pub fn send(request: NotificationRequest, _action_tx: Sender<NotificationAction>) {
        let _ = notify_rust::Notification::new()
            .appname("AgentStatusIndicator")
            .summary(&request.title)
            .body(&request.body)
            .show();
    }
}
