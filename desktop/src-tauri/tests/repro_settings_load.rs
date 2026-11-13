//! Diagnostic: load the live canonical settings body through the same
//! deserialization + validation the app uses, and print the reason it fails.
//!
//!   SETTINGS_BODY=<path> cargo test --test repro_settings_load -- --nocapture

use aworkit_desktop::runtime::SettingsConfigurationV2;

#[test]
fn loads_live_settings_body() {
    let Ok(path) = std::env::var("SETTINGS_BODY") else {
        eprintln!("SETTINGS_BODY not set; skipping");
        return;
    };
    let text = std::fs::read_to_string(&path).expect("read settings body");

    let mut settings: SettingsConfigurationV2 = match serde_json::from_str(&text) {
        Ok(settings) => {
            println!("deserialize: OK");
            settings
        }
        Err(error) => {
            println!("deserialize: ERR at line {} col {}: {}", error.line(), error.column(), error);
            return;
        }
    };

    match settings.validate() {
        Ok(()) => println!("validate: OK"),
        Err(error) => println!("validate: ERR: {error}"),
    }
}
