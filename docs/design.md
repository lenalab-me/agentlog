# agentlog の設計

2026-10-15。10 月のセッションログから次に作る道具を探したとき、使い捨ての Python で数え方を 3 回取り違えた（README の「経緯」）。その直した数え方を、道具を問わず使える 1 つのバイナリに固めたもの。

## 問題

エージェントの手間を減らす道具（rpatch、devsrv）を作るか決めるときと、作ったあとに効いているかを見るとき、根拠はセッションのログしかない。数えるのは易しいが、何を数えるかで結論が逆になる。取り違えたのは次の 3 つ。

- **どの呼び出しを窓に入れるか。** ファイルの更新時刻で選ぶと、4 月のセッションを 10 月に再開しただけで、4 月の kubectl 360 回が 10 月に入った。
- **何を失敗とみなすか。** rpatch は、失敗したら次の 1 回で直せるように作ってある。1 回の失敗を数えると、設計どおりに動いている道具が一番の問題に見える。
- **いつから比べるか。** devsrv ができる前の `lsof`・`pkill` は、devsrv からの逃げではない。

## 使い方（呼ぶ側から見た形）

```
$ agentlog repeats --since 2026-10-01 --top 3
2026-10-01 00:00 .. 2026-10-15 20:47 (local): 28668 calls, 1293 failed, in 684 contexts (a subagent is its own context)
The same failure again. rep: contexts that had it twice or more. ctx: contexts that had it.
streak: longest run of it on one tool with no passing call of that tool between.

 rep  ctx total streak days  last         signature
  23   31    87      3    3  10-14 04:27  Bash | This agent is isolated in the worktree <path>, but this command is too complex …
  12   40    62      1   13  10-15 19:46  Bash rpatch | error: edits[N] <path> (input line N): "old" not found
  12   15    47      6    1  10-02 20:58  mcp__aws-knowledge__aws___search_documentation | Error POSTing to endpoint: … Too many requests …

$ agentlog tool devsrv --since 2026-10-01
…
devsrv
  first use in the window: 2026-10-07 02:21
  uses: 270, failed: 17 (6.3%)
  other ways per day: 17.1 before the first use (6.1 days), 3.1 after (8.8 days)
  other ways: Bash pkill 42, Bash kill 28, Bash lsof 28, Bash nohup 24, Bash http.server 6, …

  after a failure (16 runs of failures; other calls between are skipped)
        9  fixed on the next call
        7  context ended after it

  day          uses  failed  other
  2026-10-06      0       0     24
  2026-10-07     12       0     19  <- first use
  …
```

読み方は `docs/recipes.md`。

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

「なければ最初の行」にしたのは、`rpatch: line 19: a FROM part needs …` のように誤りらしい語を含まない短い誤りを、まとめるため。毎回ちがう出力（`grep` の結果、表）は、1 回きりのシグネチャになって一覧から落ちる。前は `Exit code N` 1 つにまとめていたが、それが一覧の上位に来て意味を持たなかった。

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
| 別のやり方 | `went another way`（N 回の後なら `… after N failures`）。行き先を `went to` に数える |
| なし | `context ended after it` |

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

既定は表。`--json` で同じ内容を 1 つの JSON にする。**コマンドの本文は `-v` / `--examples N` のときだけ出す。** ログには認証情報が入りうる（devsrv の `baseline.py` も件数だけを出していた）。シグネチャは、ならしたエラー行なので既定で出す。

終了コードは 0 成功、2 入力の誤り（日時、正規表現、知らない引数）、3 ログのディレクトリが読めない。数えた結果が 0 件でも 0。

### モジュール

| ファイル | 役目 |
|---|---|
| `main.rs` | 引数を読み、ログを読み、3 つのうち 1 つを呼んで出す |
| `cli.rs` | 引数。オプションはサブコマンドの後のどこに置いてもよい。実行中のセッションを外す |
| `load.rs` | ファイルを集め、スレッドで並べて読み、呼び出しにする |
| `sig.rs` | プログラム、エラー行、ならし、シグネチャ、ユーザーが断ったか |
| `commands.rs` | `repeats`・`tool`・`daily` と、プリセット |
| `time.rs` | ISO 8601 の読み書き、地方時の日付、`14d` のような相対の指定 |

依存は `serde_json`、`regex-lite`、`libc`（地方時の差）だけ。10 月の半月分（約 1.3 GB、684 文脈、2.9 万回の呼び出し）を 1 秒前後で読む。行に `"tool_use` も `"tool_result` も含まれなければ JSON として読まずに飛ばすのが効いている。

## ほかに考えた形

- **Python のまま、rpatch の `tools/log-analysis/` を広げる。** 速さと、rpatch・devsrv と同じ置き方（`cargo install`）を取った。rpatch の分析は rpatch 専用の詳しさ（失敗の種類、書式の内訳）があるので、そのまま残す。
- **失敗の後を「同じファイルに触れたか」で判定する。** rpatch の誤りにはファイル名が出るが、道具によって出し方がちがう。今は「直後の別のやり方」を逃げとし、`-v` で人が確かめる。
- **シグネチャの似たものをまとめる（編集距離など）。** 今はならした文字列が同じものだけ。ならしで十分まとまっている。

## 未決と危険

- **逃げの誤判定.** 失敗の直後に、無関係のファイルを Write すると「逃げた」に数えられる。10 月の rpatch では 15 回のうち 3〜4 回がそうだった（`/tmp` 以外への新しいスクリプト）。
- **シグネチャの分かれ方.** プログラムの取り方で分かれることがある（`cat > x <<EOF && rpatch …` は `Bash cat`）。`tool` は正規表現で当てるので影響しない。
- **ログの形式.** Claude Code のログの形が変わると読めなくなる。`tool_use` / `tool_result` / `is_error` / `timestamp` / `Exit code N` に頼っている。Codex のログは読まない。
- **今のセッション.** `CLAUDE_CODE_SESSION_ID` があれば、パスにその ID を含むファイル（本体とサブエージェント）を外す。分析そのものの呼び出し（エラーの文字列を grep した、など）が、分析の結果に混ざるため。ターミナルから直接呼ぶと変数がないので、そのセッションの ID を `--exclude` で渡す。
