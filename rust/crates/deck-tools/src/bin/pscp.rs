//! PSCP: copies files over SFTP, or the old SCP protocol if the server has no
//! SFTP or `-scp` asks for it.
//!
//!   pscp [options] [-r] [-sftp|-scp] [-unsafe] source... destination
//!   Remote paths are written [user@]host:path.

use anyhow::{anyhow, Context};
use deck_core::config::Target;
use deck_core::session::{clean, connect_exec, connect_sftp};
use deck_tools::{host_keys, safe_local_name, scp, Args, RawConsole};
use russh_sftp::client::SftpSession;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

/// Splits `host:path`. A one-letter "host" is a Windows drive, as in PuTTY.
fn remote(spec: &str) -> Option<(&str, &str)> {
    match spec.split_once(':') {
        Some((host, path)) if host.len() > 1 => Some((host, path)),
        _ => None,
    }
}

/// How deep a server's folders are followed.
const MAX_DEPTH: usize = 256;

fn join(dir: &str, name: &str) -> String {
    match dir {
        "" | "." => name.to_string(),
        d if d.ends_with('/') => format!("{d}{name}"),
        d => format!("{d}/{name}"),
    }
}

fn leaf(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path)
}

async fn put_file(sftp: &SftpSession, from: &Path, to: &str) -> anyhow::Result<()> {
    let data = tokio::fs::read(from).await.with_context(|| format!("reading {}", from.display()))?;
    let mut file = sftp.create(to).await.with_context(|| format!("creating {to}"))?;
    tokio::io::AsyncWriteExt::write_all(&mut file, &data).await?;
    tokio::io::AsyncWriteExt::shutdown(&mut file).await?;
    println!("{} -> {to} ({} bytes)", from.display(), data.len());
    Ok(())
}

fn put_tree<'a>(sftp: &'a SftpSession, from: &'a Path, to: String, recurse: bool) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + 'a>> {
    Box::pin(async move {
        if !from.is_dir() {
            return put_file(sftp, from, &to).await;
        }
        anyhow::ensure!(recurse, "{} is a folder: use -r", from.display());
        if !sftp.try_exists(&to).await.unwrap_or(false) {
            sftp.create_dir(&to).await.with_context(|| format!("creating {to}"))?;
        }
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            put_tree(sftp, &entry.path(), join(&to, &name), recurse).await?;
        }
        Ok(())
    })
}

fn get_tree<'a>(sftp: &'a SftpSession, from: String, to: PathBuf, recurse: bool, depth: usize) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + 'a>> {
    Box::pin(async move {
        let shown = clean(&from);
        let meta = sftp.metadata(&from).await.with_context(|| format!("looking up {shown}"))?;
        if !meta.is_dir() {
            let mut file = sftp.open(&from).await.with_context(|| format!("reading {shown}"))?;
            let mut out = tokio::fs::File::create(&to).await.with_context(|| format!("writing {}", to.display()))?;
            let size = tokio::io::copy(&mut file, &mut out).await.with_context(|| format!("reading {shown}"))?;
            tokio::io::AsyncWriteExt::flush(&mut out).await?;
            println!("{shown} -> {} ({size} bytes)", clean(&to.display().to_string()));
            return Ok(());
        }
        anyhow::ensure!(recurse, "{shown} is a folder: use -r");
        anyhow::ensure!(depth < MAX_DEPTH, "{shown} is too deeply nested");
        std::fs::create_dir_all(&to)?;
        for entry in sftp.read_dir(&from).await? {
            let name = entry.file_name();
            // a server's listing mustn't be able to write outside the destination
            if !safe_local_name(&name) {
                continue;
            }
            get_tree(sftp, join(&from, &name), to.join(&name), recurse, depth + 1).await?;
        }
        Ok(())
    })
}

