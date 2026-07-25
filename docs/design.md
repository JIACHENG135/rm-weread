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

## 架构（2026-07 修订：PDF 流水线取代全屏 QML 阅读器）

> 本节及下面"PDF 生成流水线与冻结规则"是当前架构。原"全屏 QML 阅读
> 器"方案已废弃，原文和废弃理由保留在文末"已废弃：全屏 QML 阅读器"一
> 节——那一版在真机上跑通过（阶段 4 的 commit 还在），废弃不是因为做不
> 出来，而是因为它天花板太低：原生墨迹永远做不进去（笔迹直写
> framebuffer、绕过 Qt 合成、还会把笔画错写进底下的真实文档——"幽灵墨
> 迹"），而且等于在 xochitl 进程里重写一个功能弱化的阅读器。

```
weread_daemon（纯后台，触发文件驱动）
  generate 触发 → shelf/sync 选书 → chapterInfos → 逐章:
      拉正文分片 → content.rs 解码 → xhtml.rs 提纯文(带偏移映射)
      → /book/underlines 拉热门划线(range→正文偏移)
      → paginate.rs 字符网格分页
  → layout.rs 冻结几何(layout.json) → pdfgen.rs 生成固定版式 PDF
      (热门划线烧成下划线 + ①..⑳ 标号, 章节 outline, 嵌入 CJK 字体)
  → xochitl_doc.rs 投递进 ~/.local/share/remarkable/xochitl/
  ask_* 触发 → /book/readreviews → reviews.txt（评论弹窗的数据）
  每日 → 划线集合变化 > 20% 才做"仅装饰"重生成（几何永不变）

QML 补丁（xovi/weread.qmd，缩回 rm-agent 弹窗量级）
  四指点击 → touch generate → 显示 gen.txt 进度
  点 PDF 里的 ①标号 → 按 layout.json 命中 → touch ask_* → 弹评论
```

用户**阅读用的是 xochitl 原生 PDF 阅读器**：原生笔迹、原生延迟、笔画存
进正确的 .rm 文件；翻页、目录、缩略图全部免费。本项目不再自己渲染正文。

### 旧架构（保留作参照，代码已被替换）

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

QML 阅读器（走 qt-resource-rebuilder 的 .qmd diff-patch，跟 rm-agent 的
  xovi/three_finger_translate.qmd 同一套机制，**不是** AppLoad 独立 app）
  - 本质上是把 three_finger_translate.qmd 已经验证过的写法放大：往
    xochitl 现有 QML 树里注入一个平时 visible:false 的 Item，靠 trigger
    文件切换显隐。区别只是这个 Item 从一个小弹窗变成占满全屏的阅读器
  - 决策见下面"为什么走 patch 而不是 AppLoad"一节——这是一个明确接受了
    代价的选择，不是默认选项
