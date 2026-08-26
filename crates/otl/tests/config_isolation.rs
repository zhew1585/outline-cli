//! Story 4.1: the credential-release boundary is structural.
//!
//! The gate ([`otl::config::release_token`]) is only as strong as the state
//! it decides from, so three things have to be impossible to forge:
//!
//! - a `Settings` claiming a `UrlSource` the layers never produced;
//! - a read of the API key that does not pass through the gate;
//! - a `BindingChecked` minted without running the check.
//!
//! Rust's privacy rules make that a question about MODULE LAYOUT, not about
//! the `pub` keyword: a private field is visible to the declaring module and
//! to every descendant of it. Fields declared private in `config` would
//! still be reachable from `config::anything_added_later`. The security
//! state therefore lives in leaf modules - `config::resolved` owns
//! `Settings`, `config::secret` owns the keys, `config::release` owns the
//! proof token - and none of them is an ancestor of the others.
//!
//! These tests check that from both sides: an external crate (what a library
//! consumer can do) and a sibling module inside `config` (what the Epic 2
//! credential source would be able to do).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tempfile::TempDir;

/// Source text that must NOT compile against the public API, with the reason.
const FORGERY_ATTEMPTS: &[(&str, &str)] = &[
    (
        "forge a Flag url_source to unbind a profile's credential",
        r#"
        fn main() {
            let settings = otl::config::Settings {
                profile: Some("work".to_string()),
                base_url: "https://attacker.example.com".to_string(),
                url_source: otl::config::UrlSource::Flag,
                profile_url: None,
                auth: otl::config::AuthMethod::ApiKey,
            };
            let env = otl::config::EnvLayer::from_process();
            let _ = otl::config::release_token(&otl::config::EnvApiKey(&env), &settings);
        }
        "#,
    ),
    (
        "read the global API key straight off EnvLayer",
        r#"
        fn main() {
            let env = otl::config::EnvLayer::from_process();
            let _leaked: Option<String> = env.api_key;
        }
        "#,
    ),
    (
        "read the per-profile API keys straight off EnvLayer",
        r#"
        fn main() {
            let env = otl::config::EnvLayer::from_process();
            let _leaked = env.profile_api_keys.get("WORK").cloned();
        }
        "#,
    ),
    (
        "launder a proof onto settings the gate did not approve",
        r#"
        struct Launder<'a>(&'a otl::config::EnvLayer, &'a otl::config::Settings);
        impl otl::config::TokenSource for Launder<'_> {
            fn fetch(
                &self,
                checked: &otl::config::BindingChecked<'_>,
            ) -> Result<String, otl::config::ConfigError> {
                // No way to say "serve THESE settings instead": the approved
                // ones are the only ones `fetch` can be given.
                otl::config::EnvApiKey(self.0).fetch(self.1, checked)
            }
        }
        fn main() {}
        "#,
    ),
    (
        "mint a BindingChecked to reach a TokenSource directly",
        r#"
        struct Mine;
        impl otl::config::TokenSource for Mine {
            fn fetch(
                &self,
                _c: &otl::config::BindingChecked<'_>,
            ) -> Result<String, otl::config::ConfigError> {
                Ok(String::new())
            }
        }
        fn main() {
            let settings = resolve();
            let checked = otl::config::BindingChecked(&settings);
            let _ = otl::config::TokenSource::fetch(&Mine, &checked);
        }
        fn resolve() -> otl::config::Settings { unimplemented!() }
        "#,
    ),
    (
        "reach the key container EnvLayer holds",
        r#"
        fn main() {
            let env = otl::config::EnvLayer::from_process();
            let _keys = env.keys();
        }
        "#,
    ),
    (
        "mutate a resolved Settings to claim a different origin",
        r#"
        fn main() {
            let mut settings = resolve();
            settings.base_url = "https://attacker.example.com".to_string();
            let env = otl::config::EnvLayer::from_process();
            let _ = otl::config::release_token(&otl::config::EnvApiKey(&env), &settings);
        }
        fn resolve() -> otl::config::Settings { unimplemented!() }
        "#,
    ),
];

