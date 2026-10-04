//! Import process checks use the separate engine CLI for generic history queries.

use editchain_engine::Engine;
use serde_json::{json, Value};
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::OnceLock,
};
use {
    clap as _, ctrlc as _, editchain_git as _, editchain_store as _, glob as _,
    idle_history_import as _, serde as _,
};

#[path = "bulk.rs"]
mod bulk;
#[path = "sources.rs"]
mod sources;

fn engine_binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        let binary = std::env::var_os("IDLE_EDITCHAIN_BIN").map_or_else(
            || {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../.artifacts/engine/bin")
                    .join(format!("editchain{}", std::env::consts::EXE_SUFFIX))
            },
            PathBuf::from,
        );
        assert!(
            binary.is_file(),
            "install the engine release before integration tests (scripts/lint.sh installs it)"
        );
        binary
    })
}

fn run(chain: &Path, args: &[&str], input: &[u8], code: i32) -> Output {
    let binary = if matches!(args.first(), Some(&"import" | &"import-state" | &"convert")) {
        Path::new(env!("CARGO_BIN_EXE_idle-history-tools"))
    } else {
        engine_binary()
    };
    let mut child = Command::new(binary)
        .arg("--chain")
        .arg(chain)
        .args(["--output", "json"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start history tool");
    child
        .stdin
        .take()
        .expect("input pipe")
        .write_all(input)
        .expect("write input");
    let output = child.wait_with_output().expect("tool output");
    assert_eq!(
        output.status.code(),
        Some(code),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn result(chain: &Path, args: &[&str], input: &[u8], code: i32) -> Value {
    serde_json::from_slice(&run(chain, args, input, code).stdout).expect("tool JSON")
}
