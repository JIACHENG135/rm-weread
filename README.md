# rm-weread

reMarkable 平板（reMarkable 2 / Paper Pro）上的微信读书客户端：一个 Rust
二进制加一小块 QML 补丁。没有配套 app，没有我们自己的账号，也没有需要维护
的后端——微信读书自己的服务器就是唯一的后端，本项目全程只在设备上跑。

*[English](README.en.md) · 设计与取舍的完整记录见
[docs/design.md](docs/design.md)*

## 它做什么

从你的微信读书书架里挑一本书，daemon 会拉取并解码章节、排好版，然后把生成
的 PDF **投递进 xochitl 自己的书库**。你用平板的**原生 PDF 阅读器**读它。

这一点是整个设计的核心。用原生阅读器意味着：原生笔迹延迟、笔画写进正确的
`.rm` 文件、翻页/缩略图/目录全部免费。早期版本曾经在 QML 里自己渲染正文，
真机上跑通过，最后还是整个废弃了——因为墨迹**永远做不进去**：xochitl 的书
写引擎直写 framebuffer、绕过 Qt 合成，笔画会被记进底下那个真实打开着的文
档。详见设计文档里的「已废弃：全屏 QML 阅读器」。

别人的**热门划线**在你读到那一页时实时画在页面上，**点一下划线**就能看到
其他读者对这句话写的想法。

```
四指点击任意打开的文档，或打开「微信读书」文件夹里的「＋ 书架」卡片
  → 书架浏览器（封面 / 书名 / 作者 / 已生成打勾）
  → 选一本 → 进度条 → 生成完成后一个「刷新书库」按钮
```

## 状态

在 **reMarkable Paper Pro** 和 **reMarkable 2**（固件 3.27.3.0）上端到端
跑通，两台共用同一个微信读书账号。

已知的限制，都是刻意的取舍：

- **只读**。划线和想法不回写微信读书账号，阅读进度也不上报。你的笔迹存在
  reMarkable 文档里——那本来就是 reMarkable 意义上的批注。
- **版式冻结**。笔迹锚在页面几何上，所以只有正文和排版参数的哈希不变时，
  才允许把新 PDF 原地换进已有文档。哈希变了而文档已有笔迹，流水线会**拒绝
  覆盖**，改为生成一个「(更新版)」兄弟文档。
- **章节里的图片还没做**。这件事必须在你打算批注的书**首次生成之前**做
  对——事后补会改变几何，作废那本书已有的笔迹。
- **实时划线不写进 PDF 文件**。导出或同步出去的 PDF 是纯文本的。

## 安装

从 [Releases](https://github.com/JIACHENG135/rm-weread/releases) 下载对应
架构的二进制（`weread_daemon-aarch64` 给 Paper Pro，`weread_daemon-armv7`
给 reMarkable 2），然后按
[`skills/install/SKILL.md`](skills/install/SKILL.md) 走——那份文档是写给
agent 执行的，人照着做也一样。

里面有两条**不能跳过**的，都不是能猜出来的，而且各自消耗过好几个小时：

1. **`xovi/debug` 证明不了功能可用。** 它设了 `LD_PRELOAD` 但**没设**
   `XOVI_ROOT`，于是原生扩展不加载、`CommandExecutor` 成了未知类型、整个注
   入块**静默地**不实例化——而 qmldiff 照样报「Processing file …」、日志里
   零错误。功能验证必须在 `systemctl start xochitl` 下做。
2. **Paper Pro 上 `/etc` 是挂在 tmpfs 上的 overlay。** 写进
   `/etc/systemd/system` 的东西重启就没，连 `systemctl enable` 建的软链一起
   丢。装到只读根分区上。rM2 不是这样——先看 `mount | grep 'on /etc'`。

## 接口请求的纪律

生成一本书时**一次划线请求都不发**。早期版本在生成时把每一章的热门划线都
拉一遍——一本 288 章的书就是几分钟内 288 次请求——微信读书先回 HTTP 499，
随后整个网关对这个客户端返回 403（同一个 key 用 wget 从同一台设备打却正常，
所以封的是客户端而不是账号）。

现在划线是**你读到哪一章才拉哪一章**，一次一个请求。评论一直都是点击时才
拉的。

## 代码结构

| 模块 | 职责 |
|---|---|
| `login.rs` / `session.rs` / `cookie.rs` | 扫码登录、登录态持久化、cookie 续期 |
| `skill_gateway.rs` / `shelf.rs` | 微信读书 Skill 网关、书架 |
| `weread_sign.rs` | 请求签名（`_e` / `sign`） |
| `content.rs` | 正文解码——唯一一段微信读书专有算法 |
| `reader.rs` / `xhtml.rs` | 章节抓取；XHTML → 纯文本（带偏移映射） |
| `underlines.rs` | 热门划线与想法；range → 正文偏移 |
| `metrics.rs` | 字形宽度，分页/版面/绘制三方共用的唯一来源 |
| `paginate.rs` / `layout.rs` | 分页；冻结几何（`layout.json`） |
| `pdfgen.rs` | 手写 PDF，确定性输出，内嵌 CJK 字体 |
| `xochitl_doc.rs` | 投递进书库 + 冻结规则 |
| `pipeline.rs` | 端到端流水线 |
| `xovi/weread.qmd` | QML 补丁：书架浏览器、划线覆盖层、评论弹窗 |

设备上跑的是 `bin/weread_daemon.rs`。`bin/weread_login.rs`、
`weread_chapter.rs`、`weread_page.rs`、`weread_pdf.rs` 是在电脑上对着真实账
号驱动流水线各段的 CLI。

## 构建与测试

交叉编译到 reMarkable（链接器配置在
[`.cargo/config.toml`](.cargo/config.toml)）：

```sh
cargo build --release --target armv7-unknown-linux-musleabihf   # reMarkable 2
cargo build --release --target aarch64-unknown-linux-musl       # Paper Pro
```

测试要显式指定宿主目标，因为仓库默认交叉编译：

```sh
cargo test --target aarch64-apple-darwin
```

解码、签名、分页、版面、PDF 全部可离线测试——包括拿真实抓包的响应做断言，
以及用 Lua 解释器跑原始实现的输出当 ground truth。

## 致谢

端点契约和正文解码算法移植自
[finlater/weread.koplugin](https://github.com/finlater/weread.koplugin)
（含它自带的 API 文档）——真正的逆向工程发生在那里。
[REweread](https://github.com/nasonliu/REweread) 是先行的 reMarkable 客户
端；本项目走了另一条路，完全跳过 KOReader/LuaJIT 运行时，只移植算法。

QML 补丁机制来自 [asivery](https://github.com/asivery) 的 XOVI、
`qt-resource-rebuilder` 和 `qmldiff`。

## 注意

微信读书的接口未公开且会变；变了之后，跟进修复是这个项目自己的事。生成 PDF
意味着在设备本地留下一份付费书的完整副本——它只存在于你自己的设备上，不上
传、不分发。

**仅供个人非商业使用。** 接口未经腾讯授权，本项目与微信读书、reMarkable 均
无关联。
