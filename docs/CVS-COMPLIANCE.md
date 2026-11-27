# CVS 1.11.23 客户端实现审计

日期：2026-11-27
对象：`cvsdesk` 内置的 `cvs` 客户端（`src/client/`）
只读审计，未改动任何代码。

## 基准

| 来源 | 用途 |
|---|---|
| CVSNT 1.11.1.3 build 57k（本机 `C:\Program Files (x86)\WinCvs\cvsnt\cvs.exe`） | 项目自述的对照目标；`--help-commands`、`-H <cmd>`、`--help-options` 的实际输出 |
| CVS 1.11.23 官方源码（ftp.gnu.org `stable/1.11.23`） | 每个命令的选项解析、usage 文本、客户端行为 |
| `doc/cvsclient.texi`（同版本） | 协议请求/响应、能力协商、参数编码 |
| 本仓 `src/client/cli.rs`、`engine.rs`、`commands/*.rs`、`protocol.rs`、`work/*.rs` | 被审计实现 |
| `tests/cvs_client.rs` | 覆盖情况 |

审计方式：先对照官方 `main.c` 的 `cmds[]` 表确认命令清单，再为 29 个命令各派一个子代理，
逐项比对"选项解析 → 请求发送 → 响应处理 → 工作副本写入 → 测试覆盖"。

## 一、总览

**命令名没有缺失。** 官方 1.11.23 的 28 个客户端命令全部在册；本客户端另有 `noop`，
它是 1.12/CVSNT 的协议扩展，1.11.23 与 CVSNT 1.11.1.3 都没有 `cvs noop` 这个用户命令。

| | 数量 |
|---|---|
| 命令总数 | 29（官方 28 + `noop`） |
| 选项层面基本完整 | 1（`history`） |
| 部分实现 | 27 |
| 完全缺失 | 0 |
| 逐命令记录的差距 | 318 项（P0 0 / P1 34 / P2 118 / P3 166） |
| 横向（全局选项、协议底座） | 另计 15 项，见第三节 |

**结论：不是"缺命令"，而是"命令都在、深度不够"。** 真正的风险集中在四处：

1. 全局 `-n`（dry-run）被解析后丢弃，多个命令会真的写盘/删文件——**唯一的 P0**。
2. Windows 上通配符不展开，`cvs add *.txt` 这类最常见的用法直接不工作。
3. 协议细节错位：`New-entry` 少读一行、`Patched` 不宣告、`Global_option` 覆盖不全、
   `Max-dotdot`/`Argument --` 缺失。
4. 工作副本判定大量只比 mtime 不比内容，导致 `tag -c`、`release`、`unedit` 误判。

## 二、P0：`-n` 不安全

`cli.rs:144` 把 `-n` 存进 `opts.no_execute`，但 `engine.rs:197-358` 只向命令透传
`readonly` 和 `compression`，全仓没有一处读 `no_execute`。与此同时 `--help-options`
（`cli.rs:704`）仍写着 "Do not execute anything that will change the disk"。

后果（均为实际代码路径，不是推测）：

| 命令 | 结果 | 证据 |
|---|---|---|
| `cvs -n release -d dir` | `remove_dir_all` 删除整个目录 | `release.rs:96-103` |
| `cvs -n remove -f f` | 删除文件并写入 `Entries -rev` | `remove.rs:87-89` |
| `cvs -n update` | 正常写文件、打补丁 | `update.rs:131-132` 只认命令自己的 `-n` |
| `cvs -n commit` | 真的提交 | `commit.rs:121-133` |
| `cvs -n edit` | chmod、写 `CVS/Base` | `edit.rs:148-152` |
| `cvs -n logout` | 删除 `.cvspass` 条目 | `engine.rs:238-241` |

官方在每个连服务器的命令前统一发 `Global_option -n`（`client.c:4503-4512`），并在本地
写入路径上检查 `noexec`。

**修法**：在 `engine` 里把 `no_execute`/`quiet` 传下去，或在 `Session::start` 统一发
`Global_option -n`；本地写文件处统一走一个 `if noexec { return }` 闸门。改动集中、风险低，
应作为第一优先。

## 三、横向问题

### 3.1 `-q`/`-Q` 同样被丢弃

`cli.rs:141-142` 解析后无人读取。只有命令名之后的 `-q`（如 `cvs status -q`）生效。
`cvs -Q release -d dir` 仍会打印报告并等待确认（`release.rs:80-87`），非交互环境下
stdin EOF 被当成 "no"，最终"中止但退出 0"。

