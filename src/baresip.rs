//! Checks the installed baresip before Sippy starts it: the version, the
//! module directory and the CA bundle for a new config.

use crate::{domain, platform, storage};
use anyhow::{Context, Result, anyhow, bail, ensure};
use regex::Regex;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

/// `hangup scode=603 reason=Decline` needs baresip 3.19.0.
pub const MIN_VERSION: Version = Version::new(3, 19, 0);
pub const TESTED_VERSION: Version = Version::new(4, 6, 0);
const CONTROL_MODULE: &str = "ctrl_dbus.so";
const HELP_TIMEOUT: Duration = Duration::from_secs(2);
const HELP_LIMIT: usize = 64 * 1024;
const FALLBACK_MODULE_DIRS: [&str; 4] = [
    "/usr/lib/baresip/modules",
    "/usr/lib64/baresip/modules",
    "/usr/local/lib/baresip/modules",
    "/usr/local/lib64/baresip/modules",
];
// Arch, Debian and NixOS; Fedora and RHEL; openSUSE.
const CA_FILES: [&str; 3] = [
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/ca-bundle.pem",
];
const CA_DIRS: [&str; 1] = ["/etc/ssl/certs"];
const PACKAGE_HINT: &str = "install the packages that provide them; Fedora ships modules separately, for example baresip-ctrl_dbus, baresip-pipewire and baresip-opus";

static VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^baresip v(\d{1,9})\.(\d{1,9})\.(\d{1,9})\b").unwrap());

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Reads the version from the first line of `baresip -h`.
pub fn parse_version(help: &str) -> Option<Version> {
    let clean = domain::strip_ansi(help);
    let line = clean.lines().map(str::trim).find(|line| !line.is_empty())?;
    let captures = VERSION.captures(line)?;
    Some(Version::new(
        captures[1].parse().ok()?,
        captures[2].parse().ok()?,
        captures[3].parse().ok()?,
    ))
}

/// Fails for versions that lack commands Sippy sends. Returns a warning for
/// versions that are unknown or newer than the tested major version.
pub fn check_version(version: Option<Version>) -> Result<Option<String>> {
    Ok(match version {
        Some(version) if version < MIN_VERSION => bail!(
            "baresip {version} is too old; Sippy needs {MIN_VERSION} or newer. Debian and Ubuntu ship older versions, so build baresip from source or install a newer package"
        ),
        Some(version) if version.major > TESTED_VERSION.major => Some(format!(
            "baresip {version} is newer than the tested {TESTED_VERSION}; commands or events may have changed"
        )),
        Some(_) => None,
        None => Some(format!(
            "could not determine the baresip version; Sippy needs {MIN_VERSION} or newer"
        )),
    })
}

/// Runs `baresip -h`, which prints the version on stdout and exits with 254.
/// Only a program that cannot be executed is an error; unreadable output or
/// a timeout returns `None`.
pub fn probe_version(program: &Path) -> Result<Option<Version>> {
    let deadline = Instant::now() + HELP_TIMEOUT;
    match platform::capture_command(program, &["-h"], deadline, HELP_LIMIT) {
        Ok((_, output, _)) => Ok(parse_version(&String::from_utf8_lossy(&output))),
        Err(error) if is_exec_failure(&error) => {
            Err(error).with_context(|| format!("baresip: cannot run {}", program.display()))
        }
        Err(_) => Ok(None),
    }
}

fn is_exec_failure(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<io::Error>().is_some_and(|error| {
            matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            )
        })
    })
}

fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|info| info.is_file() && info.permissions().mode() & 0o111 != 0)
}

/// Resolves `program` the way the shell would: a name without a slash is
/// searched in `PATH`. The result is not canonicalized, so wrapper scripts stay.
pub fn locate(program: &Path) -> Result<PathBuf> {
    locate_in(program, std::env::var_os("PATH").as_deref())
}

