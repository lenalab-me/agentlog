# 引き継ぎ: agentlog（セッションログから、くり返しの失敗と道具の効き目を数える CLI）

新しいセッションは、この文書から始める。2026-10-15 に、10 月のログから次に作る道具を探すセッション（`~/.t3/scratch/2026-10-15-claude-efd99aa3`）で作った。

測った結果や試用の記録は、このリポジトリではなく `~/project/prod/harness-engineering/` に置く（道具をまたぐものは `changes.md` と `reports/`、agentlog だけのものは `notes/agentlog/`）。

## 控え（キャッシュ）：search・show・timeline をログが増えても速く（2026-10-17、ブランチ `speed`。main への取り込みと `cargo install` はまだ）

`search`・`show`・`timeline` が、ログのファイルごとに読み取った結果を `~/.cache/agentlog/`（`$XDG_CACHE_HOME`）に置き、追記された分だけを読み足す。`--raw` は控えを使わずログを直接読む。仕様は `docs/design.md` の「控え」、確かめ方は `docs/recipes.md` の 4「控えが怪しいとき」、`src/help.txt` の `Cache` の節。方針は `harness-engineering/backlog.md`（DB・全文索引は持たない。控えは作り直せる写し）。

- **並列は元からあった.** 読みのスレッド分け（`load::read_all`）は `search`・`show`・`timeline` が使っていて、`available_parallelism()` の数。作業の前提（1 本のスレッド）は main では違っていた。Codex の 26 GB は読むこと自体が支配的で、スレッドを 4 倍にしても 5.2 s → 4.9 s。そこで並列はそのまま。
- **速さ（表は `docs/design.md`）.** 2 回目以降：Claude 45〜85 ms（以前 0.55〜1.0 s）、Codex の写し 66〜121 ms（以前 3.6〜5.1 s）。1 回目（控えを作る）は Claude 約 1 秒、Codex 6〜8 秒。控えは Claude 18 MB（ログの 1.3%）、Codex 46 MB（0.18%）。
- **作り.** `lines.rs`（位置から行を読む。改行のない最後の行は渡さない）、`cache.rs`（見出し・発言の行・作業ディレクトリの行、`fetch`）、`build.rs`（読み方のソースのハッシュ）。`talk::read_lines` / `codex::read_lines` が、ログから取り出す規則の唯一の場所で、`--raw` も控えもこれを通る。
- **版の上げ忘れの防ぎ方.** 控えの見出しの `format`（作りを変えたとき人が上げる）と `reader`（`build.rs` が `src/*.rs` のうち出力・集計だけのファイルを除いた中身をハッシュにしたもの）。読み方のコードを変えると自動で古い控えが使われなくなる。ログ側の形が変わったときは見えないので、`--raw` との `diff` と控えの削除（recipes）。
- **読み方を揃えて変わったこと.** 改行のない最後の行は読まない（`--raw` も）。セッションの `cwd` と `sessionId` は言葉のある行だけから決める（以前は `show` だけツール呼び出しの行も使った）。Claude の行はバイト列で読み、UTF-8 でない行はその行だけ飛ばす。
- **確かめ.** 結合テスト 8 件（`tests/cache.rs`。手で書いた架空のログで、控えと `--raw` が同じ答えを出すことを、初回・2 回目・追記・途中で切れた行・小さくなった・書き換わった・別の inode・消えた・touch・壊れた控え・別の読み方の控え・書けない置き場所で確かめる）。わざと壊して落ちた：(1) 改行のない最後の行を読んだことにする→`a_line_still_being_written…`、(2) 追記を読んだあと最後の発言を落とす→4 件、(3) 小さくなったログの読み直しをやめる→`a_log_that_became_smaller…`（`the log shrank`）、(4) 指紋の比較だけやめる→同じテスト（`the middle changed and the file grew`）、(5) 控えに assistant の本文を書かない→6 件（`start: search … with the cache is not what the logs say`）、(6) 作業ディレクトリを書かない→6 件（timeline）。本物のログでは、Claude と Codex の写しで `search` 36 通り（語・`-i`・`--who`・窓）、`timeline` 12 通り（4 つのリポジトリ、`--source` 2 通り）、`show` 6 つ（サブエージェント、Codex 含む）が、控え・`--raw`・変更前のバイナリで一致（実行中のセッションの追記分も一致）。
- **残した課題.** 追記時の指紋の読み出し（8 KB）と控えの書き直しは毎回（実行中のセッションの控えは数百 KB まで）。古い版の控えは、そのログが読まれるまで残る。Codex の `timeline` の 1 回目は未測定。`cargo install` はまだ。`~/.claude/CLAUDE.md` の案内（`--raw`）はまだ。