/// Compile `source` against the built `otl` library; returns the compiler's
/// stderr on failure, or `None` when it compiled.
fn compile_against_otl(source: &str) -> Option<String> {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("probe.rs");
    std::fs::write(&src, source).unwrap();

    // The rlib and its dependencies, as cargo just built them for this test.
    let deps = std::path::Path::new(env!("CARGO_BIN_EXE_otl"))
        .parent()
        .unwrap()
        .join("deps");
    let output = std::process::Command::new(std::env::var("CARGO").unwrap_or("cargo".into()))
        .arg("--version")
        .output();
    assert!(output.is_ok(), "cargo must be available");

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let otl_rlib = std::fs::read_dir(&deps)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("libotl-") && n.ends_with(".rlib"))
        })
        .max_by_key(|path| std::fs::metadata(path).and_then(|m| m.modified()).ok())?;

    // `--emit=metadata`: the question is whether this TYPE-CHECKS, and a
    // privacy violation is a compile error, so linking answers nothing.
    // Emitting a binary made the probe depend on the link environment and it
    // failed on Windows (`LNK1181: cannot open input file
    // 'windows.0.52.0.lib'` - windows-sys import libraries sit behind a
    // native search path a hand-built rustc call does not reproduce),
    // leaving the harness to report itself broken: inconclusive, not a
    // verdict.
    let result = std::process::Command::new(rustc)
        .arg(&src)
        .arg("--edition=2021")
        .arg("--crate-type=bin")
        .arg("--emit=metadata")
        .arg("--extern")
        .arg(format!("otl={}", otl_rlib.display()))
        .arg("-L")
        .arg(format!("dependency={}", deps.display()))
        .arg("--out-dir")
        .arg(dir.path())
        .output()
        .unwrap();
    if result.status.success() {
        None
    } else {
        Some(String::from_utf8_lossy(&result.stderr).to_string())
    }
}

#[test]
fn the_gates_inputs_cannot_be_forged_from_outside_the_crate() {
    // Sanity: a program that only uses the sanctioned API must compile, or a
    // broken probe harness would make every case below pass vacuously.
    let sanctioned = r#"
        fn main() {
            let env = otl::config::EnvLayer::from_process();
            let overrides = otl::config::Overrides::default();
            let loaded = otl::config::load_file(&overrides, &env).unwrap();
            let settings = otl::config::resolve_settings(&overrides, &env, &loaded).unwrap();
            let _ = settings.url_source();
            let _ = otl::config::release_token(&otl::config::EnvApiKey(&env), &settings);
        }
    "#;
    let Some(stderr) = compile_against_otl(sanctioned) else {
        // Compiled, as it must. Now every forgery must fail.
        for (what, source) in FORGERY_ATTEMPTS {
            let stderr =
                compile_against_otl(source).unwrap_or_else(|| panic!("SAFE RUST CAN STILL {what}"));
            assert!(
                is_privacy_rejection(&stderr),
                "{what}: rejected for the wrong reason:\n{stderr}"
            );
        }
        return;
    };
    panic!("the probe harness is broken; sanctioned API did not compile:\n{stderr}");
}

/// Whether a compiler failure is one of the reasons an attack is supposed to
/// fail, rather than, say, a typo in the probe.
///
/// Three shapes count, in increasing order of strength:
///
/// - the item is private (`E0451`, `E0616`, `E0603`, "not a tuple struct");
/// - the field does not exist on that type at all, because it was moved into
///   a leaf module (`E0609`, `E0560`);
/// - the SIGNATURE does not admit the attack (`E0061`, `E0050`) - which is
///   how the laundering case fails now that `fetch` takes no settings
///   argument: the call cannot even be written.
///
/// Each internal attack additionally has a positive control, so a probe that
/// fails for a stale reason - a renamed field, say - is caught there rather
/// than passing here.
fn is_privacy_rejection(stderr: &str) -> bool {
    [
        "private", // the general wording
        "E0451",   // private field in a struct literal
        "E0616",   // private field access
        "E0603",   // private module / item
        "E0609",   // no such field (moved into a leaf)
        "E0560",   // struct has no such field
        "E0061",   // wrong arity: the signature refuses the attack
        "E0050",   // trait impl signature mismatch
        "no field",
        "not a tuple struct", // BindingChecked's private field
    ]
    .iter()
    .any(|marker| stderr.contains(marker))
}

