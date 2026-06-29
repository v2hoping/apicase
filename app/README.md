# apicase

API 接口调试、管理与用例编排桌面应用（Tauri 2 + Rust + React + TypeScript）。

本期为「脚手架 + 单 API 调试 MVP」：界面发起一个 HTTP 请求 → 由 Rust 后端（`reqwest`）发出 → 展示响应（即「单节点 DAG」的执行）。

## 环境要求

- Node.js（含 npm）
- Rust 工具链（`cargo` / `rustc`）
- Tauri 系统依赖：macOS 自带 WebKit；Windows 需 WebView2；Linux 需 WebKitGTK

## 开发

```bash
cd app
npm install        # 安装前端依赖（首次）
npm run tauri dev  # 启动桌面应用（热重载）
```

## 构建

```bash
cd app
npm run tauri build   # 打包为各平台安装包
```

## 自测

```bash
# 前端类型检查 + 打包
npm run build

# 后端：编译 + 运行 send_request 的单元/集成测试（real_get_request_succeeds 需联网）
cargo test --manifest-path src-tauri/Cargo.toml
```

## 目录结构

```
app/
├── src/                 # 前端（React + TS）
│   ├── App.tsx          # 请求编辑 + 响应展示（Postman 风格）
│   └── App.css
├── src-tauri/           # Rust 后端
│   ├── src/lib.rs       # send_request 命令 + 数据模型 + 测试
│   └── tauri.conf.json
└── ...
```

> 完整产品概念模型与设计决策见 `docs/1.feature/20260628-脚手架与单API调试MVP/产品技术方案.md`。
