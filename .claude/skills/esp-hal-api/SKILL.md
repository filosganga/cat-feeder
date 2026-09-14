---
name: esp-hal-api
description: Finds and cites the exact esp-hal, esp-radio, esp-rtos and embassy API for the versions pinned in this project's Cargo.lock, using the vendored crate source and version-pinned docs. Use before writing or changing any code that calls an esp-* or embassy-* API, when adapting a snippet from a blog, an LLM answer or an older example, or when a build fails with "no method named", "no function named", "this function takes N arguments" or "unresolved import".
---

# Finding the right esp-hal API

## The rule

**Never write a signature you have not read.** These crates break their API on
almost every minor release, and pre-1.0 examples on the web are usually wrong
for the pinned version. Before calling an unfamiliar item, read its definition
in the vendored source and quote the real signature in your reasoning or the
commit message.

If you cannot find the item, say so and stop. Do not approximate.

## Step 1 — get the resolved version

`Cargo.toml` holds a requirement (`~1.1.0`), not a version. The resolved
version lives in `Cargo.lock` and is what you must look up.

```sh
cargo tree -p esp-hal --depth 0     # -> esp-hal v1.1.2
cargo pkgid esp-hal                 # -> ...#esp-hal@1.1.2
```

## Step 2 — read the vendored source (primary source of truth)

Cargo already unpacked the exact crate you are building against:

```sh
ls -d ~/.cargo/registry/src/index.crates.io-*/esp-hal-*
```

Grep that directory for the item. It is the same code the compiler sees, so it
cannot be out of date or built with different features.

```sh
R=$(ls -d ~/.cargo/registry/src/index.crates.io-*/esp-hal-1.1.2)
rg -n "pub fn new" "$R/src/gpio/mod.rs"
rg -n "pub async fn wait_for" "$R/src/gpio/asynch.rs"
```

Cite what you find as `esp-hal-1.1.2/src/gpio/mod.rs:1090`, so a reviewer can
re-check it.

## Step 3 — version-pinned docs for browsing

Use docs.rs with the version in the URL. For all three esp crates, docs.rs
already builds with `esp32c6` and the RISC-V target, so what it shows is what
this project gets:

- <https://docs.rs/esp-hal/1.1.2/esp_hal/>
- <https://docs.rs/esp-radio/0.18.0/esp_radio/>
- <https://docs.rs/esp-rtos/0.3.0/esp_rtos/>

Never open `docs.rs/esp-hal/latest` — it will show a newer, different API.

## Step 4 — working examples from the repo

Examples live in the `esp-rs/esp-hal` GitHub repo at the tag matching the
pinned version, for instance `esp-hal-v1.1.2`. Each example is its own crate:
`examples/<group>/<name>/src/main.rs`. Read them with `gh api`, not from
memory. Recipes and the group list are in
[references/lookup-recipes.md](references/lookup-recipes.md).

## Traps in this project's stack

Read [references/pinned-stack.md](references/pinned-stack.md) before touching
the executor, Wi-Fi init or feature flags. The short version:

- Most of the interesting `esp-hal` API is behind the `unstable` feature and
  will not appear in docs built without it.
- The executor is `esp-rtos`, not `esp-hal-embassy`. Entry point is
  `#[esp_rtos::main]`; tasks use `#[embassy_executor::task]`.
- Never enable an `arch-*` feature on `embassy-executor` — `esp-rtos` provides
  the architecture support and the two conflict.
- `esp-radio` ships `MIGRATING-*.md` files in its vendored directory. Read them
  when adapting an older Wi-Fi example.

## Checklist before you claim an API works

- [ ] Version taken from `Cargo.lock`, not `Cargo.toml`.
- [ ] Signature read in the vendored source, path and line noted.
- [ ] Feature gate checked (`unstable`, chip feature, `log-04`).
- [ ] `cargo build` run — it is the only proof.
