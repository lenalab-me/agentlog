# agentlog の設計

セッションログから次に作る道具を探すとき、数え方を取り違えやすい（下の「問題」）。その防ぎ方を、道具を問わず使える 1 つのバイナリに固めたもの。振り返りでログを「探す」「読む」ための `search` と `show`（下の「search と show」）と、会話とコミットを時刻で並べる `timeline`（下の「timeline」）も持つ。

## 問題

エージェントの手間を減らす道具（rpatch、devsrv）を作るか決めるときと、作ったあとに使われているかを見るとき、根拠はセッションのログしかない。数えるのは易しいが、何を数えるかで結論が逆になる。取り違えやすいのは次の 3 つ。

- **どの呼び出しを窓に入れるか。** ファイルの更新時刻で選ぶと、昔のセッションを再開しただけで、昔の呼び出しが窓に入る。
- **何を失敗とみなすか。** rpatch は、失敗したら次の 1 回で直せるように作ってある。1 回の失敗を数えると、設計どおりに動いている道具が一番の問題に見える。
- **いつから比べるか。** devsrv ができる前の `lsof`・`pkill` は、devsrv からの逃げではない。

## 使い方（呼ぶ側から見た形）

```
$ agentlog repeats --since 2026-10-01 --top 3
2026-10-01 00:00 .. 2026-10-08 12:00 (local): 1200 calls, 90 failed, in 40 contexts (a subagent is its own context)
The same failure again. rep: contexts that had it twice or more. ctx: contexts that had it.
streak: longest run of it on one tool with no passing call of that tool between.

 rep  ctx total streak days  last         signature
   4    9    20      3    3  10-07 04:27  Bash | This agent is isolated in the worktree <path>, but this command is too complex …
   3    6    11      1    5  10-07 19:46  Bash rpatch | error: edits[N] <path> (input line N): "old" not found
   2    3     7      4    1  10-02 20:58  mcp__example__search | Error POSTing to endpoint: … Too many requests …

$ agentlog tool devsrv --since 2026-10-01
…
devsrv
  first use in the window: 2026-10-03 02:21
  uses: 100, failed: 5 (5.0%)
  other ways per day: 10.0 before the first use (2.0 days), 2.0 after (5.0 days)
  other ways: Bash pkill 12, Bash kill 8, Bash lsof 8, Bash nohup 6, …

  after a failure (5 runs of failures; other calls between are skipped)
        3  fixed on the next call
        2  context ended after it

  day          uses  failed  other
  2026-10-02      0       0     10
  2026-10-03     12       0      9  <- first use
  …
```

（上は形を示す作った例。本物の数ではない。）

