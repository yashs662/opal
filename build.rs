fn main() {
    // Embed the Windows executable icon. No-op on other platforms.
    // dx links its own .winres icon/versioninfo resource, so skip ours there
    // to avoid duplicate resource entries at link time. dx exposes itself to
    // build scripts via the DX_RUSTC wrapper env var.
    let under_dx = std::env::var_os("DX_RUSTC").is_some();
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() && !under_dx {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/logo/icon.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=failed to embed exe icon: {e}");
        }
    }
    println!("cargo:rerun-if-env-changed=DX_RUSTC");
    println!("cargo:rerun-if-changed=assets/logo/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
}
