# 中文界面文本。这里的每条消息都要和 en/rse.ftl 一一对应；
# `cargo test` 会检查两个文件定义的消息相同，并且都被用到。

## 通用

items = { $count } 项
list-separator = ，
cause-separator = ：
too-small = 终端太小

## 窗格

window-address = 地址
window-local = 本地
window-remote = 远程
window-transfers = 传输
window-terminal = 终端
tag-local = 本地
marked = [已选 { $count } 项]

## 底部按键提示

hint-rename = 重命名
hint-copy = 复制
hint-move = 移动
hint-mkdir = 新建
hint-delete = 删除
hint-remote = 远程
hint-filter = 过滤
hint-help = 帮助
hint-quit = 退出

## 覆写模式：开启时横在顶部的一行，以及开关时状态栏的提示

overwrite-mode = 覆写模式
overwrite-mode-note = 已有的文件会被直接覆盖，不再询问；按 { $key } 关闭
overwrite-on = 覆写模式已开启：已有的文件会被直接覆盖，不再询问
overwrite-off = 覆写模式已关闭：覆盖已有的文件前会再次询问

## 地址栏

address-revising = [修改中]
address-hint = 路径、remote:path 或 ftp://me@box/pub
empty-path = 路径为空
unknown-protocol = 未知协议 { $name }://（支持：sftp、ftp、ftps、smb、webdav、webdavs）
needs-host = { $scheme }:// 需要主机名，例如 { $scheme }://me@box
ipv6-bracket = IPv6 地址缺少 ]
bad-port = 端口无效：{ $port }

## 文件窗格

pane-error = 错误：{ $error }
pane-loading = { $spinner } 加载中…
pane-empty = （空）
pane-no-match = （没有匹配项）
dir-size = <目录>
sort-name = 名称
sort-size = 大小
sort-time = 时间
sorted-by = 按{ $key }排序
column-name = 名称
column-size = 大小
column-time = 修改日期

## 终端（窗格下方，窗格所在服务器上的 shell）

terminal-connecting = { $spinner } 正在连接…
terminal-starting = { $spinner } 正在启动 shell…
terminal-not-ssh = 只有本地磁盘和通过 ssh 连接的服务器才能打开终端
terminal-disabled = 终端功能已关闭（-T）
terminal-failed = 终端：{ $error }
terminal-cd-control = 终端：目录名里有控制字符，不输入到 shell 里
terminal-cd-unknown = 终端：无法判断 shell 里是否有命令在运行，留在原目录

## 操作

op-copy = 复制
op-move = 移动
op-delete = 删除
op-copy-title = 复制
op-move-title = 移动
op-delete-title = 删除
nothing-selected = 未选择任何项
rename-nothing = 把光标移到要重命名的项上
same-directory = 源目录和目标目录相同
into-itself = 不能把目录放进它自己里面
checking-target = 正在检查目标…
all-skipped = 无事可做：所有项都已跳过
queued = 已加入队列：{ $op } { $count } 项
probe-late = 未开始{ $op }：目标检查完成时另一个对话框正在打开
probe-failed = 无法检查目标：{ $error }
drop-dialog = 没有放下：拖动时弹出了对话框
mkdir-failed = 无法创建 { $name }：{ $error }
rename-failed = 无法重命名为 { $name }：{ $error }
cleanup-failed = 已移动，但无法删除 { $dir } 中的源目录：{ $error }
name-invalid = 名称无效
name-slash = 名称不能包含 /
name-exists = { $name } 已存在
password-failed = 无法使用该密码：{ $error }
listing-failed = 列出 { $dir } 失败：{ $error }
names-unsafe = { $dir } 中有 { $count } 个条目的名字可能指向目录之外（例如 ..），没有显示
listing-remotes-failed = 列出远程失败：{ $error }
ssh-hint = 提示：使用 ssh 选项的 sftp 远程需要无交互登录：密钥、ssh-agent 或已有的 ControlMaster 连接

## 传输队列

queue-idle = 空闲
queue-running = { $count } 个进行中
queue-queued = { $count } 个排队
queue-done = { $count } 个完成
queue-failed = { $count } 个失败
queue-elsewhere = { $count } 个在别处暂停
queue-keys = x 取消 · r 重试 · c 清除 · Esc 返回
job-to = { $op } { $name } → { $dir }
job-in = { $op } { $dir } 中的 { $name }
job-queued = 排队中
job-checking = { $spinner } 正在检查目标
job-done = 完成
job-failed = 失败：{ $error }
job-cancelled = 已取消
job-skipped = 已跳过
job-exists = 目标中已有 { $name }
job-stopping = 正在停止…
job-starting = { $spinner } 正在开始
job-lost = 丢失了 rclone 任务 { $id } 的状态：{ $error }
job-failed-unknown = 任务失败

