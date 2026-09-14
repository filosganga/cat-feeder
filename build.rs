fn main() {
    linker_be_nice();
    // make sure linkall.x is the last linker script (otherwise might cause problems with flip-link)
    println!("cargo:rustc-link-arg=-Tlinkall.x");
    inject_config();
}

/// Reads `cfg.toml` and exposes its values to the firmware as `env!()` vars.
///
/// Keeping credentials here rather than in the source means they stay out of
/// git, and it is the seam a later runtime-provisioning version plugs into:
/// only `config::load_config` changes, not its callers.
fn inject_config() {
    println!("cargo:rerun-if-changed=cfg.toml");

    let path = std::path::Path::new("cfg.toml");
    if !path.exists() {
        panic!(
            "\n\n  cfg.toml is missing.\n  \
             Copy the template and fill it in:\n\n      \
             cp cfg.toml.example cfg.toml\n\n"
        );
    }

    let text = std::fs::read_to_string(path).expect("failed to read cfg.toml");
    let table: toml::Table = text.parse().expect("cfg.toml is not valid TOML");

    for key in [
        "wifi_ssid",
        "wifi_password",
        "mqtt_host",
        "mqtt_user",
        "mqtt_password",
    ] {
        let value = table
            .get(key)
            .unwrap_or_else(|| panic!("cfg.toml is missing `{key}`"))
            .as_str()
            .unwrap_or_else(|| panic!("cfg.toml: `{key}` must be a string"));

        // A newline would silently truncate the value in the emitted env var.
        assert!(
            !value.contains('\n'),
            "cfg.toml: `{key}` must not contain a newline"
        );

        println!("cargo:rustc-env=CFG_{}={}", key.to_uppercase(), value);
    }

    let port = table
        .get("mqtt_port")
        .unwrap_or_else(|| panic!("cfg.toml is missing `mqtt_port`"))
        .as_integer()
        .unwrap_or_else(|| panic!("cfg.toml: `mqtt_port` must be an integer"));

    let port = u16::try_from(port).unwrap_or_else(|_| panic!("cfg.toml: `mqtt_port` out of range"));

    println!("cargo:rustc-env=CFG_MQTT_PORT={port}");
}

fn linker_be_nice() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let kind = &args[1];
        let what = &args[2];

        match kind.as_str() {
            "undefined-symbol" => match what.as_str() {
                what if what.starts_with("_defmt_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `defmt` not found - make sure `defmt.x` is added as a linker script and you have included `use defmt_rtt as _;`"
                    );
                    eprintln!();
                }
                "_stack_start" => {
                    eprintln!();
                    eprintln!("💡 Is the linker script `linkall.x` missing?");
                    eprintln!();
                }
                what if what.starts_with("esp_rtos_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `esp-radio` has no scheduler enabled. Make sure you have initialized `esp-rtos` or provided an external scheduler."
                    );
                    eprintln!();
                }
                "embedded_test_linker_file_not_added_to_rustflags" => {
                    eprintln!();
                    eprintln!(
                        "💡 `embedded-test` not found - make sure `embedded-test.x` is added as a linker script for tests"
                    );
                    eprintln!();
                }
                "free"
                | "malloc"
                | "calloc"
                | "get_free_internal_heap_size"
                | "malloc_internal"
                | "realloc_internal"
                | "calloc_internal"
                | "free_internal" => {
                    eprintln!();
                    eprintln!(
                        "💡 Did you forget the `esp-alloc` dependency or didn't enable the `compat` feature on it?"
                    );
                    eprintln!();
                }
                _ => (),
            },
            // we don't have anything helpful for "missing-lib" yet
            _ => {
                std::process::exit(1);
            }
        }

        std::process::exit(0);
    }

    println!(
        "cargo:rustc-link-arg=--error-handling-script={}",
        std::env::current_exe().unwrap().display()
    );
}
