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
agentlog daily --since 14d sleep='(^|[\s;&(])sleep\s+\d' loop='until\s[^\n]*sleep' --in input --json > baseline-$(date +%F).json
```

## 4. 振り返りで、前に何と言ったか・なぜそう決めたかを探す

数字ではなく会話を読む。`search` で探し、`show` で前後を読む。ツールの呼び出しではなく、人の発言と assistant の本文だけが対象。窓の既定は制限なし（古い会話も探せる）。

```sh
agentlog search '切り分けすぎ' --who user                 # 自分が何と言ったか（古い順、新しい50件）
agentlog search 'デプロイ|deploy' -i --since 30d         # どちらの言い方でも。30 日に絞る
agentlog search 'D3' --project web --who assistant
agentlog show 1a2b3c4d --at '2026-10-02 14:25' --around 3   # 上で見つけた行の id と時刻
agentlog show 1a2b3c4d --grep '切り分けすぎ' --around 5        # 時刻の代わりに、言葉で中心を決める
```

手順：

1. **言い方を変えて 2〜3 回探す。** 正規表現なので、`A|B` や `-i` が使える。一致が多すぎれば `--who user`（自分の言葉だけ）、`--project`、`--since` / `--until` で絞る。切れたとき（`N hits; the newest …`）は、古い方が落ちているので `--until` で区切って見る。
2. **行から決める。** 一覧は `時刻  誰  source  id  プロジェクト | 前後40文字`。`user` の行が「何と言ったか」、その直後の `assistant` が「どう受け取ったか」。決めた理由は、たいてい直前の assistant の提案（選択肢）と、それへの返事の組。
3. **`show` で前後を読む。** `show <id> --at '<時刻>'`（`search` の行そのまま）。中心の発言に `>>` が付く。`--around` を増やすと広がる。ツールの呼び出しは1行（`[Bash] …`）で、何をしていたかの流れは分かるが、結果は出ない。結果が要るときは、`daily` などで探す。
4. **サブエージェントの中は `親/子` で。** 一覧の id が `aaaaaaaa/bbbbbbbb` なら、それをそのまま `show` に渡す。`who` が `task` の行は、親が子に渡した依頼文（人の発言ではない）。T3 Code の依頼文も `task` に入る。

注意：

- `show` は、実行中のセッション（`CLAUDE_CODE_SESSION_ID`）も指定されれば読む。`search` は既定で実行中のセッションを外す（探している言葉が、探している最中の発言に出るため）。入れるには `--include-self`。
- 再開したセッションは、前の会話が写されて同じ発言が2つ出ることがある。時刻と中身が同じなら同じもの。
- 引用するときは、ログから抜き出した本物の発言（コマンドや出力を含む）を、公開するリポジトリの文書やコミットに残さない。認証情報や内部のホスト名が入りうるため（[harness-engineering](https://github.com/lenalab-me/harness-engineering) の決まり）。件数と、自分の言葉での要約にする。

### Codex の会話も探す

`show` は、既定で Claude Code と Codex の両方を読む（一覧の `claude` / `codex` の列で見分ける）。`search` と `timeline` は、頼んだときだけ Codex も読む（`--source codex|all` か `--codex-root` を付ける。付けないと Claude のログだけ）。片方だけなら `--source claude` / `--source codex`。使い方は同じだが、次の点がちがう。

```sh
agentlog search 'デプロイ|deploy' -i --source codex --since 2026-09-01   # Codex のログだけ
agentlog show 3c4d5e6f --grep 'デプロイ'                                  # Codex の id は末尾 8 文字
# 別の場所に写してあるログ（sessions と archived_sessions の2か所を渡す。Claude 側は --root）
agentlog search 'デプロイ' --source codex \
  --codex-root /path/to/copy/sessions \
  --codex-root /path/to/copy/archived_sessions
