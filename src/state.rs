use std::path::PathBuf;

/// 実行時ファイルの置き場: $XDG_RUNTIME_DIR > /tmp
pub fn runtime_dir() -> PathBuf {
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from("/tmp"),
    }
}

/// daemonとhookコマンドが話すソケット
pub fn socket_path() -> PathBuf {
    runtime_dir().join("touch-claude.sock")
}

pub fn log_path() -> PathBuf {
    runtime_dir().join("touch-claude.log")
}

/// touch-serverのソケット(touch-server/READMEの解決順に合わせる)
pub fn touch_server_sock() -> String {
    if let Ok(p) = std::env::var("TOUCH_SERVER_SOCK") {
        if !p.is_empty() {
            return p;
        }
    }
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(d) if !d.is_empty() => format!("{d}/touch-server.sock"),
        _ => "/tmp/touch-server.sock".to_string(),
    }
}

/// fbtermのscreen-rotate値(0=なし, 1=時計回り90°, 2=180°, 3=反時計回り90°)。
/// TOUCH_ROTATEがあれば最優先、無ければ~/.fbtermrcに追従(touch-serverと同じ方式)。
pub fn screen_rotate() -> u8 {
    if let Ok(v) = std::env::var("TOUCH_ROTATE") {
        if let Ok(n) = v.trim().parse::<u8>() {
            return n % 4;
        }
    }
    let Some(home) = std::env::var_os("HOME") else { return 0 };
    let path = std::path::Path::new(&home).join(".fbtermrc");
    let Ok(text) = std::fs::read_to_string(&path) else { return 0 };
    for line in text.lines() {
        if let Some(v) = line.trim().strip_prefix("screen-rotate=") {
            if let Ok(n) = v.trim().parse::<u8>() {
                return n % 4;
            }
        }
    }
    0
}

/// fb0の物理解像度
pub fn screen_size() -> Option<(u32, u32)> {
    let s = std::fs::read_to_string("/sys/class/graphics/fb0/virtual_size").ok()?;
    let (w, h) = s.trim().split_once(',')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}
