//! Which language server to run, and whether it is installed.
//!
//! dxdiary bundles *configuration*, not binaries. Vendoring clangd,
//! rust-analyzer, and gopls would be hundreds of megabytes and a licensing
//! tangle, so they are detected on `PATH` and their absence is reported rather
//! than papered over.

use std::path::PathBuf;

use dxdiary_syntax::Language;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSpec {
    pub language: Language,
    /// Executable name, looked up on `PATH`.
    pub command: &'static str,
    pub args: &'static [&'static str],
    /// Shown by `doctor` when the server is missing.
    pub install_hint: &'static str,
    /// Extra caveat worth surfacing even when it *is* installed.
    pub caveat: Option<&'static str>,
}

/// Servers dxdiary knows how to drive, in the order `doctor` reports them.
pub const SERVERS: &[ServerSpec] = &[
    ServerSpec {
        language: Language::Rust,
        command: "rust-analyzer",
        args: &[],
        install_hint: "rustup component add rust-analyzer",
        caveat: None,
    },
    ServerSpec {
        language: Language::Go,
        command: "gopls",
        args: &[],
        install_hint: "go install golang.org/x/tools/gopls@latest",
        caveat: None,
    },
    ServerSpec {
        language: Language::Python,
        command: "pyright-langserver",
        args: &["--stdio"],
        install_hint: "npm install -g pyright",
        caveat: None,
    },
    ServerSpec {
        language: Language::Cpp,
        command: "clangd",
        args: &["--background-index"],
        install_hint: "apt install clangd",
        caveat: Some("needs compile_commands.json to resolve includes"),
    },
    ServerSpec {
        language: Language::C,
        command: "clangd",
        args: &["--background-index"],
        install_hint: "apt install clangd",
        // The constraint from DESIGN.md §2, surfaced where a user will meet it.
        caveat: Some(
            "cannot parse GCC nested functions; tree-sitter is authoritative and \
             clangd diagnostics are filtered inside affected functions",
        ),
    },
];

pub fn spec_for(language: Language) -> Option<&'static ServerSpec> {
    SERVERS.iter().find(|s| s.language == language)
}

/// Locate an executable on `PATH`.
///
/// Hand-rolled rather than shelling out to `which`, which is one more process
/// and not present everywhere.
pub fn find_on_path(command: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(command))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub spec: &'static ServerSpec,
    /// Resolved path, or None when the server is not installed.
    pub found: Option<PathBuf>,
    /// Set when the file exists but does not actually work.
    pub broken: Option<String>,
}

impl Status {
    pub fn available(&self) -> bool {
        self.found.is_some() && self.broken.is_none()
    }
}

