# Copilot Responses Proxy

一个给 VS Code Copilot 自定义 Responses API endpoint 用的本地托盘代理。

## 背景

我在 VS Code Insiders 中把 Copilot 配到一个由 New API 搭建的 Responses 格式接口时，Agent 模式请求会返回 502。抓包和逐项回放后发现，上游对官方合法字段：

```json
"truncation": "disabled"
```

兼容不好。删除这个顶层字段后，同一批请求可以正常通过。

这个程序把这个修正固化下来：Copilot 只需要配置一次本地地址，之后代理负责转发、移除 `truncation`。鉴权可以由 Copilot 自己管理，也可以在代理里保存 token profile 后由代理注入。

## 功能

- Windows / macOS 托盘程序
- 本地监听 `http://127.0.0.1:8787/v1/responses`
- 转发到 `https://api.freshid.top/v1/responses`
- 自动删除顶层 `truncation`
- 支持多个 token profile
- 支持托盘或 CLI 切换当前 token，也可以清空当前 token 走请求头透传
- 保留摘要日志，便于排查问题

> 当前版本的 token 存在本机配置文件中。后续可以改成 Windows Credential Manager / macOS Keychain。

## 使用

发布后使用二进制文件：

```powershell
.\copilot-responses-proxy.exe init
```

添加 token：

```powershell
.\copilot-responses-proxy.exe token add main sk-your-token --label Main
```

切换 token：

```powershell
.\copilot-responses-proxy.exe token use main
```

清空当前 token，改为透传 Copilot 请求里的 `Authorization`：

```powershell
.\copilot-responses-proxy.exe token clear
```

启动托盘代理：

```powershell
.\copilot-responses-proxy.exe
```

调试时只启动代理服务：

```powershell
.\copilot-responses-proxy.exe serve
```

Windows 构建无控制台窗口版本：

```powershell
cargo build --release --features hide-console
```

开发时可以用 `cargo run -- <command>` 替代上面的 exe 命令。

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

This app makes the workaround persistent: configure Copilot once with a local endpoint, then let the proxy forward requests and remove `truncation`. Authentication can stay managed by Copilot, or the proxy can inject a selected token profile.

## Features

- Windows / macOS tray app
- Listens on `http://127.0.0.1:8787/v1/responses`
- Forwards to `https://api.freshid.top/v1/responses`
- Removes top-level `truncation`
- Multiple token profiles
- Token switching from tray or CLI, with a clear option for request-header pass-through
- Summary logs for debugging

> Token values are currently stored in the local config file. A future version can move them to Windows Credential Manager / macOS Keychain.

## Usage

```powershell
.\copilot-responses-proxy.exe init
.\copilot-responses-proxy.exe token add main sk-your-token --label Main
.\copilot-responses-proxy.exe token use main
.\copilot-responses-proxy.exe token clear
.\copilot-responses-proxy.exe
```

Set Copilot custom endpoint to:

```text
http://127.0.0.1:8787/v1/responses
```
