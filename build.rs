use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=LIBOPENMPT_DIR");
    println!("cargo:rerun-if-env-changed=TRACKERVIS_OPENMPT_LINK");
    println!("cargo:rerun-if-env-changed=TRACKERVIS_OPENMPT_PKGCONFIG_STATIC");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ARCH");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_OS");
    println!("cargo:rerun-if-changed=build.rs");

    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "x86_64".to_owned());
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_else(|_| String::new());
    if target_arch == "wasm32" {
        return;
    }

    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let link_mode = LinkMode::from_env();

    if configure_manual_link(&root, &target_arch, &target_os, link_mode) {
        return;
    }

    if configure_pkg_config(&target_os, link_mode) {
        return;
    }

    println!(
        "cargo:warning=libopenmpt was not found automatically. Set LIBOPENMPT_DIR or install libopenmpt for your target."
    );
    emit_link_directive(link_mode, "openmpt");
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinkMode {
    Auto,
    Dynamic,
    Static,
    System,
}

impl LinkMode {
    fn from_env() -> Self {
        match env::var("TRACKERVIS_OPENMPT_LINK")
            .unwrap_or_else(|_| "auto".to_owned())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "dynamic" | "dylib" => Self::Dynamic,
            "static" => Self::Static,
            "system" => Self::System,
            _ => Self::Auto,
        }
    }

    fn prefers_static(self) -> bool {
        matches!(self, Self::Static)
    }

    fn allows_manual(self) -> bool {
        !matches!(self, Self::System)
    }

    fn allows_pkg_config(self) -> bool {
        !matches!(self, Self::Dynamic)
    }
}

fn configure_manual_link(
    root: &Path,
    target_arch: &str,
    target_os: &str,
    link_mode: LinkMode,
) -> bool {
    if !link_mode.allows_manual() {
        return false;
    }

    let candidates = [
        env::var_os("LIBOPENMPT_DIR").map(PathBuf::from),
        Some(root.join(".vendor").join("libopenmpt")),
    ];

    for candidate in candidates.into_iter().flatten() {
        if let Some(config) = probe_library(&candidate, target_arch, target_os, link_mode) {
            if target_os == "windows" {
                ensure_import_lib_alias(&config.lib_dir);
                if config.stage_runtime {
                    let bin_dir = probe_bin_dir(&candidate, target_arch);
                    if bin_dir.is_dir() {
                        stage_runtime_dlls(&bin_dir);
                        println!(
                            "cargo:rustc-env=TRACKERVIS_LIBOPENMPT_BIN={}",
                            bin_dir.display()
                        );
                        println!("cargo:rustc-env=TRACKERVIS_LIBOPENMPT_DYNAMIC=1");
                    }
                }
            }

            println!(
                "cargo:rustc-link-search=native={}",
                config.lib_dir.display()
            );
            emit_link_directive(config.link_mode, &config.library);
            return true;
        }
    }

    false
}

fn configure_pkg_config(target_os: &str, link_mode: LinkMode) -> bool {
    if !matches!(
        target_os,
        "linux" | "freebsd" | "dragonfly" | "netbsd" | "openbsd"
    ) || !link_mode.allows_pkg_config()
    {
        return false;
    }

    let mut probe = pkg_config::Config::new();
    probe.statik(
        link_mode.prefers_static() || env::var_os("TRACKERVIS_OPENMPT_PKGCONFIG_STATIC").is_some(),
    );

    match probe.probe("libopenmpt") {
        Ok(_) => {
            if link_mode.prefers_static() {
                println!("cargo:rustc-env=TRACKERVIS_LIBOPENMPT_DYNAMIC=0");
            }
            true
        }
        Err(error) => {
            println!("cargo:warning=pkg-config probe for libopenmpt failed: {error}");
            false
        }
    }
}

#[derive(Debug)]
struct LibraryConfig {
    lib_dir: PathBuf,
    library: String,
    link_mode: LinkMode,
    stage_runtime: bool,
}