### 3.2 Windows 通配符不展开（P1）

官方在 Windows 上用 `windows-NT/filesubr.c:860-950` 的 `expand_wild` 自己展开，因为 cmd
不做。`client.c:5406-5407` 的 `SEND_EXPAND_WILD` 覆盖 `add`、`admin`、`annotate`、`diff`、
`edit`、`log`、`remove`、`status`、`tag`、`update`、`watch`；`checkout`/`export` 走
`recurse.c:164`。

本仓全仓 grep `wild`/`glob` 无命中，`*.txt` 原样发给服务器。`cvs add *.txt`、
`cvs status *.c`、`cvs remove *.txt` 在 Windows 上都得到 `nothing known about`，且常常退出 0。

### 3.3 短选项不能聚合（P2）

官方用 getopt，`-AP`、`-nq`、`-lR`、`-aedit`、`-bd`、`-cl` 都合法。本客户端每个命令用
`match arg.as_str()` 精确匹配，聚合形式一律 `invalid option`：

| 位置 | 例子 |
|---|---|
| `cli.rs:170-175` | `-nq`、`-Qq`、`-fz9` |
| `checkout.rs:43-84` | `-AP`、`-dout` |
| `watch.rs:117-139` | `-lR`、`-aedit` |
| `rtag.rs:47-78` | `-bd` |
| `history.rs:133-179` | `-cl`、`-cw` |
| `update.rs:41-83` | `-Pd`、`-APd`（手册和 `.cvsrc` 的常见写法） |

影响 `.cvsrc` 与历史脚本，属于"看起来支持、实际一用就炸"。

### 3.4 空 `error` 一律当成功（P2，影响退出码）

`targets::read_text`（`targets.rs:575-581`）把 `Error { message: "" }` 当作正常收尾。
这段逻辑是为 CVSNT `diff` 的收尾写的，但被 18 处共用：`admin`、`diff`、`edit`、
`history`、`log`、`annotate`、`editors`、`watchers`、`rannotate`、`rdiff`、`release`、
`rlog`、`rtag`、`status`、`tag`、`unedit`、`watch`。

官方 1.11.23 服务器在子进程非零退出时正好发空 `error`（`server.c:3429-3433`），
所以 `cvs history`（仓库无 `history` 文件）、`cvs rlog nosuchmodule`、`cvs tag -d BRANCH`
（缺 `-B`）都会打印一行错误后**退出 0**。只有 `diff`/`rdiff` 应按空 `error` 正常收尾。

### 3.5 协议底座

| # | 严重度 | 差距 | 证据 |
|---|---|---|---|
| G1 | P1 | `New-entry` 少读一行。官方实际发三段：`New-entry <dir>/`、`<repo>/<file>`、`Entries 行`；`protocol.rs:380` 只 `extra_line()` 一次，第三行会残留成下一条响应 | `server.c:3559-3575,3820-3824`；`protocol.rs:380` |
| G2 | P1 | `Max-dotdot` 从不发送。带 `..` 的参数会被 1.11.23 服务器拒绝 | `status.rs`/`log.rs`/`remove.rs`/`tag.rs`/`update.rs` 均无 |
| G3 | P2 | `Patched` 能解析却没写进 `VALID_RESPONSES`（`protocol.rs:38-64` vs `:458`），`Rcs-diff` 又故意不宣告；对原生 1.11.23 服务器永远只能整文件传输 | `protocol.rs:38-64` |
| G4 | P2 | `Global_option` 只在 8 个命令里发 `-q/-Q`；官方统一发 `-n -q -Q -r -t`，本客户端从不发 `-r`、`-t` | `client.c:4501-4552` |
| G5 | P2 | `Argument --` 在多数命令里缺失，`-` 开头的文件名会被服务器 getopt 当成选项 | `remove.c:109`；`tag.c:229-231` |
| G6 | P3 | 不支持 `Argumentx`（超长参数续行） | 全仓无引用 |
| G7 | P3 | `Valid-responses` 未宣告 `Checksum`、`Update-types`、`Wrapper-rcsOption`、`MergeMod` | `protocol.rs:38-64` |

### 3.6 只读命令上传整份文件（P2）

官方对 `add`、`admin`、`annotate`、`diff`、`edit`、`log`、`tag`、`watch` 用
`SEND_NO_CONTENTS`，只发 `Is-modified`。本客户端只有 `send_file_state` 一个实现
（`targets.rs:660-702`）：一旦判定改动就 `Modified` + 长度 + 完整内容上传。