/// The config module sources, as a compilable standalone crate in a temp dir.
///
/// `mod.rs` becomes a child module of a generated `lib.rs`, so the module
/// tree - and therefore every privacy relationship inside it - is identical
/// to the real one.
///
/// The copy RECURSES, so a directory module (`config/credentials/mod.rs`)
/// is carried over rather than leaving a `mod` declaration with no file,
/// which would fail the permission probe and read as a broken harness. Any
/// sibling module `config` references through `crate::` is copied and
/// declared too, so adding one does not silently disable this test.
fn config_tree_copy() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let copied = copy_tree(&src.join("config"), &dir.path().join("config"));
    assert!(copied >= 5, "expected the config module to have leaf files");

    // Whatever `config` reaches for outside itself has to come along.
    let mut siblings: Vec<String> = Vec::new();
    for entry in walk(&dir.path().join("config")) {
        let source = std::fs::read_to_string(&entry).unwrap();
        for (index, _) in source.match_indices("crate::") {
            let rest = &source[index + "crate::".len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() && name != "config" && !siblings.contains(&name) {
                siblings.push(name);
            }
        }
    }
    let mut lib = String::from("pub mod config;\n");
    for name in &siblings {
        let file = src.join(format!("{name}.rs"));
        let directory = src.join(name);
        if file.is_file() {
            std::fs::copy(&file, dir.path().join(format!("{name}.rs"))).unwrap();
        } else if directory.is_dir() {
            copy_tree(&directory, &dir.path().join(name));
        } else {
            panic!(
                "config references crate::{name}, which is neither src/{name}.rs nor src/{name}/"
            );
        }
        lib.push_str(&format!("pub mod {name};\n"));
    }
    std::fs::write(dir.path().join("lib.rs"), lib).unwrap();
    dir
}

/// Copy a module directory recursively; returns the number of `.rs` files.
fn copy_tree(from: &Path, to: &Path) -> usize {
    std::fs::create_dir_all(to).unwrap();
    let mut copied = 0;
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap();
        if path.is_dir() {
            copied += copy_tree(&path, &to.join(name));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            std::fs::copy(&path, to.join(name)).unwrap();
            copied += 1;
        }
    }
    copied
}

/// Every `.rs` file under `dir`, recursively.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
    found
}

/// Compile the copied tree; returns the compiler's stderr on failure.
fn compile_config_tree(dir: &TempDir) -> Option<String> {
    compile_with(dir, resolved_externs())
}

/// The `--extern` arguments that make the copied tree compile.
///
/// `target/debug/deps` can hold SEVERAL rlibs for one crate name: a
/// dependency built for the host (for a build script) and the same
/// dependency built for the target both live there, and nothing in the file
/// name tells them apart. "Newest wins" is therefore a lottery that any
/// change in build order can flip - which is exactly what happened when the
/// spec compiler became a build dependency, and it made this harness fail
/// with a confusing "the config tree did not compile".
///
/// So the choice is VALIDATED rather than guessed: candidates are tried
/// newest-first and the winner is the combination that compiles an
/// unmodified copy of the tree. A wrong pick cannot quietly weaken the
/// probes below - it cannot be picked at all.
fn resolved_externs() -> &'static [String] {
    static RESOLVED: OnceLock<Vec<String>> = OnceLock::new();
    RESOLVED.get_or_init(|| {
        let candidates: Vec<Vec<String>> = DEPENDENCIES
            .iter()
            .map(|(name, prefix)| {
                rlib_candidates(prefix)
                    .into_iter()
                    .map(|path| format!("{name}={}", path.display()))
                    .collect()
            })
            .collect();
        for (attempt, combination) in combinations(&candidates).into_iter().enumerate() {
            let probe = config_tree_copy();
            if compile_with(&probe, &combination).is_none() {
                return combination;
            }
            assert!(
                attempt + 1 < MAX_EXTERN_ATTEMPTS,
                "no combination of dependency rlibs in target/debug/deps \
                 compiles the config tree after {MAX_EXTERN_ATTEMPTS} \
                 attempts; run `cargo build --tests` and try again"
            );
        }
        panic!("no dependency rlibs found in target/debug/deps");
    })
}

/// Dependencies the copied tree needs, as (crate name, rlib prefix).
const DEPENDENCIES: &[(&str, &str)] = &[
    ("engine", "libengine-"),
    ("serde", "libserde-"),
    ("toml", "libtoml-"),
    ("directories", "libdirectories-"),
];

/// Cap on how many rlib combinations are tried before giving up, so a
/// deps directory full of stale artifacts fails fast instead of grinding.
const MAX_EXTERN_ATTEMPTS: usize = 16;

fn deps_dir() -> PathBuf {
    Path::new(env!("CARGO_BIN_EXE_otl"))
        .parent()
        .unwrap()
        .join("deps")
}