/// Check that a located server actually runs.
///
/// Being on `PATH` is not enough. `~/.cargo/bin/rust-analyzer` is a rustup
/// *shim*: it exists and is executable even when the component was never
/// installed, and running it prints `Unknown binary` — **to stderr, with exit
/// status 0**. So neither presence nor exit code can be trusted; what
/// distinguishes a working server is that `--version` writes something to
/// stdout.
pub fn probe(command: &str) -> Result<(), String> {
    use std::process::{Command, Stdio};

    let output = Command::new(command)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run: {e}"))?;

    if output.stdout.iter().any(|b| !b.is_ascii_whitespace()) {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let first = stderr.lines().next().unwrap_or("no output").trim();
    Err(if first.is_empty() {
        "produced no version output".to_string()
    } else {
        first.to_string()
    })
}

/// What is installed *and working*, for `dxdiary doctor`.
pub fn doctor() -> Vec<Status> {
    SERVERS
        .iter()
        .map(|spec| {
            let found = find_on_path(spec.command);
            let broken = match &found {
                Some(_) => probe(spec.command).err(),
                None => None,
            };
            Status {
                spec,
                found,
                broken,
            }
        })
        .collect()
}

/// Human-readable report.
pub fn report(statuses: &[Status]) -> String {
    let mut out = String::new();
    for s in statuses {
        // Three states, not two: present-but-broken is its own answer, and the
        // most confusing one to hit without being told.
        let mark = match (&s.found, &s.broken) {
            (Some(_), None) => "ok  ",
            (Some(_), Some(_)) => "BAD ",
            (None, _) => "MISS",
        };
        out.push_str(&format!(
            "[{mark}] {:<8} {:<20} {}\n",
            s.spec.language.name(),
            s.spec.command,
            match (&s.found, &s.broken) {
                (Some(p), None) => p.display().to_string(),
                (Some(p), Some(why)) => format!("{} — does not run: {why}", p.display()),
                (None, _) => format!("not installed — {}", s.spec.install_hint),
            }
        ));
        if s.broken.is_some() {
            out.push_str(&format!("         fix:  {}\n", s.spec.install_hint));
        }
        if let Some(caveat) = s.spec.caveat {
            out.push_str(&format!("         note: {caveat}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_language_has_a_server() {
        for lang in Language::ALL {
            assert!(
                spec_for(*lang).is_some(),
                "{} has no configured server",
                lang.name()
            );
        }
    }

    #[test]
    fn c_and_cpp_share_clangd_but_c_carries_the_nested_function_caveat() {
        let c = spec_for(Language::C).unwrap();
        let cpp = spec_for(Language::Cpp).unwrap();

        assert_eq!(c.command, "clangd");
        assert_eq!(cpp.command, "clangd");
        assert!(
            c.caveat.unwrap().contains("nested functions"),
            "the C entry warns about the limitation users will actually hit"
        );
    }

    #[test]
    fn doctor_covers_every_server() {
        assert_eq!(doctor().len(), SERVERS.len());
    }

    #[test]
    fn a_missing_server_is_reported_with_how_to_install_it() {
        let statuses = vec![Status {
            spec: spec_for(Language::Go).unwrap(),
            found: None,
            broken: None,
        }];
        let text = report(&statuses);
        assert!(text.contains("MISS"), "{text}");
        assert!(text.contains("go install"), "the hint is shown: {text}");
    }

    #[test]
    fn an_installed_server_reports_its_path() {
        let statuses = vec![Status {
            spec: spec_for(Language::Rust).unwrap(),
            found: Some(PathBuf::from("/usr/bin/rust-analyzer")),
            broken: None,
        }];
        let text = report(&statuses);
        assert!(text.contains("ok"), "{text}");
        assert!(text.contains("/usr/bin/rust-analyzer"), "{text}");
    }

    #[test]
    fn a_present_but_broken_server_is_not_reported_as_available() {
        // The rustup shim case: on PATH, executable, exits 0, and useless.
        let status = Status {
            spec: spec_for(Language::Rust).unwrap(),
            found: Some(PathBuf::from("/home/u/.cargo/bin/rust-analyzer")),
            broken: Some("Unknown binary 'rust-analyzer'".into()),
        };
        assert!(!status.available(), "present is not the same as working");

        let text = report(&[status]);
        assert!(text.contains("BAD"), "{text}");
        assert!(text.contains("does not run"), "{text}");
        assert!(text.contains("fix:"), "the hint is shown too: {text}");
    }

    #[test]
    fn probing_a_working_command_succeeds_and_a_missing_one_fails() {
        // `sh --version` writes to stdout on any real shell.
        assert!(probe("sh").is_ok() || probe("bash").is_ok());
        assert!(probe("definitely-not-a-real-binary-xyzzy").is_err());
    }

    #[test]
    fn path_lookup_finds_a_real_executable_and_misses_a_fake_one() {
        // `sh` exists on every unix; the other cannot.
        assert!(find_on_path("sh").is_some(), "sh is on PATH");
        assert!(find_on_path("definitely-not-a-real-binary-xyzzy").is_none());
    }
}
