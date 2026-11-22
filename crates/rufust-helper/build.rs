fn main() {
    // Static imports such as virtdisk.dll resolve before `main`, so the
    // loader policy has to be set at link time. 0x800 is
    // LOAD_LIBRARY_SEARCH_SYSTEM32: a DLL beside the executable is ignored.
    let target = |name: &str| std::env::var(name).unwrap_or_default();
    if target("CARGO_CFG_TARGET_OS") == "windows" && target("CARGO_CFG_TARGET_ENV") == "msvc" {
        println!("cargo:rustc-link-arg=/DEPENDENTLOADFLAG:0x800");
    }
}
