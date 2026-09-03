use super::status::WorkerSessionStatus;

/// Hard ceiling for concurrent sandboxes on one host (no k8s).
/// 1000s of *tasks* can queue; only this many workers run at once.
pub const MAX_PROJECT_AGENTS: i32 = 256;

pub fn free_slots(max_agents: i32, in_use: i32) -> i32 {
    let max = max_agents.max(0);
    (max - in_use).max(0)
}

pub fn count_capacity_in_use(statuses: &[WorkerSessionStatus]) -> i32 {
    statuses.iter().filter(|s| s.holds_capacity()).count() as i32
}

/// Docs §8.3 step 9 / §15.2: `max_agents` minus sessions in
/// `spawning|healthy|stuck|killing`. `stuck`/`killing` must keep the slot
/// or the tick over-spawns. Terminal `exited`/`failed_spawn` (and unknown
/// status strings) do not hold. Same mapping `schedule_and_spawn` uses.
pub fn free_slots_from_db_statuses<S: AsRef<str>>(
    statuses: impl IntoIterator<Item = S>,
    max_agents: i32,
) -> i32 {
    let parsed: Vec<WorkerSessionStatus> = statuses
        .into_iter()
        .filter_map(|s| WorkerSessionStatus::from_db(s.as_ref()))
        .collect();
    free_slots(max_agents, count_capacity_in_use(&parsed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use WorkerSessionStatus::*;

    #[test]
    fn free_slots_cases() {
        assert_eq!(free_slots(3, 0), 3);
        assert_eq!(free_slots(3, 2), 1);
        assert_eq!(free_slots(3, 3), 0);
        assert_eq!(free_slots(3, 5), 0);
        assert_eq!(free_slots(0, 0), 0);
    }

    #[test]
    fn count_in_use() {
        let s = [
            Spawning,
            Healthy,
            Stuck,
            Killing,
            Exited,
            FailedSpawn,
            Healthy,
        ];
        assert_eq!(count_capacity_in_use(&s), 5);
    }

    /// Docs §8.3 / §15.2: each of spawning|healthy|stuck|killing holds one
    /// `max_agents` slot. If stuck or killing freed the slot, the tick
    /// would over-spawn.
    #[test]
    fn sessions_in_spawning_healthy_stuck_killing_hold_capacity_slots() {
        const HOLDING: [WorkerSessionStatus; 4] = [Spawning, Healthy, Stuck, Killing];
        const TERMINAL: [WorkerSessionStatus; 2] = [Exited, FailedSpawn];

        for status in HOLDING {
            assert!(
                status.holds_capacity(),
                "{status:?} must hold a max_agents slot"
            );
            assert_eq!(
                count_capacity_in_use(&[status]),
                1,
                "{status:?} counts as 1 in-use"
            );
            assert_eq!(
                free_slots(1, count_capacity_in_use(&[status])),
                0,
                "{status:?} at max_agents=1 must leave 0 free slots"
            );
            assert_eq!(
                free_slots_from_db_statuses([status.as_str()], 1),
                0,
                "db status {:?} at max_agents=1 must leave 0 free slots",
                status.as_str()
            );
        }
        for status in TERMINAL {
            assert!(!status.holds_capacity(), "{status:?} must not hold a slot");
            assert_eq!(
                count_capacity_in_use(&[status]),
                0,
                "{status:?} is not in-use"
            );
            assert_eq!(
                free_slots(1, count_capacity_in_use(&[status])),
                1,
                "{status:?} must free the slot for the next ready task"
            );
            assert_eq!(
                free_slots_from_db_statuses([status.as_str()], 1),
                1,
                "db status {:?} must not hold a slot",
                status.as_str()
            );
        }

        // Failure mode this card locks: cap filled only by stuck+killing
        // must not look idle.
        assert_eq!(count_capacity_in_use(&[Stuck, Killing]), 2);
        assert_eq!(
            free_slots(2, count_capacity_in_use(&[Stuck, Killing])),
            0,
            "stuck+killing at max_agents=2 must not free a slot so the tick over-spawns"
        );
        assert_eq!(
            free_slots_from_db_statuses(["stuck", "killing"], 2),
            0,
            "stuck+killing db statuses at max_agents=2 must leave 0 free slots"
        );
        assert_eq!(
            free_slots_from_db_statuses(["stuck", "killing", "exited", "failed_spawn"], 2),
            0,
            "terminal siblings must not cancel stuck/killing holds"
        );
        assert_eq!(
            free_slots_from_db_statuses(["exited", "failed_spawn"], 2),
            2,
            "only terminal sessions free slots"
        );
        assert_eq!(
            free_slots_from_db_statuses(["spawning", "healthy", "stuck", "killing"], 4),
            0,
        );
        assert_eq!(
            free_slots_from_db_statuses(["stuck"], 2),
            1,
            "one stuck of max_agents=2 leaves one slot, not two"
        );
    }
}
