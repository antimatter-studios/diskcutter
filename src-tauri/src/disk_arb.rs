// DiskArbitration claim + unmount. Holds a DA session that:
//   1. Has a mount-approval callback that dissents any mount of the target
//      disk while we own the claim. Stops diskarbitrationd from auto-mounting
//      the disk back after we unmount it.
//   2. Performs the unmount via DADiskUnmount synchronously (blocks on the
//      session's runloop until the unmount callback fires). Using the same
//      session for both unmount and mount-approval closes the race window
//      that opens when an external `diskutil` CLI process exits between
//      unmount and approval-callback installation.
//
// Lifetime: construct DiskClaim::for_dev(...) before opening /dev/rdiskN.
// Drop it after both the writer and the verify reader are closed. macOS will
// then auto-mount the device as usual.
//
// Hand-rolled FFI against the DiskArbitration framework. References:
// /Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/System/Library/
// Frameworks/DiskArbitration.framework/Headers/

#![cfg(target_os = "macos")]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::sync::mpsc;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Duration;

mod ffi {
    use std::os::raw::{c_char, c_void};

    pub type CFAllocatorRef = *const c_void;
    pub type CFRunLoopRef = *const c_void;
    pub type CFStringRef = *const c_void;
    pub type CFDictionaryRef = *const c_void;
    pub type CFTypeRef = *const c_void;
    pub type DASessionRef = *const c_void;
    pub type DADiskRef = *const c_void;
    pub type DADissenterRef = *const c_void;
    pub type DAReturn = i32;
    pub type DADiskUnmountOptions = u32;

    pub type DADiskMountApprovalCallback =
        unsafe extern "C" fn(disk: DADiskRef, context: *mut c_void) -> DADissenterRef;

    pub type DADiskUnmountCallback =
        unsafe extern "C" fn(disk: DADiskRef, dissenter: DADissenterRef, context: *mut c_void);

    pub const K_CF_STRING_ENCODING_UTF8: u32 = 0x08000100;
    pub const K_DA_RETURN_NOT_PERMITTED: DAReturn = 0xF8DA_0002_u32 as i32;
    pub const K_DA_DISK_UNMOUNT_OPTION_FORCE: DADiskUnmountOptions = 0x0008_0000;
    pub const K_DA_DISK_UNMOUNT_OPTION_WHOLE: DADiskUnmountOptions = 0x0000_0001;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub static kCFAllocatorDefault: CFAllocatorRef;
        pub static kCFRunLoopDefaultMode: CFStringRef;
        pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
        pub fn CFRunLoopRun();
        pub fn CFRunLoopStop(rl: CFRunLoopRef);
        pub fn CFStringCreateWithCString(
            allocator: CFAllocatorRef,
            c_str: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        pub fn CFStringGetLength(the_string: CFStringRef) -> isize;
        pub fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
        pub fn CFStringGetCString(
            the_string: CFStringRef,
            buffer: *mut c_char,
            buffer_size: isize,
            encoding: u32,
        ) -> u8;
        pub fn CFRelease(cf: CFTypeRef);
    }

    #[link(name = "DiskArbitration", kind = "framework")]
    extern "C" {
        pub fn DASessionCreate(allocator: CFAllocatorRef) -> DASessionRef;
        pub fn DASessionScheduleWithRunLoop(
            session: DASessionRef,
            run_loop: CFRunLoopRef,
            run_loop_mode: CFStringRef,
        );
        pub fn DASessionUnscheduleFromRunLoop(
            session: DASessionRef,
            run_loop: CFRunLoopRef,
            run_loop_mode: CFStringRef,
        );
        pub fn DARegisterDiskMountApprovalCallback(
            session: DASessionRef,
            match_: CFDictionaryRef,
            callback: DADiskMountApprovalCallback,
            context: *mut c_void,
        );
        pub fn DAUnregisterApprovalCallback(
            session: DASessionRef,
            callback: *const c_void,
            context: *mut c_void,
        );
        pub fn DADissenterCreate(
            allocator: CFAllocatorRef,
            status: DAReturn,
            string: CFStringRef,
        ) -> DADissenterRef;
        pub fn DADissenterGetStatus(dissenter: DADissenterRef) -> DAReturn;
        pub fn DADissenterGetStatusString(dissenter: DADissenterRef) -> CFStringRef;
        pub fn DADiskCreateFromBSDName(
            allocator: CFAllocatorRef,
            session: DASessionRef,
            bsd_name: *const c_char,
        ) -> DADiskRef;
        pub fn DADiskUnmount(
            disk: DADiskRef,
            options: DADiskUnmountOptions,
            callback: DADiskUnmountCallback,
            context: *mut c_void,
        );
        pub fn DADiskGetBSDName(disk: DADiskRef) -> *const c_char;
    }
}

