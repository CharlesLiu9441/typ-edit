//! Compiles and embeds the app-icon Windows resource into Windows binaries.
//!
//! Build scripts are compiled for the host, so `cfg!(windows)` would ask about
//! the wrong platform when cross-compiling. Cargo exports the artifact's target
//! OS as `CARGO_CFG_TARGET_OS`; every other target skips the resource entirely.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    // embed-resource does not emit rerun-if-changed, so declare the icon and
    // the resource script explicitly.
    println!("cargo:rerun-if-changed=assets/app-icon/typ-edit-icon.rc");
    println!("cargo:rerun-if-changed=assets/app-icon/typ-edit-icon.ico");

    embed_resource::compile("assets/app-icon/typ-edit-icon.rc", embed_resource::NONE)
        .manifest_optional()
        .expect("embed app-icon resource");
}