fn locate_in(program: &Path, search: Option<&OsStr>) -> Result<PathBuf> {
    ensure!(
        !program.as_os_str().is_empty(),
        "baresip: empty program path"
    );
    if program.as_os_str().as_bytes().contains(&b'/') {
        ensure!(
            is_executable_file(program),
            "baresip: {} is not an executable file",
            program.display()
        );
        return Ok(program.to_owned());
    }
    search
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable_file(candidate))
        .ok_or_else(|| {
            anyhow!(
                "{} not found in PATH; install baresip {MIN_VERSION} or newer or pass --baresip PATH",
                program.display()
            )
        })
}

fn prefix_module_dirs(executable: &Path) -> Vec<PathBuf> {
    let Some(bin) = executable.parent() else {
        return Vec::new();
    };
    let Some(prefix) = bin
        .parent()
        .filter(|_| bin.file_name() == Some(OsStr::new("bin")))
    else {
        return Vec::new();
    };
    let multiarch = format!("{}-linux-gnu", std::env::consts::ARCH);
    vec![
        prefix.join("lib/baresip/modules"),
        prefix.join("lib64/baresip/modules"),
        prefix.join("lib").join(multiarch).join("baresip/modules"),
    ]
}

/// Module directories to try for `executable`: next to its install prefix,
/// before and after resolving symlinks, then the usual system paths.
pub fn module_dir_candidates(executable: &Path) -> Vec<PathBuf> {
    let canonical = fs::canonicalize(executable).ok();
    let mut candidates: Vec<PathBuf> = Vec::new();
    let found = canonical
        .iter()
        .map(PathBuf::as_path)
        .chain([executable])
        .flat_map(prefix_module_dirs)
        .chain(FALLBACK_MODULE_DIRS.map(PathBuf::from));
    for candidate in found {
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    candidates
}

fn missing_modules<'a>(dir: &Path, required: &'a [String]) -> Vec<&'a str> {
    required
        .iter()
        .map(String::as_str)
        .filter(|module| !dir.join(module).is_file())
        .collect()
}

/// Checks that `dir` contains every module in `required`.
pub fn ensure_modules(dir: &Path, required: &[String]) -> Result<()> {
    let missing = missing_modules(dir, required);
    ensure!(
        missing.is_empty(),
        "baresip: {} lacks {}; {PACKAGE_HINT}",
        dir.display(),
        missing.join(", ")
    );
    Ok(())
}

/// The first candidate with `ctrl_dbus.so`, which must also hold the other
/// required modules.
pub fn find_module_dir(candidates: &[PathBuf], required: &[String]) -> Result<PathBuf> {
    let Some(dir) = candidates
        .iter()
        .find(|dir| dir.join(CONTROL_MODULE).is_file())
    else {
        let checked: Vec<_> = candidates
            .iter()
            .map(|dir| dir.display().to_string())
            .collect();
        bail!(
            "baresip: no module directory with {CONTROL_MODULE} found in {}; install the baresip D-Bus module or pass --baresip-modules PATH",
            checked.join(", ")
        );
    };
    ensure_modules(dir, required)?;
    Ok(dir.clone())
}

fn first_existing(candidates: &[&str], wanted: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| wanted(path))
}

/// Paths for a new config. `module_dir` overrides the search.
pub fn config_defaults(
    executable: &Path,
    module_dir: Option<&Path>,
) -> Result<storage::ConfigDefaults> {
    let required = storage::default_modules();
    let module_dir = match module_dir {
        Some(dir) => {
            let dir = fs::canonicalize(dir)
                .with_context(|| format!("baresip: module directory {}", dir.display()))?;
            ensure_modules(&dir, &required)?;
            dir
        }
        None => find_module_dir(&module_dir_candidates(executable), &required)?,
    };
    Ok(storage::ConfigDefaults {
        module_dir,
        ca_file: first_existing(&CA_FILES, Path::is_file),
        ca_dir: first_existing(&CA_DIRS, Path::is_dir),
    })
}