## timeline：発言とコミットを時刻順に並べる（2026-10-17、ブランチ `timeline`。main への取り込みと `cargo install` はまだ）

`agentlog timeline --repo DIR [--project S] [--since WHEN] [--until WHEN] [--who user|any] [--source …] [--json]`。`git -C DIR log` のコミットと、セッションの発言を時刻順に混ぜて出す。振り返り・引き継ぎ・決定の記録を書くとき、`git log` と記憶を手で突き合わせていたのを置き換える（`harness-engineering/backlog.md` の候補）。仕様は `docs/design.md` の「timeline」、使い方は `docs/recipes.md` の 5、`src/help.txt` の `timeline:` の節。

- **決めたこと.** コミットは全ローカル・追跡ブランチ（`--branches --remotes`）、マージコミットは外し、**作者時刻**で置く（rebase で動かない。コミット時刻だと、作業ツリーで作ったコミットが取り込んだ時刻にまとまり、会話の順が崩れる）。発言は `search` と同じ見分け（既定 `--who user`、`--source` は claude）。`--project` を省くと、セッションの行ごとの作業ディレクトリ（`cwd`）が、リポジトリ・`git worktree list` の作業ツリー・`~/.t3/worktrees/<リポジトリ名>/` の中に半分以上あるものを拾う。リポジトリより上のフォルダ（親のフォルダで開いたセッション）の行は、どちらにも数えない。判断は窓の中の行だけ。`Session` に `cwds`（`cwd` ごとの行数）を足した。
- **速さ.** 全ログ（Claude 1.3 GB）で既定の窓も 2026-01-01 からも 0.5 秒ほど。`search` と同じ。
- **確かめ.** 単体 5 件、結合 3 件（手で書いたログと、テストの中で `git init` して作った小さなリポジトリ。発言・件名は架空）。わざと壊して落ちるのを確かめた：並べ替えをやめる（発言が前、コミットが後ろにまとまる）、`cwd` を見ずに全セッションを入れる、作者時刻をコミット時刻にする、親のフォルダの行を外側に数える、T3 の作業ツリーの場所を足さない、マージコミットを入れる、id を毎行出す。
- **残した課題.** 同じ名前の別のリポジトリの T3 作業ツリー（`~/.t3/worktrees/<同じ名前>/`）は混ざる。別の道具の作業ツリー（Orca の `~/orca/workspaces/…`、Codex の `~/.codex/worktrees/…`）は、`git worktree list` に残っていれば拾うが、消えたものは拾えない。再開したセッションの複製（約 2%）は同じ発言が 2 回出る。日をまたぐ長いセッションは、窓の中の行の合計で 1 つに決めるので、別のリポジトリで作業した日の発言が入りうる（窓をその日以外に絞れば外れる）。リポジトリの外で、そのリポジトリの話をした会話は拾わない（`--project` か `search`）。`--limit` はない（窓で絞る）。

## search と show で Codex のログも読む（2026-10-17、main に取り込み、`cargo install` 済み）