后果：只读查询也上传整文件；权限不足/被占用时命令直接失败；大文件明显变慢。
（`status` 是官方唯一明确不用 `SEND_NO_CONTENTS` 的命令，与本实现一致。）

### 3.7 全局选项表

| 官方选项 | 本客户端 | 证据 |
|---|---|---|
| `-d root` / `-droot` | 已实现 | `cli.rs:137-140,164-166` |
| `-q` / `-Q` | 解析后丢弃 | 见 3.1 |
| `-n` | 解析后丢弃 | 见第二节（P0） |
| `-r` / `-w` | 已实现 | `cli.rs:145-146`；`writer.rs:185-189` |
| `-t` | 仅 `login` 打印一行；协议跟踪靠 `CVSDESK_TRACE` | `engine.rs:233` |
| `-f` | 已实现 | `cli.rs:143` + `cvsrc.rs` |
| `-b` | **拒绝**（官方静默 no-op） | `cli.rs:170-175` vs `main.c:562` |
| `-e editor` | 接受但忽略（`commit.rs:10` 自述没有 `-e`） | `cli.rs:153-158` |
| `-s var=value` | 接受但忽略 | 同上 |
| `-T tmpdir` | 接受但忽略 | 同上 |
| `-x` / `-a` | 接受，空操作（与未编译加密的官方一致） | `cli.rs:159` |
| `-z n` / `-zn` | 已实现，含 `Gzip-stream` 协商与降级告警 | `protocol.rs:247-259` |
| `-H` / `--help*` / `-v` | 已实现 | `cli.rs:122-130` |
| `-l`（全局） | 接受（官方没有全局 `-l`） | `cli.rs:148` |
| `-D`（全局） | 接受并忽略（官方会报未知选项） | `cli.rs:160-163` |

## 四、逐命令矩阵

| 命令 | 结论 | P1 | P2 | P3 | 关键缺口 |
|---|---|---:|---:|---:|---|
| add | 部分实现 | 2 | 6 | 8 | `remove` 后不能 `add` 复活；Windows 通配符不展开；嵌套新目录的父块不发 |
| admin | 部分实现 | 0 | 4 | 10 | `-V1.1` 粘连写法误吞下一个参数；裸 `-t` 不读 stdin；GUI 命令行把 admin 映射成 status |
| annotate | 部分实现 | 1 | 2 | 6 | `-F` 缺失；`-D` 未归一化；只读却上传整文件 |
| checkout | 部分实现 | 3 | 5 | 8 | `-j`、`-p`、`-c/-s` 全缺；`-N` 语义反了；`-d out bar/baz` 落盘位置错 |
| commit | 部分实现 | 2 | 14 | 11 | 发送全部 `Entry`/`Unchanged` 而非仅改动文件；提交后 `Updated`/`Update-existing` 被丢弃 |
| diff | 部分实现 | 3 | 3 | 11 | 整组输出风格选项被拒；空 `error` 时退出码错 |
| edit | 部分实现 | 2 | 4 | 8 | Base 副本继承只读位；离线通知不留存；`-a` watch 顺序错 |
| editors | 部分实现 | 1 | 2 | 4 | 无文件名时应递归 `.`，现在直接退出 1；`-l/-R` 丢弃 |
| export | 部分实现 | 3 | 5 | 4 | 不强制 `-r/-D`；工作目录检查查错对象；`-n` 缺失 |
| history | 选项完整 | 0 | 2 | 8 | 选项与官方逐项一致；空 `error` 吞掉失败 |
| import | 部分实现 | 1 | 9 | 8 | 无 `-m` 不开编辑器；`.cvsignore` 错误继承到子目录；`-W` 丢弃导致二进制损坏 |
| init | 部分实现 | 1 | 1 | 2 | 本地根不能 init；init 前多发 `Root`，仓库不存在时请求到不了 `init` |
| log | 部分实现 | 2 | 7 | 6 | 裸 `-r` 被拒；`-wlogins` 粘连写法与 `-n` 缺失；`-d` 不归一化 |
| login | 部分实现 | 1 | 7 | 5 | CVSROOT 内嵌密码被丢弃；`.cvspass` key 不规范化；终端回显 |
| logout | 部分实现 | 0 | 4 | 5 | 删除用的 key 不规范，官方写的条目删不掉；CVSNT 注册表不清 |
| noop | 完整（扩展） | 0 | 2 | 3 | 1.11.23/CVSNT 没有该用户命令；多余参数不校验 |
| rannotate | 部分实现 | 1 | 2 | 5 | `-F` 缺失；空 `error` 退出 0 |
| rdiff | 部分实现 | 0 | 3 | 3 | 空 `error` 退出 0；`-D` 不校验；聚合选项被拒 |
| release | 部分实现 | 1 | 7 | 5 | `-Q` 被丢导致卡确认；`cvs -n release -d` 仍删目录；扫描只比 mtime |
| remove | 部分实现 | 0 | 5 | 7 | 通配符不展开；`-n` 下 `-f` 仍删文件；缺 `Argument --` |
| rlog | 部分实现 | 1 | 6 | 6 | 空 `error` 退出 0；`-r` 逗号列表被当单个 revision；缺 `-S/-n` |
| rtag | 部分实现 | 1 | 2 | 3 | `-B` 缺失；tag 名不校验 |
| status | 部分实现 | 3 | 6 | 10 | `-v` 丢弃；未纳管文件直接 abort；冲突文件编码错误 |
| tag | 部分实现 | 2 | 7 | 6 | `-c` 只比 mtime（touch 即误判）；`-B` 缺失 |
| unedit | 部分实现 | 1 | 3 | 5 | 未 watch 的工作区里把所有文件改只读并发假通知 |
| update | 部分实现 | 3 | 10 | 6 | `New-entry` 错位；`-p` 未实现；`-C` 无备份；`-P` 不裁本地空目录 |
| version | 部分实现 | 0 | 1 | 6 | `:local:` 根下直接失败；无 `version` 能力回退 |
| watch | 部分实现 | 0 | 2 | 3 | 不处理 `Mode` 响应，只读提醒失效 |

