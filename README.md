# apicase

**API 接口调试、管理与用例编排** 的本地优先（local-first）桌面软件。

一句话：**用文件组织的、可编排的 API 用例集**。打开一个目录即一个工作空间，folder 即分组，`.yml` 即用例；单个 API 调试与多步编排在同一套模型下统一。理念参考 [Bruno](https://www.usebruno.com/)，UI / 功能对标 [Postman](https://www.postman.com/)。

> 状态：**v0.1 · MVP**。已可打开工作空间、可视化编辑与执行单 / 多节点用例。

## 设计理念

- **文件即数据（local-first）** —— case 不进数据库，就是磁盘上的 `.yml`。Git 友好、可 diff、可 review、可离线，数据完全由用户掌控。
- **单 / 多请求统一为 DAG** —— 不做两套模型：单请求 = 退化的单节点 DAG，多步编排 = 多节点 DAG（节点间以 `dependsOn` 声明依赖）。概念更少、代码路径统一、单 → 多平滑演进。
- **变量与数据流** —— 变量就近覆盖：environment（全局） < case 级 `vars` < 上游节点 `outputs`；透传语法 `{{baseUrl}}`、`{{steps.login.outputs.token}}`；输出按 JSONPath 从响应体提取供下游引用。
- **YAML 作为载体** —— case / 配置都用 YAML，可读、可注释、Git 友好；schema 参考 Postman / HAR / Insomnia / Bruno（request）与 Arazzo / GitHub Actions（flow）。

## 功能特性

- **单 API 调试** —— Postman 风格请求行（方法 + URL + 发送），参数 / 请求头 / 认证 / 请求体四 Tab；响应区展示状态码 / 耗时 / 大小 / 响应头 / 响应体（Pretty / Raw）。请求由 Rust 后端（`reqwest`）发出，**天然绕过浏览器 CORS**。
- **工作空间与文件树** —— 打开 / 创建目录为工作空间（幂等写 `application.yml`）；懒加载文件树，支持搜索、右键新建 / 重命名 / 删除，可视化对话框新建 case。
- **flow 编排（DAG 画布）** —— `dependsOn` 自动分层布局 + SVG 连线；两级视图切换（**文本 | 可视**，可视再分 **流程 / 请求**）；内容驱动默认视图。
- **执行引擎** —— 拓扑序串行运行，步骤间**变量透传** + **输出提取**（JSONPath 常用子集）+ **断言**（`eq/ne/contains/exists/notExists/gt/lt/matches`，逐条 ✓/✗，失败标红节点）。
- **多环境** —— `application.yml` 的 `environment` 段多套环境切换，运行时注入变量；仿 GitHub 风格的可视化设置页。
- **多标签页** —— 同时打开多个 case，标签切换 / dirty 标记 / 中键关闭 / 右键批量关闭，非活动标签完整保留编辑态。
- **原生桌面体验** —— macOS 自定义标题栏；任意文本 / 二进制文件可打开（二进制由 Rust 端嗅探并友好提示）。

## 技术栈

| 层 | 选型 |
|---|---|
| 桌面框架 | Tauri 2 |
| 后端 | Rust（`reqwest` + rustls、`serde`；`tokio` 仅测试） |
| 前端 | React 19 + TypeScript + Vite 7；`js-yaml`（case 解析） |
| 存储格式 | YAML（case / 配置；格式参考 Postman / HAR / Arazzo 等） |

## 快速使用

**环境要求**：Node.js（含 npm）、Rust 工具链（`cargo` / `rustc`）；Tauri 系统依赖（macOS 自带 WebKit，Windows 需 WebView2，Linux 需 WebKitGTK）。

```bash
cd app
npm install               # 首次安装前端依赖
npm run tauri dev         # 启动桌面应用（热重载）
npm run tauri build       # 打包各平台安装包
```

自测：

```bash
npm run build             # 前端类型检查 + 打包
cargo test --manifest-path src-tauri/Cargo.toml   # 后端测试（real_get 需联网）
```

启动后：左上角「选择工作空间」打开一个目录 → 在文件树新建 / 打开 `.yml` → 编辑请求并「发送」，多节点用例点「▶ 运行」按拓扑序执行。

## 配置与格式

> 以下为要点与示例，完整字段规范见 [docs/0.latest/3.YAML格式规范.md](docs/0.latest/3.YAML格式规范.md)。

### application.yml（工作空间配置）

工作空间根目录的配置文件，定义多套环境（每套一组变量），topbar 右侧下拉切换活动环境，运行时注入：

```yaml
environment:
  dev:  { baseUrl: https://dev.example.com,  token: "" }
  test: { baseUrl: https://test.example.com, token: "" }
  prod: { baseUrl: https://api.example.com,  token: "" }
```

### case.yml（用例）

一个 `.yml` 即一个 case，模型上是 DAG；写盘时恒定使用 `requests:` 列表（单节点也是长度为 1 的列表）。

**顶层字段**：`apicase`（版本，必填）· `name`（可选）· `vars`（case 级变量，可选）· `requests`（请求节点列表，必填）· `ui.nodes`（画布坐标，可选）。

**请求节点**（顺序 `id → dependsOn → http → outputs → assertions`）：`id` 唯一标识 · `dependsOn` 上游依赖 · `http` 报文（`method/url/query/headers/auth/body`）· `outputs` 输出提取 · `assertions` 断言。

**单节点用例**（等价于「发一个 API」）：

```yaml
apicase: "0.1"
name: 获取用户
vars:
  baseUrl: https://api.example.com
requests:
  - id: getUser
    http:
      method: GET
      url: "{{baseUrl}}/users/1"
    assertions:
      - { target: status, op: eq, value: "200" }
      - { target: $.data.id, op: exists }
```

**多节点用例**（登录 → 下单，`dependsOn` 声明依赖、`outputs` 提取变量供下游透传）：

```yaml
apicase: "0.1"
name: 登录并下单
vars:
  baseUrl: https://api.example.com
requests:
  - id: login
    http:
      method: POST
      url: "{{baseUrl}}/login"
      body:
        type: json
        json: { username: admin, password: "123456" }
    outputs:
      - { name: token, path: $.data.token }
    assertions:
      - { target: status, op: eq, value: "200" }
  - id: createOrder
    dependsOn: [login]
    http:
      method: POST
      url: "{{baseUrl}}/orders"
      headers:
        - { name: Authorization, value: "Bearer {{steps.login.outputs.token}}" }
      body:
        type: json
        json: { sku: A-1001, qty: 2 }
    assertions:
      - { target: $.code, op: eq, value: "0" }
```

- **auth 类型**：`none` / `bearer` `{ token }` / `basic` `{ username, password }` / `apikey` `{ key, value, in: header|query }`。
- **body 类型**：`none` / `json` / `text`（可选 `contentType`）/ `form-urlencoded` / `form-data`。
- **断言目标**：`status` / `header.<名>` / JSONPath（如 `$.code`）。
- **变量**：`{{name}}`；跨节点引用上游输出用 `{{steps.<请求id>.outputs.<输出名>}}`；未解析保留字面量。

## 仓库结构

```
apicase/
├── app/             # 应用代码（Tauri 项目：src/ 前端 + src-tauri/ Rust 后端）
├── docs/
│   ├── 0.latest/    # 当前全局最新文档 —— 唯一事实来源
│   └── 1.feature/   # 各需求的产品技术方案（YYYYMMDD-需求名）
└── CLAUDE.md        # 全局提示词
```

## 文档

`docs/0.latest/` 是项目的**唯一事实来源**，涉及现状的判断以此为准：

- [0.概览](docs/0.latest/0.概览.md) —— 定位、当前能力、技术栈、路线。
- [1.产品概念模型](docs/0.latest/1.产品概念模型.md) —— 文件即数据、folder / case、关键设计决策。
- [2.技术架构](docs/0.latest/2.技术架构.md) —— 目录结构、后端命令与数据模型、运行与构建。
- [3.YAML格式规范](docs/0.latest/3.YAML格式规范.md) —— case / application.yml 的完整字段格式与序列化约定。

## 路线（下一步）

1. 未定义变量高亮（`{{var}}` 找不到时提示）；深色主题。
2. JSONPath 通配符 / 过滤器、flow 并发执行、断言更多目标（响应耗时 / 大小）。
3. 画布节点拖拽持久化、标签拖拽排序、最近列表持久化、文件树外部变更自动刷新、历史、导入导出（Postman / Arazzo）、OpenAPI(SPEC)。
