# 发布流程（Windows / macOS）

发布到 [CrabNebula Cloud](https://web.crabnebula.cloud/aovik/xphoneuse/releases)，流程是
**draft → upload → publish**，由 `scripts/release.sh` 封装。

一条铁律：**Tauri 的安装包只能在目标系统上构建** —— dmg 只能在 macOS 构建，NSIS
只能在 Windows 构建。所以一次完整发布会跨两台机器，靠"同一个 draft 追加资产"拼起来。

---

## 0. 一次性准备

| 项 | 说明 |
|----|------|
| `CN_API_KEY` | 在 [web.crabnebula.cloud](https://web.crabnebula.cloud) 的应用里创建（revoke 过的不能复活，只能新建）。存放二选一：`printf %s 'cn_xxx' > ~/.crabnebula/apikey && chmod 600 ~/.crabnebula/apikey`，或 `export CN_API_KEY=...` |
| `tauri.key` | 更新器私钥，仓库根目录，**已 gitignore，绝不提交**。脚本自动加载；对应的公钥在 `src-tauri/tauri.conf.json` 的 `plugins.updater.pubkey` |
| 私钥密码 | `export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=...`（自备，不要提交到仓库）。缺密码 → 构建跳过更新器产物，发布出去的包**收不到自动更新** |
| `cn` CLI | 脚本自动下载（`CN_BIN` 可指定）；手动调试时用 `cn release show/list` |
| macOS 额外 | `rustup target add x86_64-apple-darwin`（universal 构建需要） |
| Windows 额外 | 用 **Git Bash** 跑脚本 |

`CN_API_KEY` 失效的症状是 `Could not authenticate API key (code: 401)` —— 去网页端新建一把。

---

## 1. 改版本号

版本号唯一来源是 **`src-tauri/tauri.conf.json` 的 `version`**（UI 通过 `src/lib/utils.ts`
的 `APP_VERSION` 读取，别在别处再写一份）。

```sh
# 改完提交（版本号提交用 chore(release)）
git add src-tauri/tauri.conf.json && git commit -m "chore(release): 1.1.2"
```

两台机器要**同一个版本号**：先在一台提交并 push，另一台 `git pull`。

---

## 2. 常用命令

脚本会把"构建 → 建 draft → 上传 → 发布"全跑一遍：

```sh
# macOS（默认 universal dmg + 更新包），完整发布
TAURI_SIGNING_PRIVATE_KEY_PASSWORD=... scripts/release.sh

# Windows（Git Bash），只上传不发布
TAURI_SIGNING_PRIVATE_KEY_PASSWORD=... ./scripts/release.sh --no-publish
```

| 参数 | 作用 |
|------|------|
| `--only-build` | 只构建，不上传（本地测试用） |
| `--skip-build --no-publish` | 复用已有构建产物，只上传，保持 draft |
| `--publish` | 只把现有 draft 发布出去 |
| `--no-publish` | 构建 + 上传，但不发布（多机协作时用） |
| `--notes "..."` / `--notes-file f` | 发布日志；不传则用最近 tag 以来的 commit |
| `--arch arm64` | macOS 只出 arm64 dmg（体积减半，不带 Intel） |
| `--channel beta` | 预发布通道 |

---

## 3. 多机发布流程（推荐）

```
① Windows:  release.sh --no-publish      # 上传 NSIS 安装包（含 notes 由第一台确定）
② macOS:    release.sh --no-publish      # 构建 universal dmg + 更新包并上传
③ 任意一台: release.sh --publish         # 或网页端 "Publish draft"
```

- 资产追加到**同一个 draft**（按版本号匹配），每台机器只需上传自己平台那部分。
- draft 的笔记在**第一次创建时**写入，后续机器不会覆盖。
- 第 ② 步若本机已有构建产物，用 `--skip-build --no-publish` 省一次编译。
- 顺序无所谓；但**发布只能一次**，之后就不能再追加资产了（见坑 1）。

---

## 4. 每台机器产出什么

| 平台 | 产物 | 平台标记 |
|------|------|---------|
| macOS | `XPhoneUse_<v>_universal.dmg` | 公开：`dmg-universal`（一条 dmg 覆盖 Intel + Apple Silicon） |
| macOS | `XPhoneUse.app.tar.gz` + `.sig` | 更新：`darwin-aarch64` **和** `darwin-x86_64` 各注册一次 |
| Windows | `XPhoneUse_<v>_x64-setup.exe` | 公开：`nsis-x86_64`；更新：`windows-x86_64` |
| Linux | `*.AppImage` / `*.deb` | 公开：`appimage-x86_64` / `deb-x86_64`；更新：`linux-x86_64` |

macOS 构建必须打 **`dmg,app` 两种 bundle**（脚本默认值）：更新器产物只跟着 `app`
出来，只打 dmg 会报 `no updater-enabled targets were built`。

---

## 5. 国内网络构建（GitHub 拉不动时）

从 1.1.12 起有**两个依赖必须从 GitHub 取**。直连被墙、代理又不通时会**卡到超时**
（表现是 `cargo build` 长时间 0% CPU，或在日志里看到 `git fetch ... github.com` 挂着不动）：

| 依赖 | 取什么 | 卡住时的症状 |
|------|--------|--------------|
| `ocr-rs`（git 依赖） | `git fetch https://github.com/zibo-chen/ocr-rs` | 长时间零 CPU，无输出 |
| `mnn-runtime-sys` 的 build script | GitHub Releases 上 7.5MB 的预编译库 | `curl: (28) Failed to connect to github.com port 443` |

两者都能用**环境变量**改走镜像，**不需要改仓库文件，也不动 `~/.gitconfig`**：

```sh
export MIRROR=https://ghfast.top        # 备用：https://ghproxy.net、https://gh-proxy.com

# ① git 依赖走镜像（必须开 GIT_FETCH_WITH_CLI，cargo 才会用 git CLI、才认 insteadOf）
export CARGO_NET_GIT_FETCH_WITH_CLI=true
export GIT_CONFIG_COUNT=1
export GIT_CONFIG_KEY_0="url.$MIRROR/https://github.com/.insteadOf"
export GIT_CONFIG_VALUE_0="https://github.com/"

# ② MNN 预编译库走镜像（tag 见 mnn-runtime-sys 的 prebuilt.rs::PREBUILT_TAG；
#    脚本自带 sha256 校验 —— 镜像给了错文件会直接报错，不会静默用坏库）
export MNN_PREBUILT_BASE_URL="$MIRROR/https://github.com/zibo-chen/mnn-native-prebuilds/releases/download/mnn-3.6.1-d407447e-r1"

# ③ 正常构建
TAURI_SIGNING_PRIVATE_KEY_PASSWORD=... scripts/release.sh --only-build
```

**首次成功后就不用再设了**：git 依赖进 `~/.cargo/git/checkouts/`，MNN 库进
`target/<triple>/release/build/mnn-runtime-sys-*/out/downloads/`，之后构建走缓存。

怀疑卡住时，先单独验证镜像上那个文件在不在（比等 10 分钟超时快得多）：

```sh
curl -sI "$MIRROR/https://github.com/zibo-chen/mnn-native-prebuilds/releases/download/mnn-3.6.1-d407447e-r1/mnn-3.6.1-d407447e-r1-macos-universal.tar.gz" | head -1
# 期望 HTTP/2 200，content-length=7468034
```

**附带一个排查技巧**：`cn` CLI 走的是 **macOS 系统代理设置**，而 `curl` 默认不走 ——
所以会出现"curl 一切正常、cn 一直超时"的假象。判断是不是代理的锅：

```sh
curl -x http://127.0.0.1:7897 -sI https://example.com | head -1   # 走代理；失败=代理本身挂了
scutil --proxy | grep -E "HTTPEnable|HTTPSEnable"                 # 系统代理是否开着
```

---

## 6. 发布前检查清单（每版必做）

```sh
# ① draft 里的资产与笔记（状态应为 Draft，5 项，签名 sig-ok）
cn release show aovik/xphoneuse <版本>

# ② macOS 包必须带 AirPlay 修复的 entitlement（见坑 5）
codesign -d --entitlements - /Applications/XPhoneUse.app          # 应含 disable-library-validation
#    检查 dmg 里的那份（挂载点显式指定，避免与残留挂载重名）：
hdiutil attach -nobrowse -readonly -mountpoint /tmp/xpudmg \
  src-tauri/target/universal-apple-darwin/release/bundle/dmg/XPhoneUse_<v>_universal.dmg
codesign -d --entitlements - /tmp/xpudmg/XPhoneUse.app
codesign -dvvv /tmp/xpudmg/XPhoneUse.app | grep flags               # adhoc,runtime
hdiutil detach /tmp/xpudmg

# ③ 发布后验证三个更新端点都指向新版本、签名 base64 正确
for p in darwin-aarch64 darwin-x86_64 windows-x86_64; do
  curl -s "https://cdn.crabnebula.app/update/aovik/xphoneuse/$p/1.0.0"; echo
done

# ④ 官网下载直链可用（应为 302）
curl -sI "https://cdn.crabnebula.app/download/aovik/xphoneuse/latest/XPhoneUse_<v>_universal.dmg" | head -1
curl -sI "https://cdn.crabnebula.app/download/aovik/xphoneuse/latest/XPhoneUse_<v>_x64-setup.exe" | head -1
```

---

## 7. 坑（都是踩过的）

1. **已发布的 release 不能追加资产** —— 报 `Failed to create asset for uploading: The
   release was already published`。补救：先在另一台机器上把该版本重新构建好，然后
   `cn release purge aovik/xphoneuse <版本>` → `cn release draft ...`（**同版本号可以重建**）
   → 重新上传全部资产 → 发布。
   Windows 资产若只在线上有，可先备份回来：`cn release show` 拿到 asset id，
   资产地址是 `https://cdn.crabnebula.app/asset/<asset_id>`。
2. **`.sig` 文件内容是 base64**（tauri-bundler 就是这么写的，cn 原样透传）。手工恢复
   签名时必须原样保存那串 base64 文本；**不要 base64 解码成裸 minisign 文本**再上传，
   否则 Windows 自动更新验签失败（`sig-ok` 检查会显示 `SIG-BAD`）。
3. **universal 更新包要注册两次**（`darwin-aarch64` + `darwin-x86_64`）：更新器按主机
   架构请求，只注册一个会导致另一半用户收不到更新。
4. **推送前确认 `CN_API_KEY` 属于 org 内有权写该应用的那把**；401 就是 key 问题，不是网络。
   进过 git 历史的 key 一律当泄露处理（新建 + 撤销旧的）。
5. **`src-tauri/entitlements.plist` 不能删**：app 在 hardened runtime 下 dlopen
   `uxplay.dylib`，而该 dylib 是 ad-hoc 签名 —— ad-hoc 没有 Team ID，Library Validation
   必然拒绝加载（`mapping process and mapped file (non-platform) have different Team IDs`），
   表现为**打包版 AirPlay 必失败、`tauri dev` 却正常**。
   `com.apple.security.cs.disable-library-validation` 是官方解法，公证也接受。
   改动签名配置后，务必用上面第 6 节的 ② 复验 dmg 和 `app.tar.gz` 两份产物。
6. **网络抖动**：`tls handshake eof` / `error sending request` 是常态，原样重试即可，
   脚本幂等（draft 会复用，重复上传同一资产不会重复建 release）。
7. **别在 dmg 构建里开 Finder 步骤**：脚本已内置 `CI=true`（headless 下会卡死）。
8. **不带私钥构建**会静默产出"只能安装、不能自动更新"的包。看到脚本输出
   `no TAURI_SIGNING_PRIVATE_KEY → updater artifacts skipped` 就说明漏了密码或 key。
9. **构建卡住超过几分钟，先怀疑 GitHub 依赖**（1.1.12 起）：`ocr-rs` 的 git fetch 和 MNN 的
    预编译库都要从 GitHub 取，被墙时的表现不是报错退出，而是**静默挂着**（详见第 5 节）。
    判定方法：`ps -o command= -p <cargo 的子进程>`，看到 `git fetch ... github.com` 或
    `curl ... github.com` 就是它。

---

## 8. 相关文件

| 文件 | 作用 |
|------|------|
| `scripts/release.sh` | 发布脚本本体（`--help` 看全部参数） |
| `src-tauri/tauri.conf.json` | 版本号 + 更新器端点/公钥 + macOS `entitlements` |
| `src-tauri/entitlements.plist` | hardened runtime entitlements（见坑 5） |
| `tauri.key`（gitignore） | 更新器私钥 |
