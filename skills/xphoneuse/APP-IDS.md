# App identifiers — what `launch` takes

`launch` opens an app by its identifier: the **package name** on Android, the
**bundle id** on iOS. That is how you open an app — there is no reason to hunt
for its icon on the home screen, page through folders, or type its name into
Spotlight. One command, and it comes to the foreground exactly as tapping the
icon does:

```bash
python scripts/xphoneuse-cli.py launch com.tencent.xin     # 微信 on iOS
python scripts/xphoneuse-cli.py launch com.tencent.mm      # 微信 on Android
```

The iOS ids in this table were read off a real iPhone (its own list of
installed apps); the Android column is the publishers' package name. If an app
is not here, see "Finding an id" at the bottom.

## Messaging and social

| App | iOS bundle id | Android package |
|---|---|---|
| 微信 WeChat | `com.tencent.xin` | `com.tencent.mm` |
| QQ | `com.tencent.mqq` | `com.tencent.mobileqq` |
| 企业微信 WeCom | `com.tencent.ww` | `com.tencent.wework` |
| 钉钉 DingTalk | `com.laiwang.DingTalk` | `com.alibaba.android.rimet` |
| 飞书 Feishu / Lark | `com.bytedance.ee.lark` | `com.ss.android.lark` |
| 微博 Weibo | `com.sina.weibo` | `com.sina.weibo` |
| 小红书 RED | `com.xingin.discover` | `com.xingin.xhs` |
| 抖音 Douyin | `com.ss.iphone.ugc.Aweme` | `com.ss.android.ugc.aweme` |
| 哔哩哔哩 Bilibili | `tv.danmaku.bilianime` | `tv.danmaku.bili` |
| 知乎 Zhihu | `com.zhihu.ios` | `com.zhihu.android` |
| WhatsApp | `net.whatsapp.WhatsApp` | `com.whatsapp` |
| Telegram | `ph.telegra.Telegraph` | `org.telegram.messenger` |
| Signal | `org.whispersystems.signal` | `org.thoughtcrime.securesms` |
| X (Twitter) | `com.atebits.Tweetie2` | `com.twitter.android` |
| Instagram | `com.burbn.instagram` | `com.instagram.android` |
| Facebook | `com.facebook.Facebook` | `com.facebook.katana` |
| Slack | `com.tinyspeck.chatlyio` | `com.Slack` |
| Discord | `com.hammerandchisel.discord` | `com.discord` |

## Money, shopping, getting around

| App | iOS bundle id | Android package |
|---|---|---|
| 支付宝 Alipay | `com.alipay.iphoneclient` | `com.eg.android.AlipayGphone` |
| 淘宝 Taobao | `com.taobao.taobao4iphone` | `com.taobao.taobao` |
| 京东 JD | `com.360buy.jdmobile` | `com.jingdong.app.mall` |
| 拼多多 Pinduoduo | `com.xunmeng.pinduoduo` | `com.xunmeng.pinduoduo` |
| 美团 Meituan | `com.meituan.imeituan` | `com.sankuai.meituan` |
| 闲鱼 Xianyu | `com.taobao.fleamarket` | `com.taobao.idlefish` |
| 飞猪 Fliggy | `com.taobao.travel` | `com.taobao.trip` |
| 高德地图 Amap | `com.autonavi.amap` | `com.autonavi.minimap` |
| 百度地图 Baidu Maps | `com.baidu.map` | `com.baidu.BaiduMap` |
| 铁路 12306 | `cn.12306.rails12306` | `com.MobileTicket` |
| 招商证券 | `cn.com.cmschina.cmsmobilesecurities` | `cn.com.cmschina.cmsmobilesecurities` |
| Google Maps | `com.google.Maps` | `com.google.android.apps.maps` |

## Media and reading

