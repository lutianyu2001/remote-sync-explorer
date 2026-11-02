# remote-sync-explorer

<p align="right">
    <a href="../README.md">English</a> | <strong>中文</strong>
</p>

[![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![CI](https://github.com/lutianyu2001/remote-sync-explorer/actions/workflows/ci.yml/badge.svg)](https://github.com/lutianyu2001/remote-sync-explorer/actions/workflows/ci.yml)
[![Advisories](https://github.com/lutianyu2001/remote-sync-explorer/actions/workflows/audit.yml/badge.svg)](https://github.com/lutianyu2001/remote-sync-explorer/actions/workflows/audit.yml)

remote-sync-explorer（命令为 `rse`）是一个双栏终端文件管理器，可以浏览本地磁盘、远程服务器和任何 [rclone](https://rclone.org) 远程。

## 目录

- [概述](#概述)
- [功能特性](#功能特性)
- [安全审计](#安全审计)
- [前置要求](#前置要求)
- [安装](#安装)
- [用法](#用法)
- [配置](#配置)
- [语言](#语言)
- [通过 OpenSSH 使用 SFTP](#通过-openssh-使用-sftp)
- [测试](#测试)
- [兼容性](#兼容性)
- [免责声明](#免责声明)
- [许可证](#许可证)
- [作者](#作者)

## 概述

每个窗格显示一个位置，可以在两个窗格之间复制、移动、删除、重命名和新建目录，传输队列会实时显示进度。rclone 以静态库的形式编译进程序，所以发布版只有一个名为 `rse` 的二进制文件。

```text
╭───────╮╭────────────[1] Address─╮╭───╮╭────────╮╭───────────[3] Address─╮╭───╮
│ Local ││ /home/sky/projects     ││ ↻ ││ smb:// ││ nas:backup/2026       ││ ↻ │
╰───────╯╰────────────────────────╯╰───╯╰────────╯╰───────────────────────╯╰───╯
╭────────────────────────────[2] Local─╮╭───────────────────────────[4] Remote─╮
│ ..                                   ││ ..                                   │
│ rse/                           <DIR>││ photos/                         <DIR>│
│*huge.bin                       1.5 GB││ notes.md                       4.1 KB│
│*report-final-v3.pdf            2.4 MB││ report-final-v2.pdf            2.3 MB│
│ very-long-file-n…ing.tar.gz     12 MB││                                      │
╰───────────────────────────[2 marked]─╯╰──────────────────────────────────────╯
╭────────────────────────────────────────────────────────────────[5] Transfers─╮
│ 1 running · 1 queued                                                         │
│   copy huge.bin → nas:backup/2026       ██████░░░░░░  48%    52 MB/s   0:16 ✕│
│   copy report-final-v3.pdf → nas:backup/2026                         queued ✕│
╰──────────────────────────────────────────────────────────────────────────────╯
 F5 Copy  F6 Move  F7 Mkdir  F8 Delete  F10 Remote  / Filter  ? Help  q Quit
```

## 功能特性

- **远程**：浏览本地磁盘和 `rclone.conf` 里的所有远程，不需要往 rclone 配置里额外添加任何东西。
- **按地址打开服务器**：`sftp://`、scp 风格的 `me@box:/path`、`ftp://`、`smb://` 和 `webdav://` 无需在 `rclone.conf` 里配置远程即可打开。
- **文件操作**：复制、移动、删除、重命名、新建目录。移动和删除总会先确认，拖放也一样。覆盖已有文件前会询问，除非开启了覆写模式：加入队列时和每个传输开始前都会检查目标，两个传输也不会同时写同一个文件。
- **传输队列**：多个任务并发执行，显示已传字节、速度和剩余时间，可以逐个取消或重试。有任务在运行时退出会先询问，并干净地停止它们。
- **鼠标**：单击、双击、滚轮，Ctrl+单击或右键标记，拖动来复制或移动。
- **通过 OpenSSH 使用 SFTP**：`~/.ssh/config`（ProxyJump、证书、硬件密钥）照常生效，每台服务器只登录一次。
- **语言**：英文和简体中文，跟随系统语言。

## 安全审计

rse 经手密码、ssh 登录和远端服务器上的文件，所以它的代码做过安全审计。审计是项目维护者自己做的，不是独立第三方的审计：它让人对代码更有把握，但不是保证。审计不是做一次就完，而是反复跑：最近一次的结果和代码一起提交在 [`audit/`](../audit/)，每次 push 之后 CI 会重跑结果已经失效的部分。审计的每一项是什么、怎么跑，见 [SECURITY.md](../SECURITY.md)；依赖见 [SUPPLY_CHAIN.md](../SUPPLY_CHAIN.md)。下面的数字来自 2026-11-02 的运行。

| 方面 | 审计了什么 | 范围 |
|---|---|---|
| 威胁模型 | 外来输入或机密进入 rse 的每一处都人工审查过，每个决定都写了下来 | SECURITY.md 里的 15 条决定：密码交给谁、主机密钥（包括退回 rclone 内置 ssh 之后）、服务器能让 ssh 做什么、混进 ssh 的选项、`--rcd` 走明文 http、文件权限、服务器给的文件名和文字怎么显示、覆盖文件 |
| `unsafe` 代码 | rse 禁止 `unsafe`，只有点名允许的函数例外 | 8 处，每处都是没有安全写法的调用（librclone、`setlocale`、`pre_exec`），旁边写着为什么是安全的 |
| 依赖 | 来源、许可证、已知漏洞、作者放弃的和被撤回的 crate（cargo-deny）；每个 crate 都有审计记录（cargo-vet）；从校验过的源码离线构建 | 216 个 crate 审计过：从 Mozilla、Google 和 Bytecode Alliance 导入的审计，加上我们自己的 188 条；没有豁免。漏洞每天重新查 |
| 测试 | 单元测试、界面快照测试和集成测试，包括对私有 `sshd` 的 ssh 登录 | 232 个测试，覆盖 93% 的代码行和 80% 的分支 |
| Miri | 不用进程、socket 和网络的模块的测试，在能发现未定义行为的 Miri 下运行 | 11 个模块的 62 个测试 |
| Fuzz | 给每个读取不可信文本的函数喂随机输入，按 SECURITY.md 里的不变式检查 | 5 个 target（地址、ssh 选项、屏幕上显示的文字、配置文件、密码），每个一小时：每个 target 试了 100 万到 1.24 亿个输入 |
| Kani 证明 | 在界限以内对所有输入证明的性质 | 鼠标点击定位不会溢出，对所有输入（1 秒，95 MiB）；按键名不会从字符中间切开，最多 7 字节（31 秒，1009 MiB） |
| 发布构建 | `overflow-checks = true` | 整数溢出时 rse 停下，而不是回绕 |

这些说明不了的：

- Rust 排除的是 rse 自己代码里的内存错误，不排除逻辑错误；逻辑错误靠审查和测试去找。
- 默认构建的 123 个 crate 里有 56 个用了 `unsafe` 代码。这些代码靠的是它们的作者和上面的审计，不在 rse 的测试、Miri 和 Kani 的范围内。
- 编译进 rse 的 rclone 是 Go 代码，上面的审计都不覆盖它；Go 下载模块时会按 `go.sum` 校验。
- Kani 只在界限以内证明，fuzz 只是抽样。没有证明的：混淆密码的解码，以及地址、ssh 选项、连接字符串和配置文件的读取；它们由所有短输入的测试和 fuzz 检查。

## 前置要求

- Linux，或 macOS 13 及以上（Intel 或 Apple Silicon）；不支持 Windows
- Rust ≥1.88
- Go ≥1.26（rclone v1.75 的要求），需在 `PATH` 上，或用环境变量 `GO` 指定
- cgo 需要的 C 工具链（`cc`）；**不**需要 libclang
- 首次构建时能访问 Go 模块代理
- OpenSSH ≥8.4，用于通过系统 `ssh` 询问密码和一次性验证码（可选）

## 安装

### 从 crates.io 安装（推荐）

1. 安装 Rust、Go 和 C 工具链（见[前置要求](#前置要求)）。
2. 安装 `rse`：

   ```bash
   cargo install --locked remote-sync-explorer
   ```

首次构建会编译 rclone，需要一分钟以上。因为包含了整个 rclone，二进制文件约 90–115 MB。构建时会用 `librclone.h` 核对手写的绑定，rclone 导出的四个函数一旦变化，构建就会失败；在 glibc 上，rse 手写声明的那个 libintl 函数也会这样核对。

### 从源码构建

```bash
cargo --config net.offline=false vendor --locked --versioned-dirs --sync fuzz/Cargo.toml vendor
cargo build --release --locked
```

构建时的环境变量（两种方式都适用）：

| 变量 | 作用 |
|----------|--------|
| `RSE_RCLONE_VERSION` | 换用其他 rclone 版本构建（默认是固定的 `v1.75.2`） |
| `RSE_LIBRCLONE_DIR` | 存放预先构建好的 `librclone.a` 和 `librclone.h` 的目录；完全跳过 Go 构建（离线构建、打包） |
| `GO` | 使用的 Go 程序 |
| `GOTOOLCHAIN` | 原样传给 Go；未设置时 rse 将其设为 `local`，这样 Go 版本过旧时会直接报错，而不是去下载工具链 |
| `MACOSX_DEPLOYMENT_TARGET` | 构建产物能运行的最低 macOS 版本；`.cargo/config.toml` 设为 `13.0`，即内置 Go 运行时支持的最低版本 |
| `CC_<target>` / `TARGET_CC` | cgo 为目标平台使用的 C 编译器，例如 `CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc` |

注：发布的二进制文件包含 rclone（MIT）及其 Go 依赖，必须随附它们的协议文本。`scripts/third-party-licenses.sh` 会收集 Rust 和 Go 依赖的协议文本（需要 `cargo-about` 和 `go-licenses`）。

### 交叉编译

`GOOS` 和 `GOARCH` 跟随 cargo 的目标平台，所以 librclone 会和 rse 一起交叉编译：

```bash
rustup target add aarch64-apple-darwin
cargo build --release --locked --target aarch64-apple-darwin
# → target/aarch64-apple-darwin/release/rse
```

在 macOS 的两种架构之间不需要别的设置：cgo 会使用 `clang -arch <arch>`。编译到其他 Linux 架构时，cgo 使用 `<arch>-linux-gnu-gcc`，除非用 `CC_<target>` 另行指定，rustc 也需要用同一个编译器作为链接器（`CARGO_TARGET_<TRIPLE>_LINKER`）。两个 macOS 构建可以合并成一个通用二进制文件：

```bash
lipo -create -output rse <x86_64 rse> <arm64 rse>
```

### 不用 Go：`rcd` 特性

这种构建不内置 rclone，而是连接你自己运行的 `rclone rcd`：

```bash
cargo build --release --locked --no-default-features --features rcd
rclone rcd --rc-user <user> --rc-pass <password> &
RCLONE_RC_PASS=<password> rse --rcd http://localhost:5572 --rc-user <user>
```

明文 `http://` 只允许连接本机（`localhost`、`127.0.0.1`、`[::1]`）；其他机器上的 rcd 需要 `https://`（`rclone rcd --rc-cert … --rc-key …`）。

## 用法

```text
rse [--config <PATH>] [--rclone-config <PATH>] [--ask-password]
     [--forget-password <LOGIN>] [--rcd <URL> --rc-user <USER> --rc-pass <PASS>]
     [--version] [-h] [LEFT] [RIGHT]
```

- `LEFT`
    - 左窗格的初始位置，例如 `~/work`
- `RIGHT`
    - 右窗格的初始位置，例如 `nas:backup`。只给一个位置时，远程会在这里（而不是左边）打开
- `--config <PATH>`（默认：`~/.config/rse/config.toml`）
    - remote-sync-explorer 的配置文件
- `--rclone-config <PATH>`
    - rclone 的配置文件
- `--ask-password`
    - 启动前询问 rclone 配置的密码
- `--forget-password <LOGIN>`
    - 忘记为 `LOGIN`（`user@host`）记住的 ssh 密码，然后退出
- `--rcd <URL>`
    - 使用外部的 `rclone rcd`，而不是内置的 rclone（需要 `rcd` 特性）
- `--rc-user <USER>`、`--rc-pass <PASS>`
    - `--rcd` 的用户名和密码；未给出时读取 `RCLONE_RC_USER` 和 `RCLONE_RC_PASS`，这样密码不会出现在 `ps` 里
- `--version`
    - 显示 remote-sync-explorer 和 rclone 的版本
- `-h, --help`
    - 显示帮助

加密的 rclone 配置需要在 rse 启动前提供密码：使用 `--ask-password`，或在启动 rse 的 shell 里导出 `RCLONE_CONFIG_PASS`。

### 位置与地址

位置可以是本地路径（`~/x`、`/abs`、`relative`），也可以是 rclone 路径（`nas:`、`nas:backup/2026`、`sftp:/absolute/path`）。远程照常用 `rclone config` 管理。

名字按实际内容显示：显示不出来的字符（例如控制字符、零宽空格、会改变文字顺序的方向控制符）显示为 `�`。服务器列出的条目如果名字可能指向目录之外（`..`，或含 `/`），不会显示，状态栏会说明跳过了几个。

只给一个位置时，远程或服务器地址会在右窗格打开，左窗格显示配置里的 `left`（默认 `~`）：`rse sftp://me@box` 会把本地磁盘放在左边，`box` 放在右边。只给一个本地路径时，它在左边打开。

服务器也可以直接按地址打开，无需在 `rclone.conf` 里配置远程：

| 地址 | 打开方式 |
|---------|-------|
| `sftp://me@box/srv`，或 scp 风格的 `me@box:/srv` | 通过系统 `ssh` 使用 sftp（见[通过 OpenSSH 使用 SFTP](#通过-openssh-使用-sftp)） |
| `ftp://me@box/pub`、`ftps://…` | FTP；`ftps` 是 990 端口上的隐式 TLS |
| `smb://me@nas/share/dir` | SMB；第一个名字是共享名 |
| `webdav://host/path`、`webdavs://…` | 基于 http 或 https 的 WebDAV（也支持 `dav://`、`davs://`） |

端口写在主机名后面（`sftp://me@box:2222/srv`）。sftp 路径是绝对路径；`sftp://me@box/~/docs` 或不写路径表示家目录，scp 风格的 `me@box:docs` 也一样。不写用户名时，FTP 以 `anonymous` 登录。

登录需要密码而地址里没有时，rse 会询问（命令行给出的地址在启动前于终端询问），并把密码保存在内存里直到退出；列目录失败会忘掉它，下次重新询问。`ftp://me:secret@box` 也能用，但密码会留在 shell 历史里。

其他任何 rclone 后端都可以用连接字符串打开，例如 `:s3,provider=AWS,env_auth=true:bucket/path`。含有 `:` 或 `,` 的值用 `'` 或 `"` 括起来，如 `:sftp,ssh='ssh -p 2222 me@box':`。

### 地址栏

每个窗格上方有一行三个框：协议（`sftp://`、`smb://`、`s3://`……，本地磁盘显示 `Local`）、地址栏和一个按钮：`↻` 重新加载窗格。只有地址栏能获得焦点（按键 `1` 或 `3`），旁边的框会随它一起高亮，点击协议框没有反应。路径太长放不下时，地址栏只在 `«` 后显示当前目录，和 Windows Explorer 一样（`« photos`）；编辑时显示完整路径。

在获得焦点的地址栏按 Enter、在窗格里按 `:`，或点击地址栏，都会开始编辑地址：地址栏下方打开一个窗口，有箭头从地址栏指向它，同时地址栏显示 `[Revising]`（终端高度不够放下这个窗口时，直接在地址栏里编辑）。地址太长时输入框会滚动，两端高亮的 `<` 或 `>` 表示那一侧有文字被截断，与 readline 和 nano 相同。地址改动后按钮变成 `>`，点击即跳转（和 Enter 一样；地址未改动时 Enter 会重新加载）。Esc 或点击别处取消。编辑时协议框会跟随输入：输入 `ftp://` 会把它移进协议框，在输入框开头按 Backspace 会把它取回，Ctrl+U 清空输入框和协议。

已标记的条目数显示在窗格右下角（`[2 marked]`）。按 Esc 或点击窗格空白处清除标记。

### 按键

所有按键都可以在配置文件里修改。F 键有字母别名，方便没有 F 键的键盘。

| 按键 | 操作 |
|-----|--------|
| Tab | 切换窗格 |
| 1 / 2 / 3 / 4 / 5 | 聚焦窗口：左地址栏 / 左窗格 / 右地址栏 / 右窗格 / Transfers（按键显示在各窗口的右上角；窗格按显示的内容命名为 Local 或 Remote） |
| ↑ ↓ / j k、PgUp PgDn、Home End | 移动光标 |
| Enter / l | 打开目录 |
| Backspace / h | 上级目录（光标落在刚离开的目录上） |
| Space / Insert | 标记并下移 |
| Esc | 清除标记；在地址栏中则回到条目列表 |
| a | 全部标记 / 全部取消 |
| / | 按名称过滤（Enter 保留，Esc 清除） |
| s | 按名称、大小、时间排序 |
| . | 显示或隐藏点文件 |
| : | 跳转到路径、远程或地址，例如 `nas:backup`、`ftp://me@box/pub` |
| F10 / g | 选择远程 |
| Ctrl+U | 交换窗格 |
| Ctrl+R | 重新加载窗格 |
| F5 / c | 复制到另一个窗格 |
| F6 / m | 移动到另一个窗格 |
| F2 / r | 重命名 |
| F7 / n | 新建目录 |
| F8 / Delete | 删除 |
| O | 开关覆写模式：复制、移动和重命名直接覆盖已有文件，不再询问（每次启动时都是关闭的；开启时屏幕顶部有一行红色提示） |
| Ctrl+T | 进入或离开传输队列；在队列里 x 取消，r 重试，c 清除已完成的任务 |
| E | 错误日志（最近 100 条） |
| ? | 帮助 |
| q | 退出 |

复制和移动作用于已标记的条目；没有标记时作用于光标所在的条目。目标是另一个窗格的目录。

### 鼠标

鼠标支持默认开启（`[ui] mouse = true`）。每个鼠标操作都有对应的按键。

| 操作 | 效果 |
|--------|--------|
| 单击 | 把光标移到该处，清除标记，聚焦该窗格 |
| Ctrl+单击或右键 | 标记或取消标记，保留其他标记 |
| 双击 | 打开目录；在 `..` 上则返回上级 |
| 滚轮 | 滚动指针下的窗格（焦点不变） |
| 单击窗格空白处 | 清除标记，聚焦该窗格 |
| 单击地址栏 | 编辑地址 |
| 单击地址右侧的 ↻ / > 按钮 | 重新加载该窗格 / 跳转到编辑后的地址 |
| 拖动 | 复制或移动（见下文） |
| 单击队列中的一行 / 它的 ✕ | 选中该任务 / 取消它 |
| 单击底部的按键提示 | 执行它 |

拖动：在条目上按下并移动。如果该条目已被标记，就拖动所有已标记的条目，否则只拖动它。放到目录上会把条目放进该目录，放到窗格的其他地方则放进该窗格的目录。拖到顶部或底部边缘附近会滚动。Esc 取消。松开时会弹出菜单，提供 *Copy here*、*Move here* 和 *Cancel*；之后的冲突检查和确认与 F5/F6 相同。设置 `drop_action = "copy"` 可以不弹菜单直接复制。

注：开启鼠标支持时，在大多数终端里按住 Shift 才能选中文字。在 tmux 里请使用 `set -g mouse on`。在 macOS 上，有些终端会把 Ctrl+单击变成右键（同样会标记），或者弹出自己的菜单；设置 `multi_select_modifier = "alt"` 可以改用 Alt+单击。

### 冲突、移动与取消

- 目标已有同名条目时，rse 会询问：覆盖、跳过、全部覆盖、全部跳过或取消。默认是跳过，且只有大小写不同的名字视为相同（很多目标，例如 macOS 磁盘和网盘，不区分大小写）。检查时总会重新列出目标，即使某个窗格正显示着它，所以之后才出现在那里的文件不会在不询问的情况下被覆盖。
- 每个任务开始时和重试时都会再列出一次目标，所以任务等待期间才出现的文件也不会被覆盖。列出目标时看不到正在进行的传输，所以同一目录中另有任务要写入、移走或删除同名（不区分大小写）的条目，且它正在进行或排在前面时，这个任务会等它结束：两个任务不会同时写同一个文件，后一个也能看到前一个写入的内容。发现名字已被占用的任务不会开始：它在队列和错误日志里显示为失败，并弹出对话框询问：覆盖、跳过，对所有因冲突等待的任务全部覆盖、全部跳过，以及取消（所有还没决定的任务都保持失败，重试其中一个时会再检查一次）。加入队列时已选择覆盖的条目不会再问第二次。在最后一次检查和传输开始之间由别的程序放进目标的文件，仍会被覆盖。
- 重命名时会向服务器核对新名字，而不只是看窗格里的列表；已有别的条目用了这个名字（不区分大小写）时会拒绝。
- 覆写模式（`O`）跳过以上检查：复制、移动和重命名直接覆盖目标中已有的内容，不再询问，但任务之间仍像上面那样互相等待。每次启动时它都是关闭的；开启期间，屏幕顶部有一行红色提示，复制和移动的确认对话框里也会再提醒一次。
- 复制目录时会合并到已有的同名目录中：相同的文件跳过，不同的文件覆盖。
- 跨远程移动是先复制再删除源文件。中途取消时，已经复制过去的文件在源端已被删除。移动目录后，rse 会删除清空了的源目录。
- 取消传输会停止 rclone 任务；rclone 会删除写了一半的 `.partial` 文件，所以目标端不会留下损坏的文件。
- 在 macOS 上，同一 APFS 卷内复制的文件会被 rclone 克隆，而不是逐字节复制，所以这类复制会立即完成。

## 配置

`~/.config/rse/config.toml`（或 `$XDG_CONFIG_HOME/rse/config.toml`）。这个文件是可选的，缺少的设置使用下面的默认值。未知的设置会报错，所以拼写错误不会被忽略。

```toml
[rclone]
config = ""                     # rclone.conf path; empty = rclone's default
transfers = 4                   # rclone --transfers
checkers = 8                    # rclone --checkers
browse_threads = 2              # threads for listings and other short calls
system_ssh = true               # sftp through the system ssh, one login per server

[ui]
left = "~"                      # left pane at start
right = ""                      # right pane at start; empty = first remote's root
show_hidden = false
mouse = true
multi_select_modifier = "ctrl"  # or "alt"
drop_action = "ask"             # or "copy": copy on release without the menu
language = "auto"               # "en", "zh-CN", or "auto": Chinese for a zh* locale

[jobs]
max_concurrent = 2              # jobs running at once; the rest wait
poll_ms = 500                   # progress refresh

[keys]
copy = ["F5", "c"]
move = ["F6", "m"]
# Any action from the help screen (?) by its name: switch_pane, up, down,
# page_up, page_down, home, end, enter, parent, mark, mark_all, filter, sort,
# toggle_hidden, goto, remote, swap, refresh, copy, move, rename, mkdir, delete,
# overwrite_mode, queue, error_log, help, quit, queue_cancel, queue_retry, queue_clear.
# Keys are written like F5, c, E, Ctrl+U, Alt+x, Space, Enter, PgDn, Delete.
```

界面打开期间，rclone 自己的日志写入 `~/.cache/rse/rclone.log`（macOS 上是 `~/Library/Caches/rse/`）。

## 语言

rse 支持英文和简体中文：`[ui]` 里的 `language` 选择其一，`auto`（默认）像 gettext 一样跟随系统语言（`LANGUAGE`、`LC_ALL`、`LC_MESSAGES`、`LANG`）。范围包括界面、`rse --help` 及其报错，以及启动时的报错和提示；配置文件里的设置对 `--help` 同样生效。glibc 有对应翻译时（Linux），"No such file or directory" 这类系统错误也会跟随。rclone、ssh 和 TOML 解析器输出的内容保持英文，日志文件也是。

文本保存在 [Fluent](https://projectfluent.org) 文件 `locales/en/rse.ftl` 和 `locales/zh-CN/rse.ftl` 中，编译进二进制文件。

## 通过 OpenSSH 使用 SFTP

rse 默认通过系统的 `ssh` 连接 sftp 服务器，所以 `~/.ssh/config` 里的一切都会生效（主机别名、ProxyJump、密钥……）。第一次使用某台服务器前，它会登录一次并保持这个 OpenSSH 主连接，rclone 建立的所有连接都经由它。因此服务器在一次会话里只看到一次登录，而不是 rclone 的每个并行连接各登录一次，这样 fail2ban 之类的工具就不会封禁你。同一服务器上同一账户的所有远程和地址共用这个连接，rse 退出时关闭，否则在最后一次使用 15 分钟后结束。同一账户但设置不同的远程（例如各自有 `known_hosts_file`）各用各的连接。不管 `~/.ssh/config` 怎么设置，这些连接都不转发 agent、X11 和端口，也不执行本地或远程命令，和 OpenSSH 自己的 sftp 一样。

这适用于 `sftp://` 和 `me@box:` 地址、`:sftp,…` 连接字符串，以及 `rclone.conf` 里的 sftp 远程。经由其他远程访问的 sftp 远程（例如套在它上面的 `crypt`、`alias` 或 `union`）照 rclone 一贯的做法由它自己用内置的 ssh 打开；想让这一层也用 OpenSSH，就给那个 sftp 远程配上它自己的 `ssh = …`。远程的 `host`、`user`、`port`、`key_file`、`pubkey_file` 和 `known_hosts_file` 会转成 ssh 选项。自带 `ssh = …` 设置的远程保持原样。以 `-` 开头的 `host` 或 `user` 改用 rclone 内置的 ssh，因为系统 `ssh` 会把它当成选项。设置了只有内置 ssh 才支持的选项（例如 `key_pem`、`ciphers` 或代理）的远程也改用内置 ssh，仍像 ssh 一样按 `known_hosts` 校验主机密钥；固定了主机密钥（`host_keys`、`pin_host_key`）的远程同样改用内置 ssh，由 rclone 校验。远程自己的 `known_hosts_file` 就是 ssh 校验用的文件；服务器不在其中时会像下文那样询问你，而 rclone 自己会直接拒绝。

登录可能需要密码、密钥口令或一次性验证码。`rclone.conf` 里保存的密码（`pass`）或密钥口令（`key_file_pass`）会直接交给 ssh，不再询问；其他内容会在对话框里询问，例如 `me@box's password:` 或验证码。不是用 `rclone obscure` 生成的 `pass` 或 `key_file_pass`（例如写成了明文）不会被使用：rse 会改为询问，并在对话框下方的窗口里说明原因；服务器拒绝了 `pass`、或 `key_file_pass` 解不开私钥时也一样。这需要 OpenSSH 8.4 或更新版本（`ssh -V`）；版本更旧时，需要密码的远程会像以前一样使用 rclone 内置的 ssh。`rclone.conf` 里的密码和口令以及记住的密码，只回答 ssh 自己针对该账户或该远程 `key_file` 的提问：跳板机的提问、服务器要求输入密钥口令的提问都会交给你回答，你在那里输入的内容也不会被记住。登录如果两分钟没有进展（不算你作答的时间），会被放弃。

### 记住的密码

密码对话框里有一个 **Remember password** 复选框（Tab 或点击切换）。勾选且登录成功后，rse 会把密码保存在 `~/.ssh/rse-passwords`，并在该账户下次登录时交给 ssh，顺序排在 `rclone.conf` 里的密码之后。文件每行一个账户：

```text
me@box.example.com 3T9PgUymd9w-jZk2CoHnCElf
root@10.0.0.5:2222 dlGK7HGgNL9oJ5cxgg8FeI5Ne_Yjjg
```

账户按 ssh 解析后的结果写入（`ssh -G`）：真实的用户名和主机名而不是主机别名，端口不是 22 时加上 `:port`。密码按 `rclone.conf` 保存密码的方式做了混淆：`rclone reveal` 可以还原，也可以手动写入用 `rclone obscure` 生成的值。混淆只能防止密码被一眼看到，所以和 ssh 对待私钥一样，只有在文件仅你本人可读（`chmod 600`）时 rse 才会使用它。其他行（例如 `#` 注释）保持不动。

已记住密码但 rse 仍然询问时，对话框下方的窗口会说明原因：ssh 拒绝了记住的密码、文件无法使用（例如其他人可读），或者上次保存密码失败。如果 ssh 因为其他原因再次询问，窗口会显示 ssh 的原话，例如 `ssh: Permission denied, please try again.` 此时记住新密码会替换被拒绝的那个，不记住则会删除它。要忘记一个密码，删除它所在的行，或者运行：

```bash
rse --forget-password <user>@<host>
```

这些问题也会写入 rclone 日志。

### 退回 rclone 内置的 SSH

在以下情况下，会改用 rclone 内置的 ssh（每个连接各登录一次，使用 rclone 自己的设置）：

- 没有 `ssh` 程序，或在 rse 配置的 `[rclone]` 下设置了 `system_ssh = false`；
- 远程需要只有内置 ssh 才能做到的东西：内联密钥（`key_pem`）、代理或加密算法设置；
- 通过系统 ssh 登录失败，例如密钥被拒绝。这时内置 ssh 会用 ssh 的 `known_hosts` 文件检查主机密钥（除非远程设置了 `known_hosts_file`），所以 ssh 从未接受过的服务器会被拒绝，而不是被信任。你的 ssh 配置里 `StrictHostKeyChecking` 为 `no` 或 `accept-new` 时，新服务器像 OpenSSH 那样放行；只要 ssh 读的任何一个文件里、以 ssh 认得的任何名字列着这台服务器，它就必须出示其中记录的密钥。密钥变了一律拒绝（OpenSSH 在 `no` 下会继续，只是关掉密码登录）。rse 判断不了时（文件路径里有 `%`、或设置了 `KnownHostsCommand`）就校验。

原因会写入 rclone 日志。有两种失败直接报错，不会退回：主机密钥与已知的不同（内置 ssh 不检查主机密钥），以及取消了询问（内置 ssh 只会再次失败）。没见过的服务器会连同其密钥指纹一起显示给你，只有你回答“是”才会加入 `known_hosts`（默认是“否”）；你的 ssh 配置里设置的 `StrictHostKeyChecking` 照常生效。如果你的 ssh 配置已经设置了 `ControlMaster` 和 `ControlPath`，rse 会使用它们；否则它的 socket 放在 `$XDG_RUNTIME_DIR/rse`（其他用户无法在里面创建任何东西），没有这个目录的系统（例如 macOS）放在 `/tmp/rse-<uid>`。这个目录无法使用时（例如被其他用户抢先创建），sftp 会像上面那样改用 rclone 内置的 ssh，仍然校验主机密钥，rse 启动时会说明原因。使用 `--rcd` 时以上都不适用：那个 rclone 运行在它自己的机器上。

## 测试

```bash
cargo test                       # unit, snapshot and integration tests
cargo clippy --all-targets       # pedantic, unwrap_used and expect_used are errors
cargo fmt --check
```

- 单元测试用假的后端驱动界面逻辑；界面快照用 [insta](https://insta.rs) 保存（有意修改后运行 `cargo insta review`）。
- 界面快照用英文拍摄，其中几个用中文重拍，以检查宽字符是否对齐。
- 集成测试在 local 和 memory 后端上运行内置的 rclone，中途取消传输，并通过一个私有的 `sshd` 浏览 sftp 远程（没有安装 `sshd` 时跳过）。
- `tests/concurrency.rs` 检查 rclone 调用之间不会互相阻塞。
- `cargo test` 还会检查两个语言文件定义了相同的消息，且每条消息都被用到。

窗口边框、窗口之间的箭头和可滚动的文本框来自 [lazyui-ratatui](https://github.com/lutianyu2001/lazyui-ratatui)。

## 兼容性

测试环境：

- Ubuntu 24.04.4 LTS（x86_64）
  - Rust 1.99.1
  - Go 1.27.3
  - [rclone](https://rclone.org) v1.75.2
  - [OpenSSH](https://www.openssh.com) 9.6p1
- macOS 15.7.9（Sequoia），Apple Silicon

## 免责声明

This project is not affiliated with, endorsed by, or sponsored by rclone or its maintainers. "rclone" refers to the software developed by Nick Craig-Wood (https://github.com/rclone/rclone) and is referenced here solely for descriptive purposes.

## 许可证

Copyright 2026 Tianyu (Sky) Lu

The entire repository is licensed under the Apache License, Version 2.0 
(the "License"); you may not use any file in this repository except in 
compliance with the License. You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.

## 作者

Tianyu (Sky) Lu
