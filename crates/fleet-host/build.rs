fn main() {
    // Heartbeat software.git_sha is option_env!("FLEET_GIT_SHA") — compile-time only.
    // Without this, cargo cache can keep a bake that never saw the env (Windows
    // dist then reports git_sha=null and the UI used to show "unknown").
    println!("cargo:rerun-if-env-changed=FLEET_GIT_SHA");
}
