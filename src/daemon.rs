use crate::{fb::Framebuffer, img, state, touch};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 基準の表示サイズ(幅128px、高さは元画像514:318の比率)
pub const BASE_W: u32 = 128;
pub const BASE_H: u32 = 79;
/// 縦積みの行間
pub const GAP: u32 = 6;

/// 質問中(青) / 終了(黄) / 確認済み(灰) のボディ色 (B,G,R)
const BLUE: (u8, u8, u8) = (217, 144, 74); // #4A90D9
const YELLOW: (u8, u8, u8) = (76, 201, 242); // #F2C94C
const GRAY: (u8, u8, u8) = (158, 158, 158); // #9E9E9E

#[derive(Clone, Copy, PartialEq)]
pub enum St {
    Run,
    Ask,
    Done,
    /// 終了(黄)をタッチで確認した後の状態。次のstartでRunに戻る
    Seen,
}

pub struct Entry {
    pub pane: String,
    pub st: St,
    pub started: Instant,
    /// 終了(Done)時に固定した経過分。幅をそれ以上伸ばさないための凍結値
    pub done_mins: u64,
}

impl Entry {
    /// 現在の表示幅。1分ごとに基準幅の+5%(20分で2倍)。
    /// 上限なしで伸び続け、左端が論理画面の左端に達したら止まる(=max_w)
    pub fn width(&self, max_w: u32) -> u32 {
        let mins = match self.st {
            St::Done | St::Seen => self.done_mins,
            _ => self.started.elapsed().as_secs() / 60,
        };
        let w = BASE_W as u64 + BASE_W as u64 * 5 * mins / 100;
        (w.min(u32::MAX as u64) as u32).min(max_w)
    }
}

/// paneごとの状態。Vecの並び=開始順=表示の上からの並び
#[derive(Default)]
pub struct Model {
    pub entries: Vec<Entry>,
}

impl Model {
    fn find(&mut self, pane: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.pane == pane)
    }

    pub fn apply(&mut self, cmd: &str, pane: &str) {
        match cmd {
            "start" => {
                if let Some(e) = self.find(pane) {
                    e.st = St::Run;
                    e.started = Instant::now();
                    e.done_mins = 0;
                } else {
                    self.entries.push(Entry {
                        pane: pane.to_string(),
                        st: St::Run,
                        started: Instant::now(),
                        done_mins: 0,
                    });
                }
            }
            "question" => {
                // Notificationは処理終了後のアイドル通知(60秒放置)でも飛んでくるため、
                // 処理中(Run)のエントリだけを質問中(青)にする。Done中は黄のまま維持
                if let Some(e) = self.find(pane) {
                    if e.st == St::Run {
                        e.st = St::Ask;
                    }
                }
            }
            "answer" => {
                if let Some(e) = self.find(pane) {
                    if e.st == St::Ask {
                        e.st = St::Run;
                    }
                }
            }
            "stop" => {
                if let Some(e) = self.find(pane) {
                    e.done_mins = e.started.elapsed().as_secs() / 60;
                    e.st = St::Done;
                }
            }
            // タッチで終了(黄)を確認 → 灰色。次のstartでオレンジに戻る
            "seen" => {
                if let Some(e) = self.find(pane) {
                    if e.st == St::Done {
                        e.st = St::Seen;
                    }
                }
            }
            "end" => self.entries.retain(|e| e.pane != pane),
            _ => {}
        }
    }
}

