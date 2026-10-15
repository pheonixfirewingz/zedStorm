use dispatch2::{DispatchQueue, DispatchQueueGlobalPriority, DispatchTime, GlobalQueueIdentifier};
use gpui::{ActivityGuard, PlatformDispatcher, Priority, RunnableMeta, RunnableVariant};

use async_task::Runnable;
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_foundation::{NSActivityOptions, NSObjectProtocol, NSProcessInfo, NSString, NSThread};
use std::{ffi::c_void, ptr::NonNull, time::Duration};

pub struct AppleDispatcher;

impl AppleDispatcher {
    pub fn new() -> Self {
        Self
    }
}

impl PlatformDispatcher for AppleDispatcher {
    fn is_main_thread(&self) -> bool {
        NSThread::isMainThread_class()
    }

    fn dispatch(&self, runnable: RunnableVariant, priority: Priority) {
        let context = runnable.into_raw().as_ptr() as *mut c_void;

        let queue_priority = match priority {
            Priority::High => DispatchQueueGlobalPriority::High,
            Priority::Medium => DispatchQueueGlobalPriority::Default,
            Priority::Low => DispatchQueueGlobalPriority::Low,
        };

        unsafe {
            DispatchQueue::global_queue(GlobalQueueIdentifier::Priority(queue_priority))
                .exec_async_f(context, trampoline);
        }
    }

    fn dispatch_on_main_thread(&self, runnable: RunnableVariant, _priority: Priority) {
        let context = runnable.into_raw().as_ptr() as *mut c_void;
        unsafe {
            DispatchQueue::main().exec_async_f(context, trampoline);
        }
    }

    fn dispatch_after(&self, duration: Duration, runnable: RunnableVariant) {
        let context = runnable.into_raw().as_ptr() as *mut c_void;
        let queue = DispatchQueue::global_queue(GlobalQueueIdentifier::Priority(
            DispatchQueueGlobalPriority::High,
        ));
        let when = DispatchTime::NOW.time(duration.as_nanos() as i64);
        unsafe {
            DispatchQueue::exec_after_f(when, &queue, context, trampoline);
        }
    }

    fn prevent_app_nap(&self, reason: &str) -> ActivityGuard {
        AppleActivity::begin(
            reason,
            NSActivityOptions::UserInitiatedAllowingIdleSystemSleep,
        )
    }
}

pub struct AppleActivity {
    activity: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

// The activity token returned by NSProcessInfo is thread-safe
unsafe impl Send for AppleActivity {}

impl AppleActivity {
    pub fn begin(reason: &str, options: NSActivityOptions) -> ActivityGuard {
        let activity = Self {
            activity: NSProcessInfo::processInfo()
                .beginActivityWithOptions_reason(options, &NSString::from_str(reason)),
        };
        ActivityGuard::new(move || drop(activity))
    }
}

impl Drop for AppleActivity {
    fn drop(&mut self) {
        unsafe { NSProcessInfo::processInfo().endActivity(&self.activity) };
    }
}

extern "C" fn trampoline(context: *mut c_void) {
    let runnable =
        unsafe { Runnable::<RunnableMeta>::from_raw(NonNull::new_unchecked(context as *mut ())) };

    let location = runnable.metadata().location;
    let spawned = runnable.metadata().spawned;
    gpui::profiler::update_running_task(spawned, location);
    runnable.run();
    gpui::profiler::save_task_timing();
}
