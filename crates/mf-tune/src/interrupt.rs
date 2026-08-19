//! Ctrl+C handling for a run measured in hours.
//!
//! The default handler terminates the process wherever it happens to be, which for this
//! program is very likely to be in the middle of writing the checkpoint that represents
//! the last several hours of games. Installing a handler that only raises a flag turns
//! Ctrl+C into "stop after this iteration, with the checkpoint intact".
//!
//! No orphan cleanup is needed and none is attempted. On Windows, Ctrl+C is delivered to
//! every process attached to the console, so fastchess receives it at the same moment and
//! shuts its own engines down; adding a kill here would be racing a process that is
//! already exiting. What must not happen is the tuner dying first and leaving fastchess to
//! finish a batch nobody is waiting for, which is exactly what the flag prevents.

use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// True once the user has asked the run to stop.
///
/// The loop takes its stop signal as a parameter rather than calling this directly: a
/// process-wide flag cannot be set by a test without leaking into every other test in the
/// binary, and the loop's response to an interrupt is exactly the behaviour worth testing.
pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}

// Only the Windows console handler and the tests raise the flag; other platforms take
// the default termination behavior, so compiling it there would be dead code.
#[cfg(any(windows, test))]
fn request_stop() {
    INTERRUPTED.store(true, Ordering::Relaxed);
}

#[cfg(windows)]
pub fn install() {
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }

    /// Handles Ctrl+C and Ctrl+Break, and the close/logoff/shutdown events, all of which
    /// mean the same thing here: stop cleanly.
    unsafe extern "system" fn handler(_event: u32) -> i32 {
        request_stop();
        // TRUE: handled, so the default terminate-the-process handler does not run.
        1
    }

    // SAFETY: registering a handler function with the correct signature, which is the
    // entire contract of this call. The handler touches only a static atomic.
    unsafe {
        SetConsoleCtrlHandler(Some(handler), 1);
    }
}

#[cfg(not(windows))]
pub fn install() {
    // The repo builds and measures on Windows. On other platforms the run still
    // checkpoints every iteration, so an interrupted run loses at most one batch.
}

#[cfg(test)]
mod tests {
    use super::{INTERRUPTED, install, interrupted, request_stop};
    use std::sync::atomic::Ordering;

    /// One test, not two: the flag is process-wide, so two tests asserting opposite
    /// states would race under the default parallel runner. What the loop does about an
    /// interrupt is tested in `run`, against an injected signal.
    #[test]
    fn the_flag_starts_clear_survives_installing_a_handler_and_is_set_by_a_stop_request() {
        assert!(!interrupted());
        install();
        assert!(!interrupted());
        request_stop();
        assert!(interrupted());
        INTERRUPTED.store(false, Ordering::Relaxed);
    }
}
