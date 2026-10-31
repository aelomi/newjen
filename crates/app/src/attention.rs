use std::collections::HashMap;
use std::ffi::c_char;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::{Context as _, bail};
use async_channel::{Receiver, Sender};
use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, ProtocolObject, Sel};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSApplicationTerminateReply, NSRequestUserAttentionType};
use objc2_foundation::{NSArray, NSBundle, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
    UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

const NOT_REQUESTED: u8 = 0;
const REQUESTING: u8 = 1;
const AUTHORIZED: u8 = 2;
const DENIED: u8 = 3;
const FAILED: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    NeedsApproval,
    Done,
    Failed,
}

impl Kind {
    fn title(self) -> &'static str {
        match self {
            Self::NeedsApproval => "Approval needed",
            Self::Done => "Thread completed",
            Self::Failed => "Thread failed",
        }
    }
}

struct Pending {
    title: String,
    kind: Kind,
}

pub struct NativeAttention {
    app: Retained<NSApplication>,
    center: Option<Retained<UNUserNotificationCenter>>,
    _delegate: Option<Retained<NotificationDelegate>>,
    authorization: Arc<AtomicU8>,
    pending: HashMap<String, Pending>,
    clicks: Receiver<String>,
}

impl NativeAttention {
    pub fn new() -> Self {
        let mtm = MainThreadMarker::new().expect("attention runs on the main thread");
        let app = NSApplication::sharedApplication(mtm);
        let (sender, clicks) = async_channel::unbounded();
        let authorization = Arc::new(AtomicU8::new(NOT_REQUESTED));
        // Calling currentNotificationCenter outside a bundle aborts the process.
        let center = NSBundle::mainBundle()
            .bundleIdentifier()
            .map(|_| UNUserNotificationCenter::currentNotificationCenter());
        let delegate = center.as_ref().map(|center| {
            let delegate = NotificationDelegate::new(sender);
            center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            delegate
        });
        Self {
            app,
            center,
            _delegate: delegate,
            authorization,
            pending: HashMap::new(),
            clicks,
        }
    }

    pub fn clicks(&self) -> Receiver<String> {
        self.clicks.clone()
    }

    pub fn capability_note(&self) -> Option<&'static str> {
        if self.center.is_none() {
            return Some(
                "Desktop notifications need an app bundle. The Dock still shows approvals.",
            );
        }
        match self.authorization.load(Ordering::Acquire) {
            DENIED => Some(
                "Desktop notifications are off in macOS settings. The Dock still shows approvals.",
            ),
            FAILED => Some(
                "macOS could not enable desktop notifications. The Dock still shows approvals.",
            ),
            _ => None,
        }
    }

    pub fn set_badge(&self, waiting_threads: usize) {
        let label = badge_label(waiting_threads).map(|label| NSString::from_str(&label));
        self.app.dockTile().setBadgeLabel(label.as_deref());
    }

    pub fn notify(&mut self, thread_id: &str, title: &str, kind: Kind) {
        if self.app.isActive() {
            return;
        }
        self.app
            .requestUserAttention(NSRequestUserAttentionType::InformationalRequest);
        let Some(center) = &self.center else {
            return;
        };
        self.pending.insert(
            thread_id.to_string(),
            Pending {
                title: title.to_string(),
                kind,
            },
        );
        if self
            .authorization
            .compare_exchange(
                NOT_REQUESTED,
                REQUESTING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            let authorization = self.authorization.clone();
            let completion = RcBlock::new(move |granted: Bool, error: *mut NSError| {
                let state = if !error.is_null() {
                    FAILED
                } else if granted.as_bool() {
                    AUTHORIZED
                } else {
                    DENIED
                };
                authorization.store(state, Ordering::Release);
                if let Some(error) = unsafe { error.as_ref() } {
                    eprintln!(
                        "newjen: notification authorization failed: {}",
                        error.localizedDescription()
                    );
                }
            });
            center.requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert,
                &completion,
            );
        }
        self.poll();
    }

    pub fn poll(&mut self) {
        if self.app.isActive() {
            self.pending.clear();
            return;
        }
        match self.authorization.load(Ordering::Acquire) {
            AUTHORIZED => {}
            DENIED | FAILED => {
                self.pending.clear();
                return;
            }
            _ => return,
        }
        let Some(center) = &self.center else {
            return;
        };
        for (thread_id, pending) in self.pending.drain() {
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(pending.kind.title()));
            content.setBody(&NSString::from_str(&pending.title));
            content.setThreadIdentifier(&NSString::from_str(&thread_id));
            let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
                &NSString::from_str(&notification_id(&thread_id)),
                &content,
                None,
            );
            let authorization = self.authorization.clone();
            let completion = RcBlock::new(move |error: *mut NSError| {
                if let Some(error) = unsafe { error.as_ref() } {
                    authorization.store(FAILED, Ordering::Release);
                    eprintln!(
                        "newjen: notification delivery failed: {}",
                        error.localizedDescription()
                    );
                }
            });
            center.addNotificationRequest_withCompletionHandler(&request, Some(&completion));
        }
    }

    pub fn dismiss(&mut self, thread_id: &str) {
        self.pending.remove(thread_id);
        if let Some(center) = &self.center {
            let ids =
                NSArray::from_retained_slice(&[NSString::from_str(&notification_id(thread_id))]);
            center.removePendingNotificationRequestsWithIdentifiers(&ids);
            center.removeDeliveredNotificationsWithIdentifiers(&ids);
        }
    }
}

