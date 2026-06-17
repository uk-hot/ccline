# @uk-hot/ccline

> 基于 [CCometixLine](https://github.com/Haleclipse/CCometixLine)（作者 [Haleclipse](https://github.com/Haleclipse)，MIT）的 fork。
> **在原版基础上，于「上下文窗口（context window）」段中增加了 prompt 缓存命中率（cache hit-rate）显示。** 其余功能与上游一致。

CCometixLine 是用 Rust 编写的高性能 Claude Code 状态行（statusline）工具。

## ✨ 与原版的区别

context window 段在原本的 `百分比 · token 数` 之后，追加显示**全会话累计**的缓存比例：

```
原版:  ⚡ 20.0% · 40.1k tokens
本版:  ⚡ 20.0% · 40.1k tokens · cache 96.4%
```

- 比例定义：`Σcache_read / Σ(input + cache_creation + cache_read)`，对 transcript 中**所有** assistant 消息的 token 累加，反映整个会话的缓存利用率。
- context 百分比 / tokens 仍取最近一条 assistant 消息（当前上下文占用），仅 cache 为会话累计。
- 无缓存数据时显示 `cache -`；同时写入 segment metadata：`cache_hit_rate`、`cache_read_tokens`。

## 📦 安装

```bash
npm install -g @uk-hot/ccline
```

安装后会自动把对应平台的二进制放到 `~/.claude/ccline/ccline`。在 Claude Code 的 `~/.claude/settings.json` 中配置：

```json
{
  "statusLine": { "type": "command", "command": "~/.claude/ccline/ccline", "padding": 0 }
}
```

中国大陆用户可用镜像加速：

```bash
npm install -g @uk-hot/ccline --registry https://registry.npmmirror.com
```

支持平台：macOS (x64/arm64)、Linux (x64/arm64，glibc 与 musl)、Windows x64 —— 与上游一致，由 GitHub Actions 跨平台编译发布。

## 🙏 致谢与许可

- 原项目：[CCometixLine](https://github.com/Haleclipse/CCometixLine) © Haleclipse
- 许可：MIT

本 fork 仅增加 cache 命中率显示，未改动其它行为。如需上游原版，请使用 [`@cometix/ccline`](https://www.npmjs.com/package/@cometix/ccline)。
