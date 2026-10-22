# agentlog

Claude Code のセッションログ（`~/.claude/projects/`）と Codex のセッションログ（`~/.codex/`）から、エージェントが同じ失敗をくり返しているところと、作った道具が使われているかを数える Rust 製 CLI。rpatch や devsrv のような道具を作る前に何を作るか決めるとき、作ったあとに使われているかを確かめるときに使う。振り返りのために、過去の会話を探して読むこと（`search`・`show`。Claude Code と Codex の両方を読める）と、あるリポジトリの「利用者の発言」と「コミット」を時刻順に1本に並べること（`timeline`）もできる。数える `repeats`・`tool`・`daily` は Claude Code だけ。ログではなくメモリのファイルを引く `memory` と、禁止ルールの本数を数えて並べる `rules` もある。入れてある MCP サーバー・スキル・CLAUDE.md で案内している CLI を、ログの中の使用回数と並べる `unused` は、見直しのときに「何を外せるか」の材料を出す（外す判断はしない）。

個人で作り、Claude Code と Codex の作業の振り返りに使っている。動くのは macOS と Linux（Windows なら WSL2）。ほかの自作の道具（[rpatch](https://github.com/lenalab-me/rpatch)・[devsrv](https://github.com/lenalab-me/devsrv)）やスキルとまとめて入れる手順は、[harness-engineering の SETUP.md](https://github.com/lenalab-me/harness-engineering/blob/main/SETUP.md) にある。

Rust 1.88 以上が要る。

読むログの場所は、Claude Code が `~/.claude/projects/`（`CLAUDE_CONFIG_DIR` があれば `$CLAUDE_CONFIG_DIR/projects/`）、Codex が `~/.codex/sessions/` と `~/.codex/archived_sessions/`（`CODEX_HOME` があればその下）。片方しかないマシンでも、ある方だけで動く。別の場所に写したログは `--root` と `--codex-root` で渡す。

```sh
git clone https://github.com/lenalab-me/agentlog && cd agentlog
cargo install --locked --path .
agentlog repeats --since 2026-10-01                 # 同じ失敗が 2 回以上あった文脈の多い順
agentlog tool rpatch --since 2026-10-01 -v          # 失敗のあと、次で直したか、別の手段に逃げたか
agentlog tool devsrv --since 2026-10-01             # 使い始める前と後の、別のやり方の回数
agentlog daily wt='isolated in the worktree' poll='until .*sleep' --since 2026-10-01
agentlog search 'too loose' -i --who user           # 前に何と言ったか（古い順。窓は既定で制限なし）
agentlog show 1a2b3c4d --at '2026-10-02 14:25'      # search の行の id と時刻で、その前後の会話を読む
agentlog search 'Planconfig' --source codex         # Codex のログだけ（--source claude|codex|all。既定は search が claude、show が all）
agentlog timeline --repo ~/project/prod/x --since 2026-10-01 --until 2026-10-03   # 発言とコミットを時刻順に（振り返り・引き継ぎ）
agentlog memory '関所' --type feedback              # メモリのファイルから探す（必要なときに引く。毎回読むものではない）
agentlog rules --repo . --count                     # この場所で始めたセッションが読む禁止ルールの本数（--json をモデルに渡して、ぶつかりを読ませる。置き場所の列と、同じ名前のメモリが別のプロジェクトにある印つき）
agentlog unused --since 30d                         # 入れてある MCP・スキル・案内している CLI と、窓の中の使用回数（0 回が上。どこを読んだかも出す）
# 別の場所に写したログは、--root（Claude）と --codex-root（Codex。sessions と archived_sessions の2か所）で渡す
agentlog search 'Planconfig' --raw                  # 控え（~/.cache/agentlog）を使わず、ログを直接読む（遅いが確実。控えが怪しいときの突き合わせ）
```

使い方の要約は `agentlog --help`（40 行以内。`head -40` で切られても要約は全部届く）。コマンドごとの詳しい説明は `agentlog help COMMAND`（`agentlog COMMAND --help` も同じ。`codex`・`cache` の節も引ける）、全文は `agentlog help all`。Claude Code の中から呼ぶと、そのセッション自身は数えない（`--include-self` で入れる）。

`search`・`show`・`timeline` は、ログから読み取った会話の本文を `~/.cache/agentlog/`（`$XDG_CACHE_HOME`）に控えとして置く。ログが増えても、2 回目からは増えた分だけを読むので速い（1 回目は控えを作るので、ログの量に応じてかかる）。控えは元のログから作り直せる写しで、消しても次の 1 回が遅くなるだけ。`--raw` で控えを使わずにログを直接読める（遅いが確実）。仕組みは `docs/design.md` の「控え」、怪しいときの確かめ方は `docs/recipes.md` の 4。

- [設計](docs/design.md): 文脈と呼び出し、失敗、シグネチャ、`repeats` と `tool` の数え方、プリセット、`search`・`show`・`timeline` の控え（キャッシュ）、未決と危険
- [使いどころ](docs/recipes.md): 次に作るものを探す、作った道具が使われているか確かめる、新しい道具を測る、振り返りで会話を探す、会話とコミットを並べて引き継ぎを書く、メモリを引く・禁止ルールを棚卸しする、使っていないものを洗い出す

## 数え方の決まり

この 3 つを外すと、結論を取り違える。

- **時刻は呼び出しごと。** ファイルの更新時刻で選ぶと、昔のセッションを再開しただけで昔の呼び出しが入る。ファイルの更新時刻は「窓より前に書き終えたファイルを読まない」ためだけに使う。
- **1 回の失敗は数えない。** rpatch のように、失敗したらすぐ直せる作りの道具では、1 回の失敗は設計どおり。数えるのは、続けて失敗したもの（`tool` の `fixed after N failures`）と、別の手段に逃げたもの（`went another way`。rpatch は失敗したファイルと同じファイルか、ファイルが分からないとき。別のファイルへの別のやり方は素通りする）、別々の文脈でくり返したもの（`repeats` の `rep`）。
- **道具ができた日より後で比べる。** できる前の手作業は、逃げではない。`tool` は窓の中で最初に使われた時刻を出し、その前と後の「別のやり方」の 1 日あたりの回数を並べる。案内を CLAUDE.md に足した時刻が分かっていれば、`--since` にそれを渡す。

サブエージェントは、まっさらな文脈から始まるので、親とは別の文脈として数える。同じ制約に、サブエージェントごとにぶつかり直しているのが見える（worktree の隔離など）。

## 出すもの、出さないもの

コマンドや結果には認証情報が入りうる。既定では件数と、パス・ID・数字をならしたエラー行だけを出す。コマンドの本文は `-v` / `--examples N` のときだけ。

`search` と `show` は会話の本文を出す（それが目的）。`timeline` は発言の先頭 100 文字とコミットの件名を出す。`show` はツールの呼び出しも `[Bash] コマンドの先頭行…` と1行で出すので、コマンドに秘密が入っていれば出る。結果は出さない。`memory` と `rules` はメモリと指示ファイルの行をそのまま出し、`unused` は入れてある MCP サーバー・スキル・プラグインの名前と置き場所を出す。出力を人に渡すときは確かめる。