impl Drop for NativeAttention {
    fn drop(&mut self) {
        self.set_badge(0);
        if let Some(center) = &self.center {
            center.setDelegate(None);
        }
    }
}

fn badge_label(count: usize) -> Option<String> {
    (count != 0).then(|| count.to_string())
}

fn notification_id(thread_id: &str) -> String {
    format!("newjen-thread/{thread_id}")
}

struct DelegateIvars {
    clicks: Sender<String>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = DelegateIvars]
    struct NotificationDelegate;

    unsafe impl NSObjectProtocol for NotificationDelegate {}

    unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion.call((UNNotificationPresentationOptions::empty(),));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &DynBlock<dyn Fn()>,
        ) {
            if &*response.actionIdentifier() == unsafe { UNNotificationDefaultActionIdentifier } {
                let identifier = response.notification().request().identifier().to_string();
                if let Some(thread_id) = identifier.strip_prefix("newjen-thread/") {
                    let _ = self.ivars().clicks.try_send(thread_id.to_string());
                }
            }
            completion.call(());
        }
    }
);

impl NotificationDelegate {
    fn new(clicks: Sender<String>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateIvars { clicks });
        unsafe { msg_send![super(this), init] }
    }
}

static QUIT_REQUESTS: OnceLock<Sender<()>> = OnceLock::new();
static QUIT_ALLOWED: AtomicBool = AtomicBool::new(false);

pub struct NativeQuitGuard {
    requests: Receiver<()>,
}

impl NativeQuitGuard {
    pub fn install() -> anyhow::Result<Self> {
        let mtm = MainThreadMarker::new().context("quit guard needs the main thread")?;
        let app = NSApplication::sharedApplication(mtm);
        let delegate = app
            .delegate()
            .context("macOS application has no delegate")?;
        let object: &AnyObject = (*delegate).as_ref();
        let class = object.class();
        if class.name() != c"GPUIApplicationDelegate" {
            bail!("unsupported macOS application delegate for quit confirmation");
        }
        let selector = sel!(applicationShouldTerminate:);
        if delegate.respondsToSelector(selector) {
            bail!("macOS application already owns quit confirmation");
        }
        let (sender, requests) = async_channel::bounded(1);
        QUIT_REQUESTS
            .set(sender)
            .map_err(|_| anyhow::anyhow!("quit guard is already installed"))?;
        // Add only the absent selector. Replacing GPUI's delegate would break its private ivars.
        let added = unsafe {
            class_addMethod(
                class,
                selector,
                std::mem::transmute::<
                    unsafe extern "C-unwind" fn(
                        &AnyObject,
                        Sel,
                        &NSApplication,
                    ) -> NSApplicationTerminateReply,
                    Imp,
                >(should_terminate),
                c"Q@:@".as_ptr(),
            )
        };
        if !added.as_bool() {
            bail!("could not install macOS quit confirmation");
        }
        Ok(Self { requests })
    }

    pub fn requests(&self) -> Receiver<()> {
        self.requests.clone()
    }

    pub fn allow_quit(&self) {
        QUIT_ALLOWED.store(true, Ordering::Release);
    }
}

unsafe extern "C-unwind" fn should_terminate(
    _delegate: &AnyObject,
    _selector: Sel,
    _app: &NSApplication,
) -> NSApplicationTerminateReply {
    QUIT_REQUESTS
        .get()
        .map_or(NSApplicationTerminateReply::TerminateNow, |requests| {
            reply_to_quit(QUIT_ALLOWED.load(Ordering::Acquire), requests)
        })
}

fn reply_to_quit(allowed: bool, requests: &Sender<()>) -> NSApplicationTerminateReply {
    if allowed || requests.is_closed() {
        return NSApplicationTerminateReply::TerminateNow;
    }
    let _ = requests.try_send(());
    NSApplicationTerminateReply::TerminateCancel
}

#[link(name = "objc")]
unsafe extern "C" {
    fn class_addMethod(
        class: &AnyClass,
        selector: Sel,
        implementation: Imp,
        types: *const c_char,
    ) -> Bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_quit_waits_for_confirmation_and_coalesces_repeated_requests() {
        let (requests, received) = async_channel::bounded(1);
        assert_eq!(
            reply_to_quit(false, &requests),
            NSApplicationTerminateReply::TerminateCancel
        );
        assert_eq!(
            reply_to_quit(false, &requests),
            NSApplicationTerminateReply::TerminateCancel
        );
        assert_eq!(received.len(), 1);
        received.try_recv().unwrap();
        assert_eq!(
            reply_to_quit(true, &requests),
            NSApplicationTerminateReply::TerminateNow
        );
        assert!(received.is_empty());
    }

    #[test]
    fn dropping_the_quit_receiver_does_not_trap_native_quit() {
        let (requests, received) = async_channel::bounded(1);
        drop(received);
        assert_eq!(
            reply_to_quit(false, &requests),
            NSApplicationTerminateReply::TerminateNow
        );
    }
}
