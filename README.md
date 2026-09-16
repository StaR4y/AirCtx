# AirCtx

把本机 Codex 会话和笔记同步到一台小服务器，多设备 / 多个 Agent 都能读。

```
本机 TUI / CLI  →  后台同步  →  服务器 (SQLite)
```

## 三步用起来

**1. 服务器**（任意一台机器）

```bash
./run.sh                          # Docker；日志里会打印安全入口 URL
# 或交互式
airctx serve --data-dir ./data    # 询问端口，生成安全路径
```

首次启动会打印类似：

```
http://0.0.0.0:8787/a8k3mpx2nq4r/
```

用浏览器打开这个地址。第一个注册的人是管理员；之后关闭注册，只能发邀请链接。

**2. 客户端**

在 Web UI 登录后复制 CLI token：

```bash
airctx login http://<server>:8787/<安全路径> <token>
```

也可以直接 `airctx`，填带安全路径的 URL 和从网页复制的 token。之后所有通讯都走这个入口。

**3. 打开界面并同步**

```bash
airctx          # 交互界面
```

按 `s` 启动后台同步。界面默认是**设备列表**；进入一台设备后选择 Codex / Claude Code / OpenCode。会话列表默认不显示，进入客户端或输入 `:ls` 才出现。

## 界面按键

| 键 | 作用 |
|---|---|
| `j` `k` | 移动 |
| `enter` | 设备 → 客户端 → 会话 |
| `esc` | 返回上一级 |
| `:ls` | 打开会话列表 |
| `:codex` | 跳到本机 Codex 会话 |
| `/` | 在会话列表里搜索 |
| `s` | 启动同步 / 立即同步 |
| `e` | 设置 |
| `d` | 删除（会确认） |
| `?` | 帮助 |
| `q` | 退出 |

Codex 不用配置：只要本机有 `~/.codex/sessions`，daemon 会解析完整会话再上传（不是丢原始 jsonl）。

## 脚本命令

```bash
airctx set proj/notes "hello"
airctx get proj/notes
airctx ls proj/
airctx rm proj/notes
airctx watch proj/                  # SSE 订阅
airctx daemon start|stop|status|sync
airctx daemon install               # 开机自启
```

配置文件：`~/Library/Application Support/airctx/config.toml`（Linux: `~/.config/airctx/`）。

## HTTP

| 方法 | 路径 |
|---|---|
| PUT/GET/DELETE | `/v1/kv/{key}` |
| GET | `/v1/kv?prefix=&limit=` |
| GET | `/v1/watch`（SSE） |
| GET | `/v1/health` |

`Authorization: Bearer <token>`（health 除外）。

## 开发

```bash
./run.sh                 # Docker 构建并启动
cargo build
airctx serve             # 本地直接跑服务端
```
