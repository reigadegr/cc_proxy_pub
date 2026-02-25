# CLAUDE.md

本文件为 Claude Code (claude.ai/code) 提供在此代码库中工作的指导。

## 项目概述

**CC Proxy** 是专为 [Claude Code CLI](https://claude.com/claude-code) 设计的高性能 API 代理网关。

### 核心特性
- **多上游负载均衡** - 双层轮询策略（先选上游，再轮询 API key）
- **热配置重载** - 通过 `notify` crate 监听 `config.toml`，修改后立即生效
- **本地优化拦截** - 减少消耗：
  - 配额/网络探测检查
  - 标题生成
  - 建议模式
  - 历史分析
  - 文件路径提取
- **请求统计** - 追踪 Token 消耗（总量、用户输入、历史上下文、助手回复、系统提示）

## 进入项目第一步

**务必先执行 `pwd` 获取当前目录**，再进行任何文件操作。

```bash
pwd
```

## 常用命令

### 构建
```bash
# Debug 模式
sh build_native_stable.sh

# Release 模式（推荐用于生产环境）
sh build_native_stable.sh r
```

> ⚠️ 全程禁止执行 `cargo build`、`cargo build --release` 以及其他任何 `cargo build*` 构建命令。
> 如需验证或测试，请统一执行 `sh debug.sh`（已包含格式化、clippy 与 `cargo test` 的全套检查）。

### 运行
通知用户手动运行

### 开发调试
```bash
sh debug.sh
```

## 架构概览

### 入口
- **`src/main.rs`** - 初始化日志、原子配置、启动文件监听器，在 `0.0.0.0:9066` 启动 Salvo 服务器

### 配置系统 (`src/config/`)
- **`mod.rs`** - `AtomicConfig` 使用 `arc-swap` 实现无锁热重载；`UpstreamConfig` 定义上游提供商；`OptimizationConfig` 控制拦截行为
- **`selector.rs`** - `UpstreamSelector` 实现双层轮询：先选上游，再轮询其 API keys
- **`format.rs`** - TOML 格式化工具

### 网关层 (`src/gateway/`)
- **`mod.rs`** - `GatewayHandler` 持有共享的 `HttpClient`（hyper + HTTPS）和 `RequestStats`
- **`service.rs`** - 请求处理与编排
- **`handler/mod.rs`** - 顶层处理器 `claude_proxy` 负责路由请求

### 请求处理器 (`src/gateway/handler/`)
- **`mod.rs`** - 主处理逻辑
- **`request.rs`** - 出站请求构建
- **`response.rs`** - 响应流式返回与处理
- **`system_prompt.rs`** - 系统提示词优化
- **`tool_desc.rs`** - 工具定义优化
- **`content_tag.rs`** - 内容标签处理
- **`thinking_patch.rs`** - 思考模式补丁
- **`utils.rs`** - 处理器工具函数

### OpenAI 兼容层 (`src/gateway/openai_compat/`)
- **`mod.rs`** - OpenAI 格式转换
- **`request.rs`** - Claude → OpenAI 请求转换
- **`response.rs`** - OpenAI → Claude 响应转换
- **`tools.rs`** - 工具定义转换
- **`media.rs`** - 媒体/内容处理

### 优化层 (`src/gateway/optimization/`)
- **`mod.rs`** - 优化编排
- **`detection.rs`** - 请求类型检测（配额检查、标题生成等）
- **`response_builder.rs`** - 拦截请求的 mock 响应构建器
- **`command_utils.rs`** - 命令前缀提取工具

## 配置说明

代理读取 `config.toml`（或第一个命令行参数指定的路径）。示例：

```toml
[[upstream]]
endpoint = "https://api.example.com/v1"
model = "claude-3-5-sonnet-20241022"
api_keys = ["key1", "key2"]

[optimizations]
enable_network_probe_mock = true
enable_fast_prefix_detection = true
enable_historical_analysis_mock = true
enable_title_generation_skip = true
enable_suggestion_mode_skip = true
enable_filepath_extraction_mock = true
```

配置变更会通过 `notify` crate 自动检测并重载，无需重启服务。

## 请求流程

1. Claude Code CLI 向 `/claude/*` 发送请求
2. 处理器从 Salvo 状态中提取共享状态（配置、HTTP 客户端、统计数据）
3. `detection.rs` 识别请求是否应被拦截
4. 如需拦截 → `response_builder.rs` 返回 mock 响应
5. 否则 → 通过双层轮询选择上游代理请求
6. 流式返回响应，同时更新 `RequestStats`

## 核心技术栈

- **[Salvo](https://salvo.rs/)** - 异步 Web 框架
- **[Hyper](https://hyper.rs/)** - HTTP 客户端（支持 HTTP/1.1 & HTTP/2）
- **[arc-swap](https://docs.rs/arc-swap/)** - 无锁原子配置切换
- **[notify](https://docs.rs/notify/)** - 跨平台文件监听
- **[mimalloc](https://github.com/microsoft/mimalloc)** - 高性能内存分配器