use ffi::*;

struct ApprovalContext {
    prefix: String,
}

unsafe extern "C" fn mount_approval_cb(disk: DADiskRef, ctx: *mut c_void) -> DADissenterRef {
    if ctx.is_null() {
        return std::ptr::null();
    }
    let ctx = unsafe { &*(ctx as *const ApprovalContext) };
    let name_ptr = unsafe { DADiskGetBSDName(disk) };
    if name_ptr.is_null() {
        return std::ptr::null();
    }
    let name_str = match unsafe { CStr::from_ptr(name_ptr) }.to_str() {
        Ok(s) => s,
        Err(_) => return std::ptr::null(),
    };
    if name_str == ctx.prefix || name_str.starts_with(&format!("{}s", ctx.prefix)) {
        unsafe {
            let msg = CString::new("disk-cutter is writing this disk").unwrap();
            let s = CFStringCreateWithCString(
                kCFAllocatorDefault,
                msg.as_ptr() as *const c_char,
                K_CF_STRING_ENCODING_UTF8,
            );
            let dissenter = DADissenterCreate(kCFAllocatorDefault, K_DA_RETURN_NOT_PERMITTED, s);
            CFRelease(s);
            dissenter
        }
    } else {
        std::ptr::null()
    }
}

type UnmountReply = Mutex<Option<mpsc::Sender<Result<(), String>>>>;

unsafe extern "C" fn unmount_done_cb(
    _disk: DADiskRef,
    dissenter: DADissenterRef,
    ctx: *mut c_void,
) {
    if ctx.is_null() {
        return;
    }
    let reply = unsafe { &*(ctx as *const UnmountReply) };
    let result = if dissenter.is_null() {
        Ok(())
    } else {
        let status = unsafe { DADissenterGetStatus(dissenter) };
        let detail = unsafe { cf_string_to_rust(DADissenterGetStatusString(dissenter)) };
        Err(format_dissent(status, detail.as_deref()))
    };
    if let Ok(mut guard) = reply.lock() {
        if let Some(tx) = guard.take() {
            let _ = tx.send(result);
        }
    }
}

fn format_dissent(status: DAReturn, detail: Option<&str>) -> String {
    let code = da_return_name(status)
        .map(|n| n.to_string())
        .unwrap_or_else(|| format!("0x{:08X}", status as u32));
    match detail {
        Some(d) if !d.is_empty() => format!("DA unmount dissented ({code}: {d})"),
        _ => format!("DA unmount dissented ({code})"),
    }
}

fn da_return_name(code: DAReturn) -> Option<&'static str> {
    Some(match code as u32 {
        0xF8DA_0001 => "kDAReturnError",
        0xF8DA_0002 => "kDAReturnBusy",
        0xF8DA_0003 => "kDAReturnBadArgument",
        0xF8DA_0004 => "kDAReturnExclusiveAccess",
        0xF8DA_0005 => "kDAReturnNoResources",
        0xF8DA_0006 => "kDAReturnNotFound",
        0xF8DA_0007 => "kDAReturnNotMounted",
        0xF8DA_0008 => "kDAReturnNotPermitted",
        0xF8DA_0009 => "kDAReturnNotPrivileged",
        0xF8DA_000A => "kDAReturnNotReady",
        0xF8DA_000B => "kDAReturnNotWritable",
        0xF8DA_000C => "kDAReturnUnsupported",
        _ => return None,
    })
}

unsafe fn cf_string_to_rust(s: CFStringRef) -> Option<String> {
    if s.is_null() {
        return None;
    }
    let len = unsafe { CFStringGetLength(s) };
    let max = unsafe { CFStringGetMaximumSizeForEncoding(len, K_CF_STRING_ENCODING_UTF8) };
    if max <= 0 {
        return None;
    }
    let cap = (max as usize).saturating_add(1);
    let mut buf = vec![0u8; cap];
    let ok = unsafe {
        CFStringGetCString(
            s,
            buf.as_mut_ptr() as *mut c_char,
            cap as isize,
            K_CF_STRING_ENCODING_UTF8,
        )
    };
    if ok == 0 {
        return None;
    }
    let nul = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf.truncate(nul);
    String::from_utf8(buf).ok()
}

enum WorkerEvent {
    Initialized { runloop_addr: usize },
    UnmountResult(Result<(), String>),
}

/// RAII claim. While alive, holds a DA session that has unmounted the target
/// disk and dissents any mount-approval request for that disk or its slices.
/// Drop to release.
pub struct DiskClaim {
    runloop_addr: usize,
    thread: Option<JoinHandle<()>>,
}

