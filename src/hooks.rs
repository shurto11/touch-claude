//! Claude Code hooksから呼ばれる軽量クライアント。
//! hookは即returnが必須なので、ソケットに1行送るだけで終わる。
//! hook-start(UserPromptSubmit)のみ、daemon未起動なら起動してから送る。

use crate::state;
use anyhow::{bail, Context, Result};
use std::io::{IsTerminal, Read, Seek, SeekFrom, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::Duration;

pub fn send(cmd: &str, ensure_daemon: bool) -> Result<()> {
    // tmux外で動くclaudeは対象外
    let Ok(pane) = std::env::var("TMUX_PANE") else { return Ok(()) };
    let model = model_from_stdin();
    let line = serde_json::json!({"cmd": cmd, "pane": pane, "model": model}).to_string() + "\n";
    if try_send(&line).is_ok() {
        return Ok(());
    }
    if !ensure_daemon {
        return Ok(());
    }
    spawn_daemon()?;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(100));
        if try_send(&line).is_ok() {
            return Ok(());
        }
    }
    bail!("daemonに接続できません");
}

/// hookのstdin JSONからtranscript_pathを取り、直近のモデルIDを返す。
/// 手動実行時はstdinがTTYなので読まない(ブロック防止)。
/// hookは即returnが必須なため、失敗はすべてNoneに落とす。
fn model_from_stdin() -> Option<String> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut s = String::new();
    stdin.read_to_string(&mut s).ok()?;
    let v: serde_json::Value = serde_json::from_str(s.trim()).ok()?;
    last_model(v.get("transcript_path")?.as_str()?)
}

/// transcript(JSONL)の末尾64KiBから最後の "model":"claude-..." を拾う。
/// assistantメッセージのmodelフィールドが対象。"<synthetic>"などは除外する。
fn last_model(path: &str) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(64 * 1024))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    text.match_indices("\"model\":\"")
        .filter_map(|(i, m)| {
            let rest = &text[i + m.len()..];
            let id = &rest[..rest.find('"')?];
            id.contains("claude").then(|| id.to_string())
        })
        .last()
}

fn try_send(line: &str) -> Result<()> {
    let mut s = UnixStream::connect(state::socket_path())?;
    s.write_all(line.as_bytes())?;
    Ok(())
}

/// daemonをsetsidで切り離して起動する(claudeやペインの終了に道連れされないように)
fn spawn_daemon() -> Result<()> {
    let exe = std::env::current_exe()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state::log_path())?;
    Command::new("setsid")
        .arg("-f")
        .arg(exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn()
        .context("daemon起動失敗")?;
    Ok(())
}

pub fn status() -> Result<()> {
    match request("status") {
        Ok(s) => print!("daemon: 起動中\n{s}"),
        Err(_) => println!("daemon: 停止中"),
    }
    Ok(())
}

pub fn quit() -> Result<()> {
    let line = serde_json::json!({"cmd": "quit"}).to_string() + "\n";
    match try_send(&line) {
        Ok(()) => println!("daemonへ終了を送信しました"),
        Err(_) => println!("daemonは起動していません"),
    }
    Ok(())
}

fn request(cmd: &str) -> Result<String> {
    let mut s = UnixStream::connect(state::socket_path())?;
    s.write_all((serde_json::json!({"cmd": cmd}).to_string() + "\n").as_bytes())?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    Ok(out)
}