/// Every rlib matching `prefix`, newest first.
fn rlib_candidates(prefix: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(deps_dir())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix) && name.ends_with(".rlib"))
        })
        .collect();
    found.sort_by_key(|path| {
        std::cmp::Reverse(
            std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .ok(),
        )
    });
    assert!(
        !found.is_empty(),
        "no {prefix}*.rlib in {}",
        deps_dir().display()
    );
    found
}

/// Combinations of one candidate per dependency, newest-first, capped.
fn combinations(candidates: &[Vec<String>]) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = vec![Vec::new()];
    for choices in candidates {
        let mut next = Vec::new();
        for prefix in &out {
            for choice in choices {
                let mut extended = prefix.clone();
                extended.push(choice.clone());
                next.push(extended);
            }
        }
        out = next;
    }
    out.truncate(MAX_EXTERN_ATTEMPTS);
    out
}

/// Compile the copied tree against one specific set of dependencies.
fn compile_with(dir: &TempDir, externs: &[String]) -> Option<String> {
    let mut command =
        std::process::Command::new(std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string()));
    // Metadata only, so both probes ask the compiler the same question. An
    // rlib never reaches the linker, so this one was not broken on Windows.
    command
        .arg(dir.path().join("lib.rs"))
        .arg("--crate-type=lib")
        .arg("--edition=2021")
        .arg("--emit=metadata")
        .arg("-L")
        .arg(format!("dependency={}", deps_dir().display()))
        .arg("--out-dir")
        .arg(dir.path());
    for spec in externs {
        command.arg("--extern").arg(spec);
    }
    let output = command.output().unwrap();
    (!output.status.success()).then(|| String::from_utf8_lossy(&output.stderr).to_string())
}

/// Add a sibling module inside `config` containing `body`.
fn with_attacker_module(dir: &TempDir, body: &str) {
    let mod_rs = dir.path().join("config/mod.rs");
    let mut source = std::fs::read_to_string(&mod_rs).unwrap();
    source.insert_str(source.find("mod error;").unwrap(), "mod attacker;\n");
    std::fs::write(&mod_rs, source).unwrap();
    std::fs::write(dir.path().join("config/attacker.rs"), body).unwrap();
}

/// One attack a module added inside `config` must not be able to carry out:
/// what it does, the attacker source, and a WIDENING that should make it
/// succeed.
///
/// The widening is a positive control. Without it, a probe that fails for a
/// stale reason - a field renamed in the leaf, say - is indistinguishable
/// from a probe that fails because the field is unreachable, and the case
/// silently stops testing anything while still reporting green. With it, a
/// rename breaks both halves at once and the test says so.
struct InternalAttack {
    what: &'static str,
    attacker: &'static str,
    /// (file in `config/`, text to replace, replacement) edits that open the
    /// item up. Applying them must make `attacker` compile.
    widening: &'static [(&'static str, &'static str, &'static str)],
}

const INTERNAL_ATTACKS: &[InternalAttack] = &[
    InternalAttack {
        what: "forge a Settings claiming a Flag url_source",
        attacker: r#"
        use super::{AuthMethod, ProfileSource, Settings, UrlSource};
        pub fn forge() -> Settings {
            Settings {
                profile: Some("work".to_string()),
                base_url: "https://attacker.example.com".to_string(),
                url_source: UrlSource::Flag,
                profile_source: ProfileSource::Flag,
                profile_url: None,
                auth: AuthMethod::ApiKey,
            }
        }
        "#,
        // Every field, since a struct literal needs all of them visible.
        widening: &[
            (
                "resolved.rs",
                "    profile: Option<String>,",
                "    pub(super) profile: Option<String>,",
            ),
            (
                "resolved.rs",
                "    base_url: String,",
                "    pub(super) base_url: String,",
            ),
            (
                "resolved.rs",
                "    url_source: UrlSource,",
                "    pub(super) url_source: UrlSource,",
            ),
            (
                "resolved.rs",
                "    profile_source: ProfileSource,",
                "    pub(super) profile_source: ProfileSource,",
            ),
            (
                "resolved.rs",
                "    profile_url: Option<String>,",
                "    pub(super) profile_url: Option<String>,",
            ),
            (
                "resolved.rs",
                "    auth: AuthMethod,",
                "    pub(super) auth: AuthMethod,",
            ),
        ],
    },
    InternalAttack {
        what: "read the global API key out of the layer",
        attacker: r#"
        use super::EnvLayer;
        pub fn steal(env: &EnvLayer) -> Option<String> {
            env.keys().global.clone()
        }
        "#,
        widening: &[(
            "secret.rs",
            "    global: Option<String>,",
            "    pub(super) global: Option<String>,",
        )],
    },
    InternalAttack {
        what: "read a per-profile API key out of the layer",
        attacker: r#"
        use super::EnvLayer;
        pub fn steal(env: &EnvLayer) -> Option<String> {
            env.keys().per_profile.get("WORK").cloned()
        }
        "#,
        widening: &[(
            "secret.rs",
            "    per_profile: BTreeMap<String, String>,",
            "    pub(super) per_profile: BTreeMap<String, String>,",
        )],
    },
    InternalAttack {
        what: "mint a BindingChecked without running the check",
        attacker: r#"
        use super::{BindingChecked, Settings};
        pub fn forge(settings: &Settings) -> BindingChecked<'_> {
            BindingChecked(settings)
        }
        "#,
        widening: &[(
            "release.rs",
            "pub struct BindingChecked<'settings>(&'settings Settings);",
            "pub struct BindingChecked<'settings>(pub(super) &'settings Settings);",
        )],
    },
];

