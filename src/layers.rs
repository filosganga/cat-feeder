//! ADR-0028's layers, checked: every `crate::` path outside a comment points
//! at its own layer or one below.
//!
//! The layers are read from the table in `CLAUDE.md`'s *Code organisation*,
//! which is the one place they are written down, so a new module fails here
//! until it has a row there. Host tests only: this reads the source tree.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::string::{String, ToString};
use std::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layer {
    Core,
    State,
    Drivers,
    Controls,
}

/// The table's *May know* column.
fn may_use(from: Layer, to: Layer) -> bool {
    use Layer::*;
    match from {
        Core => to == Core,
        State => matches!(to, Core | State),
        Drivers => matches!(to, Core | State | Drivers),
        Controls => true,
    }
}

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Module name to layer, from the rows of `CLAUDE.md`'s layer table: the
/// first cell names the layer, the second lists its modules in backticks.
fn table() -> BTreeMap<String, Layer> {
    let doc = fs::read_to_string(root().join("CLAUDE.md")).expect("CLAUDE.md");
    let mut layers = BTreeMap::new();
    for line in doc.lines() {
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        if cells.len() < 4 {
            continue;
        }
        let layer = match cells[1] {
            "core" => Layer::Core,
            "state" => Layer::State,
            "drivers" => Layer::Drivers,
            "controls" => Layer::Controls,
            _ => continue,
        };
        for name in cells[2].split('`').skip(1).step_by(2) {
            layers.insert(name.to_string(), layer);
        }
    }
    layers
}

/// Every module in `src/`, with its source. `lib.rs` declares them, and this
/// file is a test.
fn modules() -> Vec<(String, String)> {
    let mut found = Vec::new();
    for entry in fs::read_dir(root().join("src")).expect("src/") {
        let path = entry.expect("an entry").path();
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if path.extension().is_some_and(|e| e == "rs") && !["lib", "layers"].contains(&name) {
            let source = fs::read_to_string(&path).expect("a module");
            found.push((name.to_string(), source));
        }
    }
    found.sort();
    found
}

/// The modules a source names through `crate::`, with the line each is on,
/// comments left out. `crate::{a, b}` counts both.
fn uses(source: &str) -> Vec<(usize, String)> {
    let ident = |s: &str| -> String {
        s.chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect()
    };
    let mut found = Vec::new();
    for (i, line) in source.lines().enumerate() {
        let code = line.split("//").next().unwrap_or("");
        for (at, _) in code.match_indices("crate::") {
            let rest = &code[at + "crate::".len()..];
            if let Some(group) = rest.strip_prefix('{') {
                let group = group.split('}').next().unwrap_or("");
                for item in group.split(',') {
                    found.push((i + 1, ident(item.trim())));
                }
            } else {
                found.push((i + 1, ident(rest)));
            }
        }
    }
    found.retain(|(_, name)| !name.is_empty());
    found
}

#[test]
fn every_module_has_a_layer() {
    let table = table();
    let missing: Vec<String> = modules()
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| !table.contains_key(name))
        .collect();
    assert!(
        missing.is_empty(),
        "no row in CLAUDE.md's layer table for {missing:?}"
    );
}

#[test]
fn the_table_names_only_modules_that_exist() {
    let modules: Vec<String> = modules().into_iter().map(|(name, _)| name).collect();
    let stale: Vec<String> = table()
        .into_keys()
        .filter(|name| !modules.contains(name))
        .collect();
    assert!(stale.is_empty(), "CLAUDE.md's layer table names {stale:?}");
}

#[test]
fn dependencies_point_down() {
    let table = table();
    let mut upward = Vec::new();
    for (name, source) in modules() {
        let Some(&from) = table.get(&name) else {
            continue;
        };
        for (line, dep) in uses(&source) {
            match table.get(&dep) {
                Some(&to) if dep != name && !may_use(from, to) => upward.push(std::format!(
                    "src/{name}.rs:{line}: {from:?} -> {dep} ({to:?})"
                )),
                _ => {}
            }
        }
    }
    assert!(
        upward.is_empty(),
        "upward dependencies:\n{}",
        upward.join("\n")
    );
}

#[test]
fn the_scanner_sees_grouped_paths_and_skips_comments() {
    let source = "use crate::{feeder::Feeder, tz};\n// crate::web\nlet x = crate::store::Store;";
    let names: Vec<String> = uses(source).into_iter().map(|(_, n)| n).collect();
    assert_eq!(names, ["feeder", "tz", "store"]);
}