## 五、按主题归类的缺口

### A. 选项解析
- 短选项聚合（3.3）与可选参数：`log -r`（裸）、`-w[logins]`、`-r1.1` 之外的形式。
- 粘连参数解析 bug：`admin -V1.1` 误吞下一个参数（`admin.rs:136-139`）。
- `--` 之后的文件名未统一转义（缺 `Argument --`）。
- `update -p`、`checkout -p`、`checkout -j`、`export -n`、`status -v` 等直接缺失或丢弃。

### B. 协议
- `New-entry` 三段响应少读一段（G1）。
- `Patched`/`Rcs-diff` 组合（G3）。
- `Global_option` 覆盖不全（G4）。
- `Max-dotdot`（G2）、`Argument --`（G5）、`Argumentx`（G6）。

### C. 工作副本判定
- 只比 mtime：`tag -c`（`tag.rs:212-221`）、`release` 扫描（`release.rs:143-164`）、
  `unedit`（`unedit.rs:120-129`）。官方都比内容（`no_diff.c`、`classify.c`）。
- `commit` 发送全集而非改动集，且丢弃提交后的 `Updated`/`Update-existing`
  （`commit.rs:133,189`）。
- `Mode` 响应在 `commit`/`update`/`checkout` 里被忽略，watch 的只读提醒失效。
- 冲突编码恒为 `+=`，未修改的冲突文件还按 `Modified` 传内容（`targets.rs:679-682,732-736`）。
- `update -C` 不做 `.#file.rev` 备份；`-P` 只裁服务器目录不裁本地空目录。

### D. Windows 平台
- 通配符不展开（3.2）。
- `edit` 的 `CVS/Base` 继承只读位（`edit.rs:151-152`），后续 `commit`/`unedit` 删 Base 失败。
- `login` 用 `stdin().read_line`，不回显关闭、不读 `/dev/tty`（`cli.rs:218-232`）。
- `import -m`/`admin -t` 的文本按 UTF-8 有损解码，GBK 会坏。

### E. 认证
- `Root` 无 password 字段，CVSROOT 内嵌密码被丢弃（`root.rs:70-85`）。
- `.cvspass` 写入不做 normalize（默认端口/主机大小写/默认用户/尾斜杠），
  `logout` 也用原文 key（`engine.rs:240`），官方写的条目对不上。
- CVSNT 注册表读取但不清理（`transport.rs:295-296`）。

## 六、测试覆盖

- 多数 P1 行为没有测试；部分测试反而**固化了偏差**：
  - `tests/cvs_client.rs:413` 固化 `init` 先发 `Root`；
  - `:1670-1692` 固化 `tag -c` 只比 mtime；
  - `:1982` 固化 `rtag` 不发 `Argument --`；
  - `:3351` 固化 `export` 不带 tag 也能成功；
  - `:2665-2683` 固化 `unedit` 对无 Base 文件改只读。
