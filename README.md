# OnCue

macOS 字幕对齐 + 双语生成工具。Tauri 2 + React + Rust。

## 三种模式

- **快档对齐** — 视频开头 30s–120s 抽样 ASR，与 srt 做文本相似度匹配，仅修恒定偏移。2h 视频 ≤ 15s。
- **精档对齐** — 全片 ASR + 词级时间戳 + banded Needleman-Wunsch 对齐，重写每条字幕时间码。处理变速、断帧。
- **生成双语** — 全片 ASR + 分块 LLM 翻译（OpenAI/Anthropic 官方或兼容 endpoint），输出双语 srt。

## 开发

依赖：
- Bun ≥ 1.x
- Rust stable
- macOS 12+（M1/M2，CoreML 加速）

```bash
bun install
bun run tauri dev
```

首次启动会引导下载 Whisper 模型到 `~/Library/Application Support/com.oncue.app/models/`。

### 测试

```bash
cd src-tauri
cargo test --lib              # 36 个单测
cargo test --test generate_chunker   # 集成测试 (mock provider)
cargo test -- --include-ignored      # 需要本地 fixture 视频 + 模型
```

## 打包与分发

### 1. 构建

```bash
bun run tauri build --target aarch64-apple-darwin
```

产物：`src-tauri/target/aarch64-apple-darwin/release/bundle/dmg/OnCue_*.dmg`

### 2. 安装未签名版本

当前未做 Apple Developer 签名 / 公证，所以从浏览器或 Releases 页面下载的 `.dmg`
会被 macOS 标记为 quarantine，双击打开应用时可能出现：

> "OnCue.app" cannot be opened because Apple cannot check it for malicious software.
> （"OnCue.app"已损坏，无法打开。）

把 quarantine 属性去掉就能正常运行。两种方法二选一：

**方法 A — 命令行去 xattr（推荐，最快）**

```bash
# 装到 /Applications 之后执行：
xattr -dr com.apple.quarantine /Applications/OnCue.app
```

也可以直接对 `.dmg` 操作再装：

```bash
xattr -dr com.apple.quarantine ~/Downloads/OnCue_0.1.0_aarch64.dmg
```

`-dr` 是递归删除该 xattr。完成后双击运行即可。

**方法 B — 系统设置授权**

1. 双击 `.app`，弹出"无法打开"的提示框 → 点"取消"
2. 打开 **系统设置 → 隐私与安全性**
3. 滚到底部"安全性"区域，会出现 _"OnCue 已被拦截"_ 的一行 → 点 **仍要打开**
4. 再次确认即可

之后这台机器就记住了，不再询问。

> 后续如果做了 Apple Developer 签名 + notarytool 公证，这一节就可以删掉，
> 用户从 DMG 装完直接能跑。`scripts/notarize.sh` 已经留好脚本骨架。

## 目录约定

```
~/Library/Application Support/com.oncue.app/
├── models/              ggml-*.bin
├── settings.json        provider 配置 + 上次使用模型
└── logs/
```

## 许可证

- 应用代码：见仓库 LICENSE
- ffmpeg sidecar：LGPL build（来源：https://www.osxexperts.net/）
- whisper.cpp：MIT (https://github.com/ggerganov/whisper.cpp)
- Whisper 模型：MIT (HuggingFace / ggerganov/whisper.cpp)
