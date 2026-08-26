//! The dynamic client registration lifecycle: compensation when a
//! registration cannot be recorded, refusal to strand one on the server,
//! and what two concurrent logins do to each other (Stories 2.2 and 2.4).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod oauth_harness;

use oauth_harness::*;
use serde_json::{json, Value};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

// --- findings [8] and [9]: never leave an undeletable registration --------

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_registration_that_cannot_be_saved_is_deleted_again() {
    // The window: DCR succeeded on the server, but the local save failed.
    // The registration_access_token is the only thing that can delete that
    // application, and it is about to be lost - so the flow must undo the
    // registration rather than leave an orphan nobody can remove.
    use std::os::unix::fs::PermissionsExt;

    let server = MockServer::start().await;
    mount_full_instance(&server).await;
    Mock::given(method("DELETE"))
        .and(path("/oauth/clients/dcr-client-1"))
        .and(header("authorization", "Bearer rat-1"))
        .respond_with(ResponseTemplate::new(204))
        // Exactly one compensating delete.
        .expect(1)
        .mount(&server)
        .await;

    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("readonly");
    std::fs::create_dir(&config).unwrap();
    // Writable enough to create the lock file, then sealed before the save.
    let path = config.clone();

    let run = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            // Pre-create the lock so the failure lands on the credential
            // write and not on lock creation.
            drop(otl::auth::lock::CredentialLock::acquire(&path).unwrap());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).unwrap();
            let outcome = drive_login(&path, &base, &[], |_| {});
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
            outcome
        }
    })
    .await
    .unwrap();

    assert_ne!(run.code, Some(0), "a failed save reported success");
    // `.expect(1)` on the DELETE is what proves the compensation happened.
    drop(server);
    assert!(
        !config.join("credentials.toml").exists(),
        "nothing should have been written"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn an_orphan_that_cannot_be_undone_is_reported_with_its_client_id() {
    // Save failed AND the compensating delete failed: an application is
    // stranded on the server. The user has to be told, with the client id
    // an administrator needs to find it.
    use std::os::unix::fs::PermissionsExt;

    let server = MockServer::start().await;
    mount_full_instance(&server).await;
    Mock::given(method("DELETE"))
        .and(path("/oauth/clients/dcr-client-1"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("readonly");
    std::fs::create_dir(&config).unwrap();
    let path = config.clone();

    let run = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            drop(otl::auth::lock::CredentialLock::acquire(&path).unwrap());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).unwrap();
            let outcome = drive_login(&path, &base, &[], |_| {});
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
            outcome
        }
    })
    .await
    .unwrap();

    assert_ne!(run.code, Some(0));
    assert!(
        run.stderr.contains("orphaned"),
        "the stranded application was not reported: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("dcr-client-1"),
        "the client id an admin needs is missing: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("Settings -> Applications"),
        "stderr: {}",
        run.stderr
    );
    // A client id is not a secret (it travels in the authorization URL),
    // but the management token is.
    assert!(!run.stderr.contains("rat-1"), "stderr: {}", run.stderr);
    drop(server);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_superseded_registration_that_cannot_be_removed_blocks_re_registration() {
    // The stored registration is pinned to a port that is now taken, and
    // the server refuses to delete it. Registering a replacement would
    // overwrite the only credential that could ever delete the old one, so
    // the flow must stop instead.
    let server = MockServer::start().await;
    mount_full_instance(&server).await;
    Mock::given(method("DELETE"))
        .and(path("/oauth/clients/dcr-client-1"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();

    // Hold the port the stored registration is pinned to, so rebinding
    // fails and retirement is attempted.
    let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = squatter.local_addr().unwrap().port();

    let path = dir.path().to_path_buf();
    let run = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            seed_session(&path, &base, Some(otl::auth::oauth::now_unix() + 3600));
            // Repin the stored registration to the occupied port and drop
            // the session, so login has to reuse-or-replace the client.
            let store = store(&path);
            let mut file = store.load().unwrap();
            file.profile_mut("default").oauth = None;
            if let Some(client) = file.profile_mut("default").client.as_mut() {
                client.redirect_uri = format!("http://127.0.0.1:{taken}/callback");
            }
            store.save(&file).unwrap();
            drive_login(&path, &base, &[], |_| {})
        }
    })
    .await
    .unwrap();

    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("--force-new-client"),
        "no escape hatch offered: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("overwrite"),
        "the reason must be stated: {}",
        run.stderr
    );
    // The old management credential is intact, so a retry is possible.
    let registration = stored_client(dir.path());
    assert_eq!(
        registration.registration_access_token.as_deref(),
        Some("rat-1")
    );
    // And no replacement was registered.
    let registrations = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request: &Request| request.url.path() == "/oauth/register")
        .count();
    assert_eq!(registrations, 0, "a replacement was registered anyway");
    drop(squatter);
}

