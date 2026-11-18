//! What Ctrl-C does to a run.
//!
//! The first interrupt asks the run to stop: the orchestrator tells the
//! plugin to cancel, the plugin stops its tool, the stage is recorded as
//! interrupted and the lock is released. That takes as long as the tool
//! takes to die, so a second interrupt ends the process at once, and the
//! record is left as a killed run leaves it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

/// The exit status a shell gives a command ended by Ctrl-C.
const INTERRUPTED: i32 = 130;

/// Installs the handler once, and returns the flag it sets. Handles Ctrl-C
/// and, where there is one, the termination signal a supervisor sends.
pub(crate) fn install() -> Arc<AtomicBool> {
    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    FLAG.get_or_init(|| {
        let flag = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&flag);
        let installed = ctrlc::set_handler(move || {
            if seen.swap(true, Ordering::SeqCst) {
                eprintln!("\nstopped at once; the run's record does not say how it ended");
                std::process::exit(INTERRUPTED);
            }
            eprintln!(
                "\nstopping: waiting for the tool to stop so the run can be recorded; press \
                 Ctrl-C again to exit at once"
            );
        });
        // Without a handler Ctrl-C ends the process as it always did: a
        // worse outcome, not a wrong one, so it is not a reason to refuse
        // to run.
        if let Err(e) = installed {
            eprintln!("warning: Ctrl-C will not stop the run cleanly: {e}");
        }
        flag
    })
    .clone()
}
