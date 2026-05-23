# Copilot Responses Proxy

一个给 VS Code Copilot 自定义 Responses API endpoint 用的本地托盘代理。

## 背景

我在 VS Code Insiders 中把 Copilot 配到一个由 New API 搭建的 Responses 格式接口时，Agent 模式请求会返回 502。抓包和逐项回放后发现，上游对官方合法字段：

```json
"truncation": "disabled"
```

兼容不好。删除这个顶层字段后，同一批请求可以正常通过。

这个程序把这个修正固化下来：Copilot 只需要配置一次本地地址，之后代理负责转发、移除 `truncation`。上游地址和 token 通过 provider profile 管理；provider token 留空时，代理不注入鉴权，直接透传 Copilot 请求里的 `Authorization`。需要固定推理强度时，代理也可以注入 Responses API 的 `reasoning.effort` 字段。

## 功能

- Windows / macOS 托盘程序
- 本地监听 `http://127.0.0.1:8787/v1/responses`
- 默认转发到 `https://api.freshid.top/v1/responses`
- 自动删除顶层 `truncation`
- 可选注入 `reasoning.effort`
- 支持多个 provider profile，每个 profile 包含上游地址和可选 token
- 支持托盘或 CLI 切换当前 provider
- 支持托盘或 CLI 切换推理强度，也可以清空后保持请求原样
- 保留摘要日志，便于排查问题

> 当前版本的 provider token 存在本机配置文件中。后续可以改成 Windows Credential Manager / macOS Keychain。

## 使用

发布后使用二进制文件：

```powershell
.\copilot-responses-proxy.exe init
```

添加 provider，并由代理注入 token：

```powershell
.\copilot-responses-proxy.exe provider add main api.freshid.top sk-your-token
```

添加 provider，但 token 留空，改为透传 Copilot 请求里的 `Authorization`：

```powershell
.\copilot-responses-proxy.exe provider add copilot api.freshid.top
```

provider address 可以只写域名或 IP，端口可选。裸域名会补成 `https://<host>/v1/responses`，裸 IP 或 `localhost` 会补成 `http://<host>/v1/responses`。如果地址已经带有路径，例如 `relay.example.com/custom/responses`，代理只补协议，不再追加 `/v1/responses`。`--label` 可省略，默认使用 host。

切换 provider：

```powershell
.\copilot-responses-proxy.exe provider use main
```

查看 provider：

```powershell
.\copilot-responses-proxy.exe provider list
```

设置推理强度：

```powershell
.\copilot-responses-proxy.exe reasoning use high
```

可选值为 `minimal`、`low`、`medium`、`high`、`xhigh`。不同模型支持的范围可能不同；代理只注入字段，不做模型兼容性判断。

清空推理强度，改为不改写请求里的 `reasoning`：

```powershell
.\copilot-responses-proxy.exe reasoning clear
```

查看推理强度枚举：

```powershell
.\copilot-responses-proxy.exe reasoning list
```

启动托盘代理：

```powershell
.\copilot-responses-proxy.exe
```

调试时只启动代理服务：

```powershell
.\copilot-responses-proxy.exe serve
```

## 编译指引

安装 Rust stable 工具链后，在仓库根目录执行：

```powershell
cargo build
```

开发构建产物：

```text
target\debug\copilot-responses-proxy.exe
```

发布构建：

```powershell
cargo build --release
```

发布构建产物：

```text
target\release\copilot-responses-proxy.exe
```

Windows 托盘发布版建议隐藏控制台窗口：

```powershell
cargo build --release --features hide-console
```

调试 CLI 输出时不要启用 `hide-console`，否则 `init`、`provider list`、`path config` 等命令的输出不可见。开发时可以用 `cargo run -- <command>` 替代上面的 exe 命令。

Copilot 自定义 endpoint 配置为：

```text
http://127.0.0.1:8787/v1/responses
```

---

# English

A local tray proxy for VS Code Copilot custom Responses API endpoints.

## Background

When using VS Code Insiders Copilot Agent mode with a New API-backed Responses endpoint, requests may fail with 502. After capturing and replaying the real request body, the incompatible field was found to be the valid top-level Responses API parameter:

```json
"truncation": "disabled"
```

Removing that field allows the same request to pass through the upstream service.

This app makes the workaround persistent: configure Copilot once with a local endpoint, then let the proxy forward requests and remove `truncation`. Upstream addresses and tokens are managed through provider profiles; when a provider token is empty, the proxy does not inject auth and forwards Copilot's incoming `Authorization` header. When needed, the proxy can also inject the Responses API `reasoning.effort` field.

## Features

- Windows / macOS tray app
- Listens on `http://127.0.0.1:8787/v1/responses`
- Forwards to `https://api.freshid.top/v1/responses` by default
- Removes top-level `truncation`
- Optionally injects `reasoning.effort`
- Multiple provider profiles, each with an upstream address and optional token
- Provider switching from tray or CLI
- Reasoning effort switching from tray or CLI, with a clear option for no request rewrite
- Summary logs for debugging

> Provider token values are currently stored in the local config file. A future version can move them to Windows Credential Manager / macOS Keychain.

## Usage

```powershell
.\copilot-responses-proxy.exe init
.\copilot-responses-proxy.exe provider add main api.freshid.top sk-your-token
.\copilot-responses-proxy.exe provider add copilot api.freshid.top
.\copilot-responses-proxy.exe provider use main
.\copilot-responses-proxy.exe provider list
.\copilot-responses-proxy.exe reasoning use high
.\copilot-responses-proxy.exe reasoning clear
.\copilot-responses-proxy.exe reasoning list
.\copilot-responses-proxy.exe
```

Provider addresses can be a bare domain or IP with an optional port. Bare domains become `https://<host>/v1/responses`, while bare IPs and `localhost` become `http://<host>/v1/responses`. If the address already contains a path, the proxy keeps that path and only fills in the scheme. When `--label` is omitted, the host is used as the label.

Reasoning effort values are `minimal`, `low`, `medium`, `high`, and `xhigh`. Model support can vary; unsupported values are left for the upstream API to reject.

Set Copilot custom endpoint to:

```text
http://127.0.0.1:8787/v1/responses
```

## Build

Install the Rust stable toolchain, then run from the repository root:

```powershell
cargo build
```

Debug build output:

```text
target\debug\copilot-responses-proxy.exe
```

Release build:

```powershell
cargo build --release
```

Release build output:

```text
target\release\copilot-responses-proxy.exe
```

For a Windows tray release without a console window:

```powershell
cargo build --release --features hide-console
```

Do not enable `hide-console` when debugging CLI output, because commands such as `init`, `provider list`, and `path config` print to the console.
