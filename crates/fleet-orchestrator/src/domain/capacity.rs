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

    /// Settings `update_fleet_settings` may lower max_agents below current
    /// in_use. Spawn budget must floor at 0, not go negative.
    #[test]
    fn free_slots_floors_at_zero_when_in_use_exceeds_max_agents() {
        assert_eq!(
            free_slots(3, 5),
            0,
            "in_use above a lowered max_agents must not yield a negative spawn budget"
        );
    }

    /// `create_project` persists `max_agents.unwrap_or(3).max(0)`, so 0 is live.
    #[test]
    fn free_slots_zero_max_agents_is_zero() {
        assert_eq!(
            free_slots(0, 0),
            0,
            "max_agents=0 must leave no spawn budget"
        );
    }

    /// Docs: unknown status strings (and case-sensitive from_db) do not hold.
    #[test]
    fn free_slots_from_db_statuses_unknown_strings_do_not_hold() {
        assert_eq!(
            free_slots_from_db_statuses(["", "nope", "HEALTHY", "exited"], 2),
            2,
            "empty, unknown, and uppercase HEALTHY must not hold; exited is terminal"
        );
    }

    /// Docs §8.3 / §15.2: spawning|healthy|stuck|killing hold a max_agents
    /// slot. If stuck or killing freed the slot, the tick would over-spawn.
    #[test]
    fn sessions_in_spawning_healthy_stuck_killing_hold_capacity_slots() {
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
            free_slots_from_db_statuses(["spawning", "healthy", "stuck", "killing"], 4),
            0,
            "each holding db status occupies one max_agents slot"
        );
        assert_eq!(
            free_slots_from_db_statuses(["exited", "failed_spawn"], 2),
            2,
            "terminal db statuses must not hold slots"
        );
    }
}
