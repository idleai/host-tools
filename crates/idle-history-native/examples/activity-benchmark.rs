//! Reproducible accepted-record timeline measurements with bounded native requests.

use editchain_git as _;
use sha2 as _;
use std::{
    io::{self, Write as _},
    path::{Path, PathBuf},
    time::Instant,
};

use editchain_core::{
    Op, OpId, Payload,
    activity::{ContentUpdate, ItemId, Kind, Message, MessageKind, Operation, Stage, UpdateMode},
};
use editchain_engine::{Engine, encode_op};
use idle_history::{
    binding::RepositoryChainBinding,
    timeline::{Action, Position, Request, Response, VERSION, View, Window},
};
use idle_history_native::{history::service::Binding, timeline};
use {
    editchain_index as _, idle_editor_capture as _, idle_history_graph as _,
    idle_history_import as _, idle_host_io as _, idle_protocol as _, idle_repository as _,
    serde as _, tempfile as _,
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn id(value: u64) -> OpId {
    OpId::from_bytes(*blake3::hash(&value.to_le_bytes()).as_bytes())
}

fn operation(value: u64, branches: u64) -> Result<Op> {
    let session = value
        .checked_rem(branches)
        .ok_or("Positive branch count required")?;
    let mut record = Operation::new(
        id(value),
        ItemId(id(value)),
        ItemId(id(u64::MAX)),
        Kind::Message(Message {
            category: MessageKind::Text,
            stage: Stage::Snapshot,
            audience: Vec::new(),
            blocks: vec![ContentUpdate {
                block: ItemId(id(value)),
                position: Some(0),
                mode: UpdateMode::Replace,
                previous: None,
                media_type: Payload::Empty,
                content: Payload::Inline(
                    format!("Activity {value}: recorded work on branch {session}").into_bytes(),
                ),
            }],
            coverage: None,
            outcome: None,
        }),
    );
    record.time_ms = Some(value);
    record.session = Some(ItemId(id(u64::MAX.saturating_sub(session))));
    record.parents = value
        .checked_sub(branches)
        .filter(|value| *value > 0)
        .map(id)
        .into_iter()
        .collect();
    if value > 1 && value <= branches {
        record.parents.push(id(1));
    }
    record.into_op().map_err(Into::into)
}

fn generate(root: &Path, count: u64, branches: u64) -> Result {
    let marker = root.join("activity-benchmark-count");
    let existing = if marker.exists() {
        std::fs::read_to_string(&marker)?.parse::<u64>()?
    } else {
        0
    };
    let engine = Engine::open(root)?;
    let mut writer = engine.writer()?;
    let mut value = existing.saturating_add(1);
    while value <= count {
        let end = value.saturating_add(1_000).min(count.saturating_add(1));
        let records: Vec<_> = (value..end)
            .map(|value| {
                operation(value, branches).and_then(|op| encode_op(&op).map_err(Into::into))
            })
            .collect::<Result<_>>()?;
        let _admitted =
            writer.append_encoded_batch(&records.iter().map(Vec::as_slice).collect::<Vec<_>>())?;
        value = end;
    }
    std::fs::write(marker, count.to_string())?;
    Ok(())
}

fn query(binding: &Binding, action: Action) -> Result<Response> {
    Ok(timeline::execute(
        binding,
        &Request {
            version: VERSION,
            action,
        },
    )?)
}

fn window(binding: &Binding, position: Position) -> Result<Window> {
    let mut response = query(
        binding,
        Action::Window {
            view: View::default(),
            position,
            limit: 200,
        },
    )?;
    let mut requests = 0u64;
    loop {
        let progress = match response {
            Response::Window(window) => {
                let Some(progress) = window.rebuilding else {
                    return Ok(window);
                };
                progress
            }
            Response::Building(progress) => progress,
            Response::Found(_) | Response::Cancelled | Response::Stale => {
                return Err("Unexpected benchmark response".into());
            }
        };
        requests = requests.saturating_add(1);
        if requests.is_multiple_of(100) {
            writeln!(
                io::stderr().lock(),
                "{}: {} records ({requests} batches)",
                progress.stage,
                progress.processed
            )?;
        }
        response = query(
            binding,
            Action::Advance {
                build: progress.build,
            },
        )?;
    }
}

fn p95(samples: &mut [u128]) -> u128 {
    samples.sort_unstable();
    samples
        .get(
            samples
                .len()
                .saturating_mul(95)
                .div_ceil(100)
                .saturating_sub(1),
        )
        .copied()
        .unwrap_or(0)
}

fn append_samples(binding: &Binding, count: u64, branches: u64) -> Result<serde_json::Value> {
    let start = Instant::now();
    let engine = Engine::open(&binding.chain_directory)?;
    let mut writer = engine.writer()?;
    // A capture process retains this writer. Report its one-time admission
    // replay separately from steady writes and derived timeline updates.
    let _duplicate = writer.append(&operation(1, branches)?)?;
    let writer_resume_ms = start.elapsed().as_millis();
    let mut capture = Vec::new();
    let mut timeline = Vec::new();
    for sample in 1..=5 {
        let next = count.saturating_add(sample);
        let start = Instant::now();
        let _admitted = writer.append(&operation(next, branches)?)?;
        capture.push(start.elapsed().as_micros());
        std::fs::write(
            binding.chain_directory.join("activity-benchmark-count"),
            next.to_string(),
        )?;
        let start = Instant::now();
        let appended = window(binding, Position::Latest)?;
        timeline.push(start.elapsed().as_micros());
        if appended.activities != next {
            return Err("Appended record is missing".into());
        }
    }
    Ok(serde_json::json!({"writer_resume_ms":writer_resume_ms,
        "capture_p95_us":p95(&mut capture),"timeline_p95_us":p95(&mut timeline),
        "records_after_append":count.saturating_add(5)}))
}

fn main() -> Result {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().ok_or("Output directory required")?);
    let count = args.next().ok_or("Record count required")?.parse::<u64>()?;
    let branches = args.next().unwrap_or_else(|| "100".into()).parse::<u64>()?;
    let warm = args.next().as_deref() == Some("--warm");
    if count == 0 || branches == 0 || branches > 100 {
        return Err("Use positive record and branch counts, with at most 100 branches".into());
    }
    let start = Instant::now();
    if !warm {
        generate(&root, count, branches)?;
    }
    let generation_ms = start.elapsed().as_millis();
    let binding = Binding {
        repository: RepositoryChainBinding {
            workspace_id: "benchmark".into(),
            repository_id: "benchmark".into(),
            chain: "benchmark".into(),
        },
        chain_directory: root.clone(),
        retained_directory: None,
        repository_directory: None,
    };
    let start = Instant::now();
    let latest = window(&binding, Position::Latest)?;
    let cold_ms = start.elapsed().as_millis();
    if latest.activities != count || latest.rows.len() > 200 {
        return Err(io::Error::other("Benchmark result has incorrect counts").into());
    }
    let mut windows = Vec::new();
    let mut seeks = Vec::new();
    for sample in 1u64..=50 {
        let start = Instant::now();
        let _result = window(&binding, Position::Latest)?;
        windows.push(start.elapsed().as_micros());
        let occurrence = format!(
            "current:{}",
            id(1u64.saturating_add(
                sample
                    .saturating_mul(104_729)
                    .checked_rem(count)
                    .ok_or("Positive record count required")?
            ))
        );
        let start = Instant::now();
        let _result = window(&binding, Position::Seek(occurrence))?;
        seeks.push(start.elapsed().as_micros());
    }
    let append = append_samples(&binding, count, branches)?;
    let results = serde_json::json!({"records":count,"branches":branches,"generation_ms":generation_ms,"cold_build_ms":(!warm).then_some(cold_ms),"first_window_ms":warm.then_some(cold_ms),"window_p95_us":p95(&mut windows),"seek_p95_us":p95(&mut seeks),"append":append,"window_bytes":serde_json::to_vec(&latest)?.len(),"max_lane":latest.max_lane});
    writeln!(io::stdout().lock(), "{results}")?;
    std::fs::write(
        root.join("activity-benchmark.json"),
        serde_json::to_vec_pretty(&results)?,
    )?;
    Ok(())
}