- `status`、`log`、`commit` 的请求体基本没有断言。

修行为时必须同步改这些测试，否则会出现"测试全绿但行为仍错"。

## 七、建议修复顺序

1. **安全闸门**：`-n` 与 `-q/-Q` 透传（engine + 本地写入点）。1 处设计改动，消除唯一的 P0。
2. **Windows 通配符**：在 `targets` 入口统一展开，覆盖全部命令。
3. **协议正确性**：`New-entry` 两段附加行、`Patched` 宣告、`Global_option` 统一发送、
   `Max-dotdot`、`Argument --`。
4. **工作副本判定**：`commit` 只发改动集并处理 `Updated`/`Update-existing`；
   `tag -c`/`release`/`unedit` 改内容比较；`Mode` 处理；冲突 `+=`。
5. **逐命令补选项**：按第四节 P1 列表，优先 `checkout(-j,-p,-c/-s)`、
   `diff(输出风格)`、`status(-v)`、`log(-r/-w)`、`annotate/rannotate(-F)`、
   `tag/rtag(-B)`、`update(-p)`、`expand`(export 必须带 tag)。
6. **认证与文本编码**：passfile normalize + registry、getpass、GBK 文本。

## 附：明细报告

29 份逐命令明细（含每条差距的 `file:line` 证据）在审计工作目录：

```text
%TEMP%\opencode\cvs-audit\reports\<command>.md
```

横向结论在 `reports\_global.md`，汇总索引在 `reports\INDEX.md`。

## 八、修复记录（2026-11-27）

修复在独立 worktree 完成：分支 openchamber/cvs-compliance-fixes，未提交。

### 共享底座

- Root 新增 WireOptions（compression/no_execute/quiet/trace/readonly/editor），
  engine::resolve_root 统一填充；命令从 oot.options 读取。
- Session::start 统一发送 Global_option -n/-q/-Q/-r/-t；命令内重复发送已删除。
- 全局解析支持 -b（静默 no-op）、-e、-Qq/-fz9 聚合；移除官方没有的全局 -l、-D。
- 新增 work/glob.rs：Windows 通配符展开，接入 	argets::collect/collect_remove/collect_add。
- 	argets::begin_files：按官方顺序发 Argument -- → Max-dotdot；inish_arguments 只发名字与命令。
- 	argets::read_text 默认严格失败；ead_text_allow_empty_error 仅供 diff/diff。
- New-entry 解析三段；Patched 写入 VALID_RESPONSES；update 回写 New-entry。
- 协议防护：单行上限 1 MiB、单块内容上限 512 MiB，避免损坏流触发超大分配。

### 逐命令

29 个命令全部由独立代理按审计报告修复，包括：

- checkout/export：-j -p -c -s -n、-N 语义、-d 上线、export 强制 tag/date。
- commit：只发改动文件、处理 Updated/Update-existing、-f/-e/空消息/编码。
- update：-p/-C/-P/-d/-I/-W、New-entry、冲突与 merge 记录。
- status：-v 上线、未纳管文件不 abort、冲突 +=/+modified。
- diff：完整输出风格选项、0/1/2 rev、退出码。
- tag/rtag：-B、内容比较的 -c、标签名校验。
- annotate/rannotate：-F、日期归一化、Is-modified。
- edit/unedit：Base 只读位、离线通知队列、无 Base 跳过。
- add/remove：复活已删除文件、嵌套新目录、-n 保护。
- import：编辑器、.cvsignore 作用域、-W wrapper、MT 合并提示。
- log/rlog：裸 -r、-wlogins、逗号 rev、-n/-S。
- release：-Q 不提问、-n 不删目录、按内容扫描。
- login/logout：内嵌密码、规范化 .cvspass 键、CVSNT 注册表、无回显输入。
- admin/rdiff/history/init/watch/version/noop：各自 P1/P2/P3。

### 验证

`	ext
cargo fmt                         完成
cargo build --release             通过
cargo test                        473 单元 + 154 app + 118 集成，全绿
`

集成测试 _repository_query_needs_no_working_copy 会触发假服务器 12 GiB 分配，
已按要求删除；其余 69 个测试更新到新的官方线路。

### 仍未做

- :local: 本地仓库的创建与 RCS 操作（init 仍给出清晰错误）。
- proxy=、CVS_CLIENT_PORT、Kerberos/SSPI。
- 少量 P3：-H 输出流（本实现 stdout，官方 stderr）、admin 的 GUI 入口映射、
  部分 Is-modified 降级路径在 	argets.rs。
