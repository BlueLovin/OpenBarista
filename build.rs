fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();

    if target.contains("espidf") || target.starts_with("xtensa-") {
        embuild::espidf::sysenv::output();
    }

    // Declare ESP-IDF component cfg keys so rustc's check-cfg doesn't warn
    // when they are absent (e.g. on a host / non-ESP build).
    println!("cargo::rustc-check-cfg=cfg(esp_idf_comp_mdns_enabled)");
    println!("cargo::rustc-check-cfg=cfg(esp_idf_comp_espressif__mdns_enabled)");

    // Detect explicit ota_enabled cfg flag via env var (esp-rs convention).
    // rerun-if-env-changed is required, otherwise cargo keeps the previously
    // emitted cfg flags when the env var is toggled.
    println!("cargo:rerun-if-env-changed=CFG_OTA_ENABLED");
    if let Ok(_val) = std::env::var("CFG_OTA_ENABLED") {
        println!("cargo:rustc-cfg=ota_enabled");
        println!("cargo::warning=OTA upload feature ENABLED — this is a development-only mode");
    }

    // Declare cfg for ota_enabled so rustc doesn't warn
    println!("cargo:rustc-check-cfg=cfg(ota_enabled)");

    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    println!("cargo:rerun-if-changed=.git/HEAD");

    let git_short = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
            } else {
                None
            }
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "nogit".to_owned());

    let epoch = std::env::var("SOURCE_DATE_EPOCH").unwrap_or_else(|_| "dev".to_owned());
    let build_id = format!("{}-{}", git_short, epoch);
    println!("cargo:rustc-env=OPENBARISTA_BUILD_ID={build_id}");
}
