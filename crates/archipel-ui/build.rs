//! Embeds the application icon into the Windows executable.

fn main() -> Result<(), String> {
    println!("cargo:rerun-if-changed=archipel.rc");
    println!("cargo:rerun-if-changed=../../assets/logo/archipel.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("archipel.rc", embed_resource::NONE)
            .manifest_optional()
            .map_err(|e| format!("failed to embed Windows resources: {e}"))?;
    }
    Ok(())
}
