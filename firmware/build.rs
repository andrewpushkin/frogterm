//! The settings are not built in: the device keeps them in NVS and edits
//! them with its settings page. With the `seed-config` feature, `../config.toml`
//! (validated with the device's rules) seeds an empty NVS.

fn main() -> anyhow::Result<()> {
    embuild::espidf::sysenv::output();
    if std::env::var_os("CARGO_FEATURE_SEED_CONFIG").is_some() {
        println!("cargo:rerun-if-changed=../config.toml");
        println!("cargo:rerun-if-changed=../core/src/config.rs");
        let text = std::fs::read_to_string("../config.toml").map_err(|e| anyhow::anyhow!("seed-config: cannot read config.toml ({e})"))?;
        frogterm_core::config::Config::usable(&text, true).map_err(|e| anyhow::anyhow!("seed-config: config.toml: {e:#}"))?;
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR")?).join("seed.toml");
        std::fs::write(out, text)?;
    }
    Ok(())
}