// --- finding [16]: a short-lived token must not restart the stampede ------

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_logins_never_leave_an_unmanaged_registration() {
    // Two logins start against an empty profile, both see "no cached
    // client", and both register. Only one management token fits on disk,
    // so the loser must DELETE its own registration rather than overwrite
    // the winner's - otherwise an application is left on the server that
    // nothing can ever remove.
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(metadata(&base)))
        .mount(&server)
        .await;
    // Each registration gets its own id, so the two logins really do own
    // different applications.
    for (index, client) in ["dcr-a", "dcr-b", "dcr-c", "dcr-d"].iter().enumerate() {
        Mock::given(method("POST"))
            .and(path("/oauth/register"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "client_id": client,
                "registration_access_token": format!("rat-{client}"),
                "registration_client_uri": format!("{base}/oauth/clients/{client}")
            })))
            .up_to_n_times(1)
            .with_priority((index + 1) as u8)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(format!("/oauth/clients/{client}")))
            .and(header("authorization", format!("Bearer rat-{client}")))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let outcomes = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            // Neither login gets a redirect, so both stop at the callback
            // wait - after registering and after persisting. That is the
            // window this test is about.
            let children: Vec<_> = (0..2)
                .map(|_| {
                    otl(&path, &base)
                        .args(["auth", "login", "--no-browser", "--timeout", "1"])
                        .spawn()
                        .unwrap()
                })
                .collect();
            children
                .into_iter()
                .map(|child| child.wait_with_output().unwrap())
                .collect::<Vec<_>>()
        }
    })
    .await
    .unwrap();

    for output in &outcomes {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Both fail (no browser), but neither may report an orphan.
        assert!(
            !stderr.contains("orphaned"),
            "a registration was stranded on the server: {stderr}"
        );
    }

    let requests = server.received_requests().await.unwrap();
    let registered: Vec<String> = requests
        .iter()
        .filter(|request| request.url.path() == "/oauth/register")
        .filter_map(|request| {
            serde_json::from_slice::<Value>(&request.body)
                .ok()
                .map(|_| String::new())
        })
        .collect();
    let deleted: Vec<&str> = requests
        .iter()
        .filter(|request| request.method.as_str() == "DELETE")
        .filter_map(|request| request.url.path().rsplit('/').next())
        .collect();

    // Every registration that was created and is NOT the one on disk must
    // have been deleted again.
    let stored_client_id = store(dir.path())
        .load()
        .ok()
        .and_then(|file| file.profile("default").and_then(|p| p.client.clone()))
        .map(|client| client.client_id);
    let created = registered.len();
    let survivors = created - deleted.len();
    eprintln!(
        "concurrent logins: created={created} deleted={}",
        deleted.len()
    );
    assert!(
        created >= 2,
        "the race did not happen: only {created} registration(s) were created, \
         so this test proved nothing"
    );
    assert!(
        survivors <= 1,
        "{created} registrations created, {} deleted: {survivors} left \
         unmanaged on the server",
        deleted.len()
    );
    if survivors == 1 {
        assert!(
            stored_client_id.is_some(),
            "a registration survived on the server with no local record of it"
        );
    }
    drop(server);
}

// --- finding [22]: a server-supplied client id is untrusted terminal text -

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_hostile_client_id_cannot_forge_diagnostic_lines() {
    use std::os::unix::fs::PermissionsExt;

    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(metadata(&base)))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/register"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "client_id": "evil\n\u{1b}[31merror: your credentials were stolen\u{200b}",
            "registration_access_token": "rat-1",
            "registration_client_uri": format!("{base}/oauth/clients/evil")
        })))
        .mount(&server)
        .await;
    // Deletion fails, so the orphan report prints the client id.
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("readonly");
    std::fs::create_dir(&config).unwrap();
    let path = config.clone();

    let run = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            drop(otl::auth::lock::CredentialLock::acquire(&path).unwrap());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).unwrap();
            let outcome = drive_login(&path, &base, &[], |_| {});
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
            outcome
        }
    })
    .await
    .unwrap();

    let printed = format!("{}{}", run.stdout, run.stderr);
    assert!(
        printed.contains("orphaned"),
        "the stranded application must still be reported: {printed}"
    );
    // The id is shown - an admin needs it - but stripped of anything that
    // could forge a line or move the cursor.
    assert!(printed.contains("evil"), "{printed}");
    assert!(
        !printed.contains('\u{1b}'),
        "escape sequence survived: {printed:?}"
    );
    assert!(
        !printed.contains('\u{200b}'),
        "invisible char survived: {printed:?}"
    );
    drop(server);
}
