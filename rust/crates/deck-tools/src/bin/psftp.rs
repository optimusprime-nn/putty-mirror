//! PSFTP: an interactive file transfer client.
//!
//!   psftp [options] [user@]host [-b batchfile]

use anyhow::{anyhow, bail, Context};
use deck_core::session::{clean, connect_sftp};
use deck_core::ssh::Sftp;
use deck_tools::{host_keys, Args, RawConsole};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

const HELP: &str = "cd  pwd  ls  get  put  rm  mkdir  rmdir  mv  chmod  lcd  lpwd  help  quit";

/// Splits a command line. Double quotes keep spaces, as in PSFTP.
fn words(line: &str) -> Vec<String> {
    let (mut out, mut word, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut word));
                    any = false;
                }
            }
            c => {
                word.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(word);
    }
    out
}

fn resolve(cwd: &str, path: &str) -> String {
    match path {
        p if p.starts_with('/') => p.to_string(),
        p if cwd.ends_with('/') => format!("{cwd}{p}"),
        p => format!("{cwd}/{p}"),
    }
}

fn leaf(path: &str) -> &str {
    path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(path)
}

struct Shell {
    sftp: Sftp,
    cwd: String,
}

impl Shell {
    /// Runs one command. `Ok(false)` means quit.
    async fn run(&mut self, line: &str) -> anyhow::Result<bool> {
        let w = words(line);
        let sftp = &self.sftp.session;
        let arg = |i: usize| w.get(i).map(String::as_str).ok_or_else(|| anyhow!("{} needs more arguments", w[0]));
        match w.first().map(String::as_str) {
            None => {}
            Some("quit" | "bye" | "exit") => return Ok(false),
            Some("help" | "?") => println!("{HELP}"),
            Some("pwd") => println!("Remote directory is {}", clean(&self.cwd)),
            Some("lpwd") => println!("Current local directory is {}", std::env::current_dir()?.display()),
            Some("lcd") => std::env::set_current_dir(arg(1)?)?,
            Some("cd") => {
                let path = w.get(1).map_or(".".to_string(), |p| resolve(&self.cwd, p));
                self.cwd = sftp.canonicalize(path).await?;
                println!("Remote directory is now {}", clean(&self.cwd));
            }
            Some("ls" | "dir") => {
                let path = w.get(1).map_or(self.cwd.clone(), |p| resolve(&self.cwd, p));
                let mut entries: Vec<_> = sftp.read_dir(path).await?.collect();
                entries.sort_by_key(|e| e.file_name());
                for e in entries {
                    let m = e.metadata();
                    let kind = if m.is_dir() { 'd' } else { '-' };
                    println!("{kind} {:>12} {}", m.size.unwrap_or(0), clean(&e.file_name()));
                }
            }
            Some("get") => {
                let from = resolve(&self.cwd, arg(1)?);
                let to = w.get(2).map_or_else(|| PathBuf::from(leaf(&from)), PathBuf::from);
                let mut file = sftp.open(&from).await?;
                let mut out = tokio::fs::File::create(&to).await.with_context(|| format!("writing {}", to.display()))?;
                let size = tokio::io::copy(&mut file, &mut out).await?;
                tokio::io::AsyncWriteExt::flush(&mut out).await?;
                println!("{} => {} ({size} bytes)", clean(&from), to.display());
            }
            Some("put") => {
                let from = Path::new(arg(1)?);
                let data = std::fs::read(from).with_context(|| format!("reading {}", from.display()))?;
                let to = resolve(&self.cwd, w.get(2).map_or_else(|| leaf(&w[1]), String::as_str));
                let mut file = sftp.create(&to).await?;
                tokio::io::AsyncWriteExt::write_all(&mut file, &data).await?;
                tokio::io::AsyncWriteExt::shutdown(&mut file).await?;
                println!("{} => {to} ({} bytes)", from.display(), data.len());
            }
            Some("rm" | "del") => sftp.remove_file(resolve(&self.cwd, arg(1)?)).await?,
            Some("mkdir") => sftp.create_dir(resolve(&self.cwd, arg(1)?)).await?,
            Some("rmdir") => sftp.remove_dir(resolve(&self.cwd, arg(1)?)).await?,
            Some("mv" | "ren") => sftp.rename(resolve(&self.cwd, arg(1)?), resolve(&self.cwd, arg(2)?)).await?,
            Some("chmod") => {
                let mode = u32::from_str_radix(arg(1)?, 8).context("chmod needs an octal mode")?;
                let path = resolve(&self.cwd, arg(2)?);
                let mut meta = sftp.metadata(&path).await?;
                meta.permissions = Some(mode);
                sftp.set_metadata(path, meta).await?;
            }
            Some(other) => bail!("unknown command {other}: try help"),
        }
        Ok(true)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = Args::parse(std::env::args().skip(1))?;
    let batch = match args.rest.iter().position(|a| a == "-b") {
        Some(i) => {
            args.rest.remove(i);
            anyhow::ensure!(i < args.rest.len(), "-b needs a file");
            Some(args.rest.remove(i))
        }
        None => None,
    };
    let target = args.target(args.rest.first().map(String::as_str))?;
    let raw = RawConsole::new();
    let sftp = connect_sftp(target, host_keys()).await?;
    drop(raw);
    let cwd = sftp.session.canonicalize(".").await?;
    println!("Remote working directory is {cwd}");
    let mut shell = Shell { sftp, cwd };

    if let Some(file) = batch {
        for line in std::fs::read_to_string(file)?.lines() {
            println!("psftp> {line}");
            // as in PSFTP, a failed batch command stops the batch
            if !shell.run(line).await? {
                break;
            }
        }
        return Ok(());
    }
    let stdin = std::io::stdin();
    loop {
        print!("psftp> ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            return Ok(());
        }
        match shell.run(line.trim()).await {
            Ok(true) => {}
            Ok(false) => return Ok(()),
            Err(e) => eprintln!("{e:#}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_keep_spaces() {
        assert_eq!(words(r#"put "my file" x"#), ["put", "my file", "x"]);
        assert_eq!(words("  ls  "), ["ls"]);
        assert_eq!(words(r#"rm """#), ["rm", ""]);
    }

    #[test]
    fn paths_resolve_against_the_working_directory() {
        assert_eq!(resolve("/home/me", "a"), "/home/me/a");
        assert_eq!(resolve("/", "a"), "/a");
        assert_eq!(resolve("/x", "/abs"), "/abs");
        assert_eq!(leaf(r"C:\t\f.txt"), "f.txt");
    }
}