## 对话框

button-ok = 确定
button-cancel = 取消
button-yes = 是
button-no = 否
button-back = 返回
button-overwrite = 覆盖
button-skip = 跳过
button-overwrite-all = 全部覆盖
button-skip-all = 全部跳过
button-stop-and-quit = 暂停并退出
button-pause-and-leave = 暂停并离开
button-stay = 留下
button-copy-here = 复制到这里
button-move-here = 移动到这里
confirm-title = { $op }？
confirm-from = 从：{ $dir }
confirm-to = 到：{ $dir }
confirm-in = 在：{ $dir }
summary = { $items }（{ $details }）
summary-files = { $count } 个文件共 { $size }
summary-dirs = { $count } 个目录
and-more = …… 还有 { $count } 项
note-merge = 目录会合并到已有目录中：相同的文件跳过，不同的文件覆盖。
note-cross-remote-move = 跨远程移动是先复制再删除源文件。如果中途取消，已复制的文件已经从源位置删除。
note-delete-dirs = 目录会连同所有子目录一起删除。
note-no-undo = 此操作无法撤销。
conflict-title = 已存在
conflict-exists = { $dir } 中已存在 { $name }
conflict-existing = 已有：{ $entry }
conflict-new = 新的：{ $entry }
conflict-progress = 第 { $index } 个冲突，共 { $count } 个
note-dirs-merged = 两个目录会合并：相同的文件跳过，不同的文件覆盖。
conflict-job = { $job } 没有开始：
conflict-job-note = { $count ->
    [1] 选“取消”时，任务以失败留在队列中；重试时会再次检查目标。
   *[other] 选“取消”时，还没决定的 { $count } 个任务都以失败留在队列中；重试时会再检查。
}
note-overwrite-mode = 覆写模式已开启：目标中的同名条目会被直接覆盖，不再询问。
entry-dir = 目录
entry-modified = { $size }，修改于 { $time }
quit-title = 退出
quit-note = 退出会暂停传输，下次启动时继续。下面这些不能从停下的地方接着传，正在传的文件要从头再传：
leave-title = 离开
leave-note = 离开会暂停这两个位置之间的传输，回来时继续。下面这些不能从停下的地方接着传，正在传的文件要从头再传：
interrupted-file = { $file }：已传的 { $done }（共 { $size }）要重传
interrupted-done = { $job }：已完成，不受影响
interrupted-running = { $job }：正在传输
queue-not-saved = 没传完的任务无法保存到 { $path }：{ $error }
queue-not-loaded = 无法从 { $path } 读取上次没传完的任务：{ $error }
queue-held = 另一个 rse 正在运行：{ $path } 里没传完的任务归它管，这个 rse 不保存任务
mkdir-title = 新建目录
mkdir-prompt = 在 { $dir } 中新建目录，名称：
rename-title = 重命名
rename-prompt = { $name } 的新名称：
login-title = 登录
login-prompt = { $address } 的密码（没有就留空）：
ssh-title = SSH 登录
ssh-remember = 记住密码
ssh-remember-key = （Tab 切换）
ssh-notice-title = 为什么要输入
remotes-title = 远程（输入以过滤）
remotes-filter = 过滤：{ $filter }
remotes-none = 没有匹配项
remote-local = 本地 (~)
remotes-how = rclone.conf 里还没有远程：用 `rclone config` 添加。服务器不添加远程也能打开：按 { $key } 输入它的地址，例如 sftp://me@box。
errors-title = 错误日志，最新的在前（Esc 关闭）
errors-none = 没有错误。

## 帮助

help-title = 帮助（Esc 关闭）
help-start = 入门
help-start-panes = 每个窗格显示一个目录：本地磁盘、服务器或 rclone 远程上的。{ $switch } 在两个窗格之间切换。
help-start-open = 打开服务器：按 { $goto } 输入它的地址（sftp://me@box/srv、ftp://…、smb://…）；{ $remote } 从 rclone.conf 里选一个远程，或选本地磁盘。
help-start-copy = 用 { $mark } 选中条目；{ $copy } 把它们复制到另一个窗格的目录，{ $move } 移动过去。进度显示在“传输”窗口里。
help-start-more = 完整说明见手册 man rse 和 README。
help-keys = 按键
help-mouse = 鼠标
help-shift = 在大多数终端里，按住 Shift 拖动可以选中文字。
mouse-click = 单击
mouse-click-does = 移动光标，取消选择
mouse-blank = 单击空白处
mouse-blank-does = 取消选择
mouse-mark = { $modifier }+单击、右键单击
mouse-mark-does = 选中或取消选中
mouse-double = 双击
mouse-double-does = 打开目录（.. 返回上级）
mouse-wheel = 滚轮
mouse-wheel-does = 滚动指针下的窗格
mouse-address = 单击地址栏
mouse-address-does = 编辑地址
mouse-button = 单击 ↻ / > 按钮
mouse-button-does = 刷新 / 前往编辑后的地址
mouse-drag = 拖动
mouse-drag-does = 复制或移动（拖动已选中的项时，带上所有选中项）
mouse-cancel = 单击队列中的 ✕
mouse-cancel-does = 取消该任务

