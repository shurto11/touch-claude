# touch-claude

> **廃止済み(2026-09-06)**: この機能は task-var のバー内へ統合しました。
> いまは `~/ssd/tools/touch/task-var` が、アイコン列と Spotify パネルの間の枠に
> 同じキャラクターを最大 4 行並べます。Claude Code の hooks も
> `task-var hook-*` を呼ぶよう差し替え済みで、fb-server の scenes.toml と
> `~/bin/tmux-autostart` からも touch-claude は外してあります。
> このリポジトリは記録として残しているだけなので、起動しないでください。

tmux の各ペインで動く Claude Code の状態を、フレームバッファ画面の右上に
キャラクター(clawd02.png)で可視化するツール。キャラをタッチすると、その
claude が動くウィンドウ・ペインへ遷移する。

## 状態の見え方

| 状態 | 色 | きっかけ (Claude Code hook) |
|------|-----|------|
| 処理中 | オレンジ(跳ねて走るアニメーション) | `UserPromptSubmit` |
| 質問・許可待ち | 青 | `Notification` / `PreToolUse: AskUserQuestion\|ExitPlanMode` |
| 処理終了 | 黄 | `Stop` (次のプロンプトでオレンジに戻る)。終了後のアイドル通知(60秒放置のNotification)では青にならない |
| 確認済み | 灰 | 終了(黄)をタッチしたとき。次のプロンプトでオレンジに戻る |
| (消える) | - | `SessionEnd` またはペイン消滅 |

- 処理が1分続くごとに横幅が基準の+5%ずつ伸びる(右端固定・左へ拡大、20分で2倍)。
  上限なしで伸び続け、画面左端に達したら止まる。
- 複数の claude が動いていると、開始順に右上から縦に並ぶ。
- 基準サイズは幅128px。白背景は透明として端末表示を透過する。
- キャラの上にモデル名(FABLE / OPUS など)を白のピクセルフォントで表示する。
  hooks の stdin (transcript_path) から直近のモデルIDを拾うため、
  セッション開始直後など不明な間は表示されない。

## 仕組み

- `touch-claude daemon` : 常駐。状態管理・`/dev/fb0` 直描画(150ms周期。実行中は300msごとに
  5px上下する走りアニメーション、前フレームとの差分を黒で消して残像を防ぐ)・
  touch-server クライアント。`~/.fbtermrc` の `screen-rotate` に追従して縦画面でも
  「見た目の右上」に描く。
- `touch-claude hook-*` : hooks から呼ばれる軽量クライアント。`$TMUX_PANE` と、
  hook入力JSONの `transcript_path` 末尾から拾ったモデルIDを添えて
  Unix socket (`$XDG_RUNTIME_DIR/touch-claude.sock`) でデーモンへ通知するだけ。
  `hook-start` はデーモン未起動なら `setsid` で切り離して自動起動する。
- タッチ: touch-server に表示領域を region 申告する overlay 方式(spotatui-pip /
  dopagaki と同じ)。region は hello 時のみなので、表示の増減・拡大で変わったら
  再接続して申告し直す。タップされた行のペインへ
  `tmux switch-client / select-window / select-pane` で遷移する。
  touch-server 未起動でも表示機能はそのまま動く。

## 使い方

```bash
cargo build --release

# hooks は ~/.claude/settings.json に登録済み。
# 次に起動する claude のプロンプト送信からデーモンが自動起動して表示される。

./target/release/touch-claude status   # デーモンと表示中エントリの確認
./target/release/touch-claude quit     # デーモン停止(描画も消える)
```

## 環境変数

| 変数 | 既定値 | 説明 |
|------|--------|------|
| `TOUCH_CLAUDE_IMG` | (埋め込みclawd02.png) | キャラ画像PNGのパス |
| `TOUCH_ROTATE` | (~/.fbtermrcに追従) | 画面回転の上書き(0-3) |
| `TOUCH_SERVER_SOCK` | `$XDG_RUNTIME_DIR/touch-server.sock` | touch-serverのソケット |

ログ: `$XDG_RUNTIME_DIR/touch-claude.log`

## ファイル構成

- `src/main.rs` — サブコマンド分岐
- `src/daemon.rs` — 常駐デーモン(状態管理・描画ループ・コマンド受付)
- `src/fb.rs` — fb0直描画。論理座標(回転後の画面)→物理座標の変換と透明ブリット
- `src/font.rs` — モデル名ラベル用の5x7ピクセルフォント(大文字A-Z)
- `src/img.rs` — clawd02.pngの3色分類・変倍・ボディ色差し替え
- `src/touch.rs` — touch-serverクライアント(region申告・タップでペイン遷移)
- `src/hooks.rs` — hooksから呼ばれる通知クライアント
- `src/state.rs` — パス解決・screen-rotate読み取り
