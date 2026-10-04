use super::*;

#[test]
fn source_files_and_directories_use_shared_capture_without_viewer_checkpoints() {
    let temp = tempfile::tempdir().unwrap();
    let sources = temp.path().join("sources");
    std::fs::create_dir(&sources).unwrap();
    let source = sources.join("session.jsonl");
    std::fs::write(
        &source,
        idle_history_import::fixtures::HUMAN_SESSION.as_bytes(),
    )
    .unwrap();
    let mut recorded = None;
    for (index, input) in [&source, &sources].into_iter().enumerate() {
        let chain = temp.path().join(format!("chain-{index}"));
        let args = [
            "import",
            "--provider",
            "human",
            "--input",
            input.to_str().unwrap(),
        ];
        let report = result(&chain, &args, b"", 0);
        assert!(
            report.get("raw_ops").unwrap().as_u64().unwrap() > 0,
            "raw evidence was captured"
        );
        assert_eq!(
            result(&chain, &args, b"", 0).get("written"),
            Some(&json!(0))
        );
        let history = result(&chain, &["history", "--limit", "1000"], b"", 0);
        if let Some(expected) = &recorded {
            assert_eq!(
                &history, expected,
                "source selection preserves recorded identities"
            );
        } else {
            recorded = Some(history);
        }
        for host_state in ["live-v1", "editor-v1", "search"] {
            assert!(
                !chain.join(host_state).exists(),
                "engine capture cannot create {host_state}"
            );
        }
        let _clean = result(&chain, &["integrity"], b"", 0);
    }
}

#[test]
fn shared_provider_imports_stdin_are_resumable_and_dry_run_is_read_only() {
    let temp = tempfile::tempdir().unwrap();
    let chain = temp.path().join("chain");
    let fixture = idle_history_import::fixtures::HUMAN_SESSION.as_bytes();
    let args = [
        "import",
        "--provider",
        "human",
        "--input",
        "-",
        "--source-name",
        "session.jsonl",
    ];
    let mut preview = args.to_vec();
    preview.push("--dry-run");
    let _preview = result(&chain, &preview, fixture, 0);
    assert!(!chain.exists(), "dry-run has no destination side effects");
    let first = result(&chain, &args, fixture, 0);
    assert!(
        first.get("written").unwrap().as_u64().unwrap() > 0,
        "raw human evidence was admitted"
    );
    let second = result(&chain, &args, fixture, 0);
    assert_eq!(second.get("written"), Some(&json!(0)));
    let claude = idle_history_import::fixtures::CLAUDE_SESSION.as_bytes();
    let _claude = result(
        &chain,
        &[
            "import",
            "--provider",
            "claude",
            "--input",
            "-",
            "--source-name",
            "session.jsonl",
        ],
        claude,
        0,
    );
}

#[test]
fn shared_codex_import_uses_the_recorded_exporter_contract() {
    let temp = tempfile::tempdir().unwrap();
    let chain = temp.path().join("chain");
    let projection = temp.path().join("projection.ndjson");
    std::fs::write(
        &projection,
        idle_history_import::fixtures::CODEX_PROJECTION.as_bytes(),
    )
    .unwrap();
    let helper = temp.path().join("helper.sh");
    std::fs::write(&helper, "cat \"$1\"\n").unwrap();
    let args = [
        "import",
        "--legacy",
        "--provider",
        "codex",
        "--input",
        "-",
        "--source-name",
        "rollout-contract.jsonl",
        "--workspace",
        "/workspace",
        "--codex-helper",
        "sh",
        "--codex-helper-arg",
        helper.to_str().unwrap(),
        "--codex-helper-arg",
        projection.to_str().unwrap(),
    ];
    let first = result(
        &chain,
        &args,
        idle_history_import::fixtures::CODEX_ROLLOUT.as_bytes(),
        3,
    );
    assert_eq!(first.get("raw_ops"), Some(&json!(7)));
    assert_eq!(first.get("malformed"), Some(&json!(1)));
    let repeated = result(
        &chain,
        &args,
        idle_history_import::fixtures::CODEX_ROLLOUT.as_bytes(),
        0,
    );
    assert_eq!(repeated.get("written"), Some(&json!(0)));
    let state = result(&chain, &["import-state"], b"", 0);
    assert_eq!(
        state.get("codex_items").unwrap().as_array().unwrap().len(),
        3
    );
    let before = Engine::open(&chain)
        .unwrap()
        .snapshot()
        .unwrap()
        .stats()
        .accepted;
    let copied_args: Vec<_> = args
        .iter()
        .map(|arg| {
            if *arg == "rollout-contract.jsonl" {
                "rollout-copy.jsonl"
            } else {
                *arg
            }
        })
        .collect();
    let _copied = result(
        &chain,
        &copied_args,
        idle_history_import::fixtures::CODEX_ROLLOUT.as_bytes(),
        3,
    );
    let copied = result(&chain, &["import-state"], b"", 0);
    assert_eq!(
        copied.get("codex_items").unwrap().as_array().unwrap().len(),
        3,
        "the CLI exposes shared logical reconciliation without viewer code"
    );
    assert!(
        !copied.get("copies").unwrap().as_array().unwrap().is_empty(),
        "exact copy equivalences remain inspectable"
    );
    assert_eq!(
        Engine::open(&chain)
            .unwrap()
            .snapshot()
            .unwrap()
            .stats()
            .accepted,
        before.saturating_mul(2),
        "canonical history keeps both source occurrences"
    );
}