## 命令行（`rse --help` 及其报错）

cli-about = 本地路径与 rclone 远程的双栏终端文件管理器
cli-usage-heading = 用法：
cli-usage = rse [选项] [左侧位置] [右侧位置]
cli-arguments = 参数：
cli-options = 选项：
cli-left = 左侧窗格的初始位置，例如 ~/work
cli-left-value = 左侧位置
cli-right = 右侧窗格的初始位置，例如 nas:backup。只给一个位置且是远程时，它放在右侧而不是左侧
cli-right-value = 右侧位置
cli-config = remote-sync-explorer 的配置文件
cli-set = 只在这次运行时改一个配置项，例如 ui.show_hidden=true（可以给多次）
cli-set-value = KEY=VALUE
cli-rclone-config = rclone 的配置文件
cli-path-value = 路径
cli-ask-password = 启动前先询问 rclone 配置文件的密码
cli-forget-password = 删除为该登录（user@host）记住的 ssh 密码，然后退出
cli-login-value = 登录
cli-rcd = 使用外部的 `rclone rcd`，不用内置的 rclone（需要编译时启用 rcd 功能）
cli-url-value = URL
cli-rc-user = --rcd 的用户名
cli-user-value = 用户名
cli-rc-pass = --rcd 的密码（更好的做法：用环境变量 RCLONE_RC_PASS）
cli-pass-value = 密码
cli-no-terminal = 关闭终端功能：本地和服务器上都无法打开 shell
cli-version = 显示 remote-sync-explorer 和 rclone 的版本
cli-licenses = 显示 remote-sync-explorer 及其中第三方代码的许可协议
cli-help = 显示帮助
cli-error = 错误：
cli-tip = 提示：
cli-unexpected-argument = 无法识别的参数 { $arg }
cli-after-dashes = 如果 { $arg } 是要打开的位置，请写成 { $escaped }
cli-similar = 有相近的参数：{ $args }
cli-needs-value = { $arg } 需要一个值，但没有给出
cli-invalid-value = { $arg } 的值 { $value } 无效
cli-unexpected-value = { $arg } 不接受值，却给了 { $value }
cli-repeated = 参数 { $arg } 不能重复使用
cli-invalid-utf8 = 参数中有无效的 UTF-8 字符
cli-other = 无法解析命令行：{ $detail }
cli-try-help = 想了解更多，请运行 rse { $flag }。

## 启动（`rse --version`、配置文件、rclone 与 ssh）