/// Checks that the modules and CA paths in an existing config exist, without
/// changing it. `suggest` names a module directory for the error message.
/// Returns warnings for problems that only affect TLS.
pub fn check_config(path: &Path, suggest: impl FnOnce() -> Option<PathBuf>) -> Result<Vec<String>> {
    let entries = storage::read_config_entries(path)?;
    let module_path = entries
        .module_path
        .as_ref()
        .map_or(".", |(_, value)| value.as_str());
    // baresip joins module_path and the module name with a slash.
    let missing: Vec<_> = entries
        .modules
        .iter()
        .filter(|(_, name)| !Path::new(&format!("{module_path}/{name}")).is_file())
        .map(|(line, name)| format!("{name} (line {line})"))
        .collect();
    if !missing.is_empty() {
        let location = match &entries.module_path {
            Some((line, value)) => format!("module_path {value} on line {line}"),
            None => "no module_path, so baresip looks in the working directory,".into(),
        };
        let fix = match suggest() {
            Some(dir) if Path::new(module_path) != dir => format!(
                "baresip modules were found in {}; set module_path to that directory",
                dir.display()
            ),
            _ if Path::new(module_path).is_dir() => PACKAGE_HINT.replace("them", "the modules"),
            _ => "set module_path to the directory with the baresip modules".into(),
        };
        bail!(
            "baresip: {} has {location} which lacks {}. Sippy does not change this file; {fix}",
            path.display(),
            missing.join(", ")
        );
    }
    let mut warnings = Vec::new();
    match (&entries.ca_file, &entries.ca_dir) {
        (Some((line, file)), _) if !Path::new(file).is_file() => warnings.push(format!(
            "{} line {line}: sip_cafile {file} does not exist; TLS accounts will fail server verification",
            path.display()
        )),
        (None, None) => warnings.push(format!(
            "{} sets neither sip_cafile nor sip_capath; TLS accounts will fail server verification",
            path.display()
        )),
        _ => (),
    }
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn required() -> Vec<String> {
        vec!["ctrl_dbus.so".into(), "pipewire.so".into()]
    }
    fn modules(dir: &Path, names: &[&str]) {
        fs::create_dir_all(dir).unwrap();
        for name in names {
            fs::write(dir.join(name), "").unwrap();
        }
    }
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn reads_version_from_help_output() {
        let help = "baresip v4.6.0 Copyright (C) 2010 - 2025 Alfred E. Heggestad et al.\nUsage: baresip [options]\n";
        assert_eq!(parse_version(help), Some(Version::new(4, 6, 0)));
        assert_eq!(
            parse_version("\n\x1b[1mbaresip v3.19.0\x1b[0m Copyright"),
            Some(Version::new(3, 19, 0))
        );
        assert_eq!(
            parse_version("baresip v10.0.1-dev"),
            Some(Version::new(10, 0, 1))
        );
        for help in [
            "",
            "Usage: baresip [options]\nbaresip v4.6.0",
            "baresip 4.6.0",
            "baresip v4.6",
            "baresip v99999999999.0.0",
        ] {
            assert_eq!(parse_version(help), None, "{help:?}");
        }
    }

    #[test]
    fn version_policy_blocks_old_and_warns_about_unknown_versions() {
        let error = check_version(Some(Version::new(3, 18, 9))).unwrap_err();
        assert!(error.to_string().contains("needs 3.19.0 or newer"));
        assert!(check_version(Some(Version::new(1, 1, 0))).is_err());
        assert_eq!(check_version(Some(MIN_VERSION)).unwrap(), None);
        assert_eq!(check_version(Some(Version::new(4, 11, 0))).unwrap(), None);
        assert!(
            check_version(Some(Version::new(5, 0, 0)))
                .unwrap()
                .unwrap()
                .contains("newer than the tested")
        );
        assert!(
            check_version(None)
                .unwrap()
                .unwrap()
                .contains("could not determine")
        );
    }

    #[test]
    fn probes_version_and_fails_only_when_the_program_cannot_run() {
        let temp = tempfile::tempdir().unwrap();
        let old = script(temp.path(), "old", "echo 'baresip v3.18.0 Copyright'");
        assert_eq!(probe_version(&old).unwrap(), Some(Version::new(3, 18, 0)));
        // baresip 4.6.0 prints the usage to stderr and exits with 254.
        let real_like = script(
            temp.path(),
            "real-like",
            "echo 'baresip v4.6.0 Copyright'; echo 'Usage: baresip [options]' >&2; exit 254",
        );
        assert_eq!(
            probe_version(&real_like).unwrap(),
            Some(Version::new(4, 6, 0))
        );
        let usage_only = script(
            temp.path(),
            "usage-only",
            "echo 'baresip v4.6.0' >&2; exit 1",
        );
        assert_eq!(probe_version(&usage_only).unwrap(), None);
        let silent = script(temp.path(), "silent", "true");
        assert_eq!(probe_version(&silent).unwrap(), None);
        assert!(probe_version(&temp.path().join("missing")).is_err());
        let broken = script(temp.path(), "broken", "");
        fs::write(&broken, "#!/nonexistent/interpreter\n").unwrap();
        assert!(probe_version(&broken).is_err());
    }

    #[test]
    fn locates_programs_in_path_without_resolving_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::write(first.join("baresip"), "not executable").unwrap();
        let real = script(temp.path(), "real-baresip", "true");
        symlink(&real, second.join("baresip")).unwrap();
        let search =
            std::env::join_paths(["", first.to_str().unwrap(), second.to_str().unwrap()]).unwrap();
        assert_eq!(
            locate_in(Path::new("baresip"), Some(&search)).unwrap(),
            second.join("baresip")
        );
        assert!(
            locate_in(Path::new("baresip"), None)
                .unwrap_err()
                .to_string()
                .contains("not found in PATH")
        );
        assert!(locate_in(Path::new("missing"), Some(&search)).is_err());
        assert_eq!(locate_in(&real, None).unwrap(), real);
        assert!(locate_in(&first.join("baresip"), None).is_err());
        assert!(locate_in(Path::new(""), Some(&search)).is_err());
    }

    #[test]
    fn candidates_follow_the_install_prefix_and_symlinks() {
        let multiarch = format!("{}-linux-gnu", std::env::consts::ARCH);
        let usr = module_dir_candidates(Path::new("/nonexistent/usr/bin/baresip"));
        assert_eq!(
            usr[..3],
            [
                PathBuf::from("/nonexistent/usr/lib/baresip/modules"),
                PathBuf::from("/nonexistent/usr/lib64/baresip/modules"),
                PathBuf::from(format!("/nonexistent/usr/lib/{multiarch}/baresip/modules")),
            ]
        );
        assert_eq!(usr[3..], FALLBACK_MODULE_DIRS.map(PathBuf::from));
        let fallback = module_dir_candidates(Path::new("/usr/bin/baresip"));
        assert_eq!(fallback.len(), 5, "{fallback:?}");
        assert_eq!(fallback[0], PathBuf::from("/usr/lib/baresip/modules"));
        assert_eq!(
            module_dir_candidates(Path::new("/opt/baresip")).len(),
            FALLBACK_MODULE_DIRS.len()
        );

        let temp = tempfile::tempdir().unwrap();
        let store = temp.path().join("store/baresip-4.6.0/bin");
        fs::create_dir_all(&store).unwrap();
        let real = script(&store, "baresip", "true");
        let bin = temp.path().join("profile/bin");
        fs::create_dir_all(&bin).unwrap();
        symlink(&real, bin.join("baresip")).unwrap();
        let linked = module_dir_candidates(&bin.join("baresip"));
        let store_modules = fs::canonicalize(temp.path())
            .unwrap()
            .join("store/baresip-4.6.0/lib/baresip/modules");
        assert_eq!(linked[0], store_modules);
        assert!(linked.contains(&temp.path().join("profile/lib/baresip/modules")));
    }

    #[test]
    fn finds_the_first_directory_with_the_dbus_module() {
        let temp = tempfile::tempdir().unwrap();
        let lib = temp.path().join("lib/baresip/modules");
        let lib64 = temp.path().join("lib64/baresip/modules");
        modules(&lib, &["pipewire.so"]);
        modules(&lib64, &["ctrl_dbus.so", "pipewire.so"]);
        let candidates = [temp.path().join("missing"), lib.clone(), lib64.clone()];
        assert_eq!(find_module_dir(&candidates, &required()).unwrap(), lib64);

        fs::remove_file(lib64.join("pipewire.so")).unwrap();
        let error = find_module_dir(&candidates, &required())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("lacks pipewire.so") && error.contains("baresip-pipewire"),
            "{error}"
        );

        let error = find_module_dir(&candidates[..2], &required())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("no module directory") && error.contains("--baresip-modules"),
            "{error}"
        );
        assert!(error.contains(&lib.display().to_string()));
    }

    #[test]
    fn defaults_use_an_explicit_module_directory_only_when_complete() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("modules");
        fs::create_dir_all(&dir).unwrap();
        let error = config_defaults(Path::new("/nonexistent/bin/baresip"), Some(&dir))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("ctrl_dbus.so") && error.contains("webrtc_aec.so"),
            "{error}"
        );
        let names = storage::default_modules();
        let names: Vec<_> = names.iter().map(String::as_str).collect();
        modules(&dir, &names);
        let defaults = config_defaults(Path::new("/nonexistent/bin/baresip"), Some(&dir)).unwrap();
        assert_eq!(defaults.module_dir, fs::canonicalize(&dir).unwrap());
        assert!(config_defaults(Path::new("/x"), Some(&temp.path().join("missing"))).is_err());
    }

    #[test]
    fn existing_config_errors_name_the_line_and_stay_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let mods = temp.path().join("mods");
        modules(&mods, &["ctrl_dbus.so", "pipewire.so"]);
        let ca = temp.path().join("ca.pem");
        fs::write(&ca, "").unwrap();
        let config = temp.path().join("config");
        let write = |text: String| fs::write(&config, text).unwrap();

        write(format!(
            "sip_cafile {}\nmodule_path {}\nmodule pipewire.so\nmodule_app ctrl_dbus.so # control\n",
            ca.display(),
            mods.display()
        ));
        assert_eq!(
            check_config(&config, || None).unwrap(),
            Vec::<String>::new()
        );

        let text = format!(
            "sip_cafile {}\nmodule_path /nonexistent/modules\nmodule g711.so\nmodule_app ctrl_dbus.so\n",
            ca.display()
        );
        write(text.clone());
        let error = check_config(&config, || Some(mods.clone()))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("module_path /nonexistent/modules on line 2"),
            "{error}"
        );
        assert!(
            error.contains("g711.so (line 3), ctrl_dbus.so (line 4)"),
            "{error}"
        );
        assert!(
            error.contains(&format!("found in {}", mods.display())),
            "{error}"
        );
        assert_eq!(fs::read_to_string(&config).unwrap(), text);

        write(format!("module_path {}\nmodule opus.so\n", mods.display()));
        let error = check_config(&config, || Some(mods.clone()))
            .unwrap_err()
            .to_string();
        assert!(error.contains("baresip-opus"), "{error}");

        write("module_app ctrl_dbus.so\n".into());
        let error = check_config(&config, || None).unwrap_err().to_string();
        assert!(error.contains("no module_path"), "{error}");
    }

    #[test]
    fn missing_ca_paths_only_warn() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config");
        fs::write(&config, "sip_cafile /nonexistent/ca.pem\n").unwrap();
        let warnings = check_config(&config, || None).unwrap();
        assert!(warnings[0].contains("line 1: sip_cafile /nonexistent/ca.pem does not exist"));
        fs::write(&config, "sip_verify_server yes\n").unwrap();
        assert!(
            check_config(&config, || None).unwrap()[0]
                .contains("neither sip_cafile nor sip_capath")
        );
        fs::write(&config, format!("sip_capath {}\n", temp.path().display())).unwrap();
        assert!(check_config(&config, || None).unwrap().is_empty());
    }
}
