//! The settings are not built in by default: the device keeps them in NVS
//! and edits them with its settings page. With the `copy-config` feature
//! (`make flash`), the config file (`FT_CONFIG`, default `../config.toml`),
//! checked with the device's rules, goes into the firmware together with an
//! id of this flash (`FT_FLASH_ID`): the device copies it into NVS once.

fn main() -> anyhow::Result<()> {
    embuild::espidf::sysenv::output();
    // the version: commits pushed to GitHub (origin/main), set by the Makefile
    // and CI; "dev" only for a cargo build without it
    println!("cargo:rerun-if-env-changed=FT_VERSION");
    let version = std::env::var("FT_VERSION").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "dev".into());
    println!("cargo:rustc-env=FT_VERSION={version}");
    if std::env::var_os("CARGO_FEATURE_COPY_CONFIG").is_some() {
        println!("cargo:rerun-if-env-changed=FT_CONFIG");
        println!("cargo:rerun-if-env-changed=FT_FLASH_ID");
        println!("cargo:rerun-if-changed=../core/src/config.rs");
        let path = std::env::var("FT_CONFIG").unwrap_or_else(|_| "../config.toml".into());
        println!("cargo:rerun-if-changed={path}");
        let text = std::fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("copy-config: cannot read {path} ({e})"))?;
        frogterm_core::config::Config::usable(&text, true).map_err(|e| anyhow::anyhow!("copy-config: {path}: {e:#}"))?;
        std::fs::write(std::path::PathBuf::from(std::env::var("OUT_DIR")?).join("config.toml"), text)?;
        // a new id for every flash (else for every build): the device copies once per id
        let id = match std::env::var("FT_FLASH_ID") {
            Ok(id) if !id.is_empty() => id,
            _ => std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs().to_string(),
        };
        println!("cargo:rustc-env=FT_FLASH_ID={id}");
    }
    Ok(())
}
