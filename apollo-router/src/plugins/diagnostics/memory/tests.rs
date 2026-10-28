//! Tests for the memory profiling module

use std::path::Path;

use futures::TryStreamExt;
use http::StatusCode;
use http_body_util::BodyExt;
use serde_json::Value;
use tempfile::tempdir;
use tokio::io::AsyncReadExt;

use super::*;

async fn json(response: DiagnosticsResult<Response<Body>>) -> (StatusCode, Value) {
    let response = response.expect("response");
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).expect("JSON body"))
}

/// Writes files into `memory/`, returning the service.
fn service_with_files(dir: &Path, files: &[(&str, &[u8])]) -> MemoryService {
    let memory = dir.join("memory");
    std::fs::create_dir_all(&memory).unwrap();
    for (name, contents) in files {
        std::fs::write(memory.join(name), contents).unwrap();
    }
    MemoryService::new(dir)
}

#[tokio::test]
async fn status_reports_availability() {
    let temp_dir = tempdir().unwrap();
    let (status, body) = json(MemoryService::new(temp_dir.path()).handle_status().await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["platform"], std::env::consts::OS);
    assert_eq!(
        body["heap_dumps_available"],
        crate::heap_profiling::available().is_ok()
    );
}

#[tokio::test]
async fn lists_downloads_and_deletes_only_heap_dumps() {
    let temp_dir = tempdir().unwrap();
    let service = service_with_files(
        temp_dir.path(),
        &[
            ("router_heap_dump_1.pb.gz", b"one"),
            ("router_heap_dump_2.pb.gz", b"two"),
            ("old_format.prof", b"jeprof"),
        ],
    );

    let (status, body) = json(service.handle_list_dumps().await).await;
    assert_eq!(status, StatusCode::OK);
    let mut names: Vec<_> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|dump| dump["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["router_heap_dump_1.pb.gz", "router_heap_dump_2.pb.gz"]
    );

    let download = service
        .handle_download_dump("router_heap_dump_1.pb.gz")
        .await
        .unwrap();
    assert_eq!(download.status(), StatusCode::OK);
    let contents = download.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(contents.as_ref(), b"one");
    assert!(
        service
            .handle_download_dump("old_format.prof")
            .await
            .is_err()
    );
    assert!(
        service
            .handle_download_dump("../router_heap_dump_1.pb.gz")
            .await
            .is_err()
    );

    let (status, _) = json(service.handle_delete_dump("router_heap_dump_1.pb.gz").await).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = json(service.handle_clear_all_dumps().await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["deleted_count"], 1);

    let memory = temp_dir.path().join("memory");
    assert!(!memory.join("router_heap_dump_2.pb.gz").exists());
    assert!(memory.join("old_format.prof").exists(), "not a heap dump");
}

#[tokio::test]
async fn loads_only_heap_dumps_for_the_report() {
    let temp_dir = tempdir().unwrap();
    service_with_files(
        temp_dir.path(),
        &[
            ("router_heap_dump_1.pb.gz", b"one"),
            ("old_format.prof", b"jeprof"),
        ],
    );
    let dumps = load_memory_dumps(&temp_dir.path().join("memory"))
        .await
        .unwrap();
    assert_eq!(dumps.len(), 1);
    assert_eq!(dumps[0].name, "router_heap_dump_1.pb.gz");
    assert_eq!(dumps[0].data, "b25l"); // base64 of "one"
    assert_eq!(dumps[0].size, 3);
}

#[test]
fn empty_dump_explains_why() {
    assert!(dump_message(0).contains("no allocations were sampled"));
    assert!(!dump_message(1).contains("no allocations"));
}

#[tokio::test]
async fn test_add_to_archive_with_files() {
    let temp_dir = tempdir().unwrap();
    service_with_files(
        temp_dir.path(),
        &[("router_heap_dump_1.pb.gz", b"test heap dump data")],
    );

    let (mut reader, writer) = tokio::io::duplex(1024 * 1024);
    let mut tar = tokio_tar::Builder::new(writer);
    MemoryService::add_to_archive(&mut tar, temp_dir.path())
        .await
        .unwrap();
    tar.finish().await.unwrap();
    drop(tar);

    let mut archive_data = Vec::new();
    reader.read_to_end(&mut archive_data).await.unwrap();
    let mut archive = tokio_tar::Archive::new(archive_data.as_slice());
    let paths: Vec<String> = archive
        .entries()
        .unwrap()
        .map_ok(|entry| entry.path().unwrap().to_string_lossy().to_string())
        .try_collect()
        .await
        .unwrap();
    assert_eq!(paths, ["memory/router_heap_dump_1.pb.gz"]);
}