| App | iOS bundle id | Android package |
|---|---|---|
| YouTube | `com.google.ios.youtube` | `com.google.android.youtube` |
| Spotify | `com.spotify.client` | `com.spotify.music` |
| Netflix | `com.netflix.Netflix` | `com.netflix.mediaclient` |
| 微信读书 Weread | `com.tencent.weread` | `com.tencent.weread` |
| 播客 Podcasts (Apple) | `com.apple.podcasts` | `com.google.android.apps.podcasts` |

## Apple's own (iOS) and their Android counterparts

Apple's ids are stable across iOS versions; the Android column is the closest
equivalent, and the vendor dialer/camera differ from ROM to ROM.

| App | iOS bundle id | Android package |
|---|---|---|
| 设置 Settings | `com.apple.Preferences` | `com.android.settings` |
| Safari / browser | `com.apple.mobilesafari` | `com.android.chrome` |
| 信息 Messages | `com.apple.MobileSMS` | `com.google.android.apps.messaging` |
| 电话 Phone | `com.apple.mobilephone` | `com.google.android.dialer` (varies) |
| FaceTime | `com.apple.facetime` | — |
| 照片 Photos | `com.apple.mobileslideshow` | `com.google.android.apps.photos` |
| 相机 Camera | `com.apple.camera` | `com.android.camera` (varies) |
| 备忘录 Notes | `com.apple.mobilenotes` | `com.google.android.keep` |
| 提醒事项 Reminders | `com.apple.reminders` | — |
| 日历 Calendar | `com.apple.mobilecal` | `com.google.android.calendar` |
| 时钟 Clock | `com.apple.mobiletimer` | `com.google.android.deskclock` |
| 天气 Weather | `com.apple.weather` | — |
| 快捷指令 Shortcuts | `com.apple.shortcuts` | — |
| 文件 Files | `com.apple.DocumentsApp` | `com.google.android.documentsui` |
| 钱包 Wallet | `com.apple.Passbook` | `com.google.android.apps.walletnfcrel` |
| App Store | `com.apple.AppStore` | `com.android.vending` |
| 翻译 Translate | `com.apple.Translate` | `com.google.android.apps.translate` |
| 语音备忘录 Voice Memos | `com.apple.VoiceMemos` | — |
| 查找 Find My | `com.apple.findmy` | `com.google.android.apps.adm` |
| 健身 Fitness | `com.apple.Fitness` | `com.google.android.apps.fitness` |
| 指南针 Compass | `com.apple.compass` | — |
| 测距仪 Measure | `com.apple.measure` | — |
| 无边记 Freeform | `com.apple.freeform` | — |
| 手记 Journal | `com.apple.journal` | — |
| 股市 Stocks | `com.apple.stocks` | — |
| Gmail | `com.google.Gmail` | `com.google.android.gm` |
| Chrome | `com.google.chrome.ios` | `com.android.chrome` |

## Finding an id

The device lists its own apps — **use `apps` instead of guessing**:

```bash
python scripts/xphoneuse-cli.py apps --filter 微信      # by name (iOS) or id
python scripts/xphoneuse-cli.py apps --filter tencent   # by publisher
python scripts/xphoneuse-cli.py apps --kind system      # the platform's own apps
python scripts/xphoneuse-cli.py apps                    # everything (long - filter it)
```

On **Android** this is `pm list packages` (`-3` for third-party only): package
names, nothing else. On **iOS** it is lockdown's installation proxy, which also
returns each app's display name and version — so there you can search by what
the user calls the app ("微信") rather than by an id nobody sees. Neither needs
Developer Mode; only `launch` needs the developer image.

The common apps are in the table above, and Chinese publishers keep their
prefix: Tencent `com.tencent.*`, Alibaba `com.taobao.*` / `com.alipay.*` /
`com.aliyun.*`, ByteDance `com.ss.*` / `com.bytedance.*`, Meituan
`com.sankuai.*`, Xiaomi `com.xiaomi.*`, Huawei `com.huawei.*`.

`launch` is the check that costs nothing: if the id is right the app comes up,
and if it is wrong the device says so by name.
