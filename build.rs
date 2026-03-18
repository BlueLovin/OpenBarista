fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();

    if target.contains("espidf") || target.starts_with("xtensa-") {
        embuild::espidf::sysenv::output();
    }
}
