//! Archiving a session tears down its terminal session and worktree but — unlike
//! delete — keeps the session row (marked `archived`), the git branch, and the
//! weaver history, and clears the attention tag. The teardown cascades to the
//! launcher's finished child sessions; a child that is still running (or that
//! opted out of automatic archive) survives and is reported as a warning.

use std::path::Path;
use std::time::Duration;

use serde_json::json;
use serial_test::serial;
use tokio::io::AsyncWriteExt;

use loom::backend;

use crate::fixtures::{branch_tag, plant_claude_transcript, HomeGuard, TestServer};
use weaver_api::operations::branches;

/// Archiving captures the agent's conversation log: it locates the Claude Code
/// transcript for the worktree (under `~/.claude/projects/<munged-cwd>/`),
/// normalizes it, and writes a rendered `chat.md` and an iris `chat.json` under
/// the configured session log dir — all before the worktree is torn down.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_captures_the_conversation_log() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    // Point HOME (transcript source) and the log dir (capture sink) at temp dirs.
    let home = tempfile::tempdir().unwrap();
    let _home_guard = HomeGuard::set(home.path());
    let logs = tempfile::tempdir().unwrap();

    let sess = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "log me", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap().to_string();
    let work_dir = sess["work_dir"].as_str().unwrap().to_string();

    // Set the capture sink via the settings API.
    client
        .post(
            "/api/settings/patch",
            json!({ "changes": { "session.log_dir": logs.path().to_string_lossy() } }),
        )
        .await
        .unwrap();

    // Plant a Claude transcript where the agent would have written it.
    plant_claude_transcript(
        home.path(),
        &work_dir,
        "implement the thing",
        "Done — shipped it.",
    );

    let res = client
        .post("/api/sessions/archive", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(res["archived"], true);
    let branch = res["branch"].as_str().unwrap();
    let slug = branch.replace('/', "-");

    // Both the rendered markdown and the normalized iris JSON are written.
    let md = std::fs::read_to_string(logs.path().join(&slug).join("chat.md"))
        .expect("chat.md should be captured on archive");
    assert!(md.contains("# Conversation log"), "rendered markdown: {md}");
    assert!(
        md.contains("implement the thing"),
        "user turn captured: {md}"
    );
    assert!(
        md.contains("Done — shipped it."),
        "assistant turn captured: {md}"
    );

    let raw_json = std::fs::read_to_string(logs.path().join(&slug).join("chat.json"))
        .expect("chat.json should be captured on archive");
    let iris: serde_json::Value = serde_json::from_str(&raw_json).unwrap();
    assert_eq!(iris["source"], "claude");
    assert_eq!(iris["messages"].as_array().unwrap().len(), 2);
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_survives_request_disconnect() {
    let ts = TestServer::start().await;
    let created = ts
        .client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "archive after disconnect", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let work_dir = created["work_dir"].as_str().unwrap().to_string();

    // A raw connection lets the test close the transport without waiting for a
    // response, matching a browser navigation or reverse-proxy disconnect.
    let mut connection = tokio::net::TcpStream::connect(ts.addr).await.unwrap();
    let body = json!({ "session": id }).to_string();
    let request = format!(
        "POST /api/sessions/archive HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        ts.addr,
        body.len(),
        body
    );
    connection.write_all(request.as_bytes()).await.unwrap();
    connection.flush().await.unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let session = loom::session::get(&ts.state.db, &id)
                .await
                .unwrap()
                .unwrap();
            if session.lifecycle_transition.as_deref() == Some("archiving") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("archive should start");

    drop(connection);

    let archived = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let session = loom::session::get(&ts.state.db, &id)
                .await
                .unwrap()
                .unwrap();
            if session.status == "archived" {
                break session;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("archive should finish after its request disconnects");

    assert!(archived.lifecycle_transition.is_none());
    assert!(!Path::new(&work_dir).exists());
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_keeps_branch_and_history() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let arch = client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "archive me",
                "cwd": ts.cwd(),
                "agent": "shell",
            }),
        )
        .await
        .unwrap();
    let arch_id = arch["id"].as_str().unwrap().to_string();
    let arch_session = arch["term_session"].as_str().unwrap().to_string();
    let arch_work_dir = arch["work_dir"].as_str().unwrap().to_string();
    assert!(
        backend::has_session(&arch_session).await,
        "archive session missing"
    );
    assert!(
        Path::new(&arch_work_dir).exists(),
        "archive worktree missing"
    );

    // Flag the session for attention; archiving must clear it (a torn-down
    // workstream can't still "need me"). The recorded `tag` event (authored
    // `manual`) doubles as branch history we expect to survive the archive. The
    // message (description) is a separate branch field, patched alongside.
    client
        .post(
            "/api/sessions/tags/set",
            json!({ "key": "attention", "value": "attention", "by": "manual", "session": arch_id }),
        )
        .await
        .unwrap();
    // A watch's typed loud mark (a non-well-known key on the ladder): archiving
    // must clear it too — loudness is value-driven, not a fixed key set.
    client
        .post(
            "/api/sessions/tags/set",
            json!({ "key": "review", "value": "attention", "by": "status-check", "session": arch_id }),
        )
        .await
        .unwrap();
    // The soothing `idle` mark is quiet (not on the loud ladder) but is still a
    // lifecycle signal a torn-down workstream shouldn't carry: archiving clears
    // it too.
    client
        .post(
            "/api/sessions/tags/set",
            json!({ "key": "idle", "value": "idle", "by": "agent", "session": arch_id }),
        )
        .await
        .unwrap();
    // The per-session opt-out gates automatic retention only. An explicit
    // operator Archive must still tear the session down.
    client
        .post(
            "/api/sessions/tags/set",
            json!({ "key": "auto-archive", "value": "disabled", "by": "manual", "session": arch_id }),
        )
        .await
        .unwrap();
    client
        .post(
            "/api/sessions/update",
            json!({ "description": "Waiting for input", "session": arch_id }),
        )
        .await
        .unwrap();

    let res = client
        .post("/api/sessions/archive", json!({ "session": arch_id }))
        .await
        .unwrap();
    assert_eq!(res["archived"], true);
    assert!(
        !backend::has_session(&arch_session).await,
        "archive should kill the terminal session"
    );
    assert!(
        !Path::new(&arch_work_dir).exists(),
        "archive should remove the worktree"
    );

    // The session row persists, now terminal/`archived`.
    let view = client
        .post("/api/sessions/get", json!({ "session": arch_id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "archived");
    let channel = client
        .post("/api/channels/get", json!({ "channel": arch_id }))
        .await
        .unwrap();
    assert_eq!(channel["state"], "archived");
    // Archiving cleared the attention tag so the dashboard stops flagging it
    // (absence is the calm state). The message (description) is kept as history.
    assert!(
        branch_tag(&view, "attention").is_none(),
        "archive should clear the attention tag"
    );
    assert!(
        branch_tag(&view, "review").is_none(),
        "archive should clear a watch's typed loud mark too"
    );
    assert!(
        branch_tag(&view, "idle").is_none(),
        "archive should clear the soothing idle mark too"
    );
    assert_eq!(
        branch_tag(&view, "auto-archive").unwrap()["value"],
        "disabled",
        "manual archive ignores and preserves the quiet automatic-retention override"
    );
    assert_eq!(
        view["branch"]["description"], "Waiting for input",
        "archive keeps the status message as history"
    );
    // The git branch is left intact for future reference.
    assert!(
        weaver_core::git::branch_exists(ts.repo_path(), "weaver/archive-me").await,
        "archive must not delete the branch"
    );
    // The branch event history survives the archive (unlike delete). The typed
    // client uses the branch-owned operation; `sessions.events.list` wraps the
    // same handler keyed by session and must remain an exact compatibility
    // alias.
    let branch_log = client
        .invoke::<branches::events::list::Op>(&branches::events::list::Input {
            branch: arch_id.to_string(),
        })
        .await
        .unwrap();
    let session_log = client
        .post("/api/sessions/events/list", json!({ "session": arch_id }))
        .await
        .unwrap();
    assert!(
        serde_json::to_string(&branch_log)
            .unwrap()
            .contains("manual"),
        "branch history should survive archive"
    );
    assert_eq!(
        serde_json::to_value(&branch_log).unwrap(),
        session_log,
        "sessions.events.list must remain a compatibility alias for branches.events.list"
    );

    // An archived session can still be fully removed afterwards.
    client
        .post("/api/sessions/delete", json!({ "session": arch_id }))
        .await
        .unwrap();
}

/// Archiving a launcher cascades to the children it delegated to: finished,
/// failed, and orphaned children are torn down with it — and a finished
/// child's own finished children follow — while a child that is still running,
/// and one carrying the automatic-archive opt-out, survive and are reported
/// as warnings instead.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_cascades_to_finished_children_and_warns_about_the_survivors() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let parent = client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "delegate the work",
                "cwd": ts.cwd(),
                "agent": "shell",
                "name": "launcher",
            }),
        )
        .await
        .unwrap();
    let parent_id = parent["id"].as_str().unwrap().to_string();
    let parent_branch = parent["branch"]["id"].as_str().unwrap().to_string();

    let launch_child = |name: &str, parent_branch: &str| {
        let goal = format!("delegated {name} work");
        client.post(
            "/api/sessions/launch",
            json!({
                "goal": goal,
                "cwd": ts.cwd(),
                "agent": "shell",
                "name": name,
                "parent_branch": parent_branch,
            }),
        )
    };

    let finished = launch_child("finished", &parent_branch).await.unwrap();
    let finished_id = finished["id"].as_str().unwrap().to_string();
    let finished_branch = finished["branch"]["id"].as_str().unwrap().to_string();

    // Launched while its parent branch is still active, so the grandchild's
    // provenance points at the finished child's session.
    let grandchild = launch_child("grandchild", &finished_branch).await.unwrap();
    let grandchild_id = grandchild["id"].as_str().unwrap().to_string();

    let failed = launch_child("failed", &parent_branch).await.unwrap();
    let failed_id = failed["id"].as_str().unwrap().to_string();
    let lost = launch_child("lost", &parent_branch).await.unwrap();
    let lost_id = lost["id"].as_str().unwrap().to_string();
    let busy = launch_child("busy", &parent_branch).await.unwrap();
    let busy_id = busy["id"].as_str().unwrap().to_string();
    let optout = launch_child("optout", &parent_branch).await.unwrap();
    let optout_id = optout["id"].as_str().unwrap().to_string();

    for (id, status) in [
        (&finished_id, "done"),
        (&grandchild_id, "done"),
        (&failed_id, "error"),
        (&lost_id, "orphaned"),
        (&busy_id, "running"),
        (&optout_id, "done"),
    ] {
        loom::session::set_status(&ts.state.db, id, status)
            .await
            .unwrap();
    }

    // The per-session opt-out must keep an automatic cascade away from the
    // tagged child; archiving it by hand remains available.
    client
        .post(
            "/api/sessions/tags/set",
            json!({
                "key": "auto-archive",
                "value": "disabled",
                "by": "manual",
                "session": optout_id,
            }),
        )
        .await
        .unwrap();

    let res = client
        .post("/api/sessions/archive", json!({ "session": parent_id }))
        .await
        .unwrap();
    assert_eq!(res["archived"], true);

    let warnings: Vec<String> = res["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap().to_string())
        .collect();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains(&busy_id) && w.contains("is running")),
        "a running child is reported, not archived: {warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains(&optout_id) && w.contains("opted out")),
        "the auto-archive opt-out child is reported, not archived: {warnings:?}"
    );
    for archived_id in [&finished_id, &grandchild_id, &failed_id, &lost_id] {
        assert!(
            warnings.iter().all(|w| !w.contains(archived_id.as_str())),
            "an archived child ({archived_id}) must not be reported as a survivor: {warnings:?}"
        );
    }

    for id in [&finished_id, &grandchild_id, &failed_id, &lost_id] {
        let row = loom::session::get(&ts.state.db, id).await.unwrap().unwrap();
        assert_eq!(
            row.status, "archived",
            "child {id} cascaded with its launcher"
        );
        assert!(
            !Path::new(&row.work_dir).exists(),
            "child {id} had its worktree removed"
        );
    }
    let busy_row = loom::session::get(&ts.state.db, &busy_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(busy_row.status, "running", "a running child keeps running");
    assert!(
        Path::new(&busy_row.work_dir).exists(),
        "a running child keeps its worktree"
    );
    let optout_row = loom::session::get(&ts.state.db, &optout_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        optout_row.status, "done",
        "the opted-out child is untouched by the cascade"
    );
    assert!(
        Path::new(&optout_row.work_dir).exists(),
        "the opted-out child keeps its worktree"
    );
    let optout_view = client
        .post("/api/sessions/get", json!({ "session": optout_id }))
        .await
        .unwrap();
    assert!(
        branch_tag(&optout_view, "auto-archive").is_some(),
        "the surviving child keeps its opt-out tag"
    );
}
