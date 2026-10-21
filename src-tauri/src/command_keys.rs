//! macOS: both Command keys held together start a capture. The global-shortcut API
//! needs a non-modifier key and can't tell left from right, so this watches modifier changes
//! with a listen-only event tap, which needs the Input Monitoring permission.

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};

use tauri::AppHandle;

use crate::intake;

/// The saved setting for this shortcut. Not a real accelerator: `shortcut::parse` special-cases it.
pub const ACCELERATOR: &str = "LeftCommand+RightCommand";
/// macOS only applies an Input Monitoring grant to a new process.
pub const NO_PERMISSION: &str =
    "Allow Push in System Settings > Privacy & Security > Input Monitoring, then quit and reopen Push and record it again.";
const UNAVAILABLE: &str = "Couldn’t watch the Command keys. Quit and reopen Push, then try again.";

type Ref = *mut c_void;
type TapCallback = extern "C" fn(proxy: Ref, kind: u32, event: Ref, info: Ref) -> Ref;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    safe fn CGPreflightListenEventAccess() -> bool;
    /// Shows the system prompt the first time only; later calls return immediately.
    safe fn CGRequestListenEventAccess() -> bool;
    fn CGEventTapCreate(tap: u32, place: u32, options: u32, events: u64, callback: TapCallback, info: Ref) -> Ref;
    fn CGEventTapEnable(tap: Ref, enable: bool);
    fn CGEventGetFlags(event: Ref) -> u64;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopCommonModes: Ref;
    fn CFMachPortCreateRunLoopSource(allocator: Ref, port: Ref, order: isize) -> Ref;
    fn CFRunLoopGetCurrent() -> Ref;
    fn CFRunLoopAddSource(run_loop: Ref, source: Ref, mode: Ref);
    fn CFRunLoopRun();
}

const SESSION_TAP: u32 = 1; // kCGSessionEventTap
const TAIL_APPEND: u32 = 1; // kCGTailAppendEventTap
const LISTEN_ONLY: u32 = 1; // kCGEventTapOptionListenOnly
const FLAGS_CHANGED: u32 = 12; // kCGEventFlagsChanged
const DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;

// The device-dependent bits (NX_DEVICELCMDKEYMASK, NX_DEVICERCMDKEYMASK) tell the two keys apart.
const BOTH_COMMANDS: u64 = 0x08 | 0x10;
/// Shift, Control and Option (kCGEventFlagMaskShift | Control | Alternate).
const OTHER_MODIFIERS: u64 = 0x2_0000 | 0x4_0000 | 0x8_0000;

static APP: OnceLock<AppHandle> = OnceLock::new();
/// The running tap. It lives for the rest of the process once started.
static TAP: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static STARTING: Mutex<()> = Mutex::new(());
/// The shortcut is set and not paused.
static ARMED: AtomicBool = AtomicBool::new(false);
/// Both Command keys are down.
static HELD: AtomicBool = AtomicBool::new(false);

/// Starts capturing on both Command keys. Without Input Monitoring it asks for it (the system
/// prompt shows the first time only) and errs.
pub fn watch(app: &AppHandle) -> Result<(), String> {
    if !CGPreflightListenEventAccess() {
        CGRequestListenEventAccess();
        return Err(NO_PERMISSION.into());
    }
    start(app)?;
    ARMED.store(true, Ordering::SeqCst);
    Ok(())
}

pub fn unwatch() {
    ARMED.store(false, Ordering::SeqCst);
}

