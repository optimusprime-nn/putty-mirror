//! File transfer end to end, against a real SFTP server on localhost, and a
//! check of our key files with the real PuTTY.

#[path = "support/server.rs"]
mod server;

use deck_core::config::{Protocol, Target};
use deck_core::hostkeys::{self, MemoryStore};
use deck_core::ppk::{blobs_of, Ppk};
use deck_core::session::connect_sftp;
use russh::keys::Algorithm;
use russh::MethodKind;
use server::{new_key, start_with_sftp, USER};
use std::process::Command;
use std::sync::Arc;

const PLINK: &str = r"C:\Program Files\PuTTY\plink.exe";

/// A store that already trusts `key`, so that nothing is asked for.
fn trusting(key: &russh::keys::PrivateKey, port: u16) -> Arc<MemoryStore> {
    let store = Arc::new(MemoryStore::default());
    hostkeys::remember(store.as_ref(), "127.0.0.1", port, key.public_key()).unwrap();
    store
}

#[tokio::test(flavor = "multi_thread")]
async fn files_go_there_and_back() {
    let host_key = new_key(Algorithm::Ed25519);
    let client_key = new_key(Algorithm::Ed25519);
    let (port, _) = start_with_sftp(host_key.clone(), vec![MethodKind::PublicKey]).await;

    let path = std::env::temp_dir().join(format!("deck-sftp-{}.pem", std::process::id()));
    let pem = client_key.to_openssh(russh::keys::ssh_key::LineEnding::LF).unwrap();
    std::fs::write(&path, pem.as_bytes()).unwrap();
    let mut target = Target::new(Protocol::Ssh, "127.0.0.1", port);
    target.user = Some(USER.to_string());
    target.key_file = Some(path.display().to_string());
    target.settings.set("TryAgent", deck_core::settings::Value::Int(0));

    let sftp = connect_sftp(target, trusting(&host_key, port)).await.unwrap();
    std::fs::remove_file(&path).unwrap();
    let data: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
    let mut f = sftp.session.create("big.bin").await.unwrap();
    tokio::io::AsyncWriteExt::write_all(&mut f, &data).await.unwrap();
    tokio::io::AsyncWriteExt::shutdown(&mut f).await.unwrap();
    assert_eq!(sftp.session.read("big.bin").await.unwrap(), data);
    assert_eq!(sftp.session.metadata("big.bin").await.unwrap().size, Some(200_000));
    assert_eq!(sftp.session.canonicalize(".").await.unwrap(), "/");
}

/// Run by hand: needs PuTTY installed. Connects only to a server on localhost,
/// and `-hostkey` keeps PuTTY's host key store out of it.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn real_plink_accepts_a_key_file_written_here() {
    for kind in [Algorithm::Ed25519, Algorithm::Rsa { hash: None }] {
        let host_key = new_key(Algorithm::Ed25519);
        let client_key = new_key(kind);
        let (port, seen) = server::start_with_key(host_key.clone(), client_key.public_key().clone()).await;
        let (alg, public, private) = blobs_of(&client_key).unwrap();
        let path = std::env::temp_dir().join(format!("deck-real-{}-{alg}.ppk", std::process::id()));
        std::fs::write(&path, Ppk::write(&alg, "c", &public, &private, "", None).unwrap()).unwrap();
        let print = host_key.public_key().fingerprint(russh::keys::HashAlg::Sha256).to_string();
        let out = tokio::task::spawn_blocking(move || {
            let out = Command::new(PLINK)
                .args(["-ssh", "-batch", "-noagent", "-hostkey", &print, "-P", &port.to_string(), "-l", USER, "-i"])
                .arg(&path)
                .args(["127.0.0.1", "uptime"])
                .output()
                .unwrap();
            std::fs::remove_file(&path).unwrap();
            out
        })
        .await
        .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stdout.contains("ran it"), "{alg}: stdout {stdout:?} stderr {stderr:?}");
        assert_eq!(seen.lock().unwrap().exec, ["uptime"]);
    }
}

/// Run by hand: needs PuTTY installed, and no Pageant running. Starts our
/// Pageant holding a throwaway key, and has the real plink log in with it.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn real_plink_gets_a_key_from_our_pageant() {
    let pageant = env!("CARGO_MANIFEST_DIR").to_string() + r"\..\..\target\debug\pageant.exe";
    let host_key = new_key(Algorithm::Ed25519);
    let client_key = new_key(Algorithm::Ed25519);
    let (port, seen) = server::start_with_key(host_key.clone(), client_key.public_key().clone()).await;
    let (alg, public, private) = blobs_of(&client_key).unwrap();
    let path = std::env::temp_dir().join(format!("deck-agent-{}.ppk", std::process::id()));
    std::fs::write(&path, Ppk::write(&alg, "c", &public, &private, "", None).unwrap()).unwrap();
    let print = host_key.public_key().fingerprint(russh::keys::HashAlg::Sha256).to_string();
    let key_file = path.clone();
    let out = tokio::task::spawn_blocking(move || {
        let mut agent = Command::new(pageant).arg(&key_file).stdin(std::process::Stdio::null()).spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        assert!(agent.try_wait().unwrap().is_none(), "pageant stopped: is another one running?");
        let out = Command::new(PLINK)
            .args(["-ssh", "-batch", "-hostkey", &print, "-P", &port.to_string(), "-l", USER, "127.0.0.1", "uptime"])
            .output()
            .unwrap();
        agent.kill().unwrap();
        out
    })
    .await
    .unwrap();
    std::fs::remove_file(&path).unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stdout.contains("ran it"), "stdout {stdout:?} stderr {stderr:?}");
    assert_eq!(seen.lock().unwrap().exec, ["uptime"]);
}