`search`・`show` が Claude Code と Codex の両方を読む。`--source claude|codex|all`（既定は search が claude、show が all。`--codex-root` を渡した search は all）、`--codex-root DIR`（何度でも。既定は `~/.codex/sessions` と `~/.codex/archived_sessions`）。出力の行と `show` の見出しに `claude` / `codex` の印、JSON に `source`。`repeats`・`tool`・`daily` は Claude だけのまま。仕様は `docs/design.md` の「Codex のログ」、使い方は `docs/recipes.md` の 4、`src/help.txt` の最後の節。

- **作り.** `codex.rs`（ファイル選び、人の発言の見分け、ツールの1行）と `needle.rs`（式から「必ず入る文字列」を取り出し、生の行で先に見る）を足した。`talk.rs` の `Session` に `source` を持たせ、`recall.rs` の出力は共通。
- **決めたこと.** 人の発言は、`user` の `message` の `input_text` ブロックから、アプリの差し込み（`<in-app-browser-context>` など11種のタグ、`# AGENTS.md instructions` などの書き出し、添付の一覧）を外したもの。サブエージェントのセッションの `user` は `task`。id は末尾 8 文字で、`show` はどの部分でも受け付ける。同じ id が複数の場所にあれば大きいファイルを 1 つ。分岐したサブエージェントのファイルの、親の写し（`subagent_history_start_ordinal` 未満）は飛ばす。
- **速さ.** 写し（26 GB）に全期間の `search` で 5 秒ほど（`rg -l` は 4.6 秒）。`--since` で更新時刻が窓より前のファイルは開かない。測定は `docs/design.md`。
- **確かめ.** 単体 24 件、結合 24 件（Codex は 8 件、手で書いた架空のログ）。わざと壊して落ちるのを確かめた：`<in-app-browser-context>` を人の発言に入れる、壊れた行でそのファイルの読みを止める、`sessions/` と `archived_sessions/` の同じ id を二重に読む、`"` を探す文字列に入れる、親の写しを読む、添付のあとの本文を捨てる。本物のログでは、架空でない内容をテストにも文書にも入れていない。
- **残した課題.** 行の種類をキーの並び（`"type":"response_item","payload":{"type":"message"`）で見分けているので、Codex がそれを変えると黙って何も出なくなる（テストは手書きで気づけない）。ほかのアプリ（Orca など）が送る依頼文は `codex.rs` の `SENT_STARTS` に足す。`# Diff comments:` は残した。`repeats` などを Codex に広げるかは未定。`cargo install` と、`~/.claude/CLAUDE.md` の案内（`--source` の一言）はまだ。

## search と show（2026-10-17、main に取り込み、`cargo install` 済み）

振り返りのためにログを「探す」「読む」を足した。`agentlog search REGEX`（人の発言と assistant の text を正規表現で探して1行ずつ、古い順）と `agentlog show SESSION`（`--grep` か `--at` を中心に前後 `--around N` 発言、ツールの呼び出しは1行）。窓の既定は制限なし。仕様と見分け方は `docs/design.md` の「search と show」、探し方の手順は `docs/recipes.md` の 4。DB・索引・常駐は持たない（全ログ 1.3 GB で 0.4〜0.5 秒）。

- **確かめ.** 単体 15 件、結合 16 件（うち 7 件が search/show。手で書いた架空の会話）。わざと壊して落ちるのを確かめた：tool_result を本文に入れる、`system-reminder` を人の発言に入れる、`--since` の既定を 14d に戻す、の 3 つ。
- **まだしていないこと.** main への取り込み、`cargo install`、`~/.claude/CLAUDE.md` の「CLI tools」への案内（変えたら `~/.codex/AGENTS.md` と `~/.cursor/rules/global.mdc` にも写し、`changes.md` に時刻を書く）。
- **既知の限り.** 人の発言と T3 Code が送る依頼文は、ログの印では区別できず、書き出しの文言の一覧（`talk.rs` の `sent_for_a_task`）で `task` に分けている。新しい定型が混ざったらそこに足す。再開したセッションの複製で、同じ発言が2つのセッションに出ることがある（約2%）。

