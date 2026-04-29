fn main() {
    // Re-emit Cargo build-script variables as rustc-env so they can be accessed
    // via option_env!() in source code (these are only available to build scripts
    // by default, not to the crate being compiled).
    for var in &["PROFILE", "TARGET", "OPT_LEVEL"] {
        if let Ok(val) = std::env::var(var) {
            println!("cargo:rustc-env=ROUTER_{}={}", var, val);
        }
    }

    // Re-run only if the build profile changes (normally stable within a build).
    println!("cargo:rerun-if-env-changed=PROFILE");
    println!("cargo:rerun-if-env-changed=OPT_LEVEL");
}
