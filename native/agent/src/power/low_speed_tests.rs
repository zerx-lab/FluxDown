use super::{
    PowerAction, PowerLowSpeedCondition, PowerObservation, PowerState, PowerStatusDto, RESUME_GAP,
};
use std::time::{Duration, Instant};

const CONDITION: PowerLowSpeedCondition = PowerLowSpeedCondition {
    threshold_bps: 5120,
    duration_secs: 300,
};

fn downloading() -> PowerObservation {
    PowerObservation {
        connected: true,
        active: 1,
        local_work_count: Some(0),
        ..PowerObservation::default()
    }
}

#[test]
fn low_speed_requires_continuous_interval_and_separate_warning_then_fires_once() {
    for action in [PowerAction::Shutdown, PowerAction::Sleep] {
        let mut state = PowerState::new();
        let start = Instant::now();
        assert!(state.arm_low_speed(CONDITION, action, downloading(), start));
        assert_eq!(
            state.advance_low_speed(downloading(), start + Duration::from_secs(299)),
            None
        );
        assert_eq!(state.status(start).countdown_remaining_secs, None);
        let warning = start + Duration::from_secs(300);
        assert_eq!(state.advance_low_speed(downloading(), warning), None);
        assert_eq!(state.status(warning).countdown_remaining_secs, Some(30));
        assert_eq!(
            state.advance_low_speed(downloading(), warning + Duration::from_secs(29)),
            None
        );
        assert_eq!(
            state.advance_low_speed(downloading(), warning + Duration::from_secs(30)),
            Some(action)
        );
        assert_eq!(state.status(warning), PowerStatusDto::default());
        assert_eq!(
            state.advance_low_speed(downloading(), warning + Duration::from_secs(31)),
            None
        );
    }
}

#[test]
fn recovery_at_threshold_resets_interval_even_during_warning() {
    let mut state = PowerState::new();
    let now = Instant::now();
    assert!(state.arm_low_speed(CONDITION, PowerAction::Shutdown, downloading(), now));
    let warning = now + Duration::from_secs(300);
    state.advance_low_speed(downloading(), warning);
    let recovered = PowerObservation {
        download_bps: 5120,
        ..downloading()
    };
    assert_eq!(
        state.advance_low_speed(recovered, warning + Duration::from_secs(29)),
        None
    );
    assert_eq!(state.status(warning).countdown_remaining_secs, None);
    assert_eq!(state.status(warning).low_speed_elapsed_secs, None);
    let restart = warning + Duration::from_secs(30);
    state.advance_low_speed(downloading(), restart);
    assert_eq!(
        state.advance_low_speed(downloading(), restart + Duration::from_secs(299)),
        None
    );
    assert_eq!(state.status(restart).countdown_remaining_secs, None);
    state.advance_low_speed(downloading(), restart + Duration::from_secs(300));
    assert_eq!(
        state
            .status(restart + Duration::from_secs(300))
            .countdown_remaining_secs,
        Some(30)
    );
}

#[test]
fn pauses_disconnects_unknown_stats_and_local_work_cancel_warning() {
    for blocked in [
        PowerObservation {
            active: 0,
            ..downloading()
        },
        PowerObservation {
            connected: false,
            ..downloading()
        },
        PowerObservation {
            local_work_count: None,
            ..downloading()
        },
        PowerObservation {
            local_work_count: Some(1),
            ..downloading()
        },
        PowerObservation {
            download_bps: -1,
            ..downloading()
        },
    ] {
        let mut state = PowerState::new();
        let now = Instant::now();
        state.arm_low_speed(CONDITION, PowerAction::Sleep, downloading(), now);
        state.advance_low_speed(downloading(), now + Duration::from_secs(300));
        assert_eq!(
            state.advance_low_speed(blocked, now + Duration::from_secs(330)),
            None
        );
        assert_eq!(state.status(now).countdown_remaining_secs, None);
        assert_eq!(state.status(now).low_speed_elapsed_secs, None);
        state.advance_low_speed(downloading(), now + Duration::from_secs(331));
        assert_eq!(
            state
                .status(now + Duration::from_secs(331))
                .low_speed_elapsed_secs,
            Some(0)
        );
    }
}

