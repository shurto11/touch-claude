use crate::{fb::Framebuffer, font, img, state, touch};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 基準の表示サイズ(幅128px、高さは元画像514:318の比率)
pub const BASE_W: u32 = 128;
pub const BASE_H: u32 = 79;
/// キャラ上部のモデル名ラベル帯の高さ(フォント7px×2倍 + 下余白2px)
pub const LABEL_H: u32 = 16;
/// 1行の高さ(ラベル帯 + キャラ)
pub const ROW_H: u32 = LABEL_H + BASE_H;
/// 縦積みの行間
pub const GAP: u32 = 6;
/// 走りアニメーションの上下量(px)
const BOB: u32 = 5;
/// ラベルの文字拡大率と色(B,G,R)
const LABEL_SCALE: u32 = 2;
const LABEL_COLOR: (u8, u8, u8) = (255, 255, 255);

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
    /// キャラ上部に表示するモデル名ラベル(例: "FABLE")。不明なら空
    pub model: String,
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

    pub fn apply(&mut self, cmd: &str, pane: &str, model: Option<&str>) {
        // 「途中で消えた」系の追跡ができるよう、状態が動くコマンドはログに残す
        if cmd != "answer" {
            eprintln!("[touch-claude] cmd={cmd} pane={pane}");
        }
        // モデル名はどのhookからでも届き得るので、分かった時点で既存エントリへ反映する
        // (セッション開始直後はtranscriptにまだassistantメッセージが無く、空のことがある)
        let label = model.map(model_label).unwrap_or_default();
        if !label.is_empty() {
            if let Some(e) = self.find(pane) {
                e.model = label.clone();
            }
        }
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
                        model: label,
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

/// モデルIDから表示ラベル(大文字)へ。例: claude-fable-5 → FABLE
fn model_label(id: &str) -> String {
    for (key, label) in [
        ("fable", "FABLE"),
        ("mythos", "MYTHOS"),
        ("opus", "OPUS"),
        ("sonnet", "SONNET"),
        ("haiku", "HAIKU"),
    ] {
        if id.contains(key) {
            return label.to_string();
        }
    }
    // 未知のモデルは "claude-" の次のセグメントを大文字で表示する
    if let Some(i) = id.find("claude-") {
        let name: String = id[i + 7..].chars().take_while(|c| c.is_ascii_alphabetic()).collect();
        return name.to_ascii_uppercase();
    }
    String::new()
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
    // pane→前フレームのボックスバッファ。アニメーションの残像消しに使う
    let mut last_frames: HashMap<String, Vec<u8>> = HashMap::new();
    let mut cached_margin = 0u32;
    let mut prev_rotate = 255u8;
    let mut tick = 0u64;
    while !term.load(Ordering::Relaxed) {
        // SessionEndを取りこぼしてもペイン消滅で掃除されるように約10秒ごとに照合
        if tick % 67 == 0 {
            prune(&model);
        }
        let rotate = state::screen_rotate();
        let (lw, lh) = fb.logical_size(rotate);
        // top_marginはtmuxを3回呼ぶので約2秒ごとにキャッシュ更新
        if tick % 13 == 0 || rotate != prev_rotate {
            cached_margin = top_margin(lh);
            prev_rotate = rotate;
        }
        let margin = cached_margin;
        tick += 1;

        let rows: Vec<(String, u32, (u8, u8, u8), bool, String)> = {
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
                    (e.pane.clone(), e.width(lw), color, e.st == St::Run, e.model.clone())
                })
                .collect()
        };
        // 右端固定で右上から縦積み(各行=ラベル帯+キャラ)
        let rects: Vec<(u8, u32, u32, u32, u32)> = rows
            .iter()
            .enumerate()
            .map(|(i, (_, w, _, _, _))| (rotate, lw - w, margin + i as u32 * (ROW_H + GAP), *w, ROW_H))
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
            last_frames.clear();
        }
        // 走りアニメーションの位相(300msごとに切り替え)
        let phase = (tick / 2) % 2 == 1;
        for (&(r, x, y, w, h), (pane, _, color, animate, label)) in rects.iter().zip(rows.iter()) {
            let body = if *animate {
                run_frame(&sprite, w, BASE_H, *color, phase)
            } else {
                sprite.render(w, BASE_H, *color)
            };
            // ラベル帯(上LABEL_H px) + キャラ(下BASE_H px)を1フレームに合成
            let mut frame = vec![0u8; (w as usize) * (h as usize) * 4];
            frame[(LABEL_H * w * 4) as usize..].copy_from_slice(&body);
            if !label.is_empty() {
                // キャラと同じく右寄せ(右端に少し余白)
                let tx = w.saturating_sub(font::text_width(label, LABEL_SCALE) + 4);
                font::draw_text(&mut frame, w, LABEL_H, tx, 0, label, LABEL_SCALE, LABEL_COLOR);
            }
            // 前フレームで不透明だったのに今回透明になったピクセルを黒で消してから描く
            let composed = match last_frames.get(pane) {
                Some(prev) if prev.len() == frame.len() => {
                    let mut c = frame.clone();
                    for i in (0..c.len()).step_by(4) {
                        if c[i + 3] == 0 && prev[i + 3] != 0 {
                            c[i..i + 4].copy_from_slice(&[0, 0, 0, 255]);
                        }
                    }
                    c
                }
                _ => frame.clone(),
            };
            let _ = fb.blit_logical(r, x, y, w, h, &composed);
            last_frames.insert(pane.clone(), frame);
        }
        std::thread::sleep(Duration::from_millis(150));
    }

    for &(r, x, y, w, h) in &drawn {
        let _ = fb.clear_logical(r, x, y, w, h);
    }
    let _ = Command::new("tmux").arg("refresh-client").output();
    let _ = std::fs::remove_file(&sock);
    eprintln!("[touch-claude] daemon終了");
    Ok(())
}

