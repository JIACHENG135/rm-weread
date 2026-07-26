# reMarkable Paper Pro 壁纸轮换

从你指定的文件夹里随机挑图，定时覆盖设备的睡眠/关机画面，实现"轮换壁纸"。基于 reMarkable 官方未文档化但社区多年验证的机制（SSH 直接替换系统图片）。

## 原理

reMarkable 的开机、睡眠（screensaver）、关机等画面本质上就是存放在 `/usr/share/remarkable/` 下的固定 PNG 文件，睡眠画面对应 `suspended.png`。只要用自己的图片覆盖这个文件，设备下次进入睡眠状态时就会显示新图片。

Paper Pro 上有两个关键差异，直接影响轮换脚本的写法：

1. **根文件系统默认只读**。每次开机（以及每次固件更新）后，`/usr/share/remarkable/` 都不可写，必须先执行 `mount -o remount,rw /` 才能覆盖文件。脚本已经把这一步内置在每次运行里，幂等无副作用。
2. **OS 3.25+ 新增了一层"轮播插画"遮罩**（仅 Paper Pro / Paper Pro Move）。设备会在 `/usr/share/remarkable/carousel/` 里循环显示三张 776×776 的小插画，叠加在你的睡眠画面正中间，每次锁屏/解锁都会换一张。这个遮罩和 `suspended.png` 是两套独立机制：换掉睡眠图片本身不会移除它。如果不想要这层遮罩，可以 SSH 进去把 `carousel` 目录下的文件移到备份目录（不建议直接删，方便日后恢复）：
   ```
   cd /usr/share/remarkable/carousel
   mkdir backupIllustrations
   mv * backupIllustrations/
   ```
   想恢复就 `mv backupIllustrations/* .`。

图片尺寸：Paper Pro 原生分辨率是 **1620 × 2160**（宽 × 高，竖屏），PNG 格式；建议灰度或低彩度，E Ink 刷新更干净。

## 目录结构

```
remarkable-wallpaper-rotator/
├── install.sh                    # 在你电脑上运行：一键部署到设备
├── device/
│   ├── rotate-wallpaper.sh       # 部署到设备上的轮换脚本
│   ├── random-screens.service    # systemd 定时任务（单次执行）
│   └── random-screens.timer      # systemd 定时器（控制轮换间隔）
└── tools/
    └── prepare_images.py         # 在电脑上批量把图片裁切成设备尺寸
```

## 使用步骤

### 1. 打开开发者模式 + SSH

设置 > 通用 > 软件版本 > 打开"高级" > 开发者模式，设备会重置并重新走一遍开箱流程（Connect 账号里的文件不会丢）。开启后在 设置 > 关于 > 版权和许可 里能看到 root 密码和设备 IP。首次连接需要用 USB 数据线，之后可以在设备上执行 `rm-ssh-over-wlan on` 改为走 WiFi。

### 2. （可选）预处理你的图片

```bash
python3 tools/prepare_images.py ~/Pictures/我的壁纸 ./prepared --grayscale
```

会把文件夹里所有图片居中裁切缩放成 1620×2160 并输出到 `./prepared`。不加 `--grayscale` 就保留彩色（Paper Pro 支持彩色，但彩色层分辨率较低，纯线条/文字类壁纸建议灰度）。

### 3. 一键部署

```bash
chmod +x install.sh
./install.sh <设备IP> ./prepared
```

脚本会：连接设备、remount 读写、上传轮换脚本和图片到 `/home/root/customization/images/suspended/`、安装并启用 systemd 定时器。之后按一下电源键锁屏即可看到新图。

没有准备图片文件夹也可以先跳过第二个参数，之后手动 `scp` 图片进 `/home/root/customization/images/suspended/` 即可，定时器下次触发会自动纳入新图。

### 4. 调整轮换间隔

默认每 30 分钟轮换一次。改设备上的 `/etc/systemd/system/random-screens.timer` 里的 `OnUnitActiveSec`（例如改成 `5min`），然后：

```bash
ssh root@<设备IP> "systemctl daemon-reload && systemctl restart random-screens.timer"
```

### 注意事项

- **固件更新会清空自定义文件**（`suspended.png`、以及你装的 systemd 单元都可能被覆盖/移除），升级后重新跑一次 `install.sh` 即可。
- 这属于非官方玩法（社区多年沿用，reMarkable 官方不支持也不算保修范围内的"损坏"），出问题可以恢复出厂设置兜底。
- `rotate-wallpaper.sh` 首次运行会自动备份原始 `suspended.png` 为 `suspended.original.png`，想恢复默认画面直接把它拷回 `suspended.png` 即可。

## 社区流行壁纸/灵感来源

reMarkable 圈子里几个常见的壁纸来源和风格，都可以下载图片放进你的轮换文件夹（版权各自遵循原作者许可）：

- **[reHackable/awesome-reMarkable](https://github.com/reHackable/awesome-reMarkable)** — 社区维护的 reMarkable 相关项目大合集，里面能找到不少壁纸/主题仓库的入口。
- **[engeir/remarkable-splashscreens](https://github.com/engeir/remarkable-splashscreens)** — 数学线条风格的极简壁纸包（sacks_spiral 螺旋、dragon_curve 龙形曲线、snowy_hills 雪山、collatz_sea_weed、sierpinski_triangle 谢尔宾斯基三角），尺寸比例已经适配 reMarkable，是目前最直接能拿来用的现成合集之一。
- **[Neurone/reMarkable](https://github.com/Neurone/reMarkable)** — 本项目参考的轮换机制原型，自带一套社区收集的壁纸和对应的 systemd 脚本。
- **r/RemarkableTablet**（Reddit）— 用户常晒自定义睡眠画面，比如 [Studio Ghibli 风格睡眠画面](https://www.reddit.com/r/RemarkableTablet/comments/1iocza4/custom_studio_ghibli_sleep_screen/) 这类帖子；整体趋势是走**极简线条画 / 低彩度自然风景 / 引用语录**这几类，因为 E Ink 对高对比度、低墨水密度的图更友好，刷新残影也更少。
- **[ddvk/remarkable-hacks](https://github.com/ddvk/remarkable-hacks)** — 非官方定制的社区指南合集，涵盖睡眠画面在内的多种改造。

挑图时建议优先选灰度或黑白线条类，彩色照片在 Paper Pro 彩色层上会降到约 150 PPI，细节丰富的照片容易糊。

## Sources

- [Turn rMs "suspended screen" in something useful! - abcxyz](https://abcxyz.de/2017/12/07/turn-rms-suspended-screen-in-something-useful/)
- [Switch Out Your reMarkable Paper Pro's Sleep Screen - SimplyKyra](https://www.simplykyra.com/blog/switch-out-your-remarkable-paper-pros-sleep-screen/)
- [Exploring the reMarkable Sleep Screen Overlay - SimplyKyra](https://www.simplykyra.com/blog/exploring-the-remarkable-sleep-screen-overlay-and-how-to-tinker-with-it-yourself/)
- [reMarkable Sleep Screen: How to Change or Customise It (2026) - Templacity](https://templacity.com/remarkable/remarkable-screensaver/)
- [GitHub - Neurone/reMarkable](https://github.com/Neurone/reMarkable)
- [GitHub - reHackable/awesome-reMarkable](https://github.com/reHackable/awesome-reMarkable)
- [GitHub - engeir/remarkable-splashscreens](https://github.com/engeir/remarkable-splashscreens)
