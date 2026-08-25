//! Decision 2: cheapest isolation that meets need.

pub use agent_fleet_contracts::{IsolationClass, pick_isolation};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn k8s_runtime_maps_to_container_not_its_own_class() {
        assert_eq!(IsolationClass::parse("k8s"), None);
        assert_eq!(
            IsolationClass::parse("container"),
            Some(IsolationClass::Container)
        );
    }

    #[test]
    fn workspace_vm_need_rejects_process_only_host() {
        assert_eq!(
            pick_isolation(IsolationClass::Vm, &[IsolationClass::Process]),
            None
        );
    }
}