/// Apply widenings to the copied tree, so the positive control can run.
fn widen(dir: &TempDir, edits: &[(&str, &str, &str)]) {
    for (file, from, to) in edits {
        let path = dir.path().join("config").join(file);
        let source = std::fs::read_to_string(&path).unwrap();
        assert!(
            source.contains(from),
            "the positive control for {file} is stale: {from:?} is not in the \
             current source, so the attack it pairs with may be testing nothing"
        );
        std::fs::write(&path, source.replace(from, to)).unwrap();
    }
}

#[test]
fn a_module_added_inside_config_cannot_forge_the_gates_state() {
    // Permission probe first: the unmodified tree must compile, or every
    // case below would "pass" for the wrong reason.
    let clean = config_tree_copy();
    if let Some(stderr) = compile_config_tree(&clean) {
        panic!("the probe harness is broken; the config tree did not compile:\n{stderr}");
    }

    for attack in INTERNAL_ATTACKS {
        // Positive control: with the item opened up, the attacker compiles.
        // This is what tells a genuine privacy rejection apart from a probe
        // that has gone stale.
        let control = config_tree_copy();
        widen(&control, attack.widening);
        with_attacker_module(&control, attack.attacker);
        if let Some(stderr) = compile_config_tree(&control) {
            panic!(
                "the probe for {:?} no longer targets a real item - it fails \
                 even with the item made visible:\n{stderr}",
                attack.what
            );
        }

        // The real assertion: unmodified, it must not compile.
        let dir = config_tree_copy();
        with_attacker_module(&dir, attack.attacker);
        let stderr = compile_config_tree(&dir)
            .unwrap_or_else(|| panic!("A MODULE INSIDE config CAN STILL {}", attack.what));
        assert!(
            is_privacy_rejection(&stderr),
            "{}: rejected for the wrong reason:\n{stderr}",
            attack.what
        );
    }
}

/// Whether a leaf module's source grants any code outside it access to the
/// module's insides.
///
/// A line-prefix match over four spellings of `mod` was the previous
/// version, and it missed `#[path = "..."] mod x;`, `pub(in crate::config)
/// mod x;`, `pub(crate)mod x;` (no space), a `mod` and its name split across
/// lines, and `include!` - which does not even create a submodule, it
/// splices foreign code into the leaf itself.
///
/// So this scans TOKENS with comments and string literals removed: any `mod`
/// keyword, any `#[path]` attribute, any `include!`. Comments have to go
/// first because these files discuss modules in prose constantly.
fn grants_access_beyond_the_leaf(source: &str) -> Option<String> {
    let code = strip_comments_and_strings(source);
    for (needle, why) in [
        (
            "mod",
            "declares a submodule, which would inherit access to this module's private items",
        ),
        ("#[path", "points a module declaration at another file"),
        ("include!", "splices another file's code into this module"),
    ] {
        if let Some(index) = find_token(&code, needle) {
            let line = 1 + code[..index].matches('\n').count();
            return Some(format!("line {line}: `{needle}` {why}"));
        }
    }
    None
}

