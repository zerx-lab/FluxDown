//! Regression tests for the BT file-selection timeout behavior introduced
//! in stage 6 of the fluxdown_engine decoupling (see
//! `docs/fluxdown-engine-decouple-plan.md` Verification item 5a, line 146):
//!
//! > 5a. **BT 超时行为变更验证**:针对阶段 6 步骤 24 引入的"BT 文件选择从
//! > 无限等待改为有限超时"变更,测试断言 (a) 超时阈值内收到
//! > `provide_bt_selection` 调用,返回 `UserChose` 且不受超时影响;
//! > (b) 超时阈值到期后自动返回 `TimedOutDefaulted`,任务状态机按此正确
//! > 处理(不会与真正的用户取消混淆)。
//!
//! Assertions here are grounded in the actual `RinfHostSelection::select_bt_files`
//! implementation (`native/hub/src/rinf_selection.rs`), not assumed behavior:
//! - `Ok(Ok(indices))` (answered before timeout) -> `SelectionOutcome::UserChose(indices)`.
//! - Both `Ok(Err(_))` (sender dropped, channel closed) and `Err(_)` (elapsed)
//!   map to `SelectionOutcome::TimedOutDefaulted(Vec::new())` -- an *empty*
//!   vec, not "all files"; the comment above that branch ("defaulting to all
//!   files") does not match what the code actually returns. This test
//!   documents the current returned value without editing production code.
//! - `provide_bt_selection` looks up `task_id` in the pending map and, if
//!   absent (already removed after a prior timeout/answer), only logs and
//!   returns -- it never panics. If present, `tx.send(..)` failure (receiver
//!   already dropped) is silently ignored via `let _ = tx.send(..)`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use fluxdown_engine::selection::{HostSelection, SelectionOutcome};
use hub::rinf_selection::RinfHostSelection;

/// (a) An answer delivered *within* the timeout window must win, regardless
/// of how short the timeout is -- arriving in time is what matters, not the
/// absolute duration.
#[tokio::test]
async fn provide_bt_selection_before_timeout_returns_user_chose() {
    let selector = Arc::new(RinfHostSelection::new());
    let responder = Arc::clone(&selector);

    let respond_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        responder.provide_bt_selection("task-a", vec![1, 2, 3]);
    });

    let outcome = selector
        .select_bt_files("task-a", &[], Some(Duration::from_millis(200)))
        .await;

    respond_task.await.expect("responder task panicked");

    assert_eq!(outcome, SelectionOutcome::UserChose(vec![1, 2, 3]));
}

/// (b) No answer arrives before the timeout elapses -> `TimedOutDefaulted`
/// with an empty vec (per the `Err(_) => SelectionOutcome::TimedOutDefaulted(Vec::new())`
/// branch in `rinf_selection.rs`). A late `provide_bt_selection` call for the
/// same task id afterwards must be a silent no-op, not a panic, proving the
/// pending-map entry was cleaned up (`lock_or_recover(&self.bt_pending).remove(task_id)`
/// runs unconditionally after the `match`).
#[tokio::test]
async fn select_bt_files_times_out_when_no_answer_returns_timed_out_defaulted() {
    let selector = RinfHostSelection::new();

    let outcome = selector
        .select_bt_files("task-b", &[], Some(Duration::from_millis(100)))
        .await;

    assert_eq!(outcome, SelectionOutcome::TimedOutDefaulted(Vec::new()));

    // The wait-table entry for "task-b" was removed once the timeout branch
    // ran; calling provide_bt_selection again for the same id must not panic
    // (it hits the `else` branch: no pending sender found, just logs).
    selector.provide_bt_selection("task-b", vec![9]);
}

/// `timeout: None` must apply the internal default (`BT_SELECTION_TIMEOUT`,
/// 60s) rather than waiting forever. We cannot afford to actually wait out
/// 60s in a fast test, so this is a sanity check rather than a precise
/// boundary assertion: race `select_bt_files(..., None)` against a short
/// `tokio::time::sleep` using `tokio::select!`. If the selection future
/// resolved on its own well before 60s with no answer provided, the "None
/// means apply a bounded default, not infinite wait" contract would be
/// violated in the *other* direction (returning early without an answer) --
/// but that is not the risk this change addresses, and we cannot assert the
/// full 60s bound quickly. Instead we assert the narrower, still-meaningful
/// property: with `None`, the selection call is still pending (has not
/// resolved) after a duration far shorter than the 60s default, i.e. it did
/// not fall back to an immediate/zero timeout.
#[tokio::test]
async fn select_bt_files_with_none_timeout_does_not_resolve_immediately() {
    let selector = RinfHostSelection::new();

    let select_future = selector.select_bt_files("task-c", &[], None);
    tokio::pin!(select_future);

    tokio::select! {
        outcome = &mut select_future => {
            panic!(
                "select_bt_files(..., None) resolved before any answer or a short \
                 grace period elapsed; expected it to still be pending (default \
                 timeout is 60s), got {outcome:?}"
            );
        }
        _ = tokio::time::sleep(Duration::from_millis(150)) => {
            // Still pending after 150ms with no answer -- consistent with a
            // bounded-but-long (60s) default rather than an immediate return.
        }
    }

    // Clean up: answer it so the spawned test doesn't leave a dangling
    // pending entry (harmless either way since `selector` is dropped at end
    // of test, but this also exercises the UserChose path under `None`).
    selector.provide_bt_selection("task-c", vec![]);
    let outcome = select_future.await;
    assert_eq!(outcome, SelectionOutcome::UserChose(Vec::new()));
}