/// 実行中の走りアニメーションの1フレーム。キャラを少し縮めて(h-BOB)、
/// 位相に応じて上寄せ/下寄せに描くことで、その場で跳ねながら走って見せる
fn run_frame(sprite: &img::Sprite, w: u32, h: u32, color: (u8, u8, u8), phase: bool) -> Vec<u8> {
    let hh = h.saturating_sub(BOB).max(1);
    let dy = if phase { BOB } else { 0 };
    let spr = sprite.render(w, hh, color);
    let mut boxbuf = vec![0u8; (w as usize) * (h as usize) * 4];
    let row_bytes = (w * 4) as usize;
    for row in 0..hh {
        let d = ((row + dy) * w * 4) as usize;
        let s = (row * w * 4) as usize;
        boxbuf[d..d + row_bytes].copy_from_slice(&spr[s..s + row_bytes]);
    }
    boxbuf
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
    // クライアントが複数いる場合は最小行数(=最大セル高)を採用して順序に依らず安全側へ
    let cell = get(&["list-clients", "-F", "#{client_height}"])
        .and_then(|s| {
            s.lines()
                .filter_map(|l| l.trim().parse::<u32>().ok())
                .filter(|&rows| rows > 0)
                .min()
        })
        .map(|rows| (lh + rows - 1) / rows)
        .unwrap_or(16);
    lines * cell + 2
}

/// tmuxから消えたペインのエントリを掃除する。
/// コマンドの一時的な失敗で全エントリを消さないよう、
/// 「サーバー不在」が明示されたときだけ全消しする。
fn prune(model: &Arc<Mutex<Model>>) {
    let Ok(o) = Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{pane_id}"])
        .output()
    else {
        return; // tmuxを起動できない一時障害。次回に持ち越す
    };
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr);
        if err.contains("no server running") || err.contains("error connecting") {
            let mut m = model.lock().unwrap();
            if !m.entries.is_empty() {
                eprintln!("[touch-claude] prune: tmuxサーバー不在のため全{}件を削除", m.entries.len());
                m.entries.clear();
            }
        }
        return;
    }
    let alive: Vec<String> = String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(|s| s.trim().to_string())
        .collect();
    model.lock().unwrap().entries.retain(|e| {
        let keep = alive.contains(&e.pane);
        if !keep {
            eprintln!("[touch-claude] prune: ペイン消滅につき削除 {}", e.pane);
        }
        keep
    });
}

#[derive(Deserialize)]
struct Msg {
    cmd: String,
    #[serde(default)]
    pane: Option<String>,
    /// hookが拾ったモデルID(例: claude-fable-5)。不明ならnull
    #[serde(default)]
    model: Option<String>,
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
                        "{} {} model={} elapsed={}s width={}px\n",
                        e.pane,
                        st,
                        if e.model.is_empty() { "?" } else { &e.model },
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
                    model.lock().unwrap().apply(cmd, pane, msg.model.as_deref());
                }
            }
        }
    }
}
