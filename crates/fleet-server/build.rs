fn main() {
    // /health.git_sha is option_env!("FLEET_GIT_SHA") — compile-time only.
    // kubectl set env / runtime ENV must not be able to fake this tree.
    println!("cargo:rerun-if-env-changed=FLEET_GIT_SHA");
}
