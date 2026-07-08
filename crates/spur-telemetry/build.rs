fn main() {
    println!("cargo:rerun-if-env-changed=SPUR_POSTHOG_KEY");
    println!("cargo:rustc-check-cfg=cfg(telemetry_disabled)");
    if !matches!(std::env::var("SPUR_POSTHOG_KEY"), Ok(value) if !value.is_empty()) {
        println!("cargo:rustc-cfg=telemetry_disabled");
    }
}
