# ✂️ CliReqRefiner

[![LINUX DO 社区](https://img.shields.io/badge/首发于-LINUX%20DO%20社区-000000?logo=linux&logoColor=white)](https://linux.do/)

<div align="center">

**高性能 AI 编码工具请求体精炼代理**

> ⚠️ **适配状态**：当前已完成 **Claude Code** 的适配，**Codex 尚未适配**，后续会逐步支持。

请求体精炼 · 多上游负载均衡 · 热重载配置 · Token 成本削减

[![Rust](https://img.shields.io/badge/rust-1.85+-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-GPLv3-blue.svg)](LICENSE)

[![跨平台](https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20macOS-lightgrey.svg)](https://github.com/rust-lang/rust)

</div>

---

## 📖 简介

**CliReqRefiner** 是一款专为 AI 编码工具（Claude Code、Codex 等）设计的高性能 API 代理，核心聚焦于**请求体精炼**以降低 token 消耗。

它可以帮助你：
- ✂️ **精炼请求体** — 移除冗余系统提示词、裁剪工具定义、优化上下文结构
- 🌐 **对接多个上游服务商**，自动负载均衡
- 💰 **降低 API 成本** — 智能拦截非必要请求 + 请求体优化
- ⚡ **加速响应** — 本地处理部分优化请求
- 🔧 **零停机配置** — 修改配置即时生效

### 💡 为什么需要它？

AI 编码工具（Claude Code、Codex 等）在使用过程中会发送大量"探测性"请求（如配额检查、标题生成、建议模式等），这些请求消耗 token 却对实际开发帮助甚微。CliReqRefiner 能够智能识别并拦截这些请求，直接返回本地模拟响应——在保持工具完整功能的同时，大幅降低 token 消耗。

此外，CliReqRefiner 还会**精炼系统提示词和工具定义**，进一步降低 token 用量。实际使用中，AI 编码工具发送的请求往往包含大量预设系统提示词和工具定义，它们在每次请求中重复发送，消耗大量 token。CliReqRefiner 可以：
- 📝 **移除冗余系统提示词**，仅保留核心指令
- 🔧 **裁剪工具定义**，仅保留关键工具描述
- 📊 **优化上下文结构**，减少重复信息

这种双重优化策略，使 CliReqRefiner 能在保持完整功能的前提下，将 API 调用成本降至最低。

---

## ✨ 功能特性

### ✂️ 请求体精炼（核心）

CliReqRefiner 的核心功能——在转发到上游之前精炼请求体：
- 📝 **系统提示词优化** — 移除冗余系统提示词，仅保留核心指令
- 🔧 **工具定义裁剪** — 精简冗长的工具描述
- 📊 **上下文结构优化** — 减少请求间的重复信息
- 🎯 **细粒度控制** — 每项优化可独立启用/禁用

### 🔥 热重载配置

- 配置文件变更时**自动热重载**，无需重启
- 通过 `notify` crate 实现跨平台文件监听
- 平滑切换配置，服务不中断

### ⚡ 本地优化拦截

智能识别特定请求并在本地处理，减少上游调用：

| 优化项 | 说明 |
|:-------|:-----|
| 🔍 **配额检查拦截** | 对配额探测请求返回本地模拟响应 |
| 📝 **快速前缀检测** | 识别并提取命令前缀（如 `git commit`） |
| 📋 **标题生成跳过** | 对标题生成请求返回默认响应 |
| 💡 **建议模式跳过** | 对建议模式请求返回空响应 |
| 📂 **文件路径提取** | 从命令输出中提取文件路径 |
| 📊 **历史分析跳过** | 对历史分析请求返回简化响应 |

### 🔄 多上游负载均衡

- 支持多个上游服务商
- **双层轮询**：上游之间轮询，每个上游内的 API Key 之间也轮询
- API Key 自动轮换，最大化请求分发

### 📊 请求统计与监控

- 实时请求计数和 token 消耗统计
- 区分用户输入 token、上下文 token 和助手响应 token

---

## 🚀 快速开始

### 🎯 配置 Claude Code CLI

在 Claude Code CLI 配置中设置 API 端点：

```bash
# 方式一：环境变量
export ANTHROPIC_BASE_URL="http://127.0.0.1:9066/claude"
```

或在 `~/.claude/settings.json` 中：

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:9066/claude",
    "ANTHROPIC_AUTH_TOKEN": "随便填"
  }
}
```

其中：
- `ANTHROPIC_BASE_URL` 指向 `http://127.0.0.1:9066/claude`
- `ANTHROPIC_AUTH_TOKEN` 可以随意填写——CliReqRefiner 转发时会覆盖它
- 如果你修改了 `config.toml` 里的 `port`，这里也要同步改成对应端口

### 📦 构建

```bash
# Debug 模式
sh build_native_stable.sh

# Release 模式（推荐生产使用）
sh build_native_stable.sh r
```

### ⚙️ 配置

编辑 `config.toml`：

```toml
# 负载均衡配置示例
# 可配置多个上游，每个上游支持多个 API Key
# 策略：上游之间轮询，API Key 之间轮询
# 修改后即时生效

# 服务监听端口（默认 9066，修改后需重启服务）
port = 9066

# 是否打印请求体
log_req_body = false
# 是否打印响应体
log_res_body = false

# 上游 1
[[upstream]]
enable = true
base_url = "https://open.bigmodel.cn/api/anthropic"
model = "glm-4.7"
api_keys = ["your_api_key1", "your_api_key2"]
user_agent = "Claude-Code/1.0.84 (Linux; Android 14)"
# mode 默认为 "anthropic"，也支持数组，例如 ["anthropic", "openai_responses"]
# 设置 enable = false 可临时禁用该上游
# 如需同时兼容多种协议，可设置 mode = ["anthropic", "openai_responses"]

# 上游 2：添加更多上游实现负载均衡
# [[upstream]]
# enable = true
# base_url = "https://another-provider.com/api/anthropic"
# model = "claude-3-5-sonnet-20241022"
# api_keys = ["your_key"]
# user_agent = "Claude-Code/1.0.84 (Linux; Android 14)"
# mode = ["anthropic", "openai_responses"]  # 可选: "anthropic" | "openai_responses" | "openai_chat"

[optimizations]
enable_network_probe_mock = true
enable_fast_prefix_detection = true
enable_historical_analysis_mock = true
enable_title_generation_skip = true
enable_suggestion_mode_skip = true
enable_filepath_extraction_mock = true

```

### ▶️ 运行

```bash
# 使用默认配置（config.toml）
cargo r

# 指定配置文件
cargo r /path/to/config.toml
```

服务默认监听 `0.0.0.0:9066`，可通过 `config.toml` 顶层 `port` 修改。

---

## 📖 配置参考

### 🔌 上游配置

| 字段 | 类型 | 说明 |
|:-----|:-----|:-----|
| `base_url` | `String` | 上游 API 地址 |
| `model` | `String` | 强制使用的模型名称 |
| `api_keys` | `Vec<String>` | API Key 列表 — 支持多 Key 负载均衡 |
| `user_agent` | `String` | 可选，自定义发往该上游的 `User-Agent`；未配置时透传原始请求头 |

### 🌐 服务监听配置

| 字段 | 类型 | 默认值 | 说明 |
|:-----|:-----|:-------|:-----|
| `port` | `u16` | `9066` | 服务监听端口，修改后需重启进程生效 |

### ⚙️ 优化配置

| 字段 | 类型 | 默认值 | 说明 |
|:-----|:-----|:-------|:-----|
| `enable_network_probe_mock` | `bool` | `true` | 拦截配额探测请求 |
| `enable_fast_prefix_detection` | `bool` | `true` | 快速前缀检测优化 |
| `enable_historical_analysis_mock` | `bool` | `true` | 跳过历史分析请求 |
| `enable_title_generation_skip` | `bool` | `true` | 跳过标题生成请求 |
| `enable_suggestion_mode_skip` | `bool` | `true` | 跳过建议模式请求 |
| `enable_filepath_extraction_mock` | `bool` | `true` | 文件路径提取优化 |

---

## 🏗️ 工作原理

```
┌─────────────┐     ┌──────────────┐     ┌─────────────────┐
│   客户端    │────▶│CliReqRefiner │────▶│  上游 1         │
│             │     │              │     │  (API Key 1)    │
│             │     │  ✂️ 请求体   │     │  (API Key 2)    │
│             │     │    精炼      │     ├─────────────────┤
│             │     │  🔄 负载    │────▶│  上游 2         │
│             │     │    均衡     │     │  (API Key 1)    │
│             │     │  ⚡ 本地    │     │  ...            │
│             │     │    优化     │     │                 │
│             │     │  📊 Token   │     │                 │
│             │     │    统计     │     │                 │
└─────────────┘     └──────────────┘     └─────────────────┘
```

---

## 🛠️ 技术栈

| 技术 | 说明 |
|:-----|:-----|
| **[Salvo](https://salvo.rs/)** | 高性能异步 Web 框架 |
| **[Hyper](https://hyper.rs/)** | 成熟的 HTTP/1.1 和 HTTP/2 实现 |
| **[Tokio](https://tokio.rs/)** | Rust 异步运行时核心 |
| **[arc-swap](https://docs.rs/arc-swap/)** | 无锁热重载配置 |
| **[notify](https://docs.rs/notify/)** | 跨平台文件监听 |
| **[mimalloc](https://github.com/microsoft/mimalloc)** | 高性能内存分配器 |

---

## ⚡ 性能

- ✅ Release 构建启用 LTO（链接时优化）
- ✅ mimalloc 替换默认内存分配器
- ✅ HTTP 连接复用，减少连接开销
- ✅ 无锁配置更新，无锁竞争

---

## 🏘️ 社区

本项目最初在 [LINUX DO](https://linux.do/) 分享 — 如果你是社区的朋友，欢迎来帖子下交流反馈，也期待有感兴趣的开发者一起完善这个项目 🤝

---

## 📄 许可证

GNU General Public License v3.0 (GPLv3)