```

- **id は末尾 8 文字.** Codex の id は先頭が時刻なので、同じ時期のセッションは先頭が同じ。`show` は id の**どの部分でも**受け付ける。複数に当たれば候補（`id  source  時刻  プロジェクト`）が出る。
- **別の場所に写したログは、両方の置き場所を渡す.** 片方だけだと、アーカイブ済みのセッションか、まだアーカイブされていないセッションが抜ける。本物と写しの両方を渡しても、同じ id のセッションは 1 つだけ読む（大きいほう）。
- **`--project` は作業ディレクトリ名.** Codex にはフォルダ名がなく、セッションの `cwd` から作った名前（`shop/web` など。ホームと先頭の `project/prod/` を落とした形）に含まれるかで選ぶ。
- **全期間でも、2 回目からは速い.** 最初の `search` は控えを作るので、ログの量に応じて時間がかかる（`--raw`、つまり控えなしだと毎回その分かかる）。2 回目からは控えだけを読むので速い。測った数字は `docs/design.md` の「速さ」（2026-10-17 の測定）。控えの仕組みと確かめ方は下の「控えが怪しいとき」。
- **Codex の `user` は、アプリが足した文を除いてある.** 添付の一覧、ブラウザの状態、AGENTS.md、サブエージェントの通知など。見分け方は `docs/design.md` の「Codex のログ」。サブエージェントのセッションの依頼は `task`。

### 控えが怪しいとき

`search`・`show`・`timeline` は、ログから読み取った結果を `~/.cache/agentlog/`（`$XDG_CACHE_HOME/agentlog/`）に控えとして置き、2 回目からは増えた分だけを読む。普段は気にしなくてよい。次のときは、控えを疑う。

- 知っているはずの発言が出ない、または出るはずのない発言が出る。
- Claude Code や Codex を更新した直後（ログの形が変わったかもしれない）。
- `agentlog` 自身を作り直した直後は、読み方が変わったかもしれないが、その場合は控えが自分で作り直される（`docs/design.md` の「版と、上げ忘れの防ぎ方」）。

確かめ方：同じ問い合わせを控えなしで流して、突き合わせる。

```sh
agentlog search 'デプロイ' --who any --limit 100000 --json > /tmp/with-cache.json
agentlog search 'デプロイ' --who any --limit 100000 --json --raw > /tmp/raw.json
diff /tmp/with-cache.json /tmp/raw.json && echo same          # 同じなら控えは正しい
# 違うなら、--raw のほうが正しい。控えのフォルダを消せば、次の 1 回で作り直す
rm -rf ~/.cache/agentlog
```

- `--raw` は控えを読まず、書かない。`show`・`timeline` にも付く（`diff` の相手に使う）。
- 控えのフォルダは、消しても安全（元のログから作り直せる写し。作り直しにかかる時間はログの量で変わる。2026-10-17 の測定では、Claude 1.3 GB で 1 秒ほど、Codex 26 GB で 6〜8 秒）。ログのほうは消さない。
- ログを書き換えた・別の場所に置き直したときは、控えは使われずに作り直される（見分けが変わるため）。同じ場所で中身だけ書き換えて、大きさも更新時刻も変えなかったときだけは、控えを信じてしまう。
- 実行中のセッションのログは、今この瞬間に書いている最後の行を、次の実行まで待つ（改行まで書き終わっていない行は読まない。`--raw` でも同じ）。

## 5. 会話とコミットを並べて、振り返り・引き継ぎ・決定の記録を書く

「何を言って、何がコミットされたか」を `git log` と記憶で突き合わせる代わりに、`timeline` で 1 本にする。

```sh
agentlog timeline --repo ~/project/prod/x --since 2026-10-01 --until 2026-10-03   # 利用者の発言とコミット
agentlog timeline --repo ~/project/prod/x --since 2026-10-02 --who any            # assistant の本文も
agentlog show 1a2b3c4d --at '2026-10-02 14:25' --around 3                           # 気になる発言の前後を読む
git -C ~/project/prod/x show 1a2b3c4                                                # コミットの中身
```

手順：

1. **窓を区切る。** 引き継ぎなら前回の引き継ぎの日から、決定の記録ならその機能を始めた日から。`--since`・`--until` は日付か `14d`。窓の既定は 14 日。長いと行が多いので、1 週間ほどずつ読む。
2. **発言とコミットの並びで、節目を探す。** 利用者の発言（方針、選択肢への返事、「それはやめよう」）のあとに、どんなコミットが続いたか。方針の変更は、発言の直後のコミットの件名に出る。コミットのない長い発言の連なりは、議論だけで終わった（実装されていない）決定。
3. **決めた理由が要るところで `show`。** 行の id と時刻（`2026-10-02 14:25`）をそのまま `show` に渡すと、前後の会話が読める。assistant が出した選択肢と、それへの利用者の返事が、理由になる。
4. **コミットは `git show` で確かめる。** 件名が実際の変更を言っているかは、中身を見る。
5. **書くときは、本物の発言と件名を写さない。** 日付と、自分の言葉での要約にする（ログから抜き出したものを公開するリポジトリに残さない、[harness-engineering](https://github.com/lenalab-me/harness-engineering) の決まり）。

どのセッションが並ぶかは `--repo` から決まる：リポジトリの中（と、その作業ツリー、T3 Code が作って消した作業ツリー）で主に作業したセッション。`~/project/prod/shop` のような親のフォルダで開いたセッションも、作業が `shop/web` の側にあれば入る（親のフォルダにいた行は数えない）。別のリポジトリにまたがるセッションは、作業の半分以上がこちら側なら全部入る：その日に絞るか、`--project` で名前から選ぶ。リポジトリの外（スクラッチなど）でそのリポジトリの話をした会話は入らないので、`agentlog search` で足す。

見方：

- **id が空白の行.** 1 つ上の行と同じセッション。コミットを挟むと、また id が出る。
- **`user` の行が少ない日.** サブエージェントや T3 の子スレッドに任せた日。`--who any` で、親が渡した依頼（`task`）が出る。
- **同じ発言が 2 つ.** 再開したセッションの複製。時刻と中身が同じなら同じもの。
- **コミットの時刻.** 作者時刻なので、rebase してから main に入れたコミットも、作った時刻に出る。

## 6. メモリを引く、禁止ルールを棚卸しする

メモリは毎回読むものではなく、必要なときに引く。禁止が増えると互いにぶつかることがあるので、棚卸しもできる。

```sh
agentlog memory '関所' --type feedback             # 「なぜそうしているか」を、メモリの本文から
agentlog memory 'worktree' -i --project web     # そのプロジェクトのメモリだけ
agentlog memory '禁止' --index                       # 索引（MEMORY.md）も含める
agentlog rules --repo . --count                      # このディレクトリで始めたセッションが読む禁止の本数と、ファイルごとの内訳
agentlog rules --repo . --json                       # 1 行 1 件。モデルに渡して、互いにぶつかる禁止がないか読ませる
agentlog rules --repo . --source all --count         # Codex の AGENTS.md とメモリも足す
```

読み方：

1. **本数は材料であって、答えではない.** 判定の式は loose で、禁止を説明しただけの行も当たる。多いファイルから、実際の行（`file:行  本文`）を読む。
2. **ぶつかるかは読んで決める.** 機械はぶつかりを推測しない。`--json` をモデルに渡すときは、「互いに両立しない禁止の組を、2 つの行を引用して挙げて。当てはまらなければ『なし』」と頼む。モデルが挙げた組は、元のファイルの行（`ファイル:行`）で確かめる。
3. **式を締めたいとき.** 偽陽性が多ければ `--pattern` で絞る（例：`--pattern 'しない。$|禁止'`）。
4. **メモリに顧客の内容は入れない前提.** ふつうのメモリは、利用者の好みや決めたことだけ。顧客の情報が入りうる環境なら、`--json` を外部のモデルに渡す前に中身を見る。

## 7. 数字を人に渡すとき

- 窓（`--since` / `--until`）と、外したセッションを書く。
- `rep`・`ctx`・`total` のどれを言っているかを書く。`total` は 1 つの文脈の連打で膨らむ。
- 「失敗の後」は連なりの数で、呼び出しの数ではない。
- `--json` の出力をそのまま残すと、後で同じ窓で比べられる。