async fn run_scp(target: Target, sources: &[String], dest: &str, upload: bool, recurse: bool, unsafe_names: bool) -> anyhow::Result<()> {
    if upload {
        let (_, path) = remote(dest).ok_or_else(|| anyhow!("bad destination"))?;
        let path = if path.is_empty() { "." } else { path };
        let mut exec = connect_exec(target, host_keys(), &scp::receive_command(path, recurse || sources.len() > 1)).await?;
        tokio::io::AsyncReadExt::read_u8(&mut exec.stream).await.context("the server has no scp")?;
        for source in sources {
            scp::upload(&mut exec.stream, Path::new(source), recurse).await?;
        }
        tokio::io::AsyncWriteExt::shutdown(&mut exec.stream).await?;
        return Ok(());
    }
    for source in sources {
        let (_, path) = remote(source).ok_or_else(|| anyhow!("{source} isn't remote"))?;
        anyhow::ensure!(unsafe_names || !path.contains(['*', '?', '[']), "with a wildcard the server picks the file names: add -unsafe to allow that");
        let mut exec = connect_exec(target.clone(), host_keys(), &scp::send_command(path, recurse)).await?;
        let expected = (!unsafe_names).then(|| leaf(path));
        scp::download(&mut exec.stream, Path::new(dest), recurse, expected).await?;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = Args::parse(std::env::args().skip(1))?;
    let recurse = args.rest.iter().any(|a| a == "-r");
    let force_scp = args.rest.iter().any(|a| a == "-scp");
    let force_sftp = args.rest.iter().any(|a| a == "-sftp");
    let unsafe_names = args.rest.iter().any(|a| a == "-unsafe");
    args.rest.retain(|a| !matches!(a.as_str(), "-r" | "-scp" | "-sftp" | "-unsafe"));
    anyhow::ensure!(args.rest.len() >= 2, "usage: pscp [options] [-r] source... destination");
    let dest = args.rest.pop().ok_or_else(|| anyhow!("no destination"))?;
    let (host, upload) = match remote(&dest) {
        Some((h, _)) => (h.to_string(), true),
        None => {
            let first = remote(&args.rest[0]).ok_or_else(|| anyhow!("neither side is remote: use copy"))?;
            (first.0.to_string(), false)
        }
    };
    let target = args.target(Some(&host))?;
    let raw = RawConsole::new();
    let mut sftp = None;
    if !force_scp {
        match connect_sftp(target.clone(), host_keys()).await {
            Ok(s) => sftp = Some(s),
            Err(e) if force_sftp => return Err(e),
            Err(e) if e.to_string().contains("no SFTP") => eprintln!("No SFTP here, so using SCP."),
            Err(e) => return Err(e),
        }
    }
    let Some(sftp) = sftp else {
        let result = run_scp(target, &args.rest, &dest, upload, recurse, unsafe_names).await;
        drop(raw);
        return result;
    };
    drop(raw);
    if upload {
        let (_, path) = remote(&dest).ok_or_else(|| anyhow!("bad destination"))?;
        let path = if path.is_empty() { "." } else { path };
        let into_dir = args.rest.len() > 1 || sftp.session.metadata(path).await.is_ok_and(|m| m.is_dir());
        for source in &args.rest {
            let from = Path::new(source);
            let name = from.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let to = if into_dir { join(path, &name) } else { path.to_string() };
            put_tree(&sftp.session, from, to, recurse).await?;
        }
    } else {
        let dest = Path::new(&dest);
        for source in &args.rest {
            let (_, path) = remote(source).ok_or_else(|| anyhow!("{source} isn't remote"))?;
            let to = if dest.is_dir() { dest.join(leaf(path)) } else { dest.to_path_buf() };
            get_tree(&sftp.session, path.to_string(), to, recurse, 0).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_letters_are_not_hosts() {
        assert_eq!(remote(r"C:\temp\f"), None);
        assert_eq!(remote("box:/etc/motd"), Some(("box", "/etc/motd")));
        assert_eq!(remote("me@box:"), Some(("me@box", "")));
        assert_eq!(remote("local.txt"), None);
    }

    #[test]
    fn paths_join() {
        assert_eq!(join(".", "a"), "a");
        assert_eq!(join("/x/", "a"), "/x/a");
        assert_eq!(join("x", "a"), "x/a");
        assert_eq!(leaf("/x/y/"), "y");
    }
}