fn probe_library(
    root: &Path,
    target_arch: &str,
    target_os: &str,
    requested: LinkMode,
) -> Option<LibraryConfig> {
    let lib_dirs = candidate_lib_dirs(root, target_arch);
    let dynamic_names = dynamic_library_names(target_os);
    let static_names = static_library_names(target_os);

    if requested.prefers_static() {
        for lib_dir in &lib_dirs {
            for name in static_names {
                if lib_dir.join(name).is_file() {
                    return Some(LibraryConfig {
                        lib_dir: lib_dir.clone(),
                        library: library_name_from_path(name),
                        link_mode: LinkMode::Static,
                        stage_runtime: false,
                    });
                }
            }
        }
    }

    for lib_dir in &lib_dirs {
        for name in dynamic_names {
            if lib_dir.join(name).is_file() {
                return Some(LibraryConfig {
                    lib_dir: lib_dir.clone(),
                    library: library_name_from_path(name),
                    link_mode: LinkMode::Dynamic,
                    stage_runtime: target_os == "windows",
                });
            }
        }
    }

    if matches!(requested, LinkMode::Auto) {
        for lib_dir in &lib_dirs {
            for name in static_names {
                if lib_dir.join(name).is_file() {
                    return Some(LibraryConfig {
                        lib_dir: lib_dir.clone(),
                        library: library_name_from_path(name),
                        link_mode: LinkMode::Static,
                        stage_runtime: false,
                    });
                }
            }
        }
    }

    None
}

fn candidate_lib_dirs(root: &Path, target_arch: &str) -> Vec<PathBuf> {
    vec![
        root.join("lib").join(platform_subdir(target_arch)),
        root.join("lib"),
        root.to_path_buf(),
    ]
}

fn probe_bin_dir(root: &Path, target_arch: &str) -> PathBuf {
    let arch_dir = root.join("bin").join(platform_subdir(target_arch));
    if arch_dir.is_dir() {
        arch_dir
    } else {
        root.join("bin")
    }
}

fn dynamic_library_names(target_os: &str) -> &'static [&'static str] {
    match target_os {
        "windows" => &["openmpt.lib", "libopenmpt.lib", "libopenmpt.dll.a"],
        "macos" => &["libopenmpt.dylib"],
        _ => &["libopenmpt.so"],
    }
}

fn static_library_names(target_os: &str) -> &'static [&'static str] {
    match target_os {
        "windows" => &[
            "libopenmpt_static.lib",
            "openmpt_static.lib",
            "openmpt.lib",
            "libopenmpt.lib",
        ],
        _ => &["libopenmpt.a"],
    }
}

fn emit_link_directive(mode: LinkMode, library: &str) {
    match mode {
        LinkMode::Static => println!("cargo:rustc-link-lib=static={library}"),
        _ => println!("cargo:rustc-link-lib={library}"),
    }
    if !matches!(mode, LinkMode::Dynamic) {
        println!("cargo:rustc-env=TRACKERVIS_LIBOPENMPT_DYNAMIC=0");
    }
}

fn library_name_from_path(file_name: &str) -> String {
    file_name
        .trim_end_matches(".dll.a")
        .trim_end_matches(".dylib")
        .trim_end_matches(".so")
        .trim_end_matches(".lib")
        .trim_end_matches(".a")
        .trim_start_matches("lib")
        .trim_end_matches("_static")
        .to_owned()
}

fn platform_subdir(arch: &str) -> &'static str {
    match arch {
        "x86_64" => "amd64",
        "x86" => "x86",
        "aarch64" => "arm64",
        _ => "amd64",
    }
}

fn ensure_import_lib_alias(lib_dir: &Path) {
    let canonical = lib_dir.join("libopenmpt.lib");
    let alias = lib_dir.join("openmpt.lib");
    if canonical.is_file() && !alias.is_file() {
        let _ = fs::copy(canonical, alias);
    }
}

fn stage_runtime_dlls(bin_dir: &Path) {
    let dlls = match fs::read_dir(bin_dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("dll"))
            .collect::<Vec<_>>(),
        Err(_) => Vec::new(),
    };
    if dlls.is_empty() {
        return;
    }

    let out_dir = match env::var("OUT_DIR") {
        Ok(value) => PathBuf::from(value),
        Err(_) => return,
    };
    let profile_dir = match out_dir.ancestors().nth(3) {
        Some(path) => path.to_path_buf(),
        None => return,
    };

    let deps_dir = profile_dir.join("deps");
    for dll in dlls {
        let file_name = match dll.file_name() {
            Some(name) => name.to_owned(),
            None => continue,
        };
        for target_dir in [profile_dir.as_path(), deps_dir.as_path()] {
            let _ = fs::create_dir_all(target_dir);
            let target = target_dir.join(&file_name);
            let _ = fs::copy(&dll, target);
        }
    }
}
