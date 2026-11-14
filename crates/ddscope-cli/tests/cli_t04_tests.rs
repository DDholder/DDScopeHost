//! CLI end-to-end integration tests for `ddscope-cli` (`info`, `bench`, `replay-verify`).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ddscope_core::transport::{SimConfig, DDRAW_FILE_HEADER_BYTES, DDRAW_RECORD_HEADER_BYTES};
use ddscope_core::types::SourceId;
use ddscope_core::workspace::{CoreWorkspace, SourceConfig};

static CLI_TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

struct TempCaptureFile {
    path: PathBuf,
}

impl TempCaptureFile {
    fn new(name: &str, ext: &str) -> Self {
        let seq = CLI_TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("ddscope_cli_t04_{name}_{pid}_{seq}.{ext}"));
        let _ = std::fs::remove_file(&path);
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempCaptureFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn cli_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ddscope-cli")
}

fn record_sample_ddraw(path: &Path) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let ws = CoreWorkspace::new(rt.handle().clone());
        let src = SourceId::new("rec_src");
        ws.create_source(SourceConfig::sim(
            src.clone(),
            SimConfig {
                sample_rate_hz: 100_000.0,
                analog_channels: 4,
                enable_glitch: false,
                paced: true,
                ..SimConfig::default()
            },
        ))
        .unwrap();
        ws.start_source(&src).unwrap();
        ws.start_raw_recording(&src, path).unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) < 8_192 {
            assert!(
                Instant::now() < deadline,
                "timed out generating sample .ddraw file"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        ws.stop_raw_recording(&src).unwrap();
        ws.shutdown().await.unwrap();
    });
}

#[test]
fn cli_info_prints_core_version() {
    let out = Command::new(cli_bin()).arg("info").output().unwrap();
    assert!(
        out.status.success(),
        "info failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ddscope-core v"),
        "unexpected info output: {stdout}"
    );
}

#[test]
fn cli_bench_two_sources_passes_throughput_and_glitch_check() {
    let out = Command::new(cli_bin())
        .args([
            "bench",
            "--sources",
            "2",
            "--rate",
            "200000",
            "--duration-secs",
            "0.5",
            "--quota-mib",
            "16",
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "bench exited with {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status.code()
    );
    assert!(stdout.contains("DDScopeHost Benchmark Report"));
    assert!(stdout.contains("throughput:"));
    assert!(stdout.contains("latched=true, pinned=true"));
    assert!(stdout.contains("dropped_packets=0, decode_errors=0"));
}

#[test]
fn cli_replay_verify_accepts_valid_ddraw_and_rejects_corrupted_crc() {
    let valid_file = TempCaptureFile::new("valid", "ddraw");
    record_sample_ddraw(valid_file.path());

    let ok_out = Command::new(cli_bin())
        .args([
            "replay-verify",
            "--input",
            valid_file.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let ok_stdout = String::from_utf8_lossy(&ok_out.stdout);
    let ok_stderr = String::from_utf8_lossy(&ok_out.stderr);
    assert!(
        ok_out.status.success(),
        "replay-verify failed on valid .ddraw\nstdout:\n{ok_stdout}\nstderr:\n{ok_stderr}"
    );
    assert!(ok_stdout.contains("CRC32=OK"));
    assert!(ok_stdout.contains("decode_errors=0"));

    // Flip one payload byte inside the first record to break CRC32 checksum.
    let corrupted_file = TempCaptureFile::new("corrupted", "ddraw");
    let mut raw_bytes = std::fs::read(valid_file.path()).unwrap();
    let payload_offset = DDRAW_FILE_HEADER_BYTES + DDRAW_RECORD_HEADER_BYTES;
    assert!(
        raw_bytes.len() > payload_offset,
        "recorded .ddraw unexpectedly small: {} bytes",
        raw_bytes.len()
    );
    raw_bytes[payload_offset] ^= 0xFF;
    std::fs::write(corrupted_file.path(), &raw_bytes).unwrap();

    let bad_out = Command::new(cli_bin())
        .args([
            "replay-verify",
            "--input",
            corrupted_file.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(
        bad_out.status.code(),
        Some(1),
        "expected exit code 1 for corrupted CRC32, got {:?}\nstdout:\n{}\nstderr:\n{}",
        bad_out.status.code(),
        String::from_utf8_lossy(&bad_out.stdout),
        String::from_utf8_lossy(&bad_out.stderr)
    );
    let bad_stderr = String::from_utf8_lossy(&bad_out.stderr);
    assert!(
        bad_stderr.contains("CRC32"),
        "expected CRC32 failure message in stderr, got: {bad_stderr}"
    );
}

#[test]
fn cli_soak_verifies_dod_and_writes_json_report() {
    let report_file = TempCaptureFile::new("soak_report", "json");
    let out = Command::new(cli_bin())
        .args([
            "soak",
            "--sources",
            "2",
            "--rate",
            "200000",
            "--fps",
            "60",
            "--duration-secs",
            "0.8",
            "--quota-mib",
            "16",
            "--verify-dod",
            "--json-out",
            report_file.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "soak failed ({:?})\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status.code()
    );
    assert!(stdout.contains("DDScopeHost M5 Soak Report (passed=true)"));
    assert!(stdout.contains("dropped_packets=0, decode_errors=0"));
    assert!(stdout.contains("latched=true, pinned=true"));
    assert!(stdout.contains("rearmed_pinned_bytes=0"));

    let json_text = std::fs::read_to_string(report_file.path()).expect("read soak JSON");
    assert!(json_text.contains("\"passed\": true"));
    assert!(json_text.contains("\"dropped_packets\": 0"));
}
