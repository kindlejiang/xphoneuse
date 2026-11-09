---
name: xphoneuse
version: 1.0.10
display_name: XPhoneUse 手机操作
display_name_en: XPhoneUse Phone Control
description: "Operate a real Android or iPhone as its user would — chat with people in messaging apps, browse shops and compare prices, order things, play games, fill forms, and check the results on screen. Use whenever a task involves interacting with a physical phone: tapping, typing, reading what's on the screen, launching apps, or verifying an outcome on the device."
description_zh: "像机主本人一样操作一台真实的 Android 或 iPhone：在聊天软件里给人发消息、逛商店比价下单、玩游戏、填表单，并自己看屏幕确认结果。凡是需要操作实体手机的任务——点击、输入、读屏、启动应用、验证操作结果——都应该用它。"
description_en: "Operate a real Android or iPhone as its user would — chat with people in messaging apps, browse shops and compare prices, order things, play games, fill forms, and check the results on screen. Use whenever a task involves interacting with a physical phone: tapping, typing, reading what's on the screen, launching apps, or verifying an outcome on the device."
---

# XPhoneUse — you are the hands and eyes on a real phone

Your job is not "to run commands" — it is to get the human's task done **on their actual device**: reply to grandma's message, find a cheaper pair of shoes, harvest the crops in the game. Everything you do lands on a phone someone owns. Act like a careful human: look before you tap, verify after you act, and never fire a destructive action you can't undo.

## The rig

- The **XPhoneUse desktop app** must be running on the computer (it owns the device connection and serves MCP on `http://127.0.0.1:7070/mcp`).
- The **CLI** is `scripts/xphoneuse-cli.py` next to this file — a single-file Python 3.8+ script with no pip dependencies (Pillow is used automatically when present for faster image comparison). Run it as `python <path-to-skill>/scripts/xphoneuse-cli.py <command>`; the examples below assume you `cd` into the skill directory and use `python scripts/xphoneuse-cli.py`.
- On **Windows, use `python`, never `python3`.** The `python3` on PATH is usually the Microsoft Store stub: it prints nothing, exits 49, and looks exactly like "the app is not running". `python --version` telling you nothing is the same stub.

Check the rig before anything else:

```bash
cd <path-to-skill>
python scripts/xphoneuse-cli.py list
# android:AKRSUT1611001468 - NTH_AN00 (device)
# ios:00008140-0000000000000000 - my-iphone
```

No output → check the interpreter first (the `python3` stub above produces exactly this silence), then: **the app is not running**, or no device is attached. If the app is not running, tell the user to download and start it from **https://xphoneuse.com** (Windows / macOS / Linux) — nothing here works without it, and the CLI's own error says the same. Then ask them to say when it is up; do not guess or retry in a loop.

## Doctrine

1. **Never act blind.** You must know what's on screen before touching it, and know what changed after. An unverified action is a coin flip on a real device.
2. **Read text before you look at pixels.** On Android, `ui-tree` and `find` return text and coordinates for the whole screen at ~zero token cost. On iOS there is no tree, but `ocr` reads the screen's text offline on the computer — use it instead of viewing the PNG. A screenshot is your most expensive sense — spend it only when neither can answer (games, maps, canvas, photos).
3. **One action, one verification.** Tap → check → tap. Batching five taps before looking once is how you end up in the wrong conversation.
4. **Type only into a field you've confirmed is focused.** Text lands in whatever has focus — if nothing does, you just typed into the void (or worse, into a chat you didn't mean to). On Android, `ui-tree` marks the focused view (`EditText [focused] (0.500, 0.938)`) — check that line before `text`; on iOS, look for the caret in a screenshot.
5. **Destructive actions get confirmed.** Sending a message, placing an order, deleting, paying, posting — anything that leaves the device or is hard to undo: state what you're about to do and get the user's go-ahead first, unless they already explicitly asked for exactly that action.
6. **Wait out animations (~0.5–1s).** Comparing a mid-transition frame produces garbage verdicts.
7. **To open an app, `launch` it — never hunt for its icon.** `launch <id>` brings the app to the foreground on both platforms (package name on Android, bundle id on iOS), from wherever the phone is: no home-screen pages, no folders, no Spotlight. Ids for the common apps are in [APP-IDS.md](APP-IDS.md), and `apps` lists what the device actually has (both platforms - search it by name, not just by id).