impl DiskClaim {
    pub fn for_dev(dev_path: &str) -> Result<Self, String> {
        let bsd_name = bsd_name(dev_path)?;
        let bsd_cstr = CString::new(bsd_name.clone()).map_err(|e| e.to_string())?;
        let approval_ctx = Box::into_raw(Box::new(ApprovalContext { prefix: bsd_name })) as usize;

        let (worker_tx, worker_rx) = mpsc::channel::<WorkerEvent>();
        let (unmount_tx, unmount_rx) = mpsc::channel::<Result<(), String>>();
        let unmount_reply: UnmountReply = Mutex::new(Some(unmount_tx));
        let unmount_ctx_addr = Box::into_raw(Box::new(unmount_reply)) as usize;

        let worker_tx_clone = worker_tx.clone();
        let thread = std::thread::spawn(move || unsafe {
            let approval_ctx = approval_ctx as *mut c_void;
            let unmount_ctx = unmount_ctx_addr as *mut c_void;

            let session = DASessionCreate(kCFAllocatorDefault);
            let runloop = CFRunLoopGetCurrent();
            DASessionScheduleWithRunLoop(session, runloop, kCFRunLoopDefaultMode);
            DARegisterDiskMountApprovalCallback(
                session,
                std::ptr::null(),
                mount_approval_cb,
                approval_ctx,
            );

            let disk = DADiskCreateFromBSDName(kCFAllocatorDefault, session, bsd_cstr.as_ptr());
            if disk.is_null() {
                let _ = worker_tx_clone.send(WorkerEvent::UnmountResult(Err(
                    "DADiskCreateFromBSDName returned NULL".to_string(),
                )));
                let _ = worker_tx_clone.send(WorkerEvent::Initialized {
                    runloop_addr: runloop as usize,
                });
                CFRunLoopRun();
                DAUnregisterApprovalCallback(
                    session,
                    mount_approval_cb as *const c_void,
                    approval_ctx,
                );
                let _ = Box::from_raw(approval_ctx as *mut ApprovalContext);
                let _ = Box::from_raw(unmount_ctx as *mut UnmountReply);
                CFRelease(session);
                return;
            }

            DADiskUnmount(
                disk,
                K_DA_DISK_UNMOUNT_OPTION_WHOLE | K_DA_DISK_UNMOUNT_OPTION_FORCE,
                unmount_done_cb,
                unmount_ctx,
            );

            let _ = worker_tx_clone.send(WorkerEvent::Initialized {
                runloop_addr: runloop as usize,
            });

            CFRunLoopRun();

            DAUnregisterApprovalCallback(session, mount_approval_cb as *const c_void, approval_ctx);
            DASessionUnscheduleFromRunLoop(session, runloop, kCFRunLoopDefaultMode);
            CFRelease(disk);
            CFRelease(session);
            let _ = Box::from_raw(approval_ctx as *mut ApprovalContext);
            let _ = Box::from_raw(unmount_ctx as *mut UnmountReply);
        });

        // Wait for the worker to schedule its runloop. Up to 5 seconds.
        let runloop_addr = match worker_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(WorkerEvent::Initialized { runloop_addr }) => runloop_addr,
            Ok(WorkerEvent::UnmountResult(Err(e))) => return Err(e),
            Ok(WorkerEvent::UnmountResult(Ok(()))) => 0,
            Err(_) => return Err("DA worker did not start within 5s".to_string()),
        };

        // Wait for unmount callback. Generous because Spotlight/Time Machine
        // can delay unmount when they're scanning.
        let unmount = unmount_rx
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| "DADiskUnmount did not complete within 30s".to_string())?;
        match unmount {
            Ok(()) => Ok(Self {
                runloop_addr,
                thread: Some(thread),
            }),
            Err(e) => {
                if runloop_addr != 0 {
                    unsafe { CFRunLoopStop(runloop_addr as CFRunLoopRef) };
                }
                let _ = thread.join();
                Err(e)
            }
        }
    }
}

impl Drop for DiskClaim {
    fn drop(&mut self) {
        if self.runloop_addr != 0 {
            unsafe { CFRunLoopStop(self.runloop_addr as CFRunLoopRef) };
        }
        // Don't join — main.rs calls std::process::exit() after run_helper returns,
        // which kills all threads anyway. Joining can hang if the runloop is in a
        // state CFRunLoopStop can't promptly wake (race between thread spawn and
        // run start, or stuck on a port wait). The kernel reaps everything cleanly
        // on process exit.
        let _ = self.thread.take();
    }
}

fn bsd_name(dev_path: &str) -> Result<String, String> {
    let stripped = dev_path
        .strip_prefix("/dev/")
        .ok_or_else(|| format!("not a /dev/ path: {dev_path}"))?;
    let normalized = stripped.strip_prefix('r').unwrap_or(stripped);
    if !normalized.starts_with("disk") {
        return Err(format!("not a disk BSD name: {normalized}"));
    }
    Ok(normalized.to_string())
}