## 今どこまでできたか（2026-10-15、repeats / tool / daily）

- **動くもの.** `repeats` / `tool` / `daily` の 3 つ。`cargo install --locked --path .` で `~/.cargo/bin/agentlog`（0.1.0）に入れた。数え方は `docs/design.md`、使いどころは `docs/recipes.md`。
- **確かめ.** `cargo test` に、単体テスト 9 件（時刻、シグネチャ）と、手で書いたログを読ませる結合テスト 9 件。わざと壊したとき（窓を外す、ユーザーの拒否を数える、断られたものにプログラム名を付ける、rpatch の失敗の判定を外す、Write を常に数える、実行中のセッションを外さない）に、6 つとも落ちることを確かめた。
- **実行中のセッション.** Claude Code の中から呼ぶと、`CLAUDE_CODE_SESSION_ID` のセッションを自動で外す（`--include-self` で入れる）。
- **本物のログ.** 10 月 1〜15 日（684 文脈、28,668 回）で、使い捨ての Python で出した数字とほぼ合う（rpatch の失敗の連なり 188 対 193。差は失敗の判定の細部）。1 秒前後。
- **リポジトリ.** https://github.com/lenalab-me/agentlog （private）。CI はない。

## そのときの結論（10 月 1〜15 日）

- rpatch: 失敗の連なり 188 のうち 159 は次の 1 回で直った。続けて失敗は 12、逃げたのは 15（Write 5、Edit 3、sd 3、`cat >` 3、sed 1。うち 3〜4 は `/tmp` 以外への新しいスクリプトで、逃げではない）。
- devsrv: 別のやり方は、使い始める前（10/7 02:21 より前）1 日 17.1 回、後 3.1 回。後に残ったのは、network-designer の 1 セッションで、直す前と後のサーバーを 2 つ並べて立てたもの（約 6 回）が主。
- くり返しの一番: worktree に隔離されたサブエージェントが、複合コマンド（`cd && …`、heredoc、`. file`、`$()`）をガードに断られる。23 の文脈でくり返し、合計 87 回（似た断りを合わせて 150 回ほど）。network-designer と exam-platform の 2 つの親セッションの、サブエージェント 38 個。サブエージェントごとに同じ制約を学び直している。
- 片付いていたもの: hook に止められた編集（10/7〜9 に 72 回、10/10 から 0）、aws-knowledge のレート制限（10/2 の 1 日だけ）。
- rpatch のプリセットで「別のやり方」に数える Edit が 10 月に 1,447 回。CLAUDE.md は「ファイルの変更は rpatch」としているので、どの文脈で使われているかは見る価値がある（`agentlog tool Edit` と `-v`）。

## 次にやること

1. worktree のガードへの対策を決める。サブエージェントへの依頼文に「git は `git -C <絶対パス>` の単独のコマンドで。`cd && …`、heredoc、`. file` を使わない」を足すだけで減るかもしれない。足したら、その時刻から `agentlog daily wt='isolated in the worktree'` で測る。
2. プリセットを足す候補: worktree の対策に道具を作るならそれ、`waitfor` を作るならそれ。足し方は `docs/recipes.md` の 3。
3. 逃げの誤判定を減らす: 失敗の結果に出るファイル名（rpatch なら `edits[N] <file>`）と、逃げた先のファイルが同じかを見る。道具ごとにファイル名の出し方がちがうので、プリセットに正規表現を持たせる形が考えられる。
4. 週に 1 回くらい `agentlog repeats --since 7d` を見る習慣にするなら、T3 の schedule_task に載せる。

## 経緯の要点

最初の調べは、ファイルの更新時刻で選んだため 4 月の kubectl を 10 月に数え、rpatch の 1 回の失敗も、devsrv ができる前の手作業も「問題」に数えた。ユーザーに 3 回指摘されて直した（README の「数え方の決まり」）。同じ取り違えをしないための入口として、この CLI にした。