8. **Before typing, check the phone's keyboard.** Type through a composing IME (any Chinese or Japanese keyboard) and the keystrokes go into *it*: letters and spaces are recomposed, and what lands in the field is not what you sent — the classic casualty is a dropped space, which turns "test test" into "testtest". **The keyboard must be the English one, or the AgentInput one.** Look at it in a screenshot before typing: `AgentInput` on the keys means the app's own route (best - no alerts, clipboard untouched), plain English letters means the HID keyboard is safe, and `中/英` with a candidate row means a composing IME — switch it (globe key, bottom-left of the keyboard) or ask the user to, and only then type. Non-ASCII text is not affected: it is pasted or inserted, never typed key by key.

## Seeing: the cheap senses first

### Android: the UI tree (preferred)

```bash
python scripts/xphoneuse-cli.py find --text "Sign in" --clickable
# Sign in  clickable  (0.512, 0.873)  bounds 140-405, 2201-2272
```

`find --text T [--id I] [--class C] [--clickable]` returns normalized coordinates ready for `touch`. `ui-tree` dumps the whole screen — use it when you don't know what's there or need to read content (prices, message text, list items). Reading text out of the tree is ~10× cheaper than reading it out of a screenshot.

### iOS: OCR (the tree substitute)

iOS has no UI tree, so **`ocr` is how you read an iPhone** — it recognizes the text on screen on the computer (PaddleOCR, offline, nothing leaves the machine) and hands you back lines, no image tokens spent:

```bash
python scripts/xphoneuse-cli.py ocr                       # captures the screen and reads it
# 9:41
# 奶奶
# 好的，周末回家吃饭
# 发送
python scripts/xphoneuse-cli.py ocr --json                # same, with per-line confidence and boxes
# {"width":1179,"height":2556,"lines":[{"text":"发送","confidence":0.99,"bbox":[1030,2380,90,60]}, …]}
python scripts/xphoneuse-cli.py ocr --path shot.png       # read a local file instead of the screen
```

- **It works on Android too**, but there prefer the tree — it also gives you clickability and exact bounds.
- `--json` gives each line's `bbox` `[x, y, w, h]` **in screen pixels**, plus the image `width`/`height` — divide by them to get the normalized coordinates `tap` wants (see Recipes).
- `--min-confidence` (default 0.5) drops shaky lines; lower it when the text is faint, raise it when OCR hallucinates on artwork.
- `ocr` reads text only. It cannot tell you a button is a button, or what an icon means — for layout and icons, screenshot.
- An empty result is not an error: the screen simply has no text (a game canvas, a photo).

### Both platforms: screenshots with local comparison

The CLI diffs images locally, so you only pay image tokens when something actually changed:

```bash
python scripts/xphoneuse-cli.py screenshot                      # spend tokens once: view the image
python scripts/xphoneuse-cli.py press home
python scripts/xphoneuse-cli.py screenshot --compare xphoneuse-screenshot.png
# VERDICT: CHANGED (86.95% of pixels differ, threshold 0.50%)
# changed region: x 0-1178, y 0-2555 (normalized 0.000-1.000, 0.000-1.001)
```

- **UNCHANGED** → trust your mental model, don't view anything.
- **CHANGED** → view the new image (the command overwrites it) and keep it as the next reference.
- A plain `screenshot` is a **scaled JPEG** (1568px on the longest side): a phone capture is
  megabytes of PNG and looking at it does not need them. `--compare` captures a **lossless PNG**
  instead, which is what its diffing wants; the MCP tool spells the same choice as
  `format: "png"`, and `max_dimension: 0` keeps the capture's own resolution (e.g. for reading
  small text off the image).
- The `changed region` line alone often tells you what happened — "the bottom third changed" = keyboard opened; "a small box in the middle" = a dialog appeared.
- Tuning: `--threshold 2.0` (percent of pixels that must differ), `--tolerance 32` (per-channel noise). Clocks, spinners and cursors stay under the 0.5% default.
- `compare a.png b.png` works without a device.

### When to use vision

Neither the tree nor OCR can see: game canvases, maps, photos, videos, custom-drawn UI. There, loop on screenshots: capture → decide → act → `--compare`. For reaction-speed games, accept UNCHANGED verdicts between moves instead of viewing every frame.

## Acting

```bash
python scripts/xphoneuse-cli.py connect <serial-or-udid> --platform android|ios
```

