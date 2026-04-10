## 项目概述

**CliReqRefiner** 是面向 AI 编程工具（Claude Code、Codex 等）的高性能 API 代理网关，核心功能是请求体精炼优化。

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

## 提交说明

提交时必须遵循约定式提交（Conventional Commits）。

提交标题必须完整，并且标题使用中文。

提交正文的第一行必须是提交标题的英文翻译。

提交正文的主要内容必须采用中英逐句对照的写法，先中文，下一行对应英文，逐句成对出现。

提交信息必须包含 DCO。

DCO 中的姓名和邮箱必须从本地 git 配置获取，不得手写、不得使用占位符、不得替换为其他身份。

获取 DCO 身份时，使用以下命令读取本地配置：

```bash
git config user.name
git config user.email
```

生成 `Signed-off-by` 时，必须直接使用上面两个命令的输出结果。

## 补丁说明

如果补丁工具提示打补丁失败，这有可能是误报。

遇到这种情况时，请先执行 `git diff` 检查刚才的修改是否已经正确应用，因为大部分这类报错都是误报。

在确认 `git diff` 后，再重新读取修改后的文件内容，判断补丁是否真的失败；不要仅凭补丁工具的返回结果下结论。

## 架构概览

### 入口
- **`app/src/main.rs`** - 初始化日志、原子配置、启动文件监听器，并按配置端口（默认 `0.0.0.0:9077`）启动 Salvo 服务器

### 配置系统
- **`crates/config/src/lib.rs`** - 对外导出 `AtomicConfig`、`Config`、`OptimizationConfig` 以及 selector 相关类型
- **`crates/config/src/runtime.rs`** - `AtomicConfig` 使用 `arc-swap` 实现无锁热重载
- **`crates/config/src/model.rs`** - `Config` 与 `OptimizationConfig` 定义
- **`crates/selector/src/model.rs`** - `UpstreamConfig`、`Mode`、`UpstreamModes` 与全局 UA 配置定义
- **`crates/selector/src/selector.rs`** - `UpstreamSelector` 实现双层轮询：先选上游，再轮询其 API keys
- **`crates/config/src/format.rs`** - TOML 格式化工具

### 网关层 (`app/src/gateway/`)
- **`mod.rs`** - `GatewayHandler` 持有共享的 `HttpClient`（hyper + HTTPS）和 `RequestStats`
- **`service.rs`** - 请求处理与编排
- **`handler/mod.rs`** - 顶层处理器 `claude_proxy` 负责路由请求

### 请求处理器 (`app/src/gateway/handler/`)
- **`mod.rs`** - 主处理逻辑
- **`request.rs`** - 出站请求构建
- **`response.rs`** - 响应流式返回与处理
- **`system_prompt.rs`** - 系统提示词优化
- **`tool_desc.rs`** - 工具定义优化
- **`content_tag.rs`** - 内容标签处理
- **`thinking_patch.rs`** - 思考模式补丁
- **`utils.rs`** - 处理器工具函数

### 优化层 (`app/src/gateway/optimization/`)
- **`mod.rs`** - 优化编排
- **`detection.rs`** - 请求类型检测（配额检查、标题生成等）
- **`response_builder.rs`** - 拦截请求的 mock 响应构建器
- **`command_utils.rs`** - 命令前缀提取工具

## 配置说明

代理读取 `config.toml`（或第一个命令行参数指定的路径）。示例：

```toml
port = 9077
log_req_body = false
log_res_body = false

# Upstream 1: 智谱 AI Anthropic 兼容接口
[[upstream]]
enable = true
base_url = "https://open.bigmodel.cn/api/anthropic"
model = "glm-4.7"
api_keys = ["your_api_key1", "your_api_key2"]
# mode 默认为 "anthropic"，也支持数组，例如 ["anthropic", "openai_responses"]
# 设置 enable = false 可临时禁用该 upstream
# 也可写成 mode = ["anthropic", "openai_responses"]

[optimizations]
enable_network_probe_mock = true
enable_fast_prefix_detection = true
enable_historical_analysis_mock = true
enable_title_generation_skip = true
enable_suggestion_mode_skip = true
enable_filepath_extraction_mock = true
```

配置变更会通过 `notify` crate 自动检测并重载，无需重启服务。
监听端口 `port` 仅在启动时读取，修改后需要重启服务。

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