pub fn run() -> Result<()> {
    let sock = state::socket_path();
    // 単一インスタンス: 生きたdaemonが居るならソケット接続が成功する
    if UnixStream::connect(&sock).is_ok() {
        bail!("daemonは既に起動しています");
    }
    let _ = std::fs::remove_file(&sock);
    let listener =
        UnixListener::bind(&sock).with_context(|| format!("bind失敗: {}", sock.display()))?;

    let term = Arc::new(AtomicBool::new(false));
    for sig in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(sig, Arc::clone(&term))?;
    }

    let model = Arc::new(Mutex::new(Model::default()));
    {
        let m = Arc::clone(&model);
        let t = Arc::clone(&term);
        std::thread::spawn(move || accept_loop(listener, m, t));
    }
    touch::spawn(Arc::clone(&model));

    let fb = Framebuffer::open()?;
    let sprite = img::load()?;
    eprintln!(
        "[touch-claude] daemon起動 (pid {}, fb {}x{})",
        std::process::id(),
        fb.width,
        fb.height
    );

    // 前回描いた矩形(rotate + 論理座標)。変化したら消してから引き直す
    let mut drawn: Vec<(u8, u32, u32, u32, u32)> = Vec::new();
    let mut tick = 0u64;
    while !term.load(Ordering::Relaxed) {
        // SessionEndを取りこぼしてもペイン消滅で掃除されるように10秒ごとに照合
        if tick % 10 == 0 {
            prune(&model);
        }
        tick += 1;

        let rotate = state::screen_rotate();
        let (lw, lh) = fb.logical_size(rotate);
        let margin = top_margin(lh);
        let rows: Vec<(u32, (u8, u8, u8))> = {
            let m = model.lock().unwrap();
            m.entries
                .iter()
                .map(|e| {
                    let color = match e.st {
                        St::Run => sprite.body,
                        St::Ask => BLUE,
                        St::Done => YELLOW,
                        St::Seen => GRAY,
                    };
                    (e.width(lw), color)
                })
                .collect()
        };
        // 右端固定で右上から縦積み
        let rects: Vec<(u8, u32, u32, u32, u32)> = rows
            .iter()
            .enumerate()
            .map(|(i, (w, _))| (rotate, lw - w, margin + i as u32 * (BASE_H + GAP), *w, BASE_H))
            .collect();

        if rects != drawn {
            // 縮んだ・消えた領域に残像が出ないよう前回分を消し、fbterm/tmuxに再描画させる
            for &(r, x, y, w, h) in &drawn {
                let _ = fb.clear_logical(r, x, y, w, h);
            }
            if !drawn.is_empty() {
                let _ = Command::new("tmux").arg("refresh-client").output();
            }
            drawn = rects.clone();
        }
        for (&(r, x, y, w, h), (_, color)) in rects.iter().zip(rows.iter()) {
            let buf = sprite.render(w, h, *color);
            let _ = fb.blit_logical(r, x, y, w, h, &buf);
        }
        std::thread::sleep(Duration::from_secs(1));
    }

    for &(r, x, y, w, h) in &drawn {
        let _ = fb.clear_logical(r, x, y, w, h);
    }
    let _ = Command::new("tmux").arg("refresh-client").output();
    let _ = std::fs::remove_file(&sock);
    eprintln!("[touch-claude] daemon終了");
    Ok(())
}

/// tmuxステータスバー(status-position top)と重ならないための上マージン。
/// ステータス行数×セル高(論理画面高÷クライアント行数)で見積もる。
pub fn top_margin(lh: u32) -> u32 {
    let get = |args: &[&str]| -> Option<String> {
        let o = Command::new("tmux").args(args).output().ok()?;
        if !o.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    match get(&["show", "-gv", "status-position"]).as_deref() {
        Some("top") => {}
        _ => return 0,
    }
    let lines: u32 = match get(&["show", "-gv", "status"]).as_deref() {
        Some("off") => return 0,
        Some("on") | None => 1,
        Some(n) => n.parse().unwrap_or(1),
    };
    let cell = get(&["list-clients", "-F", "#{client_height}"])
        .and_then(|s| s.lines().next()?.trim().parse::<u32>().ok())
        .filter(|&rows| rows > 0)
        .map(|rows| (lh + rows - 1) / rows)
        .unwrap_or(16);
    lines * cell + 2
}

/// tmuxから消えたペインのエントリを掃除する
fn prune(model: &Arc<Mutex<Model>>) {
    let out = Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{pane_id}"])
        .output();
    let alive: Vec<String> = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|s| s.trim().to_string())
            .collect(),
        // tmuxサーバー不在=全ペイン消滅
        _ => Vec::new(),
    };
    model.lock().unwrap().entries.retain(|e| alive.contains(&e.pane));
}

#[derive(Deserialize)]
struct Msg {
    cmd: String,
    #[serde(default)]
    pane: Option<String>,
}

fn accept_loop(listener: UnixListener, model: Arc<Mutex<Model>>, term: Arc<AtomicBool>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Msg>(line.trim()) else { continue };
        match msg.cmd.as_str() {
            "quit" => {
                term.store(true, Ordering::Relaxed);
                return;
            }
            "status" => {
                let m = model.lock().unwrap();
                let mut s = String::new();
                for e in &m.entries {
                    let st = match e.st {
                        St::Run => "run",
                        St::Ask => "ask",
                        St::Done => "done",
                        St::Seen => "seen",
                    };
                    s.push_str(&format!(
                        "{} {} elapsed={}s width={}px\n",
                        e.pane,
                        st,
                        e.started.elapsed().as_secs(),
                        e.width(u32::MAX)
                    ));
                }
                if s.is_empty() {
                    s = "(表示中のclaudeなし)\n".to_string();
                }
                let _ = (&stream).write_all(s.as_bytes());
            }
            cmd => {
                if let Some(pane) = msg.pane.as_deref() {
                    model.lock().unwrap().apply(cmd, pane);
                }
            }
        }
    }
}
