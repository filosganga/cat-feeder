fn main() {
    // Must stay first: this function is also how the linker re-invokes this
    // binary to explain undefined symbols, and that path exits immediately.
    linker_be_nice();

    // The linker script and the error-handling hook are only meaningful when
    // building for the board. Host test builds link with the system linker,
    // which rejects both, so emit them only for the bare-metal target.
    if is_embedded_target() {
        // linkall.x must be the last linker script, or flip-link misbehaves.
        println!("cargo:rustc-link-arg=-Tlinkall.x");
        println!(
            "cargo:rustc-link-arg=--error-handling-script={}",
            std::env::current_exe().unwrap().display()
        );
    }

    inject_config();
}

/// True when cargo is building for the ESP32-C6 rather than for the host.
fn is_embedded_target() -> bool {
    std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("none")
}

/// Exposes `ap_secret` from `cfg.toml` to the firmware as an `env!()` var.
///
/// **One value, and deliberately only one.** Wi-Fi and broker credentials used
/// to be compiled in from here; they are not any more. They reach a unit either
/// through `dev/provision.sh`, which writes them straight into the `nvs`
/// partition without a compiler, or through the setup form over the unit's own
/// access point. A release binary therefore carries no network credentials at
/// all — see *Credentials: getting them out of the binary* in CLAUDE.md.
///
/// `ap_secret` stays because it is not a credential: it salts the per-unit
/// setup password, and the firmware has to derive the same string that
/// `dev/ap-password.sh` prints on a sticker. There is nowhere else it could
/// live that both sides can read.
fn inject_config() {
    println!("cargo:rerun-if-changed=cfg.toml");

    let path = std::path::Path::new("cfg.toml");
    if !path.exists() {
        // cfg.toml is git-ignored, so CI never has one. A placeholder salt
        // still builds, lints and tests; the binary is not meant to be flashed,
        // and with no credentials compiled in there is nothing to leak by it.
        if std::env::var_os("CI").is_some() {
            println!("cargo:warning=cfg.toml missing; building with a placeholder ap_secret");
            println!("cargo:rustc-env=CFG_AP_SECRET=ci-placeholder");
            return;
        }

        // Locally, fail loudly. A firmware built with a placeholder salt would
        // flash fine and then show a setup password that no sticker matches.
        panic!(
            "\n\n  cfg.toml is missing.\n  \
             Copy the template and fill it in:\n\n      \
             cp cfg.toml.example cfg.toml\n\n"
        );
    }

    let text = std::fs::read_to_string(path).expect("failed to read cfg.toml");
    let table: toml::Table = text.parse().expect("cfg.toml is not valid TOML");

    let value = table
        .get("ap_secret")
        .unwrap_or_else(|| panic!("cfg.toml is missing `ap_secret`"))
        .as_str()
        .unwrap_or_else(|| panic!("cfg.toml: `ap_secret` must be a string"));

    // A newline would silently truncate the value in the emitted env var.
    assert!(
        !value.contains('\n'),
        "cfg.toml: `ap_secret` must not contain a newline"
    );

    println!("cargo:rustc-env=CFG_AP_SECRET={value}");
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

    // Registering the hook itself happens in `main`, so it is skipped on host
    // builds. Everything above only runs when the linker calls us back.
}
