//! touch-serverクライアント。
//!
//! 表示領域全体を画面に対する0..1の矩形(region)で申告し、その中のタッチだけを
//! 受け取る(spotatui-pip / dopagakiと同じoverlay方式)。regionはhello時にしか
//! 申告できないため、表示の増減・拡大で変わったら接続を張り直して更新する。
//! タップされた行のclaudeが動くtmuxペインへ遷移する。

use crate::daemon::{top_margin, Model, BASE_H, GAP};
use crate::state;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// タップとみなす最大移動量(画面に対する割合)。これを超えたらスワイプ扱いで無視
const TAP_FRAC: f64 = 0.05;

/// 画面に対する0..1の矩形(touch-serverのFracRectに対応)
#[derive(Serialize, Clone, Copy, PartialEq, Debug)]
struct FracRect {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

/// 接続直後に1行だけ送る申告メッセージ
#[derive(Serialize)]
struct Hello {
    hello: &'static str,
    pane: Option<String>,
    region: Option<FracRect>,
}

/// サーバーから届くイベント。upの始点・終点だけ使う
#[derive(Deserialize)]
struct RawEvent {
    #[serde(rename = "type")]
    typ: String,
    #[serde(default)]
    fx0: f64,
    #[serde(default)]
    fy0: f64,
    #[serde(default)]
    fx1: f64,
    #[serde(default)]
    fy1: f64,
}

/// touch-clientスレッドを起動する(detached)。touch-server未起動でも
/// リトライし続けるだけで、表示機能には影響しない。
pub fn spawn(model: Arc<Mutex<Model>>) {
    std::thread::spawn(move || loop {
        let Some(region) = current_region(&model) else {
            std::thread::sleep(Duration::from_millis(500));
            continue;
        };
        if let Err(e) = session(region, &model) {
            eprintln!("[touch-claude] touch-client: {e}");
        }
        std::thread::sleep(Duration::from_millis(300));
    });
}

/// 回転後(論理)の画面サイズ
fn logical_size() -> Option<(u32, u32)> {
    let (w, h) = state::screen_size()?;
    Some(match state::screen_rotate() % 4 {
        1 | 3 => (h, w),
        _ => (w, h),
    })
}

/// いま表示している全行を覆う論理fracの矩形。表示なしならNone。
/// touch-serverのイベント座標は回転補正済み(論理frac)なので、これと一致する。
fn current_region(model: &Arc<Mutex<Model>>) -> Option<FracRect> {
    let (lw, lh) = logical_size()?;
    let m = model.lock().unwrap();
    if m.entries.is_empty() {
        return None;
    }
    let margin = top_margin(lh);
    let max_w = m.entries.iter().map(|e| e.width(lw)).max().unwrap_or(0);
    let bottom = (margin + m.entries.len() as u32 * (BASE_H + GAP)).min(lh);
    Some(FracRect {
        left: (lw - max_w) as f64 / lw as f64,
        top: margin.min(lh) as f64 / lh as f64,
        right: 1.0,
        bottom: bottom as f64 / lh as f64,
    })
}

/// 1接続ぶんの受信ループ。EOFかregion変更でOkを返して張り直しさせる
fn session(region: FracRect, model: &Arc<Mutex<Model>>) -> Result<()> {
    let stream = UnixStream::connect(state::touch_server_sock())?;
    let hello = Hello {
        hello: "touch-claude",
        pane: None,
        region: Some(region),
    };
    (&stream).write_all(format!("{}\n", serde_json::to_string(&hello)?).as_bytes())?;
    // region変更を検知するため、ブロックしっぱなしにせず定期的に起こす
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;

    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match (&stream).read(&mut chunk) {
            Ok(0) => return Ok(()), // サーバー切断
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=pos).collect();
                    handle_line(&line, model);
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                if current_region(model) != Some(region) {
                    return Ok(());
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// upのタップなら、y座標から行を割り出して該当ペインへ遷移する
fn handle_line(line: &[u8], model: &Arc<Mutex<Model>>) {
    let Ok(ev) = serde_json::from_slice::<RawEvent>(line) else { return };
    if ev.typ != "up" {
        return;
    }
    if (ev.fx1 - ev.fx0).abs() > TAP_FRAC || (ev.fy1 - ev.fy0).abs() > TAP_FRAC {
        return;
    }
    let Some((_, lh)) = logical_size() else { return };
    let y = (ev.fy1.max(0.0) * lh as f64) as u32;
    let margin = top_margin(lh);
    if y < margin {
        return; // ステータスバー上のタップは対象外
    }
    let row = ((y - margin) / (BASE_H + GAP)) as usize;
    let pane = model.lock().unwrap().entries.get(row).map(|e| e.pane.clone());
    if let Some(pane) = pane {
        goto_pane(&pane);
    }
}

/// 表示中のtmuxクライアントを、指定ペインのセッション・ウィンドウ・ペインへ遷移させる。
/// daemonはtmuxクライアントの外で動くため、switch-clientは対象クライアントを明示する。
fn goto_pane(pane: &str) {
    if let Ok(o) = Command::new("tmux")
        .args(["list-clients", "-F", "#{client_name}"])
        .output()
    {
        if let Some(client) = String::from_utf8_lossy(&o.stdout).lines().next() {
            let _ = Command::new("tmux")
                .args(["switch-client", "-c", client, "-t", pane])
                .output();
        }
    }
    let _ = Command::new("tmux").args(["select-window", "-t", pane]).output();
    let _ = Command::new("tmux").args(["select-pane", "-t", pane]).output();
}