読み方は
```

読み方は `docs/recipes.md`。

**help は 2 段。** エージェントは `agentlog --help | head -40` のように先頭だけを読むことが多く、何行で切るかは呼ぶ側で決められない。そこで `--help`（`-h`・`help`・引数なし）は要約だけ（使い方・例・出力・終了コードと、詳しい説明の引き方の案内）を出し、全体で 40 行以内に収める（`tests/cli.rs`）。詳しい説明は `agentlog help COMMAND`（`agentlog COMMAND --help` も同じ）で引き、`agentlog help all` が全文。文章の正本は `src/help.txt` の 1 ファイルで、要約は「先頭から `Exit status:` の段落が終わる空行まで」、それ以降は「空行のあとの行頭の見出し」から次の見出しの前までが 1 つの節（`src/help.rs`）。名前と節の対応は `src/help.rs` の `TOPICS` の 1 か所だけで、どの名前でも引けない節があるとテストが落ちる。知らない名前は終了コード 2。

## 形

### 文脈と呼び出し

`~/.claude/projects/<project>/<session>.jsonl` と、その下の `<session>/subagents/*.jsonl` を全部読む（シンボリックリンクのディレクトリもたどる）。**1 つのファイルが 1 つの文脈。** サブエージェントは親の文脈を持たずに始まるので、親とは別に数える。worktree のガードに、サブエージェントごとにぶつかり直しているのが見えるのは、このため。

呼び出しは、`assistant` の行の `tool_use` と、`user` の行の `tool_result` を `tool_use_id` で組にしたもの（`load::Call`）。持つのは、時刻、ツール名、入力の要約（Bash はコマンド、Read/Write/Edit はファイルのパス、ほかは入力の JSON）、失敗したか、結果の本文。入力と結果は 4000 バイトで切る（シグネチャと例に足りる）。

**窓は呼び出しごとの時刻に当てる。** ファイルの更新時刻は、窓より前に書き終えたファイルを読まないためだけに使う（必要条件であって十分条件ではない）。時刻は UTC で書かれているので、`--since` に時差なしで書いた時刻と、表の日付は、このマシンの地方時で扱う（`time.rs`。`localtime_r` の `tm_gmtoff`）。

### 失敗

`is_error` が立っているか、結果が `Exit code N`（N は 0 以外）で始まるもの。Bash の結果には 2 種類ある。

- **動いて 0 以外で終わった**: `Exit code N` で始まる。プログラムの出力が続く。
- **動く前に断られた**: ハーネス（worktree のガード、安全の判定）や hook のことば。`Exit code` で始まらない。

`repeats` では、ユーザーが断ったもの・止めたもの（`The user doesn't want to …`、`Error calling tool (Bash): interrupt`、`[Request interrupted by user`）を外す。エージェントの誤りではない。

### シグネチャ（`sig.rs`）

同じ誤りが、別のファイルや別のセッションで同じ文字列になるようにする。

1. **何が失敗したか.** 動いた Bash は `Bash <プログラム>`。プログラムは、先頭の `cd X &&`・`VAR=x`・`timeout N`・`rtk proxy` を飛ばした最初の語。`npm`・`git`・`docker` などは次の語まで（`npm test`）。断られた Bash はプログラムを付けない（ハーネスのことばは、何を走らせようとしたかによらない）。Bash 以外はツール名。
2. **何と言われたか.** 動いた Bash は、`Exit code` の行より後で、誤りらしい語（error、not found、denied、timed out、blocked …）を含む最初の行。なければ出力の最初の行。断られたものと Bash 以外は、結果の最初の行。
3. **ならす.** パス（`/` を含む語）を `<path>`、7 桁以上の 16 進を `<id>`、数字を `N` に。空白を詰めて 110 文字で切る。

「なければ最初の行」にしたのは、`rpatch: line 19: a FROM part needs …` のように誤りらしい語を含まない短い誤りを、まとめるため。毎回ちがう出力（`grep` の結果、表）は、1 回きりのシグネチャになって一覧から落ちる。`Exit code N` 1 つにまとめると、それが一覧の上位に来て意味を持たない。

### repeats

シグネチャごとに、次を数える。

| 列 | 意味 |
|---|---|
| `rep` | その失敗が 2 回以上あった文脈の数。並びの第 1 キー |
| `ctx` | その失敗があった文脈の数。第 2 キー |
| `total` | 回数。第 3 キー |
| `streak` | 同じ文脈・同じツールで、その失敗が続いた最長。間に同じツールの成功があれば切れる。ほかのツール（Read など）は間に挟まっても切らない |
| `days` | 起きた日数（地方時） |

1 つの文脈で 1 回、ほかでも起きていないものは出さない（`--all` で出す）。「同じ失敗を 2 回」の基準に合わせた。

### tool

1 つの道具について、文脈ごとに呼び出しを 2 種類に分け、ほかは飛ばす。

- **使った**（`Use`）: プリセットか `--uses` の正規表現に Bash のコマンドが当たったもの。`--uses` も プリセットもない名前は、ツール名そのもの（`Edit`、MCP のツール）。**失敗**は、失敗した呼び出しのうち、`--fail` の正規表現に結果が当たったもの。`rpatch … && cargo test` で後ろの `cargo test` が落ちたのは rpatch の失敗ではない。
- **別のやり方**（`Alt`）: `--alt` の正規表現か `--alt-tool` のツール。日ごとに数え、失敗の直後なら「別の手段に逃げた」に数える。`--escape` / `--escape-tool` は、失敗の直後だけ数える（日ごとには数えない）。Write や `cat >` は新しいファイルを作るのに毎日使うが、失敗した直後に同じことをすれば、書き直しへの逃げになるため。`--ignore` に当たるもの（`/tmp` への書き出し）は数えない。

使ったもののうち失敗が続いた連なりを 1 つと数え、その直後の分類で終わり方を決める。

| 直後 | 終わり方 |
|---|---|
| 使った（成功） | 連なりが 1 回なら `fixed on the next call`、N 回なら `fixed after N failures in a row` |
| 別のやり方 | `went another way`（N 回の後なら `… after N failures`）。行き先を `went to` に数える。rpatch はファイルで 3 つに分ける（下） |
| なし | `context ended after it` |

rpatch（プリセットの `same_file`）は、失敗した呼び出しが編集しようとしていたファイルと、直後の別のやり方が書いたファイルを比べる。逃げは、失敗したファイルを別の手段で直したもの。失敗の直後に別のファイルを Edit で直したのは、その仕事の続きで、逃げではない。

| ファイル | 終わり方 | 数え方 |
|---|---|---|
| 同じ | `went another way on the same file` | 逃げ。`went to` |
| 別 | `another way on another file (not an escape)` | 逃げではない。`on another file`（JSON は `another_file`） |
| 分からない | `went another way, file not known` | 逃げ（今までどおり）。`went to` |

- **失敗した側.** 連なりの失敗すべての、`rpatch FILE` の引数、`=== FILE` の行、JSON の `"file"`、誤りの `edits[N] FILE`。
- **別のやり方の側.** Edit・MultiEdit・Write の `file_path`。Bash は、コマンドの頭の `sed -i`（BSD の `-i ''`・`-i.bak` と GNU の `-i`、`-e` があれば位置引数はすべてファイル）、`perl -i`（スイッチの `i` から後ろはバックアップの接尾辞）、`sd`（3 つ目からの位置引数。パイプの中なら無し、`-p` は書かない）、`cat > F`・`>| F`、python の `open(F, 'w')`・`Path(F).write_text`（どちらもなく `.write_` があれば、`Path(F)` のすべて）。
- **比べ方.** `~/` は HOME、`.`・`..` は字の上でたどり、`/private/tmp` は `/tmp`。コマンドの頭の `cd DIR` の後の相対パスは DIR の下。ログの呼び出しには作業ディレクトリがないので、相対パスはフルパスの末尾（`/` の境で）と合えば同じとする。別のフォルダの同じ名前のファイル（`README.md`）は同じに数えうる。
- **分からない.** 変数（`$f`）・バッククォート・グロブを含むパス、中身のない rpatch、ファイルを名指ししない python（パスを組み立てる）、別のやり方の正規表現には当たるが書かない python（スクリプトを実行しただけ）。分けて出すので、`-v` で確かめられる。
- **ほかの道具.** devsrv と、名前や `--uses` で渡した道具は、ファイルを比べない（`went another way` のまま）。

最初に使われた時刻で窓を前後に分け、「別のやり方」の 1 日あたりの回数を並べる。前が 1 日に満たないときは出さない。

#### プリセット

| 名前 | 使った | 失敗 | 別のやり方 | 失敗の直後だけ |
|---|---|---|---|---|
| `rpatch` | `rpatch` を語として含む | `rpatch: ` か `error: edits[` で始まる行がある | Edit、MultiEdit、コマンドの頭にある `python3 …` の `.replace(`・`write_text`・`open(…,'w')`、`sd`、`sed -i`、`perl -i` | Write、`cat >`、`>|` |
| `devsrv` | `devsrv start|stop|ls|logs|who|wait|restart` | `devsrv: ` で始まる行がある | `lsof -i`、`pkill`、`nohup`、`kill N` / `kill $(…)`、裏に回した `http.server`・`npm run dev`・`vite`、`until … curl` | なし |

「コマンドの頭」は、行頭・改行・`;`・`&`・`(` の直後。パイプの中の `sd`（`rg x | sd a b`）はファイルを書き換えないので数えない（CLAUDE.md でも、パイプの中の sd は勧めている）。python の heredoc は、置き換えの式が 2 行目以降に来るので、行をまたいで探す。「裏に回した」は単独の `&` で、`&&`・`>&`・`&>` は含まない。

別のやり方のラベルは、当たった部分の最初の語（`Bash sed`、`Bash kill`）。`kill 123` と `kill $(…)` を 1 つにするため。

### daily

各 `NAME=REGEX` について、入力か結果（`--in` で片方に絞れる）が当たった呼び出しを、地方時の日ごとに数える。仮説を日付で確かめるのに使う（hook を入れた日を境に減ったか、ある日だけの出来事か）。

### 出力と終了コード

既定は表。`--json` で同じ内容を 1 つの JSON にする。**コマンドの本文は `-v` / `--examples N` のときだけ出す。** ログには認証情報が入りうる。シグネチャは、ならしたエラー行なので既定で出す。

終了コードは 0 成功、2 入力の誤り（日時、正規表現、知らない引数）、3 ログのディレクトリが読めない。数えた結果が 0 件でも 0。

### モジュール

| ファイル | 役目 |
|---|---|
| `main.rs` | 引数を読み、ログを読み、8 つ（`repeats`・`tool`・`daily`・`search`・`show`・`timeline`・`memory`・`rules`）のうち 1 つを呼んで出す。`search`・`show`・`timeline` は Claude と Codex の両方を読んで 1 つの一覧にする |
| `cli.rs` | 引数。オプションはサブコマンドの後のどこに置いてもよい。実行中のセッションを外す |
| `load.rs` | ファイルを集め（`all_files`・`select_from`・`select_files`）、スレッドで並べて読み（`read_all`）、呼び出しにする |
| `sig.rs` | プログラム、エラー行、ならし、シグネチャ、ユーザーが断ったか |
| `commands.rs` | `repeats`・`tool`・`daily` と、プリセット |
| `talk.rs` | Claude Code のログから会話を読む（`read_lines`）：人の発言とそうでないものの見分け、assistant の text、ツールの1行、セッションの id、プロジェクト名。`Session`・`Event`（どちらのログかを持つ）もここ |
| `codex.rs` | Codex のログから同じ `Session` を作る：ファイル選び（重複の除外）、行の先頭での分類、人の発言の見分け、ツールの1行（`read_lines`） |
| `cache.rs` | 控え：読み取った結果（`Extract`）の置き場所、見分け、読み足し、書き出しと読み戻し（`fetch`） |
| `lines.rs` | ログを行ごとに、位置から読む（要らない行は持たない。改行で終わらない最後の行は渡さず、次に読む位置を返す） |
| `help.rs`・`help.txt` | `--help` と `help COMMAND` の文。文は `help.txt` の 1 か所だけにあり、`help.rs` が要約（`--help`、「Exit status:」の段落まで）と節（`help COMMAND`）に切り分けて出す |
| `build.rs` | 読み方を決めるソースのハッシュ（`AGENTLOG_READER`）を作る。控えの `reader` |
| `needle.rs` | `search` の正規表現から「一致する行に必ず入る文字列」を取り出し、生の行で先に当たりを見る（JSON に読む前の絞り込み） |
| `recall.rs` | `search` と `show` の出力 |
| `timeline.rs` | `timeline`：`git log` と `git worktree list` を呼び、セッションがリポジトリのものかを `cwd` で決め、発言とコミットを時刻順に並べて出す |
| `memory.rs` | `memory`：メモリのファイル（Claude Code の `~/.claude/projects/<フォルダ>/memory/`、Codex の `~/.codex/memories/`）を探して読み（frontmatter・コードブロックの区別、実体のパスでの重複除き、プロジェクト名）、正規表現で探して出す。`rules` も同じ読み方を使う |
| `rules.rs` | `rules`：メモリと指示ファイルから禁止の行を拾い、ファイルごとの本数と行の一覧を出す。禁止の判定の正規表現（`DEFAULT_PATTERN`）はここに 1 か所 |
| `time.rs` | ISO 8601 の読み書き、地方時の日付、`14d` のような相対の指定 |

依存は `serde_json`、`regex-lite`、`libc`（地方時の差）、`memchr`（行の分割と文字列探し。`serde_json` がすでに使っている）だけ。行に `"tool_use` も `"tool_result` も含まれなければ JSON として読まずに飛ばす。

### search と show

`repeats`・`tool`・`daily` が道具の呼び出しを数えるのに対し、この 2 つは会話の本文を読む。振り返りで「前に何と言ったか」「なぜそう決めたか」を探すためのもの。使うのは主に Claude（Bash から呼ぶ）。Claude Code のログと Codex のログの両方を読む（Codex は下の「Codex のログ」。この節の以下は Claude Code のログの話）。

**何を会話とみなすか.** 対象は、人の発言（`user`）と assistant の `text`。tool_use の入力、tool_result、thinking は入れない（ツールの方は既存の `daily` などの領分）。`user` の行には、人が打った文のほかに、ツールの結果とハーネスが差し込んだ文が混ざるので、`talk.rs` の `read_session` と `human_text` の 1 か所で分ける。

| 見るところ | 扱い |
|---|---|
| `user` の行で、内容が tool_result だけ | 外す（`"type":"tool_result"` を含み `"type":"text"` を含まない行は、JSON に読む前に飛ばす） |
| `isMeta`、`isCompactSummary`、`isVisibleInTranscriptOnly` | 外す |
| `promptSource` が `system`、`turnOrigin` が `task_notification` / `peer` | 外す |
| `<system-reminder>…</system-reminder>`、`<local-command-caveat>`、`<local-command-stdout/stderr>`、`<bash-input/stdout/stderr>`、`<task-notification>` | 差し込まれたブロック。メッセージのどこにあっても取り除く。残りが空なら発言なし |
| `<command-name>/x</command-name>` と `<command-args>` | スラッシュコマンドは打ったもの。`/x 引数` 1 つの発言にする（後に続く展開されたスキル本文は `isMeta` で外れる） |
| `[Request interrupted by user…`、`[Image: source:`、`This session is being continued from…`、`Base directory for this skill:`、`<scheduled-task`、`<command-message>` で始まる | ハーネスが書いた文。外す |
| サブエージェントのファイルの `user` 行（`isSidechain` が真、またはファイル名が `agent-`） | 親が書いた依頼文。人の発言ではないので `who` を `task` にする |
| T3 Code が人の代わりに送る定型の依頼（`Delegated task`、`Act as the `、`Generate a title `、`You generate concise git branch names`、`Continue where you left off`、`You have N orchestration message`） | これもログ上は打った文と同じ印（`promptSource: sdk`）なので、書き出しの文言で見分けて `task` にする |
| assistant の `model` が `<synthetic>` | 外す（APIの代わりにハーネスが返した定型文） |
| `queue-operation`、`attachment`、`system` など、`user`・`assistant` 以外の行 | 見ない |

ログの形：人が打った文は `promptSource` が `typed`（Claude Code の端末）か `sdk`（T3 Code などの SDK 経由）で、**印だけでは T3 が送る依頼文と区別できない**（どちらも `sdk`）。そのため、T3 Code の定型の書き出しの一覧（表の該当の行）は、ログを見て足していく経験則。新しい定型が混ざったら `sent_for_a_task` に足す。`--who user` が `task` を含まないのはこのため（`--who any` には含まれる）。

**セッションの id.** 先頭 8 文字。サブエージェントは `親の8文字/エージェントの8文字`（`show` にそのまま渡せる）。親のセッションは、ファイルが `<session>.jsonl` ならその名前、サブエージェントは `<session>/subagents/agent-X.jsonl` の `<session>`、古い形で同じディレクトリに `agent-X.jsonl` があるときは、行の中の `sessionId`。

**プロジェクト名.** フォルダ名（`-Users-me-project-prod-shop-web`）は、ハイフンが区切りか名前の一部か分からないので、読みやすくできない。ssh で入ったセッションのフォルダ（`ssh-<id>`）には、パスがまったく出ない。そこで、**セッションで最初に出る `cwd`**（開始したディレクトリ）から作る。ホーム（`/Users/<名前>/`、`/home/<名前>/`）と、先頭の `project/prod/`（なければ `project/`）を落とす。`/Users/me/project/prod/shop/web` は `shop/web`、`~/.t3/worktrees/x/y` は `.t3/worktrees/x/y`、ssh 先の `/home/u/project/designer` は `designer`、ホームそのものは `~`。`cwd` がなければフォルダ名を従来どおり短くしたもの。`--project S` は、フォルダ名か、この表示名のどちらかに S が含まれればよい（ssh のセッションを名前で拾えるように、読んだあとで判定する）。

**search.** 発言ごとに最初の一致を 1 行にする（時刻は地方時の分まで、`who`、`source`、id、プロジェクト、一致箇所の前後 40 文字ずつ。改行は空白）。古い順。`--limit`（既定 50）を超えたら**新しい方を残して**、全件数と切った数を出す（振り返りで欲しいのは最近の方が多いため。古い方は `--since`・`--until` で絞って見る）。`--json` は 1 件 1 オブジェクトで 1 行ずつ、切ったことの知らせは標準エラーに出す。0 件でも終了コードは 0。窓の既定は**制限なし**（`--since 14d` ではない。古い会話を探すため）。

**show.** `SESSION` は id の先頭（`search` が出した 8 文字、サブエージェントは `親/子`）。複数に当たれば、候補を出して終了コード 2。実行中のセッションを自動で外す動きは `show` では行わない（名前を指定しているので）。本文は順に、ツールの呼び出しは `[Bash] コマンドの先頭行…` の 1 行（結果は出さない）。`--grep RE`（最初に一致した発言）か `--at 時刻`（もっとも近い発言。分までなら、その分の中の最初の発言）を中心に、前後 `--around N`（既定 5）発言を出す。中心の発言には `>>` を付け、各発言に `#番号` を付ける。どちらもなければ全部。1 発言は 2000 文字で切る（`--full` で全部）。

**なぜ DB を持たないか.** 毎回全部を読む作りでは、ログの量に比例して遅くなるため、読み取った結果の控えを持つ（下の「控え」）。DB や索引は持たない。更新の遅れ、置き場所、作り直しの手順が増え、ログの形が変わったときに壊れる場所も増えるため。公式にセッション検索が来たら、`talk.rs`・`codex.rs`・`needle.rs`・`recall.rs`・`cache.rs`・`lines.rs`・`build.rs`・`cli.rs` の `search`/`show` の分岐・`main.rs` の `search`/`show` 関数を消せば終わる形にしてある（`load.rs` の `select_files` / `read_all` は残って `repeats` などが使う）。

### Codex のログ（`search`・`show`）

`--source claude|codex|all` で選ぶ。既定は search が claude、show が all。Codex のログは大きいので、search は頼まれたときだけ読む（`--source` か `--codex-root` を渡したとき。`--codex-root` だけなら all）。show は Codex の id をファイル名で探すので、両方を見ても軽い。`--codex-root DIR`（何度でも）で置き場所を渡す。既定は `~/.codex/sessions` と `~/.codex/archived_sessions` のうち、あるもの。写しを読むときは、写しの `sessions` と `archived_sessions` の 2 か所を渡す。`repeats`・`tool`・`daily` は今も Claude だけ（`--source`・`--codex-root` を付けると入力の誤り、終了コード 2）。`--source codex` だけで置き場所がなければ終了コード 3。`all` のとき Codex がない機械は黙って飛ばす。出力の各行（JSON は `source`）と `show` の見出しに `claude` / `codex` の印が付く。

**ファイル.** `rollout-<時刻>-<uuid>.jsonl`。時刻はそのマシンの地方時で、日付のフォルダ（`YYYY/MM/DD`）も同じ。行は `{"timestamp":UTC,"ordinal":N,"type":…,"payload":{"type":…}}`。1 行目は `session_meta`（id・`cwd`・`originator`、サブエージェントなら `source.subagent`・`parent_thread_id`・`subagent_history_start_ordinal`）。実際のログは、`token_count`・`item_completed`・`reasoning`・ツールの出力が大半で、メッセージの行は少ない。

| 行 | 扱い |
|---|---|
| `response_item` の `message`、`role` が `user` | 人の発言の候補（次の表で差し込みを外す） |
| `response_item` の `message`、`role` が `assistant`（`output_text`） | assistant の本文。途中経過（commentary）も最後の答えも入れる |
| `function_call`・`custom_tool_call` | `show` だけ。1 行の要約（`[exec_command] 先頭行…`）。`arguments` の `cmd`・`command`（配列なら最後の要素）・`path`…。コードモードの `exec` は中の `exec_command({cmd:"…"})` を `[exec_command]` として出し、`apply_patch` は `[apply_patch] Update File: パス`。結果は出さない |
| `role` が `developer` の `message`、`reasoning`、`*_output`、`agent_message`、`event_msg`、`turn_context`、`token_count`、`compacted` など | 見ない。`event_msg` の `user_message` は、ないログがあるので使わない |
| JSON として読めない行（途中で切れた最終行、壊れた行） | 飛ばして続ける |

**人の発言の見分け.** 1 つの `user` の `message` は複数の `input_text` ブロックで、ハーネスの差し込みは**ブロックごとに別**になっている。次を外し、残りのブロックを改行でつないだものが発言。残りがなければ発言なし。

| 見るところ | 扱い |
|---|---|
| `<in-app-browser-context …>`、`<environment_context>`、`<recommended_plugins>`、`<subagent_notification>`、`<turn_aborted>`、`<codex_internal_context …>`、`<heartbeat>`、`<skill>`、`<realtime_delegation>`、`<external_codex_apps_open_page>`、`<image …>` | 差し込まれたブロック。ブロックのどこにあっても取り除く（属性つきも） |
| `# AGENTS.md instructions`、`<INSTRUCTIONS>`、`Here is a list of plugins`、`# In app browser:`、`[User attached one or more files`、`[coordinator] `、`[oracle-completion:`、`The next image shows the browser`、`<image`、`</image>` で始まるブロック | 外す |
| `# Files mentioned by the user:` … `## My request for Codex:`（電話のアプリでは `## My request:`）… | この行より後が、打った文。後が空なら発言なし。この行がない添付の一覧も外す |
| `<send_user_message_question_reply>` | アプリの選択肢への返事（JSON）。`answer` だけを発言にする。`question` は assistant の言葉なので入れない |
| サブエージェントのセッション（`session_meta` に `source.subagent` か `parent_thread_id`）の `user` | 親が書いた依頼文なので `who` を `task` に。T3 Code の定型（Claude と同じ一覧）と、`You are working inside Orca` で始まる文も `task` |

`user` ブロックの書き出しを見て決めた（`<…>` や `# …` で始まるものを多い順に見た）。上の一覧にないものが `--who user` に混ざったら、`codex.rs` の `INJECTED`（タグ）か `INJECTED_STARTS`（書き出し）に足す。`# Diff comments:`・`# Browser comments:` は利用者が書いたコメントなので残す。

**サブエージェントの複製.** 親から分岐したサブエージェントのファイルは、先頭に親の行の写しを持つ（`subagent_history_start_ordinal` より小さい `ordinal`）。そのまま読むと同じ発言が親の数だけ出るので、その `ordinal` 未満の行は飛ばす。

**id.** Codex の id は UUIDv7 で先頭が時刻なので、同じ時期のセッションは先頭 8 文字が同じになる。そこで**末尾 8 文字**（乱数の部分）で表す。`show SESSION` は id の**どの部分でも**受け付け（含まれていれば当たり。完全な UUID も可）、複数に当たれば Claude と同じく候補を出して終了コード 2。Claude の id は先頭一致のまま。`/` を含む指定は Claude のサブエージェント用で、Codex は探さない。

**重複.** 同じ id のセッションが `sessions/` と `archived_sessions/`（や、本物と写し）の複数にあれば、**いちばん大きいファイルを 1 つ**だけ読む（同じ大きさなら、先に並んだほう）。途中で切れたものより、長いほうが完全に近いため。id はファイル名の末尾の UUID。

**プロジェクト名.** `session_meta` の `cwd` から、Claude と同じ規則（ホームと `project/prod/` を落とす）。`--project S` はこの名前に S が含まれるものを選ぶ（Claude にあるフォルダ名での一致はない）。

**期間.** 各発言の時刻で絞るのは Claude と同じ。読むファイルを先に絞るため、`--since` はファイルの更新時刻（これより前に書き終えたファイルには窓の中の行がない）、`--until` はファイル名の時刻（地方時なので UTC から最大 14 時間のずれと 1 時間の余裕をみて、それより後に始まったファイル）を使う。ファイル名の時刻では `--since` は絞れない（再開したセッションは後で書き足される）。

**速さ.** 行の先頭の約 200 バイトで種類を決め（`"type":"response_item","payload":{"type":"message"` など）、メッセージ（`show` ではツールの呼び出しも）以外の行は JSON に読まず、長い行は取っておかずに飛ばす。`search` はさらに、`needle.rs` が正規表現から取り出した文字列が生の行になければ、メッセージも JSON に読まない。取り出すのは、一致に必ず含まれ、JSON のなかで書き方が変わらない文字だけ。`"`・`\`・制御文字（改行・タブ）は JSON では `\"`・`\\`・`\n`・`\t` と 2 文字になり、`.`・`\s`・`[^x]` の数える文字数も変わるので、そこで途切れる。`?`・`*`・`{` が付いた直前の 1 文字は外し、`(…)` と `[…]` の中は見ない。`|` は各候補から取り出し、1 つでも取り出せなければ絞り込みなし。`(?` を含む式（フラグ）も絞り込みなし。`-i` は ASCII の大小だけを無視して探す（大小のある ASCII 以外の文字が入るところでは途切れる）。非 ASCII の文字を含む文字列は、`\uXXXX` と書かれた行（まれ）も通す。絞り込みは「通してしまう」ことはあっても「落とす」ことはない（最後に正規表現が決める）。`tests/cli.rs` の `codex_search_finds_what_the_log_writes_differently` が、改行・引用符・タブ・大小・日本語・`|` の式で確かめる。

**限り.** 行の種類を先頭の `"type":"response_item","payload":{"type":"` で見分けるので、Codex が書き出しのキーの並びを変えると、何も見つからなくなる（エラーにならず、テストも手で書いた今の並びなので気づけない）。Codex を更新したあとは、知っている言葉で `search --source codex` が当たるかを一度見る。`cwd` はセッションを動かしたマシンのパス（ssh 先ならリモート側）。時刻は UTC で、表示は Claude と同じく地方時の分まで。

### timeline

振り返り・引き継ぎ・決定の記録を書くとき、`git log` と記憶を手で突き合わせていたのを、機械にしたもの。あるリポジトリで「利用者が何を言ったか」と「何がコミットされたか」を、時刻順に 1 本に並べる。`search` が読むセッション（`talk.rs`・`codex.rs`）と `git log` の上に薄く作ってあり、DB も索引も持たない。

```
$ agentlog timeline --repo ~/project/prod/app --since 2026-10-02 --until 2026-10-04
--- 2026-10-02 ---
2026-10-02 09:58  commit     1a2b3c4   | Add the login form
2026-10-02 10:03  user       0a1b2c3d  | start with the login screen
2026-10-02 10:08  user                 | and keep the form small
2026-10-02 10:20  commit     5d6e7f8   | Validate the form on the server
--- 2026-10-03 ---
2026-10-03 08:12  user       9f8e7d6c  | continue from yesterday
```

（上は作った例。本物の発言・件名ではない。）

**行の形.** 地方時の分まで、種類（`user` / `commit`。`--who any` なら `assistant` と `task` も）、id、先頭 100 文字（改行は空白）。日が変わるところに `--- 日付 ---` の行。同じセッションの発言が続くときは 2 行目から id を空白にする（コミットを挟むとまた出す）。id は `search` と同じ（Claude は先頭 8 文字、サブエージェントは `親/子`、Codex は末尾 8 文字）なので `show` にそのまま渡せる。コミットの id は `git log` の短いハッシュ（`git show` に渡せる）。`--json` は 1 件 1 オブジェクトで 1 行ずつ（`kind` が `user`・`assistant`・`task`・`commit`。発言には `session`・`agent`・`project`・`source`、コミットには完全な `hash`）。空のときの知らせは、表では本文の最後、`--json` では標準エラー。終了コードは 0（0 件でも）、2 は入力の誤り（`--repo` がない・git のリポジトリでない・`--repo` を `timeline`・`rules` 以外に付けた）。

**発言.** 何を人の発言とみなすかは `search` と同じ（`docs/design.md` の「search と show」「Codex のログ」）。既定は `--who user`。assistant の本文は量が多く、流れは利用者の発言で追えるため。`--who any` は `search` の `any` と同じで、`task`（親がサブエージェントに渡した依頼文）も入る。`--source` の既定も `search` と同じ claude（Codex は頼まれたときだけ）。窓は `--since`（既定 14d）と `--until`。実行中のセッションを外す動きも `search` と同じ。

**コミット.** `git -C DIR log --branches --remotes --no-merges`。

- **範囲は全部のブランチ（ローカルと追跡ブランチ）.** 作業ツリーのブランチで作って main にまだ入っていないものや、入れたあとで消したブランチのコミットも、発言と同じ時期の出来事として並べたいため。同じコミットが複数のブランチにあっても git が 1 回だけ出す。タグと stash は入れない。マージコミット（`Merge branch …`）は外す。何が作られたかではなく、いつ取り込んだかだけを言うため、並べると読みにくい。
- **時刻は作者時刻（author date）.** コミット時刻（committer date）は、rebase・cherry-pick・amend で、まとめて取り込んだ時刻に動く。T3 Code のように作業ツリーで作ったコミットを後で rebase して main に入れると、コミット時刻は発言よりずっと後にまとまり、会話との順が崩れる。作者時刻は rebase で動かない。限り：amend は作者時刻を残すので、直したコミットは最初に作った時刻に出る。`git commit --date` で過去に作ったコミットは、その時刻に出る。
- **窓は自分で絞る.** `git log --since` はコミット時刻で絞る（作者時刻より前にはならない）ので、`--since` だけ git に渡して取りこぼしを防ぎ、`--until` と最終の絞りは作者時刻で行う。

同じ秒の発言とコミットは、発言を先に置く（依頼が先で、結果のコミットが後）。

**どのセッションを拾うか（`--project` を省いたとき）.** `--repo` から決める。`--project S` を付けたら、これまでと同じ（フォルダ名か最初の作業ディレクトリ名に S が含まれるか）で、リポジトリの判定は行わない。

セッションのログの各行には、そのときの作業ディレクトリ（`cwd`）がある。セッションごとに、窓の中の行を `cwd` ごとに数え（`Session.cwds`）、次で決める。

| `cwd` | 扱い |
|---|---|
| リポジトリの中（本体、`git worktree list` の作業ツリー、`~/.t3/worktrees/<リポジトリ名>/`） | 中の行 |
| リポジトリより上のディレクトリ（`~/project/prod/shop` で開き、`shop/web` を作業していたとき。ホームや `/` も） | 数えない。どこで始めたかを言うだけで、何に取り組んだかではない |
| それ以外（兄弟のリポジトリ、`/tmp`、別の作業ツリー置き場） | 外の行 |

中の行が 1 つ以上あり、外の行以下（半分以上がリポジトリ）なら、そのセッションの発言を全部入れる。判断は窓の中の行だけで行うので、長く生きるセッションでも、窓を絞れば（その日だけ別のリポジトリで作業していたなら、その日に窓を合わせれば）そのとき作業していた側で決まる。

- **T3 Code の作業ツリー.** `~/.t3/worktrees/<リポジトリ名>/<ブランチ名>`。消した作業ツリーは `git worktree list` に出ないが、ログには残っているので、リポジトリ名（主たる作業ツリーのディレクトリ名）から場所を決めて拾う。同じ名前の別のリポジトリがあれば混ざる。
- **親のフォルダで開いたセッション.** Claude Code はセッションをフォルダ名（開いた場所）ごとのディレクトリに置くので、`~/project/prod/shop` で開いたセッションは `web` や `api` の作業のあとも `shop` のフォルダに入る。フォルダ名や最初の `cwd` で選ぶと、作業した場所に関係なく親の名前の下に落ちる。行ごとの `cwd` で判定するのはこのため。

**拾いすぎと拾い漏れ.**

- 拾いすぎ：1 つのセッションが日をまたいで別のリポジトリ（`shop/api`）にも移ったとき、窓にその日が入っていれば、そのリポジトリでの発言も入る（そのセッションの発言の一部は、作業の場所が `shop/api` だった）。窓をその日以外に絞れば外れる。
- 拾い漏れ：リポジトリの中で作業しない会話は入らない。スクラッチの会話（`~/.t3/scratch/…`）や別のプロジェクトで、そのリポジトリの話をしたもの。名前では `--project` で拾える。
- 外れない：サブエージェントの依頼だけのセッション（T3 Code の `Delegated task` など）は、`user` の発言がないので、`--who user` では行にならない。

**速さ.** 窓の中の `user`・`assistant` の行だけを JSON に読む（`search` と同じ）。`cwd` の数え上げは、すでに読んだ行の `cwd` を見るだけで、読む行を増やさない。`git log` を 1 回呼ぶ。

**なぜ DB を持たないか／公式が来たら.** `search` と同じ。公式にセッションの要約・引き継ぎが来たら、`timeline.rs`・`main.rs` の `timeline` 関数・`cli.rs` の `timeline` の分岐を消せば終わる（`Session.cwds` は残っても害はない）。

### memory と rules

ここまでの命令はセッションのログを読むが、この 2 つはエージェントが残したメモリのファイルを読む。控え（キャッシュ）は使わない（ファイルは小さい）。`build.rs` の「読み方」には数えない（`memory.rs`・`rules.rs` は `NOT_READERS`）。

**メモリの置き場.** Claude Code：`~/.claude/projects/<符号化したパス>/memory/*.md`。先頭の frontmatter に `name`・`description`・`metadata.type`（`user`・`feedback`・`project`・`reference`。古いファイルは `type` が直下）。`MEMORY.md` はほかのファイルの索引。Codex：`~/.codex/memories/` の下の `.md`（`raw_memories*.md`・`rollout_summaries/` も。形は自由で type はない）。

**`memory REGEX`.** 本文・`name`・`description` から探す（frontmatter のほかの行、`originSessionId` や `modified` は探さない）。コードブロックの中は探す。出力は 1 ファイルごとに、プロジェクト・ファイル名・type・description、続けて当たった行を `行番号: 本文`（前後なし）、最後に行数とファイル数。`--json` は 1 ファイル 1 オブジェクト。

- **索引は既定で外す.** `MEMORY.md` は他のファイルの題を並べたもので、探すと同じ語が二重に当たる。`--index` で含める。
- **同じファイルは 1 件.** `-Users-…-shop-web/memory` が `-Users-…-shop/memory` へのシンボリックリンクのように、1 つのメモリが複数のプロジェクトのフォルダから見える。実体のパス（`canonicalize`）で束ね、実体のあるフォルダのプロジェクトで出す。ほかのフォルダの名前は `--json` の `also_in` に残し、`--project`・`--exclude` はそれらの名前でも当たる（`--project shop-web` で、リンク先にある共有のメモリも見つかる）。
- **プロジェクト名.** フォルダ名は符号化されていて読みにくい（`-Users-me-project-prod-x`）ので、`search` と同じ読み方（`talk::project_name`）で、そのフォルダのセッションのログの最初の方にある `cwd` から出す。`cwd` を符号化し直したものがフォルダ名と一致するときだけ採用し、引けなければフォルダ名のまま。Codex は `codex`。
- **窓はない.** `--since`・`--until` は効かず、`--raw`・`--codex-root` はエラー。Codex のメモリの場所は `--codex-memories DIR`。

**`rules`.** 禁止ルールが複数あると互いにぶつかって、エージェントの振る舞いがおかしくなることがある。何本あるかを数え、人かモデルが見比べられるように並べるための棚卸し。**ぶつかりの判定は意味の判断なので、機械ではしない**（キーワードの組み合わせで衝突を推測する仕組みは作らない）。使い方は、`agentlog rules --repo . --json` をモデルに渡して、互いにぶつかる禁止がないか読ませる。

- **対象.** 既定は Claude のメモリ（索引を除く、重複は `memory` と同じ）。`--repo DIR` でそのディレクトリで始めたセッションが読むものに絞る：`~/.claude/CLAUDE.md`、`DIR/AGENTS.md`・`DIR/CLAUDE.md`（片方がもう一方へのリンクなら 1 つ）、`DIR` に対応するプロジェクトのフォルダ（`DIR` の実体のパスを符号化したもの）のメモリ。`--source codex|all` は `~/.codex/AGENTS.md` と Codex のメモリを足す（`--repo` を付けても Codex のメモリは全部。Codex に作業場所ごとの区別はない）。
- **禁止の行.** 本文の行（見出し・frontmatter・コードブロックの中・空行を除く）のうち `DEFAULT_PATTERN`（`rules.rs`）に当たるもの。日本語の「しない／しません／使わない／書かない／入れない／渡さない／提案しない／禁止／やめる／避ける／ならない／してはいけ／いけません／使うな」、文末の「ない」、英語の never・don't・do not・must not・avoid。**偽陽性は許す**（「禁止を挙げた説明」も数える。棚卸しの材料だから）。行ごとに見るので、2 行にわたる箇条書きの項目は行ごとに数える。`--pattern RE` で全部を差し替える（大文字小文字を区別しない `(?i)` は既定の式に入っている）。
- **出力.** ファイルごとの本数の表（多い順）、合計（当たりのあったファイル数・読んだファイル数）、空行のあと `ファイル:行番号  [置き場所]  本文`。`--count` は合計まで。`--json` は 1 行 1 件（`--count` なら 1 ファイル 1 件と、最後に合計）。
- **置き場所の列.** どのファイルかを `global`（`~/.claude/CLAUDE.md`・`~/.codex/AGENTS.md`）・`repo`（`--repo` のディレクトリの `AGENTS.md`・`CLAUDE.md`）・`memory`（Claude のプロジェクトのメモリ）・`codex-memory`（`~/.codex/memories`）の 4 つで示す。表の列、行の一覧の `[…]`、`--json` の `place`。目的は棚卸しで「グローバルに置くべき禁止がプロジェクトにある」「1 つのプロジェクトの事情がグローバルにある」を見つけやすくすること。**見つけるのは人かモデルで、機械は置き場所を示すだけ**（どちらに置くべきかは推測しない）。
- **複数のプロジェクトにある印.** Claude のメモリで、同じファイル名（`feedback-xxx.md` など。`MEMORY.md` は除く）が 2 つ以上のプロジェクトにあるとき、そのファイルに `also in: shop-api, blog-web` と付ける（表は行末の括弧、行の一覧は `[memory; also in: …]`、`--json` は `also_in`。印がなければ空の配列）。シンボリックリンクで同じ実体を見せているだけのフォルダは別のプロジェクトに数えない（`collect` が実体のパスで束ねたあとの、実体のあるフォルダのプロジェクトで数える）。`--repo`・`--project`・`--exclude` で読む対象を絞っていても、ほかのプロジェクトにあるかは全プロジェクトを見て決める（`projects_by_name`）。**内容が同じか違うかは比べない**（名前が同じというだけの印。同じ名前で別の話のこともある）。
- **終了コード.** `--repo` がディレクトリでなければ 2、`--pattern` が正規表現でなければ 2、メモリのフォルダが読めなければ 3。

**限り.** 判定は行単位で、前後の文脈は見ない（「〜しないほうがよい」「〜しないでよい」も当たる）。Claude のプロジェクトのフォルダは作業ツリーごとに別なので、`--repo` は渡した場所のフォルダだけを見て、作業ツリーや親のフォルダで開いたセッションのメモリは入らない。`DIR/.claude/` の下の指示や、`@` で取り込まれるファイルは読まない。

### 控え（`~/.cache/agentlog/`）

`search`・`show`・`timeline` が毎回ログ全体を読むと、ログの量に比例して遅くなる。ところが読み取るのは会話の本文（人の発言と assistant の `text`）だけで、これはログのごく一部（残りはツールの入出力・thinking・メタ情報）。そこで、**ログのファイルごとに、読み取った結果の写しを置き**、ログが追記されたら増えた分だけを読み足す。

**並列は控えと別.** 読みを `available_parallelism()` の数のスレッドに分ける作り（`load::read_all`）は、`repeats` だけでなく `search`・`show`・`timeline` も使う。大きいログではディスクから読むこと自体が支配的で、スレッドを増やしても変わらない。控えは、読む量そのものを減らす。

#### 置き場所と名前

`$XDG_CACHE_HOME/agentlog/`（なければ `~/.cache/agentlog/`）。ルート（`--root`・`--codex-root` の 1 つ）ごとに下のフォルダを分ける。

```
~/.cache/agentlog/claude-<ルートのハッシュ>/<ログのパスのハッシュ>.jsonl
~/.cache/agentlog/codex-<ルートのハッシュ>/<ログのパスのハッシュ>.jsonl
```

ログ 1 ファイルにつき控え 1 ファイル。ハッシュは FNV-1a 64 ビット（パスの文字列から）。控えの先頭にログのフルパスを入れ、読むときに突き合わせる。フォルダは `0700`、ファイルは `0600`（会話の本文が入るため）。置けない（フォルダを作れない・書けない）ときは、何も言わずにログを読む。

#### 形（JSON Lines、1 行 1 件）

```
{"format":1,"reader":"81d92be999bba628","path":"/…/s1.jsonl","size":34535,"mtime":1760000000,"mtime_ns":123456789,"ino":1,"dev":2,"pos":34535,"head":…,"tail":…,"cwd":"/…","inner":"…","own_from":0,"subagent":false}
{"w":"u","t":1760000000,"x":"人の発言"}
{"w":"a","t":1760000060,"x":"assistant の text"}
{"w":"t","t":…,"x":"サブエージェントへの依頼文"}
{"w":"c","t":…,"x":"[Bash] コマンドの先頭行…"}
{"c":"/Users/me/project/x","t":[1760000000,1760000060,…]}
```

（上は形の例。本物の発言ではない。）

- **1 行目（見出し）.** 控えの作り（`format`）と読み方（`reader`）の版、ログの見分け（`path`、`size`、`mtime`・`mtime_ns`、`ino`、`dev`）、読んだ位置（`pos`）、読んだ部分の指紋（`head`・`tail`）、読みの途中の状態（Claude: 最初に言葉のある行の `cwd` と `sessionId`。Codex: `session_meta` の `cwd`、親の写しの境目 `own_from`、サブエージェントか）。
- **発言の行.** `w` は `u`（人）・`a`（assistant）・`t`（`task`、親が子に渡した依頼文）・`c`（ツール呼び出しの 1 行。`show` だけが使う）。`t` は UTC の秒。`x` は本文（JSON の文字列）。キーの並びは固定で、読むときに `w` を行の先頭で見て、要らない種類（`search` と `timeline` ではツールの行）は JSON に読まずに飛ばす。窓は控えの外（`Session` を作るとき）で当てるので、1 つの控えがどの窓にも使える。
- **作業ディレクトリの行（`timeline` だけが使う）.** Claude の、言葉のある行ごとの `cwd` と時刻。`timeline` は窓の中の行を数えて「どのリポジトリの作業か」を決めるので、数ではなく時刻の並びを持つ。Codex は `cwd` が見出しにしかない。
- **`search` の絞り込み.** `search` には、正規表現から取り出した「必ず入る文字列」（`needle.rs`）があり、控えの行にも先に当てて、当たらない発言は JSON に読まない。

#### 見分けと読み足しの規則

起動のたびに、ログ 1 ファイルずつ `stat` して、控えの見出しと比べる。

| 控えの見出しと比べて | 扱い |
|---|---|
| `format`・`reader`・パス・`ino`・`dev` のどれかが違う | 使わない。ログを最初から読んで、控えを書き直す |
| 大きさも更新時刻（ナノ秒まで）も同じ | そのまま使う（ログを開かない） |
| 大きさか更新時刻が違い、`pos` ≦ 今の大きさで、ログの先頭 4 KB と `pos` の直前 4 KB の指紋が同じ | 追記。`pos` から後ろだけを読んで控えに足し、書き直す |
| 今の大きさが `pos` より小さい、または指紋が違う | 小さくなった・書き換わった。最初から読み直して、控えを書き直す |
| 控えが途中で切れている、読めない、形が違う | 使わない（上と同じ） |

- **途中で切れた最後の行.** 改行で終わっていない最後の行は、書いている途中かもしれないので**読まない**。`pos` はその行の手前（最後の改行の次）で、次の実行がそこから読む。`--raw` も同じ規則で読むので、同じ結果になる（実際のログは必ず改行で終わる）。
- **指紋は FNV-1a 64 ビット.** 「同じ inode のまま、同じ先頭で、途中から書き換わったファイル」を、追記と取り違えないため。Claude Code や Codex のログは追記だけなので、普段は通るだけ。
- **控えは全部入れて持つ.** 追記で読み足すときは、`search` が要らない種類（ツールの行、作業ディレクトリ）も含めて足す。どのコマンドで作られた控えも、どのコマンドにも使える。
- **書き込み.** 隣の一時ファイルに書いて `rename` する（読む側が書きかけを見ない。同時に 2 回動いても、どちらかの完全な控えが残る）。
- **消えたログの控え.** ログ全体の一覧（窓で絞る前）にないログの控えは、`search`・`timeline` の実行のたびに消す（そのルートのフォルダの中だけ）。1 時間より古い書きかけの一時ファイルも消す。`show` は消さない（一覧を作らないため）。

#### 版と、上げ忘れの防ぎ方

控えが「元の読み方」と食い違うと、見えないところで答えが変わる。2 つの版で防ぐ。

1. **`format`（数）.** 控えの**作り**（上の形）を変えたときに、人が上げる。`cache.rs` の `FORMAT`。
2. **`reader`（ハッシュ）.** ログから何を取り出すかを決めるソース（`talk.rs`、`codex.rs`、`cache.rs`、`lines.rs`、`time.rs`。`build.rs` が `src/*.rs` から出力・集計だけのファイルを除いたもの）の中身をビルドのたびにハッシュにして（`AGENTLOG_READER`）、控えの見出しに入れる。**読み方のコード（人の発言の見分け、差し込みの一覧、ツールの 1 行など）を 1 文字でも変えると、ハッシュが変わり、古い控えは使われず、最初から作り直される。** 人が版を上げる必要がない。`build.rs` の除外の一覧（`cli.rs` など、読み取った結果を数える・出すだけのファイル）は、新しいファイルを足したときに既定で読み方に数える向きにしてある（要らない作り直しが増えるだけで、答えは誤らない）。

作り直しは 1 回目の実行だけが遅い。コメントだけを直しても `reader` は変わるので、開発中は作り直しが増える（`cargo install` のたびに一度）。

読み方のコードを変えずにログの形が変わったとき（Claude Code や Codex の更新）は、`reader` は変わらないので、控えは古い読み方のままの結果を返す。**`--raw` で同じ問い合わせを流して食い違いを見て、控えのフォルダを消す**（`docs/recipes.md`）。

#### `--raw`

`search`・`show`・`timeline` の `--raw` は、控えを読まず・書かず、ログを直接読む。遅いが確実。控えが怪しいとき、ログの形が変わった直後に使う。読み方は控えを作るときと同じ関数（`talk::read_lines`、`codex::read_lines`）なので、**`--raw` と控えの違いは、控えの置き方（書き出し・読み戻し・読み足し・作り直し）だけ**になる。`repeats`・`tool`・`daily` は控えを使わず、`--raw` は受け付けない。

#### 窓・絞り込みは控えの外

`--since`・`--until`・`--project`・`--exclude`、実行中のセッションを外す動き、Codex の重複の選び方（`sessions/` と `archived_sessions/` で同じ id は大きい方、分岐したサブエージェントの親の写しを飛ばす）は、すべて今までどおり。ファイルの選び方（更新時刻が窓より前なら開かない、重複の選択）は控えより前、窓の当て方（発言の時刻、`cwd` の数え上げ）は控えを読んだあと。親の写しの境目（`own_from`）と Claude の差し込みの除外は、読み方の中（控えに入る前）で済む。

#### 読み方の揃え

1 つの規則を `--raw` と控えで共有するために、読み方を次のとおり揃えている。

- 言葉のある行（`"text"` か `"content":"` を含む行）だけが、セッションの `cwd`・`sessionId`・`cwd` の数え上げに使われる。ツール呼び出しだけの行は使わない。
- Claude のログの行は、バイト列として読む。UTF-8 でない行があっても、そのファイル全体を捨てずに、その行だけ飛ばす。
- 改行のない最後の行は読まない（上）。

#### なぜ DB（SQLite の FTS など）にしないか

DB や全文索引は、更新の管理（どこまで入れたか、消えたログ、壊れたとき）に加えて、日本語の区切り（形態素解析か n-gram か）と、その版の管理が要る。控えは、ログのファイルごとの**元のログから作り直せる写し**で、消しても次の 1 回が遅いだけ。正本はログのまま 1 か所で、2 か所に持たない。検索そのものは正規表現を控えの本文に当てる（全件を線形に見る）が、控えは元のログよりずっと小さいので、索引で速くする余地が小さい。索引の管理は要らない。

公式にセッションの検索が来たら、控えのフォルダごと消し、`cache.rs`・`lines.rs`・`build.rs`・`--raw` を外す（`talk::read_lines`・`codex::read_lines` は `Extract` を返す形のまま、直接読む形に戻せる）。

#### 速さ

- **2 回目以降.** ログの一覧と `stat`、控えの読み出しだけで済む。読む量は、更新されたファイルの増えた分だけ。
- **1 回目（控えを作る実行）.** ログ全体を読むので、ログの量に応じてかかる。ディスクの読みが上限なので、控えでは減らせない。ツール呼び出しの行も読むので、`--raw` より少し重い。
- **並列.** 読みはスレッドに分ける（`load::read_all`）。控えの有無によらない。

#### 限り

- **ログの形が変わったとき.** `reader` のハッシュは読み方のコードの変更しか見ない。ログの側が変わったら、`--raw` と突き合わせて控えを消す（上）。
- **中身の書き換わりを見ない.** 大きさも更新時刻も同じまま中身だけが変わったファイルは、控えを信じる。同じ inode で、先頭と読んだ位置の直前（各 4 KB）の指紋が同じまま、間だけが書き換わり、長さが増えた場合も追記と取り違える。ログは追記だけなので、実際には起きない。
- **指紋の読み出しは追記のたびに 8 KB.** 実行中のセッションのログは、1 回ごとに控えを書き直す。
- **古い版の控えは、そのログが読まれるまで残る.** `reader` が変わっても、古い控えを一斉には消さない（読まれたものから書き直す）。消したいときはフォルダごと消す。

## ほかに考えた形

- **Python のまま、rpatch の `tools/log-analysis/` を広げる。** 速さと、rpatch・devsrv と同じ置き方（`cargo install`）を取った。rpatch の分析は rpatch 専用の詳しさ（失敗の種類、書式の内訳）があるので、そのまま残す。
- **別のファイルへの別のやり方を飛ばして、その先を見る。** 別のファイルを Edit したあと rpatch で直せば `fixed` に、同じファイルを Edit すれば逃げに数えられ、より正確になる。ただし連なりの数え方が変わり、前と後で比べにくくなるので、直後の 1 回で決め、別のファイルは分けて出すだけにした。
- **プリセットごとに、ファイル名の正規表現を持たせる。** 道具ごとに出し方がちがうので一般にはできるが、今ファイルを扱うのは rpatch だけなので、読み方はコードに書いた（`written`）。
- **シグネチャの似たものをまとめる（編集距離など）。** 今はならした文字列が同じものだけ。ならしで十分まとまっている。

## 未決と危険

- **逃げの誤判定.** ファイルが分からないもの（上）は、今までどおり逃げに数える。相対パスは末尾で合わせるので、別のフォルダの同じ名前のファイルを同じに数えることがある。`--ignore` は Bash だけに効くので、`/tmp` への Write は、失敗したファイルが分かれば別のファイル、分からなければ逃げに数える。
- **シグネチャの分かれ方.** プログラムの取り方で分かれることがある（`cat > x <<EOF && rpatch …` は `Bash cat`）。`tool` は正規表現で当てるので影響しない。
- **ログの形式.** Claude Code のログの形が変わると読めなくなる。`tool_use` / `tool_result` / `is_error` / `timestamp` / `Exit code N` に頼っている。`repeats`・`tool`・`daily` は Codex のログを読まない（`search`・`show` だけが読む。上の「Codex のログ」）。
- **今のセッション.** `CLAUDE_CODE_SESSION_ID` があれば、パスにその ID を含むファイル（本体とサブエージェント）を外す。分析そのものの呼び出し（エラーの文字列を grep した、など）が、分析の結果に混ざるため。ターミナルから直接呼ぶと変数がないので、そのセッションの ID を `--exclude` で渡す。
- **`search` の見分けは経験則.** 人の発言と T3 Code が送る依頼文は、ログの印では区別できない。一覧にない新しい定型が来ると、`--who user` に混ざる。`--exclude t3code-claude-title` のように、題名づけ用の一時セッションをフォルダ名で外す手もある。
- **再開したセッションの複製.** 再開や分岐で、前のセッションの行が新しいファイルに写されることがあり、`search` が同じ発言を 2 つのセッションで見つけることがある。重複は消していない（どちらのセッションの id も本物のため）。
- **書き出されなかった発言.** Claude Code が `queue-operation`（実行中に打った文の待ち行列）にしか書かなかった文や、ログに残らなかった文は見えない。
