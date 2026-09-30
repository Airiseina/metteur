//! Cooperative run control shared by the interpreter and the ReAct loop.
//!
//! Both loops park between units of work so a long LLM call or a slow node
//! never blocks the control RPCs. Parking is the only place where these flags
//! are observed, which makes it the only place a control request can be lost.

use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::error::{DaemonError, DaemonResult};

use super::context::ExecutionContext;

/// Poll interval while parked on a pause request.
const PAUSE_POLL: Duration = Duration::from_millis(50);

/// Returns `Err` when the run has been cancelled.
pub fn check_cancelled(ctx: &ExecutionContext) -> DaemonResult<()> {
    if ctx.cancel_requested.load(Ordering::SeqCst) {
        return Err(DaemonError::Interrupted("cancelled by user".to_string()));
    }
    Ok(())
}

/// Parks the run while a pause is requested.
///
/// Cancellation is re-checked on every tick. A parked run must stay
/// cancellable: without this, pausing a run and then cancelling it would leave
/// the task spinning forever while it still holds the workspace execution slot.
pub async fn wait_while_paused(ctx: &ExecutionContext) -> DaemonResult<()> {
    while ctx.pause_requested.load(Ordering::SeqCst) {
        check_cancelled(ctx)?;
        tokio::time::sleep(PAUSE_POLL).await;
    }
    check_cancelled(ctx)
}

/// Honours both control flags at a yield point between units of work.
pub async fn gate(ctx: &ExecutionContext) -> DaemonResult<()> {
    check_cancelled(ctx)?;
    wait_while_paused(ctx).await
}
