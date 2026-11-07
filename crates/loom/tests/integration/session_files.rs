use std::path::Path;

use serde_json::json;
use serial_test::serial;

use crate::fixtures::TestServer;

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_file_previews_and_raw_bytes_stay_inside_the_worktree() {
    let ts = TestServer::start().await;
    let session = ts
        .client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "file previews", "cwd": ts.cwd(), "agent": "shell"
            }),
        )
        .await
        .unwrap();
    let id = session["id"].as_str().unwrap();
    let root = Path::new(session["work_dir"].as_str().unwrap());
    std::fs::create_dir(root.join("reports")).unwrap();
    let cases: &[(&str, &[u8], &str)] = &[
        ("reports/index.md", b"# Report\n", "markdown"),
        ("report.html", b"<h1>Report</h1>", "html"),
        ("script.rs", b"fn main() {}", "text"),
        ("results.json", b"{\"rows\": 42}", "text"),
        ("plot.APNG", b"image data", "image"),
        ("photo.jfif", b"image data", "image"),
        ("binary.dat", b"\0\xff", "download"),
        (
            "image.svg",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
            "image",
        ),
        ("report.pdf", b"%PDF-1.4", "download"),
    ];
    for (path, bytes, kind) in cases {
        std::fs::write(root.join(path), bytes).unwrap();
        let preview = ts
            .client
            .post("/api/sessions/file", json!({ "session": id, "path": path }))
            .await
            .unwrap();
        assert_eq!(preview["path"], *path);
        assert_eq!(preview["kind"], *kind);
        if matches!(*kind, "markdown" | "html" | "text") {
            assert_eq!(preview["content"], std::str::from_utf8(bytes).unwrap());
        } else {
            assert!(preview["content"].is_null());
        }
    }
    std::fs::write(root.join("large.txt"), vec![b'x'; 2 * 1024 * 1024 + 1]).unwrap();
    let large = ts
        .client
        .post(
            "/api/sessions/file",
            json!({ "session": id, "path": "large.txt" }),
        )
        .await
        .unwrap();
    assert_eq!(large["kind"], "download");
    assert!(large["content"].is_null());

    let outside = tempfile::NamedTempFile::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("escape.md")).unwrap();
    std::os::unix::fs::symlink(root.join("reports/index.md"), root.join("inside.md")).unwrap();
    let inside = ts
        .client
        .post(
            "/api/sessions/file",
            json!({ "session": id, "path": "inside.md" }),
        )
        .await
        .unwrap();
    assert_eq!(inside["content"], "# Report\n");

    let http = reqwest::Client::new();
    for path in [
        "../outside",
        "/etc/passwd",
        "reports/../../outside",
        "reports\\..\\..\\outside",
        "escape.md",
        "reports",
    ] {
        let preview = http
            .post(format!("{}/api/sessions/file", ts.client.base()))
            .json(&json!({ "session": id, "path": path }))
            .send()
            .await
            .unwrap();
        assert_eq!(preview.status(), 400, "preview accepted {path}");
        let raw = http
            .get(format!("{}/api/sessions/raw", ts.client.base()))
            .query(&[("session", id), ("path", path)])
            .send()
            .await
            .unwrap();
        assert_eq!(raw.status(), 400, "raw accepted {path}");
    }
    let missing = http
        .post(format!("{}/api/sessions/file", ts.client.base()))
        .json(&json!({ "session": id, "path": "missing.md" }))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
    let raw = http
        .get(format!("{}/api/sessions/raw", ts.client.base()))
        .query(&[("session", id), ("path", "report.html")])
        .send()
        .await
        .unwrap();
    assert_eq!(raw.status(), 200);
    assert_eq!(raw.headers()["x-content-type-options"], "nosniff");
    assert_eq!(raw.headers()["cross-origin-resource-policy"], "same-origin");
    assert!(raw.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .split(';')
        .any(|directive| directive.trim() == "sandbox allow-scripts"));
    assert_eq!(raw.text().await.unwrap(), "<h1>Report</h1>");
    let image = http
        .get(format!("{}/api/sessions/raw", ts.client.base()))
        .query(&[("session", id), ("path", "plot.APNG")])
        .send()
        .await
        .unwrap();
    assert_eq!(image.status(), 200);
    assert_eq!(image.headers()["content-type"], "image/apng");
    assert_eq!(image.bytes().await.unwrap().as_ref(), b"image data");
}

/// `sessions.image` — chat prose embeds host-absolute paths
/// (`![mockup](/tmp/shot.png)`); the conversation view points the `<img src>`
/// here, so the endpoint serves loom-visible image bytes and nothing else.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_path_images_serve_sniffed_raster_bytes_only() {
    let ts = TestServer::start().await;
    let session = ts
        .client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "host images", "cwd": ts.cwd(), "agent": "shell"
            }),
        )
        .await
        .unwrap();
    let id = session["id"].as_str().unwrap();
    let png = b"\x89PNG\r\n\x1a\npayload";

    // Outside the worktree — the point of the host form.
    let outside = tempfile::tempdir().unwrap();
    let shot = outside.path().join("shot.png");
    std::fs::write(&shot, png).unwrap();
    let link = outside.path().join("link.png");
    std::os::unix::fs::symlink(&shot, &link).unwrap();

    // A worktree file addressed absolutely — agents paste worktree paths too.
    let root = Path::new(session["work_dir"].as_str().unwrap());
    std::fs::write(root.join("capture.png"), png).unwrap();

    let http = reqwest::Client::new();
    let base = format!("{}/api/sessions/image", ts.client.base());
    for path in [
        shot.to_str().unwrap(),
        link.to_str().unwrap(),
        root.join("capture.png").to_str().unwrap(),
    ] {
        let image = http
            .get(&base)
            .query(&[("session", id), ("path", path)])
            .send()
            .await
            .unwrap();
        assert_eq!(image.status(), 200, "refused host image {path}");
        assert_eq!(image.headers()["content-type"], "image/png");
        assert_eq!(image.headers()["x-content-type-options"], "nosniff");
        assert_eq!(
            image.headers()["cross-origin-resource-policy"],
            "same-origin"
        );
        assert_eq!(image.bytes().await.unwrap().as_ref(), png);
    }

    // Anything that is not a sniffable raster is refused whatever the name
    // claims — a host path is agent-chosen, so bytes are the only truth.
    let text = outside.path().join("notes.txt");
    std::fs::write(&text, b"definitely not an image").unwrap();
    let fake = outside.path().join("fake.png");
    std::fs::write(&fake, b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>").unwrap();
    for path in [
        text.to_str().unwrap(),
        fake.to_str().unwrap(),
        "/nonexistent/missing.png",
    ] {
        let image = http
            .get(&base)
            .query(&[("session", id), ("path", path)])
            .send()
            .await
            .unwrap();
        assert_eq!(image.status(), 404, "served {path}");
    }

    // Shape violations are the caller's fault: relative paths (that is
    // `sessions.raw`'s job), backslashes, and the empty string.
    for path in ["shot.png", "reports/../shot.png", "C:\\shot.png", ""] {
        let image = http
            .get(&base)
            .query(&[("session", id), ("path", path)])
            .send()
            .await
            .unwrap();
        assert_eq!(image.status(), 400, "accepted {path}");
    }
}
