# touch-claude

## 概要
右上にキャラクターを表示して、claudeの実行の終了を可視化する

## 前提条件
- 実装はRust
- ~/ssd/tools/touch-serverとリンクさせることでタッチが可能
- タッチクライアントは別ファイルとして作成する
- 環境は、Ubuntu server + fbterm + tmux
- 画像はフレームバッファに直接書き込む

## 要求定義
- claudeが処理を始めた時、右上に画像ファイル(clawd02.png)を表示する
- tmuxで複数claudeが実行している時、その下に同じ画像を表示させることにより、1対1で対応させる
- タッチすることで、そのclaudeが実行しているtmuxのウィンドウ、ペインに遷移する
- claudeからの質問(planモードなど)があったときは、画像のオレンジ色の部分を青くする
- ユーザーが質問に答え、再び処理になったら、画像をデフォルトの色に戻す
- claudeの処理が終了したときは、画像のオレンジ色の部分を黄色にする
- 黄色(終了)の画像をタッチしたら灰色にする(確認済み)。再びclaudeが実行されたらデフォルトの色に戻す
- 処理が1分かかるごとに、横幅を長くする
  - 画像の右端の座標はそのままで、左端を左に動かすことにより拡大する
  - 20分処理がかかると、画像の2倍となる
  - 20分以降も同じペース(1分ごとに基準幅の+5%)で伸び続け、左端が画面左端(x=0)に達したら停止する
- 終了(黄色)の画像は次のプロンプト送信でオレンジに戻る。claudeセッション自体が終了したら画像を消し、下の画像を詰める

## アーキテクチャ
1バイナリのサブコマンドで、常駐デーモンと軽量hookコマンドの2役構成(dopagakiと同じ形)。

- `touch-claude daemon` : 状態管理・フレームバッファ描画・touch-serverクライアント
- `touch-claude hook-start|hook-question|hook-answer|hook-stop|hook-end` :
  Claude Codeのhooksから呼ばれ、`$TMUX_PANE`を添えてUnix socketでデーモンに通知する

デーモンはpaneごとの状態(開始時刻・状態・表示行)を持ち、右上から縦に並べて描画する。
並び順は開始順。セッションが終了した行は消して下の行を詰める。

## 状態検知 (Claude Code hooks)
`~/.claude/settings.json`のhooksに追記する(dopagakiのエントリと並べる。`async: true`)。

| hook | コマンド | 動作 |
|------|---------|------|
| `UserPromptSubmit` | `hook-start` | 処理開始 = オレンジ表示・タイマー開始/リセット。黄色からの復帰もここ |
| `Notification` | `hook-question` | 質問・許可待ち = 青 |
| `PostToolUse` (matcher `*`) | `hook-answer` | 回答後に処理再開 = オレンジに戻す |
| `Stop` | `hook-stop` | 終了 = 黄色、タイマー停止 |
| `SessionEnd` | `hook-end` | 画像を消す・下の行を詰める |

## 描画仕様
- `/dev/fb0`直書き。`dopagaki/src/fb.rs`と同方式(virtual_size / stride / BGRA)を流用する
- 画面は1366x768。fbtermが再描画で上書きしてくるため、デーモンが定期的(1秒間隔)に再ブリットする
- 基準サイズ: 幅128px(高さは元比率514:318を維持し約79px)。右上に右端固定で表示し、複数claudeは下に縦積み
- 拡大は横方向のみの引き伸ばし(アスペクト比は変わる)。右端座標は固定し、左へ伸ばす
- 色: clawd02.pngは3色パレット(白・オレンジ・黒)のPNGなので、パレットのオレンジを状態に応じて置換する
  - 処理中 = オレンジ(デフォルト) / 質問中 = 青 / 終了 = 黄
- 縦画面対応: `~/.fbtermrc`の`screen-rotate`を描画のたびに読み、3(縦)のときは回転描画して「物理画面の右上」に表示する(dopagaki / touch-serverと同じ追従方式)

## タッチ連携 (touch-server)
- touch-serverにoverlayクライアントとして接続し、表示領域全体を`region`(画面0..1の矩形)で申告する(spotatui-pip / dopagakiと同方式)
- regionはhello時のみ受け付けられるため、画像の数・幅が変わったら再接続してregionを更新する(touch-server側の改修は不要)
- `up`イベントのy座標からどのclaudeの画像かを判定し、`tmux switch-client / select-window / select-pane`で該当ペインへ遷移する
- touch-server未起動・タッチデバイス不在でも表示機能だけは動く(接続はリトライし続ける)
