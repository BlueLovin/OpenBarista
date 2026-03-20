fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();

    if target.contains("espidf") || target.starts_with("xtensa-") {
        embuild::espidf::sysenv::output();
    }

    // Declare ESP-IDF component cfg keys so rustc's check-cfg doesn't warn
    // when they are absent (e.g. on a host / non-ESP build).
    println!("cargo::rustc-check-cfg=cfg(esp_idf_comp_mdns_enabled)");
    println!("cargo::rustc-check-cfg=cfg(esp_idf_comp_espressif__mdns_enabled)");
}