`connect` routes through the desktop app: its window switches to the mirror screen, and input is shared — the user can operate the phone at the same time as you. If the app is already mirroring, `connect` adopts the session instantly. `disconnect` only releases your view; the app keeps mirroring.

On iOS the transport is automatic: iOS 27+ uses the **USB tunnel** (screen, input and ~0.7s screenshots over the cable); older iOS falls back to AirPlay mirroring + the Bluetooth HID bridge — for those, see the failure playbook for the phone-side steps. The tunnel's services come from a **developer image** the app installs on the phone, and the phone only accepts that install with **Developer Mode** on. That is the app's job, not yours: it prepares the image, and when the switch is off it **asks the user first** (a dialog on the desktop side, with a countdown, because turning it on restarts the iPhone once). Two things can leave the last step to the user — an iPhone with a passcode refuses the request from the computer outright (iOS 27), and the Developer Mode row is not even shown in Settings until the app reveals it — and both are the app's dialog's business, not yours. Your part: say the phone needs Developer Mode, let them finish it on the phone, and reconnect. Never retry in a loop.

### The verbs

| Verbs | Notes |
|---|---|
| `tap <x> <y>` | **The default way to touch anything.** Normalized coordinates: (0,0) top-left, (1,1) bottom-right. One command, one press — no long-press risk. |
| `longtap <x> <y> [ms]` | Long-press at a point: down, hold `ms` milliseconds (default 800, range 100-10000), up. Use for context menus, rearranging icons, "press and hold" interactions — otherwise prefer `tap`. |
| `touch down\|up\|move <x> <y>` | Raw touch events, for drags only. Two separate commands (`down` then `up`) with latency in between read as a long-press — don't use them for taps, and prefer `longtap` over hand-rolled timing. |
| `text <TEXT> [--no-enter]` | Full Unicode and multi-line on both platforms. Android takes any text directly. On iOS, ASCII is typed key by key and anything else is delivered the way the app is configured for (device clipboard, or the AgentInput keyboard) — see Typing rules. **Enter is pressed by default, and that is the point: the message is sent.** In chat apps Enter *is* the send button, so `text "..."` is the whole job — do not add `--no-enter` to it. `--no-enter` drafts instead, for the cases where the words themselves still need a look (see Typing rules). |
| `unicode-text <TEXT> [--method clipboard\|agentinput] [--no-enter]` | Same typing, but iOS non-ASCII delivery is chosen per call instead of using the app's setting. `clipboard` = device pasteboard + Cmd+V (nothing to install; may pop the system Allow-Paste alert, which someone has to tap, and it replaces the device clipboard). `agentinput` = the AgentInput keyboard types it (no alerts, clipboard untouched; the keyboard must be installed and active, otherwise the app falls back to the clipboard and says so in its reply). ASCII is typed either way, and Android ignores the method. Omitting `--method` leaves the choice to the app's setting. |
| `press home\|back\|recents\|power\|volume_up\|volume_down` | iOS has no `back`; `recents` is the app-switcher gesture. |
| `scroll <x> <y> <dx> <dy>` | dy negative scrolls down. |
| `swipe <fx> <fy> <tx> <ty>` | |
| `shell <CMD>` | Android only: any adb shell command — battery, packages, settings, `input` injection. |
| `launch <ID>` | **How you open an app, on Android and iOS alike.** The identifier is the package name on Android (`com.tencent.mm`), the bundle id on iOS (`com.tencent.xin`) — see [APP-IDS.md](APP-IDS.md). The app comes to the foreground the same way tapping its icon does. If it is already running it is restarted (that is the API's behaviour, not a bug). Needs the iPhone's developer image on iOS, which the app prepares itself. |
| `apps [--filter S] [--kind user\|system\|all]` | What is installed, with the identifiers `launch` takes. Android answers with package names (`pm list packages`); iOS with bundle id, name and version, so `--filter 微信` works there. **Filter it** - a full list is hundreds of lines. |
| `open-url <URL>` | Android only. For a URL, prefer this over launching a browser by id. |
| `ocr [--path FILE] [--json] [--min-confidence C]` | Read the screen's text with offline OCR on the computer — **this is how you read an iPhone**, and with `--path` it reads any image file instead of the screen. Plain text lines by default; `--json` adds per-line confidence and pixel boxes. See "iOS: OCR". |
| `ui-tree` / `find ...` / `rotate` | Android only. |
| `macro record\|play\|...` | Record a flow once, replay it forever - on Android and iOS alike. A macro replays the exact touches and text as they happened, so start it from the same screen (see Recipes). |
| `files [PATH]` / `push LOCAL [DIR]` / `pull PATH [-o DIR]` | Move files between this computer and the phone. Locations are `platform:/path` (`android:/sdcard/DCIM`, `ios:/DCIM`) or a bare path for the connected phone; **on iOS the media area is all there is** (`/var/mobile/Media`). Nothing is overwritten: an existing name becomes `name (1).ext` and the reply says which. **On iOS only `pull` is worth much** - the media area is invisible to the phone's apps, so a pushed file cannot be opened there. Needs an active session. |
| `wifi enable\|connect\|disconnect\|ip` | Android only. |

`screenshot`, `shell`, `list`, `wifi *` and `ocr --path FILE` work without `connect`; the input
verbs and the file commands need an active session (`ocr` on its own needs a device to capture from).

### Typing rules

- Confirm the field is focused first (see Doctrine #4). A reliable tap-then-type: tap the field, then check the tree for the focused/cursor state, then `text`.
- **Check which keyboard is up before typing (Doctrine #8).** On iOS, screenshot and read the keys: the word `AgentInput` means the AgentInput keyboard (use `unicode-text --method agentinput`), an English QWERTY means HID typing is safe, and a Chinese keyboard with a candidate row (`中/英`, 候选字) will intercept the keystrokes — letters land in its composing buffer and space commits instead of typing a space. Switch first (the globe key at the keyboard's bottom-left, or 设置 > 通用 > 键盘 for the order) or ask the user to; on Android the phone's IME intercepts `text` the same way. Then type, then verify with a screenshot: if characters are missing or merged, this is why.
- Long text is fine; newlines are delivered as-is on Android.
- **Do not add `--no-enter`. Typing sends, by default, and that is what you are here for.** `text "..."` types the message and presses Enter, and in a chat app Enter is the send button — one command, message delivered. Drafting is the exception, not the cautious habit: reach for `--no-enter` only when the words themselves are still in question (the user asked you to *write* something, the phrasing is sensitive, or Doctrine #5 applies because nobody has asked for this message at all). When they did ask, send it — "type it, then ask again" turns one step into two and makes the agent look like it is second-guessing.
  - Drafting, when it is warranted: `text "..." --no-enter` puts the words in the field, a screenshot verifies them, and `text ""` (an empty text just presses Enter) or the app's own send button finishes the job.
- **Non-ASCII on iOS: two delivery methods.** The HID keyboard types ASCII only, so anything else travels either as a **clipboard paste** (the app sets the device pasteboard and presses Cmd+V) or through the **AgentInput keyboard** (it types the text for the app). Which one the app uses by default is a setting on the desktop side; `unicode-text --method clipboard|agentinput` picks one for a single call.
  - Deliver with `unicode-text`, not `text`, whenever the choice matters — e.g. the Allow-Paste alert is in the way, or the clipboard must be left alone.
  - **If the keyboard on screen reads "AgentInput", the user has AgentInput installed — use it.** That keyboard app is something a person deliberately installed, so seeing it (in a screenshot, or in the app's own status) means the AgentInput route is the one to take: `unicode-text --method agentinput`. It beats the clipboard on both counts that matter — no Allow-Paste alert, and the device clipboard is left alone. Switching keyboards costs a moment, and the one that is up decides how typing goes, not this bullet - see Doctrine #8 for which keyboards are safe to type through.
  - `clipboard`: nothing to install, but it **replaces the device clipboard** and iOS asks to allow the paste the first time in each app. The reply says `via: clipboard paste`.
  - `agentinput`: no alerts and the clipboard is untouched, but the AgentInput keyboard must be installed and be the active keyboard on the phone. Check it with a screenshot: its keyboard shows the word **"AgentInput"**. The reply says `via: AgentInput`.
  - If AgentInput is asked for but unreachable (not installed, or not the active keyboard), the app falls back to the clipboard and says so in the reply — read the reply rather than assuming the clipboard is untouched.
  - Either way: verify the characters landed with a screenshot comparing against the previous frame.
- **The iOS paste-permission alert.** Pasting into an app over the tunnel can pop the system dialog “Allow Paste?” /「允许粘贴」— every app does this on the first paste, including Apple's own. Don't treat it as a failure:
  1. Screenshot: the dialog sits over the target field. Tap its confirm button (**Allow Paste** / 「允许粘贴」, usually bottom-right of the alert) — a plain `tap` works, it's a normal on-screen button.
  2. Screenshot again to verify the text actually landed; if it didn't (the paste was declined or dropped), resend the `text` command.
  3. For apps the user works in often, suggest the one-time permanent fix: **设置 > [App 名] > 从其他App粘贴 > 允许** (Settings > [App] > Paste from Other Apps > Allow). Spotlight and other system surfaces have no such setting — there you always have to tap through the alert.
  4. ASCII-only text never pastes (typed key by key), so it never triggers the alert — and neither method applies to it.

## Recipes

Every recipe below assumes a session is already open (`list` → `connect <serial>`). Android and
iOS differ more than the verb table suggests, so each one is split where it matters:
**Android has a UI tree** (`find`, `ui-tree`, `launch`) and iOS does not — on iOS `ocr` is what
stands in for it, and screenshots cover what OCR cannot (layout, icons, images).

### Open an app

```bash
python scripts/xphoneuse-cli.py launch com.tencent.xin     # 微信, iOS bundle id
python scripts/xphoneuse-cli.py launch com.tencent.mm      # 微信, Android package
python scripts/xphoneuse-cli.py ocr                        # confirm where you landed
```

Do this instead of navigating to the app: no icon hunting, no Spotlight, and
it works from any screen. The phone needs to be unlocked for the app to be
usable, and on iOS the launch needs the developer image (the app prepares it;
if the image cannot be installed the reply says so and names Developer Mode).
Ids for the common apps are in [APP-IDS.md](APP-IDS.md), and `apps --filter
<name>` lists what the device actually has (on iOS by display name too, so
"微信" finds `com.tencent.xin`). Reach for those before tapping through a home
screen looking for an icon.

### Chat with someone (messaging app)

The whole job is: open the right conversation, compose, **check what you composed**, send.
Sending is irreversible (Doctrine #5), so the verification before it is not optional.

**Android**

```bash
python scripts/xphoneuse-cli.py launch com.tencent.mm
python scripts/xphoneuse-cli.py find --text "奶奶" --clickable   # the conversation, by name
# 奶奶  clickable  (0.512, 0.873)  bounds 140-405, 2201-2272
python scripts/xphoneuse-cli.py tap 0.512 0.873
python scripts/xphoneuse-cli.py ui-tree | head -40               # read the header: right chat?
python scripts/xphoneuse-cli.py find --class EditText            # the message box
# EditText  (0.500, 0.938)  bounds 24-1155, 2300-2400
python scripts/xphoneuse-cli.py tap 0.500 0.938                  # focus it
python scripts/xphoneuse-cli.py ui-tree | grep -i focused        # EditText [focused] (…) → go
python scripts/xphoneuse-cli.py text "好的，周末回家吃饭"        # types it and sends it
python scripts/xphoneuse-cli.py screenshot --compare xphoneuse-screenshot.png
#   CHANGED → the bubble went out; UNCHANGED → nothing was sent, look before retrying
```

Adding `--no-enter` instead of sending is the exception: it is for when the words still need a
look (nobody has asked for this message, the phrasing is delicate). Then `text ""` — an empty
text presses Enter on its own — or the app's send button finishes it.

**iOS** (no tree: `ocr` to find the text, screenshots for layout, and tap the app's own send button)

```bash
python scripts/xphoneuse-cli.py connect <udid> --platform ios
python scripts/xphoneuse-cli.py screenshot                       # look once, note the layout
python scripts/xphoneuse-cli.py tap 0.50 0.30                    # the conversation row
python scripts/xphoneuse-cli.py screenshot --compare xphoneuse-screenshot.png
python scripts/xphoneuse-cli.py tap 0.50 0.96                    # the input field at the bottom
python scripts/xphoneuse-cli.py unicode-text "好的，周末回家吃饭" --method agentinput
#   typed and Enter pressed: the message is sent. (--no-enter drafts instead, and then the
#   app's own send button at the bottom-right of the input row finishes it.)
python scripts/xphoneuse-cli.py screenshot --compare xphoneuse-screenshot.png   # bubble went out?
```

Notes that save a retry either way:

- **Sending is the default.** `text "..."` types the message and presses Enter, and in a chat
  app Enter *is* send — one command, delivered. `--no-enter` is for the exceptions (the words
  themselves still need a look); `text ""` then presses Enter on its own.
- On iOS the non-ASCII delivery method is a setting; `unicode-text --method agentinput` avoids
  the Allow-Paste alert, `--method clipboard` needs no app on the phone (see Typing rules). When
  the phone's keyboard reads "AgentInput", the user has it installed — use it.
- Confirm the conversation header before typing — `ui-tree` on Android, `ocr` on iOS.
  "奶奶" and "妈妈" are one tap apart.
- If the paste-permission alert appears, tap **允许粘贴 / Allow Paste** first, then re-check the field.

### Read something off the screen

Answering "what's the code / price / status" needs no taps at all.

**Android — read the tree, not the pixels** (roughly 30× cheaper in tokens):

```bash
python scripts/xphoneuse-cli.py ui-tree | grep -i "验证码\|余额"     # narrow it to the line you need
python scripts/xphoneuse-cli.py find --text "验证码"                # or get it as an element with bounds
```

**iOS — read the screen with `ocr`, not with your eyes** (also ~30× cheaper):

```bash
python scripts/xphoneuse-cli.py ocr                                 # every line of text on screen
python scripts/xphoneuse-cli.py ocr | grep "验证码"                  # narrow it to the line you need
python scripts/xphoneuse-cli.py ocr --json                          # boxes too, if you must tap what you read
```

Report the value as text. Keep a screenshot only if the user asked for evidence, or if what you
need is not text (an icon, a chart, a photo, the layout of a dialog).

### Tap a thing by its text (iOS's `find`)

There is no `find` on an iPhone, but `ocr --json` gives pixel boxes — turn one into a `tap`:

```bash
python scripts/xphoneuse-cli.py ocr --json
# {"width":1179,"height":2556,"lines":[{"text":"允许粘贴","confidence":0.99,"bbox":[700,1380,300,90]}, …]}
# centre = (700 + 300/2, 1380 + 90/2) = (850, 1425);  normalized = (850/1179, 1425/2556) = (0.721, 0.557)
python scripts/xphoneuse-cli.py tap 0.721 0.557
python scripts/xphoneuse-cli.py ocr | grep "允许粘贴"                # gone → it worked
```

Divide by the `width`/`height` the same reply reports, never by numbers you remember: the image
is the device screen, and the two change with rotation. Matching a line you did not expect? Read
all of them — OCR happily reads the text *behind* a dialog too.

### Compare prices

Two shops, same product; the numbers come from the tree, the image is only the receipt.

```bash
python scripts/xphoneuse-cli.py open-url "https://item.jd.com/…"    # or launch the shop app
python scripts/xphoneuse-cli.py find --text "¥"                     # price text and its bounds
python scripts/xphoneuse-cli.py screenshot -o jd.png                # evidence #1
python scripts/xphoneuse-cli.py open-url "https://detail.tmall.com/…"
python scripts/xphoneuse-cli.py find --text "¥"
python scripts/xphoneuse-cli.py screenshot -o tmall.png             # evidence #2
```

`find` returns every match, so check which one is the price the buyer actually pays (promo
price vs. struck-through list price, per-unit vs. per-pack). On iOS there is no `find`: use
`ocr` to read each shop's numbers (a screenshot only if you need the receipt).

### Play a game (vision loop)

Games are canvases: the tree sees nothing, so this is the one place to spend screenshots freely.

```bash
python scripts/xphoneuse-cli.py screenshot                          # establish the scene once
# then loop: decide from the current image → act → cheap verify
python scripts/xphoneuse-cli.py tap 0.73 0.62
python scripts/xphoneuse-cli.py screenshot --compare xphoneuse-screenshot.png
#   UNCHANGED → the tap probably missed, or the game ignored it; try once more, then re-look
#   CHANGED   → view the PNG only if the change matters (score, new screen, new state)
```

Loosen the verdict when the game animates on its own: `--threshold 3 --tolerance 32` keeps
spinners, timers and particle effects from reading as CHANGED. For reaction-speed games, act on
the previous frame instead of re-viewing after every tap. To leave a game: `press home`
(`press back` does not exist on iOS).

### Fill a form / search

One field at a time: locate → tap → **confirm focus** → type → next.

```bash
python scripts/xphoneuse-cli.py find --text "收货人"                 # the label
python scripts/xphoneuse-cli.py tap 0.5 0.32                        # the field next to it
python scripts/xphoneuse-cli.py ui-tree | grep -i focused           # EditText [focused] (…)?
python scripts/xphoneuse-cli.py text "张三"
python scripts/xphoneuse-cli.py scroll 0.5 0.5 0.0 -0.5             # submit button below the fold
python scripts/xphoneuse-cli.py find --text "提交"                  # re-find AFTER scrolling
python scripts/xphoneuse-cli.py tap 0.5 0.78
python scripts/xphoneuse-cli.py screenshot --compare xphoneuse-screenshot.png
```

Two traps: coordinates from before a scroll are stale (always re-`find`), and typing into an
unfocused field goes nowhere — or worse, into whatever else had focus.

### Automate a repeated flow

Record once, replay forever, on either platform:

```bash
python scripts/xphoneuse-cli.py macro record send_daily_report
# …do the flow once, verifying each step as usual…
python scripts/xphoneuse-cli.py macro stop
python scripts/xphoneuse-cli.py macro play send_daily_report            # replay
python scripts/xphoneuse-cli.py macro play send_daily_report --repeat 3 # or several times
python scripts/xphoneuse-cli.py macro list                              # what exists
python scripts/xphoneuse-cli.py macro delete send_daily_report
```

Macros capture touches, text and buttons exactly as they happened — coordinates included. They
do not re-run `find`, so a replay only works from the same starting screen: always begin from
the state you recorded on, and re-record when the app's layout changes. `macro play` does
everything for real, so treat it like any other action on someone's phone.

Both platforms record and replay: taps, long-presses, scrolls, swipes, typed text and button
presses. On iOS a recorded `recents` replays as the app-switcher gesture, and text goes back out
through the same delivery the app is configured for (clipboard or AgentInput) — if the
transport cannot carry the characters it says so in the playback error instead of typing
something shorter. On iOS there is no `find`, so a macro is pure coordinates: expect to re-record
it after an app update.

### Move a file on or off the phone

```bash
# Where things are. A bare path means the connected phone; iOS's whole tree is
# the media area, so `ios:/` is the camera roll's grandparent.
python scripts/xphoneuse-cli.py files                       # the phone's files root
python scripts/xphoneuse-cli.py files /sdcard/DCIM -f mp4   # only what matches
python scripts/xphoneuse-cli.py files ios:/DCIM

# Off the phone: name a directory on this computer to keep it.
python scripts/xphoneuse-cli.py pull /sdcard/DCIM/123.mp4 -o ./out
python scripts/xphoneuse-cli.py pull ios:/DCIM/100APPLE/IMG_0001.HEIC -o ./out

# Onto the phone: the name is kept, and the reply says where it went.
python scripts/xphoneuse-cli.py push ./report.pdf /sdcard/Download
```

- Locations are `platform:/path` (`android:/sdcard/DCIM`, `ios:/DCIM`), or a bare path for the
  connected phone. The prefix is worth using in a transcript: `/DCIM` exists on both platforms
  and means different files.
- **Nothing is overwritten, in either direction.** A pull that would land on a local file of the
  same name becomes `name (1).ext`; a push does the same on the phone. The reply names the file
  that was actually written — use that name, don't assume the one you asked for.
- **On iOS the direction worth using is pulling.** AFC reaches the media area
  (`/var/mobile/Media`: the camera roll under `DCIM`, `Downloads`, `Books`…) and nothing else, and
  it is the *iTunes-era* area, which the phone's own apps do not read. Measured on iOS 27: a photo
  pushed into `DCIM/113APPLE/IMG_9001.PNG` never appeared in Photos, and a file pushed into
  `Downloads` never appeared in the Files app (that folder belongs to itunesstored — App
  Store/iBooks — not to Files). So **pull an iPhone's photos and files freely; do not push files
  onto one expecting an app to see them.** To get a file into an app (WeChat, Files…), use that
  app's own channel: WeChat on the computer, AirDrop, iCloud. It needs no Developer Mode, so the
  pull side works on a phone this session can only watch.
- Directories are listed, not copied: `files` into one, and move the files in it.
- Phones allow names this computer does not (`:`, `?`, trailing dots); a download substitutes
  them, so the local name can differ by a character from the one on screen.

### Answer a permission dialog

System dialogs sit above the app and swallow input until they are answered.

```bash
python scripts/xphoneuse-cli.py screenshot --compare xphoneuse-screenshot.png
#   a small box in the middle → a dialog appeared
python scripts/xphoneuse-cli.py screenshot                          # view it and read the buttons
python scripts/xphoneuse-cli.py tap 0.66 0.58                       # the confirm/allow button
python scripts/xphoneuse-cli.py screenshot --compare xphoneuse-screenshot.png   # gone?
```

The common ones: iOS **Allow Paste / 允许粘贴** when the clipboard method types (see Typing
rules — tapping Allow is the fix, and per-app settings make it permanent), Android runtime
permission prompts on first launch, and the "Allow USB debugging" prompt on the phone itself
(that one needs a human). Never tap through a dialog you have not read — some of them are the
app asking for real consent.

## Failure playbook

| Symptom | Remedy |
|---|---|
| `No device connected` | `connect` first; if that fails, `list` and check the rig. |
| Nothing on `list`, or the CLI says the request failed | The desktop app isn't running (or no device is attached). Tell the user to start it — and if they don't have it, to download it from **https://xphoneuse.com**. Nothing works without it; don't retry in a loop. |
| iOS screenshot slow or `dvt channel unusable` | Screenshots prefer the live mirror frame and the USB tunnel capture (~0.7s) before DVT. A DVT failure means neither is up: retry once, then ask the user to replug the USB cable (tunnel) or start AirPlay mirroring (Control Center → Screen Mirroring → XPhoneUse). A *first* screenshot on an iPhone can take a while (plus a download) while the app prepares the developer image the service comes from: iOS 16 and older need one for `screenshotr` too, so it is not always a fault. |
| iOS input errors (tunnel or HID bridge) | No input transport is attached — the session is view-only. On iOS 27+ replugging the cable usually brings the tunnel back (reconnect); on older iOS the Bluetooth bridge must be powered on. Tell the user; don't retry input. |
| iPhone mirror falls back to AirPlay (blurry, no input) | The phone's developer image is missing or stale, usually after an iOS update. The app installs a current one by itself, and turns **Developer Mode** on itself when that is the blocker — which restarts the iPhone once. If the connect ends in "the iPhone is restarting to turn on Developer Mode", that is the app doing it: tell the user to unlock the phone once it has booted and connect again. Only if the app reports Developer Mode is still off should the user flip it by hand (Settings → Privacy & Security → Developer Mode). Don't retry in a loop. |
| `ocr` reports it cannot find the models | The app's install is incomplete (the OCR models ship inside it). Reinstall the app; if you are running it from a build tree, point `XPHONEUSE_OCR_MODELS` at a directory holding `PP-OCRv6_small_det.mnn`, `PP-OCRv6_small_rec.mnn` and `ppocr_keys_v6_small.txt`. The error names the directory it looked in. |
| `ocr` returns nothing on a screen that clearly has text | Raise the sensitivity with `--min-confidence 0.3` (faint or small text), or the screen really is a canvas/photo. Fall back to a screenshot to see why. |
| Verdict UNCHANGED after a tap | Probably missed (wrong coords, element moved) or a no-op button. Re-locate with `find`/tree and try once more before escalating. |
| Text didn't appear | Field wasn't focused — tap the field, re-check focus, retype. On iOS over the Bluetooth bridge with non-ASCII, also check the keyboard is AgentInput (its UI shows "AgentInput"); on the USB tunnel, non-ASCII should just work. If the paste-permission alert was up, tap **Allow Paste** /「允许粘贴」first, then re-check (see Typing rules). |
| `push`/`pull` on iOS says the iPhone is locked | iOS refuses its file service while the screen is locked (and while it has never been trusted by this computer). Unlock the phone, tap Trust if it asks, and run it again. |
| `pull` on an Android path says `Permission denied` | That directory is not readable by the adb shell user — `/data`, other apps' private storage. `/sdcard` and everything below it is, which is where user files live. |
| `push` to iOS lands somewhere unexpected, or fails | iOS is writable inside the media area only: `ios:/`, `/DCIM`, `/Downloads`. An app's own folder is out of reach. |
| Tool call fails with a transport error | The app may have restarted. `connect` again and resume from your last verified state. |

## Safety

- The phone is real and probably someone's daily driver. Destructive/irreversible actions (send, order, pay, delete, post, call) need explicit user intent — ask when in doubt.
- Logins, payments, and password fields: hand the phone to the user. Don't type credentials even if asked casually; offer to navigate there and let them type.
- Android `text` with non-ASCII or newlines **replaces the device clipboard** — fine for automation, but mention it if the user just copied something.
- Non-zero exit codes mean the tool failed; the message explains why. Don't ignore them and assume success.
- The MCP endpoint is local; the CLI ignores system proxies.
