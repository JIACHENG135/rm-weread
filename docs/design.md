# WeRead 阅读器设计（书架 / 批注 / 热门划线）

> 这份文档原来在 `rm-agent` 仓库的 `docs/reading-app-design.md`，随项目
> 独立成 `rm-weread` 仓库一起搬过来，避免两边各留一份、慢慢漂移（参见
> `rm-agent` 里 `three_finger_translate.qmd` 曾经因为两地各放一份、编辑
> 时漏改一份导致功能被静默删除的教训）——这里是唯一的版本。

## 目标与非目标

做一个比 [REweread](https://github.com/nasonliu/REweread) 更简洁的微信读书 reMarkable 客户端：

1. 微信读书书架
2. 每本书的批注
3. 看别人对某句话的评价（热门划线/评论）

**非目标**：不做自己的账号系统、不做自己的评论/社交后端、不维护任何服务器。微信读书自己的服务器就是唯一的后端；本项目全程只在设备上跑，只做一个瘦客户端。这不是偷懒，是刻意的架构选择——见下面"为什么不需要后端"。

**参考实现**：REweread 依赖的 [finlater/weread.koplugin](https://github.com/finlater/weread.koplugin) 是本设计里"照抄端点契约/移植解码算法"的具体来源——它自带
`docs/weread-api-reference.md`（端点+请求签名的语言无关伪代码）和
`lib/crypto.lua`/`lib/content.lua`（实现）。下面"具体技术细节"一节就是
调研这份源码后得到的。

## 仓库关系：独立于 rm-agent，不是它的一部分

`rm-weread` 是单独的私有仓库，不是 `rm-agent` 的一个 bin。原因：这个项
目本身体量（QML UI、WeRead 协议客户端、自己的 systemd 服务组合）已经跟
`rm-agent` 主线（手写问答/翻译/生词）没什么关系，混在一起会让两边都变
成大杂烩；参照的是 `inkwell-suite` 这种"小而专注、单独一个仓库"的模式，
而不是继续往 `rm-agent` 的 `src/bin/` 里加文件。

沿用 `rm-agent` 已经验证过的写法/风格（`gemini.rs`/`xochitl.rs` 的
ureq 直连风格、`translate_daemon.rs`/`vocab_daemon.rs` 的文件触发/轮询
IPC、`evdev.rs` 的圈画+三指点击手势追踪）**是抄写法，不是共享代码**——
两个仓库之间没有 Cargo path/git 依赖关系，需要的模块（尤其是手势追踪部
分）到用的时候再决定是照着抄一份精简版，还是把 `rm-agent` 对应模块拆出
来发成一个内部 crate 给两边一起依赖。这是留到"批注"那个阶段再具体决定
的开放问题，现在先不选。

发布方式跟进度挂钩：现在是纯源码开发阶段，还没有值得发布的二进制。等
第一个可用的东西出来（大概率是"能登录+看书架"那一步），再决定是像
`inkwell-suite` 那样把编译好的二进制提交进仓库，还是走 GitHub Release
附件——上次讨论定的是 Release 附件模式（不像 `inkwell-suite` 现在这样
直接把二进制文件提交进 git）。

## 和 REweread 的关键差异：跳过 KOReader/Lua 这一层

REweread 的数据流：

```
Qt Quick UI -> C++ Store -> KOReader 的 LuaJIT -> weread-move/tools/*.lua
  -> weread.koplugin 的 client/content/cookie 模块 -> 微信读书 API
```

它为了拿到"正文解密 + 分页排版"这两个已经被 KOReader 社区解决多年的硬骨头，把整个 KOReader/LuaJIT 运行时也一起背了过来，UI 层（Qt）反而是自己重写的。

本设计反过来：**只搬"已经解决的算法"，不搬"运行时"**。用单一 Rust 二进制 + ureq 直接打 HTTP API 的风格（沿用 `rm-agent` 的 `gemini.rs`、`xochitl.rs` 已经验证过的写法），不引入 Lua/KOReader 依赖。

## "不重复造轮子"分三类处理

| 部分 | 难度 | 处理方式 |
|---|---|---|
| 账号/书架/进度/评论类 API（登录、书架、章节列表、热门划线、评论） | 普通 JSON HTTP，但端点和坑是 koplugin 几年踩出来的 | **照抄端点契约**，用 Rust+ureq 直接实现，不跑 Lua |
| 正文解密 | 私有加密算法，真正的逆向工程成果 | **移植 koplugin 里那一小段纯函数**（输入密文输出明文，无状态、无 UI 依赖），一次性机械翻译成 Rust，不重新逆向 |
| 正文分页/排版 | REweread 复用 KOReader 通用 reflow 引擎（要处理任意 EPUB/PDF） | **新写，但范围窄很多**——只需要处理微信读书自己这一种私有 chapter 格式，不需要通用排版引擎 |

## 架构

```
src/weread_client.rs（库模块，风格同 rm-agent 的 gemini.rs/xochitl.rs）
  - 二维码登录 + cookie 续期（照抄 login-qr.lua 的流程：getLoginUid → 轮询 → getLoginInfo）
  - 书架/进度/章节列表拉取
  - 章节正文拉取 + 解密（移植自 koplugin 的纯函数）
  - 某页/某段的热门划线与评论拉取
  - （可选 v2）写回自己的划线/笔记到微信读书账号

src/bin/weread_daemon.rs（复用 rm-agent 的 translate_daemon.rs/vocab_daemon.rs
  已验证的文件触发/轮询/结果 IPC 模式——QML 侧只会 touch 触发文件、轮询结
  果文件，不需要新的 IPC 机制）
  - 书架：写 shelf.json 供 QML 轮询
  - 翻页：解密+分页当前章节，写当前页文本
  - 评论：仅按需拉取"当前可见页"的评论，短生命周期缓存，不做持久化
    存储（微信读书自己的服务器已经是权威存储，没必要在本地重复维护一份）
  - 批注：复用现有圈画+三指点击手势链路，锚点直接用微信读书返回的真实
    段落/range id（不需要我之前设计里那套 quote-text 模糊匹配兜底方案，
    因为锚点数据是微信读书 API 免费给的）

QML 阅读器 app（不是 rm-agent 的 xovi/three_finger_translate.qmd 那种
  "patch 进 xochitl"的小 overlay，而是独立全屏 app）
  - 通过 XOVI 的 AppLoad 启动器接入（REweread 也是这个机制），不是走
    qt-resource-rebuilder 的 .qmd diff-patch 技术——两者是 XOVI 里不同的
    机制，patch 技术是给"在 xochitl 里加一个小功能"用的，AppLoad 是给
    "启动一个独立 app，暂停 xochitl"用的，这次场景对应后者
```

## 数据落地（全部在设备本地，没有一处是"我们自己的后端"）

- `session.json` — 微信读书登录态，沿用 REweread 已验证的路径约定
  （`/home/root/.local/share/rm-weread/`），没必要另起一套
- `shelf.json` — 书架缓存，来源永远是微信读书 API，本地只是缓存
- `chapters/<book_id>/<chapter_id>.json` — 解密后的章节正文缓存，避免
  每次翻页重新拉取+解密
- `annotations/<book_id>.jsonl` — 自己的批注。优先写回微信读书账号本身
  （如果写接口可用），本地只是离线兜底；这样"批注跨设备同步"这件事也
  不需要自己维护，微信读书自己的账号同步机制已经做了
- 热门划线/评论 — **不做本地持久化**，永远按需现拉，短 TTL 缓存。这是
  "不维护后端"最直接的体现：评论数据的存储、去重、点赞数这些都完全留
  在微信读书服务器上，本项目连一份影子拷贝都不需要

## 复用 REweread 已验证的防御性细节

它的故障排查表里这两条是真实踩过的坑，直接抄：

- 评论只按**当前页**加载，翻页立刻取消上一页请求，停留 3 秒才发起请求
  （避免翻页时打一堆无用请求，也避免点击评论后卡死）
- 分页位置要保存 `textOffset` 而不是只存 `pageIndex`（字号变化后
  `pageIndex` 会失真）

## 真正要新写的工作量（对比 REweread 少了什么）

1. `weread_client.rs`：把 koplugin 的 client/cookie Lua 模块重新表达成
   Rust struct + ureq 调用——不新建协议，照抄端点行为
2. 解密函数：从 Lua 机械移植到 Rust，一次性，之后离线可测（同一段密文
   应该解出同一段明文，可以脱离设备直接写单测）
3. 窄范围分页/排版：只服务微信读书自己的 chapter 格式，比通用 EPUB 引
   擎小得多
4. XOVI AppLoad 接入：复用 REweread 已经验证过的接入方式，不是自己发明
5. **抄写法**（不是共享代码，见上面"仓库关系"）：圈画+三指点击手势链
   路、文件触发/轮询 IPC 模式、systemd + `deploy.sh` 设备部署流程、
   `gemini.rs` 的 HTTP 客户端写法作为 `weread_client.rs` 的模板

## 具体技术细节（源码调研：finlater/weread.koplugin）

之前"移植解密算法"这条一直是抽象的占位符。翻了 koplugin 的
`lib/crypto.lua`、`lib/content.lua` 和它自带的
`docs/weread-api-reference.md` 之后，可以落到具体实现了，而且比预想的
简单——这**根本不是加密**，是一套无密钥、确定性的字符打乱算法，纯函数、
可离线单测。

### 正文"解密"其实是三步可逆变换

1. 每个 shard（`e_0`/`e_1`/`e_3` 或 TXT 的 `t_0`/`t_1`）响应是
   `<32位大写MD5前缀><编码正文>`，校验 `MD5(编码正文) == 前缀` 即可确认
   完整性
2. 按 chapter 顺序拼接各 shard 的编码正文，去掉第一个字符
3. **字符位置对调**（`swap_positions` 算出一组要互换的下标，
   `reverse_swaps` 按这组下标两两 swap 字符）——这是唯一"微信读书专有"的
   部分，其余都是标准算法：
4. 再做一次 base64url→base64 解码（`-`→`+`、`_`→`/`、补 `=`），拿到
   UTF-8 正文/XHTML

`swap_positions`/`reverse_swaps` 这两个函数在 `content.lua` 里一共不到
40 行，纯字符串/整数运算，无外部依赖，Rust 移植是纯机械翻译，写完可以直
接拿真实抓包的 shard 响应做单元测试（输入密文→断言解出预期正文），不需
要上设备就能验证对不对。

**MD5/SHA256 不需要移植**——koplugin 自己手写了一份 MD5/SHA256（因为
LuaJIT 标准库没有），这是 Lua 环境的限制，不是微信读书的专有算法。Rust
直接用 `md-5`/`sha2` crate（已经加进 `Cargo.toml`），这部分连"移植"都
不算，是真正的"不重复造轮子"。

### 请求签名（写请求/内容请求都要用）

- `_e(value)`：一个基于 MD5 的确定性 hash，用于 `bookHash`/`chapterHash`
  以及请求体里的 `b`/`c`/`pc` 字段（数字输入按 9 位分块转十六进制，非数
  字输入按字符码拼接，再和 MD5 片段组合）
- `weread_sign`：对请求字段按 key 字典序拼成 query string（JS 风格
  `false` 而非 Python `False`，`encodeURIComponent` 规则），跑一个自定
  义的双累加器异或哈希（`a = a ^ (charCode << ...)`，从右往左扫描，见
  `docs/weread-api-reference.md` §3.4 的伪代码），输出十六进制签名
- 两个都是纯函数、无状态、无密钥，一次性照抄伪代码实现即可，附带的伪代
  码已经是语言无关的形式，不需要再去啃 Lua

### 关键 API 端点一览（对应我们的三个功能）

| 功能 | 端点 | 备注 |
|---|---|---|
| 登录 | 扫码：`getLoginUid` → 轮询 → `getLoginInfo`；Web session 续期：`POST /web/login/renewal` | |
| 书架 | `POST /api/agent/gateway` `api_name=/shelf/sync` | 官方 skill 接口，走 `Authorization: Bearer` |
| 阅读进度 | `api_name=/book/getprogress` / 上报走 `POST /web/book/read`（签名请求） | |
| 章节目录 | `api_name=/book/chapterinfo`（元数据）或 `POST /web/book/chapterInfos`（含图片资源 `tar` 地址） | |
| 正文内容 | `POST /web/book/chapter/e_0,e_1,e_3`（EPUB 格式）或 `t_0,t_1`（TXT 格式），`sc=1` 拿完整正文 | 按上面三步解码 |
| 我的批注 | `api_name=/book/bookmarklist`（划线）、`/review/list/mine`（想法/书评） | |
| **别人的热门划线** | `api_name=/book/bestbookmarks`（某章热门划线原文）、`/book/underlines`（划线热力：range+count，不含正文） | |
| **别人对某句话的评价** | `api_name=/book/readreviews`（给一批 range，拿每个 range 下的想法列表：作者、内容、引用原文） | 这就是"看别人对某句话的评价"的直接实现 |

### 为什么我们不需要 koplugin 的 EPUB-脚注-拦截那套花活

koplugin 展示"点击划线弹出想法"的方式很巧妙：下载时把想法**烧进 EPUB**
（做成标准的 `epub:type="noteref"`/`footnote"` 脚注结构，CSS 隐藏），
阅读时**抢在 KOReader 内建脚注弹窗之前拦截点击**，用 `getHTMLFromXPointer`
从隐藏脚注节点里把 HTML 抠出来自己渲染。这一整套是因为 koplugin 复用的
是 KOReader 的 **CREngine**（通用 EPUB 渲染引擎），必须绕开人家已有的
脚注机制、还要用 XPointer 做定位。

我们不用这套，也不需要：因为分页/排版是我们自己写的（上面"正文分页/排
版"那行），我们从解码出的正文到屏幕上每一页的字符范围，这个映射本来就
握在自己手里，不需要像 koplugin 那样从渲染引擎里"抠"出定位信息。用户
圈画+三指点击时，直接拿"当前页文本 + 触点对应的字符 offset"去查
`/book/underlines`/`/book/readreviews`，不需要 OCR、不需要 XPointer、
也不需要把想法预先烧进文件——这是自己写渲染器换来的简化，值得在这里记
一笔。

## 需要正视的风险（不是"以后再说"，是现在就要接受的取舍）

- **未公开 API 会变**：REweread 自己的故障排查表已经证明这套接口会
  breaking change。跳过 koplugin 意味着以后接口一变，是这个项目自己
  （其实是维护它的 agent）负责跟进修，不再有 koplugin 社区帮忙先踩坑。
  这是"更简洁"的真实代价，不是没有代价。
- **解密算法可能不是单一路径**：不同书籍/出版方可能有不同保护方式，
  koplugin 的 Lua 大概率已经处理了一些边界情况。第一版移植只做"能跑通
  的主路径"，遇到解不出来的书再补——和 `rm-agent` 一贯的"先上真机验证，
  再补边界情况"的风格一致（参见它的 goMarkableStream 那次教训：不要在
  没有真实失败信号之前过度设计）。
- **ToS/合规**：和 REweread 一样，仅供个人非商用使用，接口本身未经腾讯
  授权。

## 分阶段

1. `weread_client.rs`：登录 + 书架 + 章节列表，先在本机（非交叉编译）
   跑通并对着真实账号验证，不碰渲染 ← **当前阶段**
2. 解密函数移植 + 单元测试（离线可测，不需要上设备）
3. 最小分页渲染 + QML 阅读器 app + AppLoad 接入，先跑通"打开一本书翻页"
4. 批注（复用现有手势链路——到这一步再定手势追踪代码怎么在两个仓库间
   共享）
5. 热门划线/评论（按需拉取 + REweread 的防御性节流策略）