startup-no-rcd = --rcd { $url }：这个 rse 编译时没有启用 `rcd` 功能
startup-no-embedded = 这个 rse 编译时没有内置 rclone；请用 --rcd <URL>
startup-keys = 配置文件有误
startup-config-path = 设置 rclone 配置文件路径失败
startup-configuring = 配置 rclone 失败
startup-config-password = rclone 配置文件的密码：
startup-reading-password = 读取密码失败
startup-unlocking = 解锁 rclone 配置文件失败
startup-transfers = 设置传输参数失败
startup-reading-config = 读取 rclone 配置失败：{ $error }
startup-reading-encrypted-config = 读取 rclone 配置失败：{ $error }（请加 --ask-password，或设置 RCLONE_CONFIG_PASS）
startup-obscuring = 混淆密码失败
startup-terminal = 设置终端失败
startup-events-closed = 事件通道已关闭
startup-still-listing = 退出时仍有目录在加载
version-unknown = 未知（{ $error }）
version-rclone = rclone { $running }（{ $backend }；编译时基于 { $built }）
config-in = { $path } 有误
config-in-variable = 环境变量 { $variable } 有误
config-on-command-line = 命令行（--set）有误
config-set-needs-equals = --set 要写成 KEY=VALUE，不是 { $arg }
keys-unknown-key = 未知的按键 `{ $key }`
keys-unknown-action = [keys]：未知的动作 `{ $name }`
keys-bad-key = [keys] { $name }：{ $error }
keys-bound-twice = [keys]：`{ $key }` 同时绑定给了 `{ $first }` 和 `{ $second }`
backend-embedded = 内置的 librclone
backend-rcd = { $url } 上的 rclone rcd
rcd-unreachable = 无法连接 { $url } 上的 rclone rcd：{ $error }
rcd-plain-http = 拒绝连接 { $url }：明文 http 会让密码在网络上被人读到；请用 https，或用 http 连接 localhost
rcd-bad-reply = 读取 rcd 的回复失败：{ $error }
rclone-shut-down = rclone 已关闭
rclone-bad-call = 无效的 rc 调用：{ $error }
rclone-bad-json = rclone 返回了无效的 JSON：{ $error }
rclone-unexpected-reply = { $method } 的回复不符合预期：{ $error }
rclone-no-jobid = { $method } 没有返回任务 ID
ssh-socket-dir = 无法把 { $dir } 用作 ssh 套接字目录（{ $error }），sftp 改用 rclone 内置的 ssh，按 ssh 的设置校验主机密钥
ssh-dir-not-private = 已存在，但不是当前用户的目录
ssh-missing = 找不到 ssh 程序：sftp 改用 rclone 内置的 ssh
ssh-cancelled = 已取消 ssh 登录
ssh-timed-out = ssh 登录两分钟没有进展（不算等你回答的时间），已放弃
ssh-secret-unusable = { $remote } 设置的 { $key } 不是 rclone obscure 生成的值（可能写成了明文），无法使用。
ssh-password-refused = 服务器拒绝了 { $remote } 设置的 pass。
ssh-passphrase-wrong = { $remote } 设置的 key_file_pass 解不开私钥。
passwords-refused = 服务器拒绝了记住的密码。
passwords-unusable = 无法使用记住的密码：{ $error }
passwords-not-saved-before = 上次没能记住密码：{ $error }
passwords-not-private = { $path } 其他用户也能读取，rse 不会使用它；请运行 chmod 600 { $path }
passwords-bad-entry = { $path }：{ $login } 的密码不是 rclone 混淆后的格式（{ $error }）
passwords-not-saved = 无法把 { $login } 的密码保存到 { $path }：{ $error }
passwords-no-home = 找不到用户主目录
passwords-updating = 更新 { $path } 失败
passwords-forgotten = 已删除 { $login } 记住的密码（{ $path }）。
passwords-none-for = { $path } 中没有为 { $login } 记住的密码
ssh-host-key = ssh 拒绝了服务器的主机密钥（{ $reason }）；请核实密钥，如果更换是预期的，请修正 known_hosts
rsync-stopped = rsync 已停止；传到一半的文件已保留，下次运行会接着传
rsync-not-started = 无法启动 rsync：{ $error }
rsync-exit = rsync 失败（退出码 { $code }）
rsync-signal = rsync 被信号 { $signal } 终止
internal-lock = 内部锁已损坏

## 按键（帮助页），每个动作一条

key-switch_pane = 在两个窗格之间切换
key-up = 光标上移
key-down = 光标下移
key-page_up = 上一页
key-page_down = 下一页
key-home = 第一项
key-end = 最后一项
key-enter = 打开目录
key-parent = 上级目录
key-mark = 选中并下移
key-mark_all = 全选 / 全不选
key-filter = 按名称过滤
key-sort = 按名称、大小、时间排序
key-sort_reverse = 反转排序顺序
key-toggle_hidden = 显示或隐藏点文件
key-goto = 前往路径、remote:path 或网址
key-remote = 为窗格选择 rclone.conf 里的远程，或本地磁盘
key-swap = 交换两个窗格
key-refresh = 刷新窗格
key-copy = 复制到另一个窗格
key-move = 移动到另一个窗格
key-rename = 重命名
key-mkdir = 新建目录
key-delete = 删除
key-overwrite_mode = 开关覆写模式：不询问，直接覆盖已有文件
key-queue = 进入或离开传输队列
key-terminal = 在窗格所在目录打开终端（本地或通过 ssh 连接的服务器）；在终端里则回到窗格
key-focus_left_address = 聚焦左侧地址栏
key-focus_left = 聚焦左侧窗格
key-focus_right_address = 聚焦右侧地址栏
key-focus_right = 聚焦右侧窗格
key-focus_queue = 聚焦传输队列
key-error_log = 错误日志（最近 100 条）
key-help = 本帮助
key-quit = 退出
key-queue_cancel = 队列：取消任务
key-queue_retry = 队列：重试失败的任务
key-queue_clear = 队列：清除已结束的任务
