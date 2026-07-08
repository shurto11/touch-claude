use anyhow::{Context, Result};

const WHITE: u8 = 0;
const BLACK: u8 = 1;
const BODY: u8 = 2;

/// clawd02.png(白/オレンジ/黒の3色パレット)をピクセル分類して保持し、
/// 任意サイズ・任意ボディ色のBGRAを生成するスプライト。
/// 白は背景なので透明(alpha=0)として扱う。
pub struct Sprite {
    pub w: u32,
    pub h: u32,
    class: Vec<u8>,
    /// 元画像のボディ色(B,G,R)。処理中(デフォルト=オレンジ)はこの色を使う
    pub body: (u8, u8, u8),
}

pub fn load() -> Result<Sprite> {
    let data: Vec<u8> = match std::env::var("TOUCH_CLAUDE_IMG") {
        Ok(p) if !p.is_empty() => {
            std::fs::read(&p).with_context(|| format!("画像読み込み失敗: {p}"))?
        }
        _ => include_bytes!("../clawd02.png").to_vec(),
    };
    let img = image::load_from_memory(&data).context("PNGデコード失敗")?.to_rgba8();
    let (w, h) = img.dimensions();
    let mut class = Vec::with_capacity((w * h) as usize);
    let mut body = (80u8, 108u8, 217u8); // 読み取れなかった時の保険(clawdのオレンジ)
    let mut found = false;
    for p in img.pixels() {
        let [r, g, b, a] = p.0;
        let c = if a < 128 || (r >= 200 && g >= 200 && b >= 200) {
            WHITE
        } else if r <= 60 && g <= 60 && b <= 60 {
            BLACK
        } else {
            if !found {
                body = (b, g, r);
                found = true;
            }
            BODY
        };
        class.push(c);
    }
    Ok(Sprite { w, h, class, body })
}

impl Sprite {
    /// 最近傍で(out_w, out_h)へ変倍し、ボディ色を差し替えたBGRAを返す。
    /// ドット絵なので最近傍で十分(横伸ばしもここで吸収する)。
    pub fn render(&self, out_w: u32, out_h: u32, body: (u8, u8, u8)) -> Vec<u8> {
        let mut buf = vec![0u8; (out_w as usize) * (out_h as usize) * 4];
        for oy in 0..out_h {
            let sy = (oy as u64 * self.h as u64 / out_h as u64) as u32;
            for ox in 0..out_w {
                let sx = (ox as u64 * self.w as u64 / out_w as u64) as u32;
                let d = ((oy * out_w + ox) * 4) as usize;
                match self.class[(sy * self.w + sx) as usize] {
                    BLACK => buf[d..d + 4].copy_from_slice(&[0, 0, 0, 255]),
                    BODY => buf[d..d + 4].copy_from_slice(&[body.0, body.1, body.2, 255]),
                    _ => {} // 白=透明(alpha 0のまま)
                }
            }
        }
        buf
    }
}