```

## PDF 生成流水线与冻结规则（当前主线）

### 为什么是 PDF 而不是 EPUB

生成 EPUB 交给 xochitl reflow，"字符 offset → 屏幕坐标"的映射就丢了
（koplugin 当年绕 CREngine 的 XPointer 坑，而 xochitl 连那样的 API 都
不给）。PDF 版式由 paginate.rs 自己排，每个字符的页码+坐标全部已知，
微信读书 range（源 XHTML 字符偏移）经 `xhtml::Text` 的双向偏移映射即可
对到版面上任何位置。

### layout.json：几何的唯一权威

`layout.rs` 生成，存两份：`~/.local/share/rm-weread/layout/<book>.json`
（daemon 用）和 `/home/root/xovi/exthome/weread/layout.json`（QML XHR
读，做标号命中判定）。比对话里最初的草稿更省：排版是均匀字符网格，行盒
几何可以从行号+网格常量推导，所以只存每行的 `off/len` 和每个标号的归一
化包围盒（0..1、左上原点——触摸事件的坐标系；rM2 和 Paper Pro 都是
3:4，PDF 页面也生成 3:4，归一化坐标跨设备通用）。

### 冻结规则（xochitl_doc.rs 强制执行）

墨迹锚在页面几何上，所以：

- `content_sha256`（章节纯文本 + 全部网格常量）一致 → **仅装饰刷新**：
  原地覆盖 `<uuid>.pdf`，删缩略图，touch .metadata
- 哈希变了、文档还没有 .rm 笔迹 → 整体覆盖
- 哈希变了、文档已有笔迹 → **拒绝覆盖**，生成"(更新版)"新文档，旧的留
  给用户。绝不出现"悄悄重排导致笔迹错位"

pdfgen 输出是确定性的（无时间戳、无随机 ID），同输入必产生同字节——
装饰刷新的可信度靠这个撑着。

### 热门划线的呈现

生成时把 `/book/underlines` 的 range 烧进版面：下划线粗细/虚实映射热度
（≥1000 粗实线、≥100 实线、其余细虚线），每条划线尾部一个 ①..⑳ 圈号
（每章最多 20 个，超出的只画线不给号）。评论**不**烧进版面——由 QML 弹
窗按需拉 `/book/readreviews`（对话里定的取舍：烧进去的评论是死的、截
断的；弹窗能看全）。

### 每日刷新（阈值门控）

daemon 每天对已生成的书拉一次 underlines，和 layout.json 里的 hot 集合
算对称差/并集；**> 20% 才重新生成 PDF**（从章节缓存重建，绝不重新下
载正文——冻结几何必须由冻结时的文本重建），否则只等下次。评论缓存独立，
每次 generate 后清空。

### IPC 协议（沿用文件触发/轮询，参数编码进文件名）

QML 的 CommandExecutor 只能跑 `/bin/touch`，写不了文件内容，所以参数
全部放进被 touch 的文件名：

```
generate                          → 重新生成书架第一本书
ask_<chapterUid>_<range>_<nonce>  → 拉该 range 的评论
gen.txt      seq / working|done|error / 消息
reviews.txt  seq / ok|error / 引用原文 / 总数 / 作者<TAB>内容…
```

### 字体

`assets/NotoSansSC-Regular.ttf`：Noto Sans CJK SC Regular 抽取自
NotoSansCJK-Regular.ttc、CFF→TrueType 轮廓转换（cu2qu）、子集化到
CJK 统一表意 + 标点 + 假名 + 圈号等区段（约 2.2 万字形，7MB），OFL 许
可可以进仓库。pdfgen 以 CIDFontType2/Identity-H 全量嵌入（CID==GID，
PDF 侧不需要 cmap）；按书子集化是后续体积优化，不是前提。

### 待真机验证清单（2026-07-25 真机过了一轮，剩余项标注如下）

1. **QML 侧拿当前 PDF 页码**（weread.qmd 里 `weReadCurrentPage()` 的
   TODO）：dump documentview 的 QML 找 SceneView/父链上的页码和缩放
   transform 属性。找到之前标号点击禁用，生成/阅读不受影响
   ——**唯一剩下的设计级未知数**
2. ~~xochitl 对换 .pdf 的反应~~ **部分验证**（Paper Pro 3.27.3）：
   重启 xochitl 后新文档正常出现、worker 正常渲染缩略图、无报错；
   装饰刷新的原地覆盖（同 uuid 换 PDF）在文档未打开时执行成功。
   还没验证的：不重启能否出现新文档；文档打开着的时候换 PDF 会怎样
   （daemon 侧后续可以加"检查 lastOpened 再换"的保守逻辑）
3. ~~两个端点的真实响应形状~~ **已验证并回填**（真实抓包进了单测）：
   - `/book/underlines`：顶层 `underlines` 数组，`count` 常为 0、热度
     在 `score`（0..1 浮点）——解析取 `max(count, score*1000)`
   - `/book/readreviews`：请求必须带 `reviews` 数组且 `maxIdx: 0`
     （非 0 时 `pageReviews` 返回空——文档里没写，实测发现）；响应在
     `reviews[0].pageReviews[].review.{content, abstract, author.name}`
   - 真机端到端：40 章书生成 300 页 PDF/4.9MB、840 处划线、322 个标
     号；ask_* 触发返回了真实的 1184 条想法列表
4. 图片：章节里的图还没进 PDF（chapterInfos 的 tar 资源）。**必须在给
   某本书首次生成前做对**，事后补会改几何、作废该书已有笔迹
5. 阅读进度不再回传微信读书（只读的必然结果）——README 要写清楚，免得
   被当 bug 报
6. （运维教训，真机踩到）daemon 的 stdout 不能接在会断的 tty/管道上：
   ssh 断开后管道缓冲写满，`println!` 阻塞把整个 daemon 卡死。已改为
   systemd 服务（journal 接管输出），service 文件不再依赖 xovi/xochitl

## 为什么走 patch 而不是 AppLoad

> 2026-07 修订后本节的天平变了：patch 侧的代价清单大幅缩水（QML 从全
> 屏阅读器缩回小弹窗，代价 1 的风险敞口回到 rm-agent 翻译卡片的量级，
> 代价 2 的全屏事件捕获整个不需要了，代价 3 的入口只剩"四指触发生成"），
> 结论反而更稳固：更没有理由为一个小弹窗引入 AppLoad 了。原文保留：

XOVI 里有两套不同的机制：qt-resource-rebuilder 的 .qmd diff-patch（往
xochitl 已有界面里注入东西），和 AppLoad（装一个有自己图标、自己进程、
自己生命周期的独立 app，REweread 用的就是这套）。按"独立全屏阅读器"这
个产品形态，AppLoad 才是它设计出来要解决的场景。

**但这里选 patch**，理由和代价都写清楚：

- **省掉一整个未知数**：AppLoad 的 README 开头就写着它是 "a xovi
  extension for the **RMPP**"，依赖的仓库也叫 `rmpp-xovi-extensions`/
  `rmpp-appload`，很可能是 Paper Pro 专属、不支持 rM2（armv7）。走 patch
  这条路直接绕开这个问题，不用去验证、也不用为两种设备准备两套接入方式
- **机制已经在真机上验证过**：rm-agent 的三指翻译/生词卡片就是这么做
  的，deploy.sh 里那套"改 .qmd → 用 qmldiff 按固件 hashtable 哈希 →
  先跑 xovi/debug 前台验证 → 再切 xovi/start 持久化"的流程可以整套复用

### 代价（明确接受，不是"以后再说"）

1. **没有进程隔离——这是最大的一条**。AppLoad 的 app 崩了不影响
   xochitl；patch 进去的 QML/JS 是跑在 xochitl 自己进程里的。现有那几个
   小弹窗逻辑很薄（显示几行文字），出错概率低；这次要塞进去的是完整书架
   + 分页阅读器 + 图片渲染，复杂度高一个量级。这块 QML 一旦出问题，拖垮
   的是**整个 xochitl**——rm-agent 的 deploy.sh 里已经记着前车之鉴：一次
   QML 属性名写错直接把设备搞成"重启循环"。风险随塞进去的东西变多而放
   大，所以 `xovi/debug` 前台验证这一步在这个项目里是硬性的，不能跳
2. **全屏事件捕获要自己做**：阅读器打开期间所有触摸/笔迹事件都不能漏到
   底下的 xochitl 页面（翻页手势不能同时翻底下的文档）。需要一个覆盖全屏
   的高 z-order 捕获层，做起来不难，但边界情况（笔迹落在捕获层边缘等）
   必须真机验证
3. **入口要自己造**：AppLoad 天然有主屏图标，patch 没有。"怎么从主屏进
   到阅读器"得自己解决——往 xochitl 文件浏览器界面补个按钮，或复用现成的
   角落多指手势入口，本质上又是一次小 patch
4. **补丁越大越脆弱**：hashtable 不能跨固件版本复用（deploy.sh 已记录），
   每次固件升级都要重新验证；补丁体量越大，每次改完过一遍 debug 验证的
   成本越高

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
4. QML 补丁（书架 + 全屏阅读器 + 事件捕获层 + 入口）：机制和 deploy 流程
   照抄 rm-agent 的 three_finger_translate.qmd，但补丁体量大得多，见上面
   "为什么走 patch 而不是 AppLoad"的代价清单
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

1. ✅ **登录 + 书架**（`src/login.rs`、`src/shelf.rs`、`src/skill_gateway.rs`，
   本机验证，`src/bin/weread_login.rs` 是验证用的 CLI）——已对着真实账号
   跑通，包括二次运行复用 `session.json`、不用重新扫码。过程中三个跟
   koplugin 文档/伪代码不完全一致的真实情况，已经写成代码注释，这里汇
   总一下，免得以后又踩一遍：
   - `webLoginVid` 实际返回的是裸 JSON 数字，不是字符串（`login.rs` 的
     `string_or_number` 反序列化器处理）
   - 登录确认成功后，`userInfo`/`apikeyGet` 的一次性 401 比 koplugin 自
     己的 3 次/500ms 重试撑得更久，观察到需要更长——已经放宽到 8 次/1.5s
   - 账号必须先在 App 里手动开通"微信读书 Skill"（我 → 设置 → 微信读书
     Skill → 获取 API Key）才有 `apikey`，这是账号侧开关，不是接口 bug；
     开关状态在文档里没提前提到，实测才发现
   - `/shelf/sync` 响应会**同时**带着完整数据和 `upgrade_info`
     字段——`upgrade_info` 存在不代表这次请求失败，只是"有新版本可用"的
     旁路提示，一开始误把它当错误直接拒绝了整个响应
2. ✅ **内容解码移植 + 单元测试**（`src/content.rs`：`swap_positions`/
   `reverse_swaps`/`base64_decode`/`checked_body`/`decode_content_shards`，
   离线可测，没上设备）——没有直接信"读代码翻译一遍应该对了"，而是本地
   装了个 Lua 解释器，跑 koplugin **真正的**函数、拿真实输出当测试期望
   值，结果真的抓到两个只读代码翻译不出来的 bug：
   - `swap_positions` 的分块循环条件写成了 `i + step <= tmp.len()`，
     Lua 原文其实是严格小于 `i + step < tmp.len()`——用
     `"UJDREVGR2hpams="` 这个例子验证时，Rust 版算出 8 个下标，Lua 只
     有 6 个，才发现这个差一位的错误
   - `base64_decode` 一开始想省事直接用现成的 `base64` crate，结果真实
     输入喂进去经常静默解出空字符串——因为微信读书这个格式**根本不是**
     标准 base64（padding 可以补到 3 个 `=`，这在 RFC 里是非法的），而
     是逐字符按 6 bit 拼接、无视 `=`、按 8 bit 切字节、结尾不满 8 bit
     直接丢弃的手搓拼接法，只能按 Lua 原样手写，不能偷懒复用库——这跟
     "MD5/SHA256 不用移植"那条判断刚好相反，取舍标准是"标准算法直接用
     库，非标准/魔改过的必须照抄"
   - 7 个单测覆盖了三个长度分支（`<4`/`4..11`/`>=11`）
3. ✅ **内容抓取管线**（`src/weread_sign.rs`、`src/reader_state.rs`、
   `src/reader.rs`，`src/bin/weread_chapter.rs` 是验证用的 CLI）——把阶段
   2 只做"解码"的 `content.rs` 接上真实网络请求，端到端跑通"登录态 → 续
   期 Web session → 拉章节目录 → 带签名请求正文分片 → 解码"，对着真实
   账号真的拉到了一整段可读的、格式良好的 XHTML（三体前传《球状闪电》
   版权页）。这是 `content.rs` 解码逻辑第一次在**真实**数据上被验证，
   不再只是合成测试字符串。同样靠真实 Lua 输出做 ground truth（这次是
   `WeRead.e`/`WeRead.sign`，`bit` 库换成 Lua 5.5 原生位运算重新实现），
   又抓到一个 API 文档伪代码没提到的坑：请求里的 `r` 字段来自
   `tostring(math.random(0,9999) ^ 2)`，Lua 的 `^` 永远返回浮点数，所以
   真实请求发的是带 `.0` 后缀的字符串（如 `"1522756.0"`），不是纯整数——
   用整数运算算出数值后手动拼 `.0` 后缀实现的，没有经过 `f64`（Rust 的
   `f64` Display 对整数值不会自动带 `.0`，直接用会漏掉这个后缀）。
4. ~~最小分页渲染 + QML 阅读器补丁~~ → **改道：PDF 生成流水线**
   ← **当前阶段**（方案见上面"PDF 生成流水线与冻结规则"；改道理由见
   "已废弃：全屏 QML 阅读器"）。已实现，待真机验证：
   - `layout.rs`（冻结几何 + 标号命中）、`pdfgen.rs`（手写 PDF，字符
     网格 + CJK 字体嵌入 + 下划线/标号/outline）、`underlines.rs`
     （热门划线/评论 API + range→正文偏移映射）、`xochitl_doc.rs`
     （投递 + 冻结规则）、`pipeline.rs`（端到端 + 阈值刷新）
   - `weread_daemon` 改造成 generate/ask/每日刷新三件事
   - `weread.qmd` 缩回小弹窗（四指生成 + 点标号看评论）
   - `weread_pdf` CLI：桌面端对真实账号跑整条流水线
   - 待验证清单见"PDF 生成流水线与冻结规则"末尾——其中 QML 拿当前页码
     是唯一的设计级未知数，其余是接口形状/缓存行为级别
   - `xovi/debug` 前台验证的纪律不变（弹窗虽小，仍在 xochitl 进程里）
5. **设备原生登录 UI**——阶段 1 现在的登录方式（CLI 打印链接，人在另一台
   电脑上用 `qrencode` 生成图片看）只是验证协议用的脚手架，**不是最终产
   品该有的样子**：如果手头只有 rmb、没有电脑，这套完全没法用。真正要做
   的是把二维码渲染这件事搬到设备本身：
   - `weread_daemon` 拿到 `login::begin()` 的 confirm URL 后，本地用一个
     小的 `qrcode` crate 把文本编码成矩阵、画成图，写到 QML 能读的路径
     （不依赖任何外部电脑/工具）
   - QML 全屏显示这张图 + 提示文字
   - 轮询逻辑原样复用阶段 1 已经在真实账号上跑通的 `login::poll`/
     `login::complete`，只是把 CLI 的 print/stdin 换成"写结果文件"/
     "读 QML 触发文件"（跟 translate_daemon 一样的文件触发/轮询 IPC，
     不需要新机制）
   - `NEED_OTP` 那步不需要手写识别：reMarkable 没有物理键盘，但 4 位数
     字验证码用一个简单的 QML 数字键盘（点数字）就够了，没必要上 OCR
   - 这一步是可用性的硬前提（没有它，设备离开电脑就没法首次登录），不
     是锦上添花，得跟阶段 4 的 QML 补丁一起做，不能拖到最后
6. ~~批注（复用现有手势链路）~~ → 改道后**只读**：不做自由手写批注的
   识别/回写（见"已废弃"一节——笔画意图判别是最容易做砸、最容易惹恼用
   户的一环；原生笔迹现在直接存在 PDF 文档里，本来就是批注）。回写微
   信读书划线保持"可选 v2"不启动
7. 热门划线/评论 → 已并入阶段 4 的流水线（划线烧进 PDF，评论走弹窗按
   需拉取 + REweread 的防御性节流策略）

## 已废弃：全屏 QML 阅读器（2026-07）

阶段 4 原本的形态：weread.qmd 往 SceneViewGestures.qml 注入一个
`z: 2000` 的全屏 Rectangle + 铺满的 MouseArea，daemon 逐页喂已换行文
本。真机跑通过翻页。废弃理由，按分量排：

1. **原生墨迹做不进去，而且比"显示不出来"更糟**。xochitl 的书写引擎拿
   到 Wacom 事件后直写 framebuffer（EPDC A2/DU 局部刷新），不经过 QML
   事件分发、不参与 Qt 场景图合成——`z: 2000` 对它完全无效。实际后果是
   **幽灵墨迹**：笔迹视觉上出现在阅读器上层（直写 fb），却被记进底下
   那个真实打开着的文档；QML 重绘擦掉屏上笔迹，关掉阅读器后笔迹留在错
   误的文档里——污染用户真实的笔记文件。要根治只有 EVIOCGRAB 独占笔设
   备 + 自己实现整套笔画渲染（把 rmkit 重写一遍），工程量和"跑在
   xochitl 进程内"的风险不成比例
2. **本质是在 xochitl 进程里重写一个功能弱化的阅读器**：翻页闪全屏
   GC16、同步 XHR 卡 UI 线程、无缩略图/目录/进度，全都要自己补
3. 全屏事件捕获只做了触摸没做笔（MouseArea 接不到笔事件），就算补上
   grab 也回到第 1 条的渲染问题

改成"生成 PDF 交给 xochitl 原生阅读器"后，这三条整体消失——墨迹问题不
是被绕开，是**不存在了**：用户用的就是原生书写路径。换来的新约束（版
式冻结、只读、进度不回传）见"PDF 生成流水线与冻结规则"。

ToS 注意：落地 PDF 意味着在设备本地生成付费书的完整副本，比"临时全屏
显示"的分量重。仅供个人非商用；章节缓存和生成的 PDF 都只存在用户自己
的设备上，不上传、不分发。
