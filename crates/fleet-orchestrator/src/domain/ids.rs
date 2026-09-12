use uuid::Uuid;

/// Fixed namespace for board task UUIDv5. Never change after first deploy.
pub fn board_task_namespace() -> Uuid {
    // DNS namespace UUID (RFC 4122) as stable root; name string carries fleet coordinates.
    Uuid::parse_str("6ba7b810-9dad-11d1-80b4-00c04fd430c8").expect("valid ns")
}

/// UUIDv5(ns, "project_id:plan_id:plan_task_id") for idempotent materialize.
pub fn stable_board_task_id(project_id: Uuid, plan_id: Uuid, plan_task_id: &str) -> Uuid {
    let name = format!("{project_id}:{plan_id}:{plan_task_id}");
    Uuid::new_v5(&board_task_namespace(), name.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden() {
        let project = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        let plan = Uuid::parse_str("ffffffff-0000-1111-2222-333333333333").unwrap();
        let got = stable_board_task_id(project, plan, "login-api");
        // Same algorithm as Go (SHA1 UUID v5 + same ns + name format).
        let want = Uuid::parse_str("5ae246f5-ebfc-5bd4-884c-54bae8813983").unwrap();
        assert_eq!(got, want);
    }
}