#[tokio::test]
async fn test_add_to_archive_empty_directory() {
    let temp_dir = tempdir().unwrap();

    let (mut reader, writer) = tokio::io::duplex(1024 * 1024);
    let mut tar = tokio_tar::Builder::new(writer);
    MemoryService::add_to_archive(&mut tar, temp_dir.path())
        .await
        .unwrap();
    tar.finish().await.unwrap();
    drop(tar);

    let mut archive_data = Vec::new();
    reader.read_to_end(&mut archive_data).await.unwrap();
    let mut archive = tokio_tar::Archive::new(archive_data.as_slice());
    let entries: Vec<_> = archive.entries().unwrap().try_collect().await.unwrap();
    assert_eq!(entries.len(), 1, "just the empty memory directory");
    assert_eq!(entries[0].path().unwrap().to_string_lossy(), "memory/");
    assert!(entries[0].header().entry_type().is_dir());
}

#[cfg(not(all(feature = "global-allocator", not(feature = "dhat-heap"), unix)))]
#[tokio::test]
async fn profiling_is_not_implemented_where_unavailable() {
    let temp_dir = tempdir().unwrap();
    let service = MemoryService::new(temp_dir.path());
    for response in [
        service.handle_start().await,
        service.handle_stop().await,
        service.handle_dump().await,
    ] {
        let (status, body) = json(response).await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        assert_eq!(body["status"], "not_supported");
    }
}

#[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
mod supported {
    use super::*;
    use crate::heap_profiling;
    use crate::heap_profiling::TEST_LOCK;
    use crate::heap_profiling::test_pprof;

    /// Start a session, dump a symbolized pprof heap profile, then stop.
    #[tokio::test]
    async fn dumps_symbolized_pprof() {
        let _lock = TEST_LOCK.lock().await;
        let temp_dir = tempdir().unwrap();
        let service = MemoryService::new(temp_dir.path());

        let (status, body) = json(service.handle_start().await).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, body) = json(service.handle_status().await).await;
        assert_eq!(body["profiling_active"], true);
        assert_eq!(body["sampling_active"], true);

        let held = test_pprof::allocate_for_profile();
        let (status, body) = json(service.handle_dump().await).await;
        drop(held);
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["samples"].as_u64().unwrap() > 0, "{body}");
        let dump_path = Path::new(body["dump_path"].as_str().unwrap()).to_owned();
        assert_eq!(dump_path.parent().unwrap(), temp_dir.path().join("memory"));
        let name = dump_path.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with("router_heap_dump_") && name.ends_with(".pb.gz"),
            "{name}"
        );

        let pprof = test_pprof::Profile::decode_gzipped(&std::fs::read(&dump_path).unwrap());
        assert_eq!(pprof.sample_type(), ("inuse-space", "bytes"));
        let total = pprof.total();
        assert!(total >= 32 * 1024 * 1024, "in-use bytes: {total}");
        assert!(
            pprof.has_function("allocate_for_profile"),
            "profile is symbolized in-process"
        );

        let (status, _) = json(service.handle_stop().await).await;
        assert_eq!(status, StatusCode::OK);
        let (_, body) = json(service.handle_status().await).await;
        assert_eq!(body["profiling_active"], false);
        assert_eq!(body["sampling_active"], false);
    }

    /// Stopping a session leaves sampling on for other users of the heap profiler, and vice versa.
    #[tokio::test]
    async fn stopping_leaves_sampling_on_for_other_users() {
        let _lock = TEST_LOCK.lock().await;
        let temp_dir = tempdir().unwrap();
        let service = MemoryService::new(temp_dir.path());

        let other = heap_profiling::start_sampling().unwrap();
        json(service.handle_start().await).await;
        json(service.handle_stop().await).await;
        assert!(heap_profiling::is_sampling().unwrap());

        json(service.handle_start().await).await;
        drop(other);
        assert!(heap_profiling::is_sampling().unwrap());

        json(service.handle_stop().await).await;
        assert!(!heap_profiling::is_sampling().unwrap());
    }
}
