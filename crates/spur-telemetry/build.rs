fn main() {
    println!("cargo:rerun-if-env-changed=SPUR_POSTHOG_KEY");
    println!("cargo:rustc-check-cfg=cfg(telemetry_disabled)");
    if std::env::var("SPUR_POSTHOG_KEY").is_err() {
        println!("cargo:rustc-cfg=telemetry_disabled");
    }
}
