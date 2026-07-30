use super::status::WorkerSessionStatus;

pub fn free_slots(max_agents: i32, in_use: i32) -> i32 {
    let max = max_agents.max(0);
    (max - in_use).max(0)
}

pub fn count_capacity_in_use(statuses: &[WorkerSessionStatus]) -> i32 {
    statuses.iter().filter(|s| s.holds_capacity()).count() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

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
        use WorkerSessionStatus::*;
        let s = [
            Spawning, Healthy, Stuck, Killing, Exited, FailedSpawn, Healthy,
        ];
        assert_eq!(count_capacity_in_use(&s), 5);
    }
}