#[test]
fn retry_wait_counts_as_work_but_seeding_only_and_idle_do_not_arm() {
    let now = Instant::now();
    let mut state = PowerState::new();
    let idle = PowerObservation {
        active: 0,
        ..downloading()
    };
    assert!(!state.arm_low_speed(CONDITION, PowerAction::Shutdown, idle, now));
    assert!(!state.arm_low_speed(
        CONDITION,
        PowerAction::Shutdown,
        PowerObservation { waiting: 1, ..idle },
        now,
    ));
    assert!(!state.arm_low_speed(
        CONDITION,
        PowerAction::Shutdown,
        PowerObservation {
            connected: false,
            ..downloading()
        },
        now
    ));
    assert!(!state.arm_low_speed(
        CONDITION,
        PowerAction::Shutdown,
        PowerObservation {
            local_work_count: None,
            ..downloading()
        },
        now
    ));
    let retry = PowerObservation {
        waiting: 1,
        retry_waiting: 1,
        ..idle
    };
    assert!(state.arm_low_speed(CONDITION, PowerAction::Shutdown, retry, now));
    state.advance_low_speed(retry, now + Duration::from_secs(300));
    assert_eq!(
        state.advance_low_speed(retry, now + Duration::from_secs(330)),
        Some(PowerAction::Shutdown)
    );
}

#[test]
fn unsafe_condition_values_are_refused_without_replacing_an_existing_plan() {
    for condition in [
        PowerLowSpeedCondition {
            threshold_bps: 0,
            ..CONDITION
        },
        PowerLowSpeedCondition {
            threshold_bps: u64::MAX,
            ..CONDITION
        },
        PowerLowSpeedCondition {
            duration_secs: 0,
            ..CONDITION
        },
        PowerLowSpeedCondition {
            duration_secs: 59,
            ..CONDITION
        },
        PowerLowSpeedCondition {
            duration_secs: 86401,
            ..CONDITION
        },
        PowerLowSpeedCondition {
            duration_secs: u64::MAX,
            ..CONDITION
        },
    ] {
        let mut state = PowerState::new();
        let now = Instant::now();
        state.arm(Duration::from_secs(60), PowerAction::Sleep, false, now);
        let before = state.status(now);
        assert!(!state.arm_low_speed(condition, PowerAction::Shutdown, downloading(), now));
        assert_eq!(state.status(now), before);
    }
}

#[test]
fn sleep_or_explicit_cancel_removes_low_speed_plan() {
    let mut state = PowerState::new();
    let now = Instant::now();
    state.arm_low_speed(CONDITION, PowerAction::Shutdown, downloading(), now);
    assert!(state.on_suspended(RESUME_GAP));
    assert_eq!(state.status(now), PowerStatusDto::default());
    state.arm_low_speed(CONDITION, PowerAction::Shutdown, downloading(), now);
    state.advance_low_speed(downloading(), now + Duration::from_secs(300));
    state.disarm();
    assert_eq!(
        state.advance_low_speed(downloading(), now + Duration::from_secs(330)),
        None
    );
}

#[test]
fn switching_back_to_completion_does_not_retain_low_speed_timing() {
    let mut state = PowerState::new();
    let now = Instant::now();
    state.arm_low_speed(CONDITION, PowerAction::Shutdown, downloading(), now);
    state.arm(Duration::from_secs(5), PowerAction::Sleep, true, now);
    assert_eq!(state.status(now).low_speed, None);
    assert_eq!(state.advance(1, 0, now + Duration::from_secs(1000)), None);
    assert_eq!(state.advance(0, 0, now + Duration::from_secs(1000)), None);
    assert_eq!(
        state.advance(0, 0, now + Duration::from_secs(1005)),
        Some(PowerAction::Sleep)
    );
}

#[test]
fn delayed_observation_still_allows_a_full_visible_warning() {
    let mut state = PowerState::new();
    let now = Instant::now();
    state.arm_low_speed(CONDITION, PowerAction::Sleep, downloading(), now);
    let delayed = now + Duration::from_secs(1000);
    assert_eq!(state.advance_low_speed(downloading(), delayed), None);
    assert_eq!(state.status(delayed).countdown_remaining_secs, Some(30));
}