/// Creates the tap on its own thread and runs it there.
fn start(app: &AppHandle) -> Result<(), String> {
    let _starting = STARTING.lock().unwrap();
    if !TAP.load(Ordering::SeqCst).is_null() {
        return Ok(());
    }
    let _ = APP.set(app.clone());
    let (started, result) = mpsc::channel();
    std::thread::Builder::new()
        .name("command-keys".into())
        .spawn(move || {
            // SAFETY: plain CoreGraphics/CoreFoundation calls; the tap and its source are never
            // released, and `on_event` only reads the event it is given.
            unsafe {
                let tap = CGEventTapCreate(SESSION_TAP, TAIL_APPEND, LISTEN_ONLY, 1 << FLAGS_CHANGED, on_event, ptr::null_mut());
                if tap.is_null() {
                    let _ = started.send(false);
                    return;
                }
                let source = CFMachPortCreateRunLoopSource(ptr::null_mut(), tap, 0);
                CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
                TAP.store(tap, Ordering::SeqCst);
                let _ = started.send(true);
                CFRunLoopRun();
            }
        })
        .map_err(|_| UNAVAILABLE.to_owned())?;
    match result.recv() {
        Ok(true) => Ok(()),
        _ => Err(UNAVAILABLE.into()),
    }
}

extern "C" fn on_event(_proxy: Ref, kind: u32, event: Ref, _info: Ref) -> Ref {
    match kind {
        // macOS switches a tap off if it stalls; switch it back on.
        // SAFETY: TAP is set before the run loop delivers anything.
        DISABLED_BY_TIMEOUT | DISABLED_BY_USER_INPUT => unsafe { CGEventTapEnable(TAP.load(Ordering::SeqCst), true) },
        FLAGS_CHANGED => {
            // SAFETY: `event` is the live flags-changed event this callback was handed.
            let flags = unsafe { CGEventGetFlags(event) };
            if pressed(flags, &HELD)
                && ARMED.load(Ordering::SeqCst)
                && let Some(app) = APP.get()
            {
                intake::capture(app);
            }
        }
        _ => {}
    }
    event
}

/// True when this change brings both Command keys down with no Shift, Control or Option.
/// `held` remembers they are down, so holding them fires once.
fn pressed(flags: u64, held: &AtomicBool) -> bool {
    if flags & BOTH_COMMANDS != BOTH_COMMANDS {
        held.store(false, Ordering::SeqCst);
        return false;
    }
    !held.swap(true, Ordering::SeqCst) && flags & OTHER_MODIFIERS == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMAND: u64 = 0x10_0000; // kCGEventFlagMaskCommand, set with either key
    const LEFT: u64 = COMMAND | 0x08;
    const RIGHT: u64 = COMMAND | 0x10;
    const SHIFT: u64 = 0x2_0000 | 0x02;

    #[test]
    fn fires_once_when_both_commands_go_down() {
        let held = AtomicBool::new(false);
        assert!(!pressed(LEFT, &held));
        assert!(pressed(LEFT | RIGHT, &held));
        assert!(!pressed(LEFT | RIGHT, &held), "still held");
        assert!(!pressed(RIGHT, &held), "one released");
        assert!(pressed(LEFT | RIGHT, &held), "pressed again");
        assert!(!pressed(0, &held));
        assert!(!pressed(RIGHT, &held));
        assert!(pressed(RIGHT | LEFT, &held), "either order");
    }

    #[test]
    fn other_modifiers_block_it() {
        let held = AtomicBool::new(false);
        assert!(!pressed(LEFT | RIGHT | SHIFT, &held));
        assert!(!pressed(LEFT | RIGHT, &held), "letting go of Shift doesn't fire");
        assert!(!pressed(LEFT | RIGHT | 0x8_0000, &AtomicBool::new(false)), "Option");
        assert!(!pressed(LEFT | RIGHT | 0x4_0000, &AtomicBool::new(false)), "Control");
        assert!(pressed(LEFT | RIGHT | 0x1_0000, &AtomicBool::new(false)), "Caps Lock is fine");
    }

    /// Links and calls CoreGraphics. Run with `--ignored`; it only reads the permission state.
    #[test]
    #[ignore]
    fn preflight_links() {
        println!("input monitoring granted: {}", CGPreflightListenEventAccess());
    }
}
