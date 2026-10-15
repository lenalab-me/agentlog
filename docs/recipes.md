# agentlog の使いどころ

道具を作る前と作ったあとに、何を見てどう読むか。数え方の定義は `docs/design.md`。

Claude Code の中から呼ぶと、そのセッション（`CLAUDE_CODE_SESSION_ID`）とそのサブエージェントは自動で外れる（分析そのものの呼び出しを数えないため）。入れたいときは `--include-self`。ほかに外したいセッションがあれば `--exclude <ID の一部>`。

## 1. 次に何を作るか探す

```sh
agentlog repeats --since 14d
```

上から、`rep`（同じ失敗が 2 回以上あった文脈の数）の多い順に並ぶ。見るところは次の 3 つ。

- **`rep` が多く、`days` も多い.** 慢性。道具かガードで減らせる候補。
- **`rep` が多いのに `days` が 1〜2.** ある日の出来事（並べて走らせたサブエージェントがまとめてレート制限に当たった、など）。道具を作る前に `daily` で確かめる。
- **`streak` が 1.** 同じ文脈で起きても、毎回すぐ直している。rpatch の `"old" not found` がこれで、設計どおり。

気になる行は `-v` で実際のコマンドを見る（認証情報が出うるので、人に見せる前に確かめる）。

```sh
agentlog repeats --since 14d --tool Bash --examples 5
```

`days` が少ないものは、日ごとに並べる。hook や道具を入れた日を境に減っていれば、もう片付いている。

```sh
agentlog daily wt='isolated in the worktree' edit_hook='Blocked by a hook' rate='Too many requests' --since 2026-10-01
```

## 2. 作った道具が効いているか確かめる

CLAUDE.md に案内を足した時刻か、最初に使った時刻から後を見る。それより前の手作業は、逃げではない。

```sh
agentlog tool devsrv --since 2026-10-01          # 前と後を並べたいので、窓は入れる前から
agentlog tool rpatch --since 2026-10-06T23:57Z -v
```

- **`first use in the window`** と **`other ways per day: X before … Y after`.** 前より後が大きく減っていれば、置き換わっている。前が 1 日に満たないと、この行は出ない（窓を前に広げる）。
- **`after a failure`.** `fixed on the next call` が大半なら、失敗の案内が効いている。数えるべきは `fixed after N failures in a row`（同じ道具で続けて失敗）と `went another way`（別の手段に逃げた）。
- **`went to`.** どこへ逃げたか。`-v` で、失敗の行と逃げた先のコマンドを見る。新しいスクリプトを `/tmp` 以外に書いただけのもの（逃げではない）が混じる。
- **`other ways` の内訳.** 道具ができた後にも残っている別のやり方。多いものが、案内か hook で塞ぐ候補。

日ごとの表の `<- first use` の前後で、`other` の列が減っているかも見る。

## 3. 新しい道具を測る

プリセットがない道具は、正規表現で渡す。

```sh
# Bash から呼ぶ CLI
agentlog tool waitfor --uses '(^|[\s;&|(])waitfor\s' --fail '(?m)^waitfor: ' \
  --alt '(^|[\s;&(])until\s[^\n]*sleep|(^|[\s;&(])sleep\s+\d' --since 2026-10-20

# MCP のツールや組み込みのツールは、名前だけで
agentlog tool mcp__aws-knowledge__aws___search_documentation --since 2026-10-01
agentlog tool Edit --since 2026-10-01
```

- `--fail` を付ける。付けないと、同じコマンドに `&&` で連ねた後ろのコマンドの失敗まで、道具の失敗に数える。
- 新しいファイルを作るのにも使う手段（Write、`cat >`）は、`--alt` ではなく `--escape` / `--escape-tool` に入れる。失敗の直後だけ数える。
- よく測るようになったら、`src/commands.rs` の `preset` に足す（`PRESETS` と `help.txt` の Presets も）。結合テスト（`tests/cli.rs`）に、使った・失敗・別のやり方・逃げの最小の例を 1 つ足す。

作る前の基準を残しておくと、後で比べやすい。

```sh
agentlog daily --since 14d sleep='(^|[\s;&(])sleep\s+\d' loop='until\s[^\n]*sleep' --in input --json > ~/project/prod/<道具>-notes/baseline-$(date +%F).json
```

## 4. 数字を人に渡すとき

- 窓（`--since` / `--until`）と、外したセッションを書く。
- `rep`・`ctx`・`total` のどれを言っているかを書く。`total` は 1 つの文脈の連打で膨らむ。
- 「失敗の後」は連なりの数で、呼び出しの数ではない。
- `--json` の出力をそのまま残すと、後で同じ窓で比べられる。
