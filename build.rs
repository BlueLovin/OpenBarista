fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();

    if target.contains("espidf") || target.starts_with("xtensa-") {
        embuild::espidf::sysenv::output();
    }

    // Declare ESP-IDF component cfg keys so rustc's check-cfg doesn't warn
    // when they are absent (e.g. on a host / non-ESP build).
    println!("cargo::rustc-check-cfg=cfg(esp_idf_comp_mdns_enabled)");
    println!("cargo::rustc-check-cfg=cfg(esp_idf_comp_espressif__mdns_enabled)");

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
