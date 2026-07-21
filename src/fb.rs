use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;

/// /dev/fb0 への直接描画(dopagaki/src/fb.rsと同方式)。
/// fbterm環境ではstride(1376px)≠width(1366px)なので行オフセットは必ずstride基準。
/// ピクセルはBGRA(リトルエンディアンXRGB8888)。
///
/// このツールでは表示回転(fbtermのscreen-rotate)に合わせて描くため、
/// 「論理座標」(ユーザーから見た回転後の画面)で受けて物理座標へ変換する。
pub struct Framebuffer {
    file: File,
    pub width: u32,
    pub height: u32,
    /// 1行あたりのピクセル数(strideバイト/4)
    stride: u32,
}

impl Framebuffer {
    pub fn open() -> Result<Self> {
        let vsize = std::fs::read_to_string("/sys/class/graphics/fb0/virtual_size")
            .context("fb0 virtual_size読み取り失敗")?;
        let (w, h) = vsize.trim().split_once(',').context("virtual_sizeの形式が不正")?;
        let stride_bytes: u32 = std::fs::read_to_string("/sys/class/graphics/fb0/stride")
            .context("fb0 stride読み取り失敗")?
            .trim()
            .parse()?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/fb0")
            .context("/dev/fb0を開けません(videoグループ権限が必要)")?;
        Ok(Self {
            file,
            width: w.parse()?,
            height: h.trim().parse()?,
            stride: stride_bytes / 4,
        })
    }

    /// 回転後(論理)の画面サイズ
    pub fn logical_size(&self, rotate: u8) -> (u32, u32) {
        match rotate % 4 {
            1 | 3 => (self.height, self.width),
            _ => (self.width, self.height),
        }
    }

    /// 論理座標の矩形(lx,ly,w,h)が占める物理座標の矩形。fb-server の重なり調停は
    /// 物理 fb 座標で行うため、rect 申告にはこの変換後の値を使う。
    pub fn phys_region(&self, rotate: u8, lx: u32, ly: u32, w: u32, h: u32) -> (u32, u32, u32, u32) {
        self.phys_rect(rotate, lx, ly, w, h)
    }

    /// 論理座標の矩形(lx,ly,w,h)が占める物理座標の矩形(x0,y0,bw,bh)
    fn phys_rect(&self, rotate: u8, lx: u32, ly: u32, w: u32, h: u32) -> (u32, u32, u32, u32) {
        let (pw, ph) = (self.width, self.height);
        match rotate % 4 {
            1 => (pw.saturating_sub(ly + h), lx, h, w),
            2 => (pw.saturating_sub(lx + w), ph.saturating_sub(ly + h), w, h),
            3 => (ly, ph.saturating_sub(lx + w), h, w),
            _ => (lx, ly, w, h),
        }
    }

    /// 論理座標(lx,ly)へBGRA画像(w×h)を描く。表示回転に合わせて画像も回転する。
    /// alpha=0のピクセルは書かない(透明として下の端末表示を残す)。
    pub fn blit_logical(&self, rotate: u8, lx: u32, ly: u32, w: u32, h: u32, bgra: &[u8]) -> Result<()> {
        if w == 0 || h == 0 {
            return Ok(());
        }
        let (x0, y0, bw, bh) = self.phys_rect(rotate, lx, ly, w, h);
        // 画像を物理向きへ回転したバッファを作る
        let mut buf = vec![0u8; (bw as usize) * (bh as usize) * 4];
        for py in 0..bh {
            for px in 0..bw {
                let (i, j) = match rotate % 4 {
                    1 => (py, h - 1 - px),
                    2 => (w - 1 - px, h - 1 - py),
                    3 => (w - 1 - py, px),
                    _ => (px, py),
                };
                let s = ((j * w + i) * 4) as usize;
                let d = ((py * bw + px) * 4) as usize;
                buf[d..d + 4].copy_from_slice(&bgra[s..s + 4]);
            }
        }
        self.write_masked(x0, y0, bw, bh, &buf)
    }

    /// 論理座標の矩形を黒でクリアする
    pub fn clear_logical(&self, rotate: u8, lx: u32, ly: u32, w: u32, h: u32) -> Result<()> {
        let (x0, y0, bw, bh) = self.phys_rect(rotate, lx, ly, w, h);
        self.fill(x0, y0, bw, bh, 0, 0, 0)
    }

    /// alpha>0のピクセルだけを、行内の連続区間ごとにまとめて書き込む
    fn write_masked(&self, x: u32, y: u32, w: u32, h: u32, bgra: &[u8]) -> Result<()> {
        if x >= self.width || y >= self.height {
            return Ok(());
        }
        let vis_w = w.min(self.width - x);
        let vis_h = h.min(self.height - y);
        for j in 0..vis_h {
            let row = &bgra[(j * w) as usize * 4..][..(w as usize) * 4];
            let mut px = 0u32;
            while px < vis_w {
                while px < vis_w && row[(px as usize) * 4 + 3] == 0 {
                    px += 1;
                }
                let run0 = px;
                while px < vis_w && row[(px as usize) * 4 + 3] != 0 {
                    px += 1;
                }
                if px > run0 {
                    let src = &row[(run0 as usize) * 4..(px as usize) * 4];
                    let off = (((y + j) * self.stride + x + run0) * 4) as u64;
                    self.file.write_at(src, off)?;
                }
            }
        }
        Ok(())
    }

    fn fill(&self, x: u32, y: u32, w: u32, h: u32, b: u8, g: u8, r: u8) -> Result<()> {
        let (x, y, w, h) = self.clamp(x, y, w, h);
        let mut row = Vec::with_capacity((w * 4) as usize);
        for _ in 0..w {
            row.extend_from_slice(&[b, g, r, 0]);
        }
        for j in 0..h {
            let off = (((y + j) * self.stride + x) * 4) as u64;
            self.file.write_at(&row, off)?;
        }
        Ok(())
    }

    fn clamp(&self, x: u32, y: u32, w: u32, h: u32) -> (u32, u32, u32, u32) {
        let x = x.min(self.width);
        let y = y.min(self.height);
        (x, y, w.min(self.width - x), h.min(self.height - y))
    }
}