/// Remove `//` and `/* */` comments and the contents of string literals,
/// leaving byte positions (and therefore line numbers) intact.
fn strip_comments_and_strings(source: &str) -> String {
    let bytes: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut index = 0;
    while index < bytes.len() {
        let rest: String = bytes[index..].iter().take(2).collect();
        if rest.starts_with("//") {
            while index < bytes.len() && bytes[index] != '\n' {
                out.push(' ');
                index += 1;
            }
        } else if rest == "/*" {
            while index < bytes.len() && !bytes[index..].iter().take(2).eq(['*', '/'].iter()) {
                out.push(if bytes[index] == '\n' { '\n' } else { ' ' });
                index += 1;
            }
            for _ in 0..2.min(bytes.len() - index) {
                out.push(' ');
                index += 1;
            }
        } else if bytes[index] == '"' {
            out.push(' ');
            index += 1;
            while index < bytes.len() && bytes[index] != '"' {
                out.push(if bytes[index] == '\n' { '\n' } else { ' ' });
                index += 1;
            }
            if index < bytes.len() {
                out.push(' ');
                index += 1;
            }
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    out
}

/// Find `needle` as a standalone token (not part of a longer identifier).
fn find_token(code: &str, needle: &str) -> Option<usize> {
    let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '_');
    code.match_indices(needle).find_map(|(index, _)| {
        let before = code[..index].chars().next_back();
        let after = code[index + needle.len()..].chars().next();
        let starts_token = needle.starts_with(|c: char| c.is_alphabetic());
        let ok_before = !starts_token || boundary(before);
        let ok_after = !needle.ends_with(|c: char| c.is_alphanumeric()) || boundary(after);
        (ok_before && ok_after).then_some(index)
    })
}

#[test]
fn the_security_state_lives_in_leaf_modules() {
    // The whole argument rests on `resolved`, `secret` and `release` having
    // no descendants: a submodule of any of them would inherit exactly the
    // access the compile probes prove a sibling does not have.
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/config");
    for leaf in ["resolved.rs", "secret.rs", "release.rs"] {
        let source = std::fs::read_to_string(config.join(leaf)).unwrap();
        assert_eq!(
            grants_access_beyond_the_leaf(&source),
            None,
            "{leaf} opens up the module the credential gate depends on"
        );
    }

    // And the state must be declared in those leaves, not in `config` itself,
    // where every sibling could reach it.
    let mod_rs = std::fs::read_to_string(config.join("mod.rs")).unwrap();
    for (item, leaf) in [
        ("pub struct Settings", "resolved.rs"),
        ("pub enum UrlSource", "resolved.rs"),
        ("pub struct EnvKeys", "secret.rs"),
        ("pub struct BindingChecked", "release.rs"),
    ] {
        assert!(
            !mod_rs.contains(item),
            "{item} is declared in config/mod.rs; it belongs in {leaf}"
        );
        let source = std::fs::read_to_string(config.join(leaf)).unwrap();
        assert!(source.contains(item), "{item} is not declared in {leaf}");
    }
}

#[test]
fn the_leaf_guard_catches_every_way_of_opening_a_leaf() {
    // Guards the guard. Each of these compiles and grants a submodule (or
    // spliced code) the leaf's private access; the previous line-prefix
    // version saw only the first two.
    let real = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/config/resolved.rs"),
    )
    .unwrap();
    for opener in [
        "mod attacker;",
        "pub mod attacker;",
        "pub(super) mod attacker;",
        "pub(crate) mod attacker;",
        "pub(in crate::config) mod attacker;",
        "pub(crate)mod attacker;",
        "#[path = \"attacker.rs\"] mod attacker;",
        "#[path = \"attacker.rs\"]\nmod attacker;",
        "mod\n    attacker;",
        "include!(\"resolved_extra.rs\");",
        "    mod attacker;",
    ] {
        let tampered = format!("{real}\n{opener}\n");
        assert!(
            grants_access_beyond_the_leaf(&tampered).is_some(),
            "the guard does not see {opener:?}"
        );
    }

    // And it must not fire on the prose these files are full of: they
    // discuss modules, `mod.rs`, and "module-tree" constantly.
    assert_eq!(grants_access_beyond_the_leaf(&real), None);
    assert_eq!(
        grants_access_beyond_the_leaf("// mod attacker;\nlet x = 1;"),
        None,
        "a commented-out declaration is not a declaration"
    );
    assert_eq!(
        grants_access_beyond_the_leaf("/// see `mod.rs`\nfn f() {}"),
        None,
        "a doc comment mentioning mod.rs is not a declaration"
    );
    assert_eq!(
        grants_access_beyond_the_leaf("let s = \"mod attacker;\";"),
        None,
        "a string literal is not a declaration"
    );
    assert_eq!(
        grants_access_beyond_the_leaf("fn modify() {}\nlet models = 1;"),
        None,
        "`mod` inside a longer identifier is not a declaration"
    );
}
