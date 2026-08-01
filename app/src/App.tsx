import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  Case,
  Request,
  RequestOutput,
  Assertion,
  AssertOp,
  UiNodes,
  Environments,
  WorkspaceSettings,
  DEFAULT_WS_SETTINGS,
  analyzeCase,
  dumpCase,
  splitQueryFromUrl,
  parseAppConfig,
  dumpAppConfig,
} from "./case";
import { ReqDraft, RequestDraft, draftToRequest, emptyDraft, caseToRequests } from "./draft";
import {
  type RunReport,
  type RunOpts,
  type RunContext,
  type ClientConfig,
  type AssertRecord,
  type KVPair,
  type StepResult,
  debugRunOpts as makeDebugOpts,
  batchRunOpts as makeBatchOpts,
  runStep,
  runBatch,
  cancelRun as cancelRunIpc,
  listenRun,
  reportShell,
  parseReport,
  topoOrder,
} from "./run";
import { RequestEditor, KVTable, METHODS, methodClass, Select, OP_LABELS } from "./RequestEditor";
import { FlowCanvas, FlowNode } from "./FlowCanvas";
import { TerminalPane } from "./TerminalPane";
import { type ThemeMode, resolveTheme, applyTheme } from "./theme";
import { type AppSettings, loadCachedSettings, loadAppSettings, saveAppSettings, filterExistingPaths, pathExists } from "./settings";
import { type ProxyConfig, type ProxyMode, proxyPayload } from "./proxy";
import { AiChat } from "./AiChat";
import { MarkdownEditor } from "./markdown";
import { baseName, dirName, joinPath, relPath, isUnder, retargetPath, uniqueName } from "./pathutil";
import {
  ACTIONS,
  ACTION_MAP,
  type ActionDef,
  type ActionId,
  type Overrides,
  eventToAccel,
  accelKey,
  formatAccel,
  accelTokens,
  resolveBindings,
  buildLookup,
  findConflict,
  isDefaultBinding,
} from "./shortcuts";
import "./App.css";

interface DirEntry {
  name: string;
  path: string;
  isDir: boolean;
  hidden?: boolean; // `.` 开头；仅在「显示隐藏文件」打开时才会出现，用于淡色渲染
}

// 应用自身的存储位置（后端 app_paths 命令返回）；exists 决定「显示位置」是否可点
interface AppPaths {
  settingsFile: string;
  settingsFileExists: boolean;
  home: string; // 用户主目录，供 tildify 缩写显示
}

/** 显示用：把主目录前缀缩成 `~`（省 15+ 字符，多数路径因此能一行放下）。原始路径仍用于打开文件管理器。 */
function tildify(path: string, home: string): string {
  if (!home || !path.startsWith(home)) return path;
  const rest = path.slice(home.length);
  return rest === "" || rest.startsWith("/") || rest.startsWith("\\") ? `~${rest}` : path;
}

// 执行语义（变量透传 / 请求组装 / 认证 / 断言 / 报告）全在 Rust 执行内核里，
// 见 core/src/runner.rs。本文件只负责把配置递下去、把结果画出来。

/** 响应区展示用的一次响应——由 `StepResult.response` 折平而来（调试运行不截断，preview 即全文）。 */
interface RespView {
  status: number;
  statusText: string;
  headers: KVPair[];
  body: string;
  elapsedMs: number;
}

interface RunState {
  status: "idle" | "running" | "ok" | "err";
  resp?: RespView | null;
  error?: string | null;
  asserts?: AssertRecord[];
}

/** `StepResult.response` → 响应区要的形状。 */
function respViewOf(r: StepResult): RespView {
  return {
    status: r.response?.status ?? 0,
    statusText: r.response?.statusText ?? "",
    headers: r.response?.headers ?? [],
    body: r.response?.body.preview ?? "",
    elapsedMs: r.response?.elapsedMs ?? 0,
  };
}

// 一个标签页的完整编辑态快照（切换/后台保存时用）
interface TabSnapshot {
  path: string;
  caseName: string;
  caseVars: Record<string, unknown> | undefined;
  caseVersion: string;
  dirty: boolean;
  requests: RequestDraft[];
  selectedRequestId: string;
  uiNodes: UiNodes | undefined;
  textMode: boolean;
  showFlow: boolean;
  showRequest: boolean;
  rawText: string;
  caseValid: boolean;
  textError: string | null;
  binaryFile: boolean;
  configVisual: boolean;
  runMap: Record<string, RunState>;
  outputsCtx: Record<string, Record<string, unknown>>;
  respTab: "body" | "headers" | "assert";
  error: string | null;
}

function statusClass(status: number): string {
  if (status >= 200 && status < 300) return "status-2xx";
  if (status >= 300 && status < 400) return "status-3xx";
  if (status >= 400 && status < 500) return "status-4xx";
  return "status-5xx";
}

function prettyBody(body: string): string {
  try {
    return JSON.stringify(JSON.parse(body), null, 2);
  } catch {
    return body;
  }
}

// 路径工具（baseName / dirName / joinPath / relPath / isUnder / retargetPath / uniqueName）见 pathutil.ts

// 可在编辑器打开的 yml/yaml（case 渲染结构化，其余落文本）
function isYamlFile(path: string): boolean {
  const n = baseName(path).toLowerCase();
  return n.endsWith(".yml") || n.endsWith(".yaml");
}

// 工作空间根配置文件
function isAppConfig(path: string): boolean {
  const n = baseName(path).toLowerCase();
  return n === "application.yml" || n === "application.yaml";
}

// Markdown 文本文件：用 markdown 编辑器（编辑/预览/分屏）打开
function isMarkdownFile(path: string): boolean {
  const n = baseName(path).toLowerCase();
  return n.endsWith(".md") || n.endsWith(".markdown");
}

function isHtmlFile(path: string): boolean {
  const n = baseName(path).toLowerCase();
  return n.endsWith(".html") || n.endsWith(".htm");
}

// ── 运行报告 ────────────────────────────────────────

/** 运行报告的输出位置（相对工作空间根）。隐藏目录：默认不出现在文件树，用例树保持干净。 */
const REPORTS_REL = ".apicase/reports";

/**
 * 运行报告标签用**伪路径**占位。tabOrder 存的是路径字符串，加前缀即可与真实文件分流——
 * 伪路径不走 openCase、不进 tabCacheRef（它不是编辑态，TabSnapshot 那套字段一个都不需要）。
 */
const RUN_TAB_PREFIX = "apicase://run/";
const isRunTab = (p: string): boolean => p.startsWith(RUN_TAB_PREFIX);
const runIdOf = (p: string): string => p.slice(RUN_TAB_PREFIX.length);
/** 历史报告以其文件路径为会话 id，重复打开同一份即复用同一个标签。 */
const reportKey = (path: string): string => "file:" + path;

/** 一次运行的会话状态（live 运行或读回的历史报告）。 */
interface RunSession {
  runId: string;
  report: RunReport | null;
  file: string; // report.html 的绝对路径
  dir: string; // 报告目录
  total: number; // 预计要跑的用例数（进度条分母；报告 summary 在跑完前不含未跑的）
  cancelling?: boolean;
  readOnly?: boolean; // 历史报告：没有进度条与取消
}

/** 运行配置对话框状态。files=null 表示正在扫描用例。 */
interface RunDialogState {
  target: string;
  isDir: boolean;
  recursive: boolean;
  env: string;
  files: string[] | null;
}

// 已知二进制/媒体扩展名：直接短路，不读取整个文件（避免大文件读入内存）
const BINARY_EXTS = new Set([
  // 图片
  "png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tif", "tiff", "heic", "avif",
  // 音频
  "mp3", "wav", "flac", "aac", "ogg", "m4a", "wma", "opus",
  // 视频
  "mp4", "mov", "avi", "mkv", "webm", "flv", "wmv", "m4v",
  // 压缩包
  "zip", "tar", "gz", "tgz", "bz2", "xz", "rar", "7z",
  // 文档/办公
  "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx",
  // 字体
  "ttf", "otf", "woff", "woff2", "eot",
  // 可执行/库/数据库/其它
  "exe", "dll", "so", "dylib", "bin", "class", "jar", "wasm", "sqlite", "db",
]);
function isBinaryExt(path: string): boolean {
  const n = baseName(path).toLowerCase();
  const i = n.lastIndexOf(".");
  return i > 0 && BINARY_EXTS.has(n.slice(i + 1));
}

// ── 文件树图标（SVG，currentColor 由 CSS 控色）──
function FolderIcon({ className = "tree-ico ico-folder", size = 15 }: { className?: string; size?: number } = {}) {
  return (
    <svg className={className} viewBox="0 0 16 16" width={size} height={size} aria-hidden="true">
      <path
        fill="currentColor"
        d="M1.75 4c0-.69.56-1.25 1.25-1.25h3.09c.4 0 .78.19 1.02.51l.63.84c.05.06.12.1.2.1h5.06c.69 0 1.25.56 1.25 1.25v6.05c0 .69-.56 1.25-1.25 1.25H3c-.69 0-1.25-.56-1.25-1.25z"
      />
    </svg>
  );
}
// 「打开工作空间」动作专用：线描边文件夹 + 加号，与列表里的实心文件夹作区分
function FolderPlusIcon({ className = "ws-item-glyph", size = 15 }: { className?: string; size?: number } = {}) {
  return (
    <svg className={className} viewBox="0 0 16 16" width={size} height={size} aria-hidden="true">
      <path
        fill="none"
        stroke="currentColor"
        strokeWidth="1.2"
        strokeLinejoin="round"
        d="M1.75 4.25c0-.55.45-1 1-1h2.86c.33 0 .64.16.83.44l.5.72c.19.28.5.44.83.44h4.48c.55 0 1 .45 1 1v5.96c0 .55-.45 1-1 1H2.75c-.55 0-1-.45-1-1z"
      />
      <path fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" d="M8 8.1v3M6.5 9.6h3" />
    </svg>
  );
}
function FileIcon({ active }: { active?: boolean }) {
  return (
    <svg className={`tree-ico ico-file ${active ? "is-case" : ""}`} viewBox="0 0 16 16" width="15" height="15" aria-hidden="true">
      <path fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinejoin="round" d="M4.25 2h4.5l3 3v8.75a.25.25 0 0 1-.25.25H4.25a.25.25 0 0 1-.25-.25V2.25A.25.25 0 0 1 4.25 2z" />
      <path fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinejoin="round" d="M8.5 2.25V5h2.75" />
    </svg>
  );
}
// 工作空间配置文件（application.yml）专用：齿轮图标，一眼可辨"这是配置文件"
// 线条描边、中心镂空（背景透出）——只有轮廓、无实心填充；viewBox 24×24 留边，缩放不裁齿尖
function ConfigIcon({ className = "tree-ico ico-config", size = 15 }: { className?: string; size?: number }) {
  return (
    <svg className={className} viewBox="0 0 24 24" width={size} height={size} aria-hidden="true">
      <path
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinejoin="round"
        d="M19.14 12.94c.04-.3.06-.61.06-.94 0-.32-.02-.64-.07-.94l2.03-1.58c.18-.14.23-.41.12-.61l-1.92-3.32c-.12-.22-.37-.29-.59-.22l-2.39.96c-.5-.38-1.03-.7-1.62-.94l-.36-2.54c-.04-.24-.24-.41-.48-.41h-3.84c-.24 0-.43.17-.47.41l-.36 2.54c-.59.24-1.13.57-1.62.94l-2.39-.96c-.22-.08-.47 0-.59.22L2.74 8.87c-.12.21-.08.47.12.61l2.03 1.58c-.05.3-.09.63-.09.94s.02.64.07.94l-2.03 1.58c-.18.14-.23.41-.12.61l1.92 3.32c.12.22.37.29.59.22l2.39-.96c.5.38 1.03.7 1.62.94l.36 2.54c.05.24.24.41.48.41h3.84c.24 0 .44-.17.47-.41l.36-2.54c.59-.24 1.13-.56 1.62-.94l2.39.96c.22.08.47 0 .59-.22l1.92-3.32c.12-.22.07-.47-.12-.61l-2.01-1.58z"
      />
      <circle cx="12" cy="12" r="3.15" fill="none" stroke="currentColor" strokeWidth="1.5" />
    </svg>
  );
}
// 文件图标（仅文件用，供文件树 / 搜索结果 / 标签页统一复用）：
// application.yml → 齿轮；.yml/.yaml 用例 → 高亮文件；其余 → 普通文件
function FileTypeIcon({ path }: { path: string }) {
  if (isRunTab(path)) return <ReportIcon />;
  return isAppConfig(path) ? <ConfigIcon /> : <FileIcon active={isYamlFile(path)} />;
}

/** 文件树「显示隐藏文件」开关的图标（睁眼 / 闭眼，对齐 Finder 与 VSCode 的心智） */
function EyeIcon() {
  return (
    <svg viewBox="0 0 16 16" width="15" height="15" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.4">
      <path d="M1.5 8s2.4-4 6.5-4 6.5 4 6.5 4-2.4 4-6.5 4S1.5 8 1.5 8Z" strokeLinejoin="round" />
      <circle cx="8" cy="8" r="1.8" />
    </svg>
  );
}
function EyeOffIcon() {
  return (
    <svg viewBox="0 0 16 16" width="15" height="15" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.4">
      <path d="M1.5 8s2.4-4 6.5-4c1.2 0 2.2.3 3.1.8M14.5 8s-2.4 4-6.5 4c-1.2 0-2.2-.3-3.1-.8" strokeLinecap="round" />
      <path d="M3 13 13 3" strokeLinecap="round" />
    </svg>
  );
}

/** 运行报告标签的图标：带勾的清单（与用例的文件图标区分开） */
function ReportIcon() {
  return (
    <svg className="tree-ico ico-report" viewBox="0 0 24 24" width="16" height="16" aria-hidden="true">
      <path fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinejoin="round" d="M6 3h8l4 4v14H6z" />
      <path fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinejoin="round" d="M14 3v4h4" />
      <path
        fill="none"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinecap="round"
        strokeLinejoin="round"
        d="m9 13.5 1.8 1.8L15 11"
      />
    </svg>
  );
}
// 设置页左导航图标：统一线条描边（currentColor 跟随文字色），16×16 viewBox
const SETTINGS_NAV_ICONS: Record<string, ReactNode> = {
  // 通用：调节滑块
  通用: (
    <>
      <path fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" d="M2.5 5h11M2.5 11h11" />
      <circle cx="6" cy="5" r="1.9" fill="var(--panel)" stroke="currentColor" strokeWidth="1.4" />
      <circle cx="10.5" cy="11" r="1.9" fill="var(--panel)" stroke="currentColor" strokeWidth="1.4" />
    </>
  ),
  // 主题：半明半暗圆（明暗对比）
  主题: (
    <>
      <circle cx="8" cy="8" r="5.6" fill="none" stroke="currentColor" strokeWidth="1.4" />
      <path fill="currentColor" d="M8 2.4a5.6 5.6 0 0 0 0 11.2z" />
    </>
  ),
  // 代理：地球（赤道 + 经线，表示网络出口）
  代理: (
    <>
      <circle cx="8" cy="8" r="5.8" fill="none" stroke="currentColor" strokeWidth="1.4" />
      <path
        fill="none"
        stroke="currentColor"
        strokeWidth="1.3"
        strokeLinejoin="round"
        d="M2.4 8h11.2M8 2.2c1.7 1.7 2.6 3.7 2.6 5.8s-.9 4.1-2.6 5.8c-1.7-1.7-2.6-3.7-2.6-5.8s.9-4.1 2.6-5.8Z"
      />
    </>
  ),
  // 环境：层叠（多套环境）
  环境: (
    <>
      <path fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinejoin="round" d="M8 2 14 5.2 8 8.4 2 5.2Z" />
      <path fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" d="M2.4 8.4 8 11.4l5.6-3M2.4 11.2 8 14.2l5.6-3" />
    </>
  ),
  // 快捷键：键盘
  快捷键: (
    <>
      <rect x="1.5" y="4" width="13" height="8" rx="1.6" fill="none" stroke="currentColor" strokeWidth="1.4" />
      <path fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" d="M4 6.5h.01M6.4 6.5h.01M8.8 6.5h.01M11.2 6.5h.01M5 9.5h6" />
    </>
  ),
  // 关于：信息 i
  关于: (
    <>
      <circle cx="8" cy="8" r="5.8" fill="none" stroke="currentColor" strokeWidth="1.4" />
      <path fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" d="M8 7.4v3.4" />
      <circle cx="8" cy="5.1" r="0.9" fill="currentColor" />
    </>
  ),
};
function SettingsNavIcon({ name, className = "settings-nav-ico", size = 16 }: { name: string; className?: string; size?: number }) {
  return (
    <svg className={className} viewBox="0 0 16 16" width={size} height={size} aria-hidden="true">
      {SETTINGS_NAV_ICONS[name]}
    </svg>
  );
}
// 展开/折叠 chevron（默认指向右，展开时旋转 90° 指向下，带过渡）
function Chevron({ open }: { open: boolean }) {
  return (
    <svg className={`chevron ${open ? "is-open" : ""}`} viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
      <path fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" d="M6 3.5 10.5 8 6 12.5" />
    </svg>
  );
}
// 布局切换图标（仿 VSCode）：外框方块 + 对应一侧填充块；`on` 时高亮激活。
// side 决定填充块位置：left=左列、bottom=底行、right=右列。
function PanelIcon({ side }: { side: "left" | "bottom" | "right" }) {
  // 内部填充块的位置（16×16 视图，外框内边距 2）
  const fill =
    side === "left"
      ? { x: 2.5, y: 2.5, width: 4.5, height: 11 }
      : side === "right"
      ? { x: 9, y: 2.5, width: 4.5, height: 11 }
      : { x: 2.5, y: 9.5, width: 11, height: 4 };
  return (
    <svg className="panel-ico" viewBox="0 0 16 16" width="16" height="16" aria-hidden="true">
      <rect x="1.5" y="2.5" width="13" height="11" rx="1.6" fill="none" stroke="currentColor" strokeWidth="1.2" />
      <rect {...fill} rx="0.6" fill="currentColor" className="panel-ico-fill" />
    </svg>
  );
}
// 下拉指示 caret（指向下，展开时旋转 180° 指向上）
function CaretDown({ open }: { open: boolean }) {
  return (
    <svg className={`caret-down ${open ? "is-open" : ""}`} viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
      <path fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" d="M4 6 8 10 12 6" />
    </svg>
  );
}

function byteSize(s: string): string {
  const bytes = new Blob([s]).size;
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
}

// 行内「更多」按钮图标：水平三点（同 VSCode 文件树行尾的 ⋯）
function MoreIcon() {
  return (
    <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true" fill="currentColor">
      <circle cx="3.5" cy="8" r="1.3" />
      <circle cx="8" cy="8" r="1.3" />
      <circle cx="12.5" cy="8" r="1.3" />
    </svg>
  );
}

// 文件树节点（递归渲染，支持展开/折叠 + 右键菜单 + 行尾「⋯」菜单）
function TreeNode({
  entry,
  depth,
  expanded,
  childrenMap,
  selectedPath,
  menuPath,
  onToggle,
  onSelect,
  onContext,
  onMore,
}: {
  entry: DirEntry;
  depth: number;
  expanded: Set<string>;
  childrenMap: Record<string, DirEntry[]>;
  selectedPath: string;
  menuPath: string; // 菜单正打开的那一行：「⋯」保持显形，否则鼠标移到菜单上按钮会闪掉
  onToggle: (entry: DirEntry) => void;
  onSelect: (path: string) => void;
  onContext: (e: React.MouseEvent, entry: DirEntry) => void;
  onMore: (e: React.MouseEvent, entry: DirEntry) => void;
}) {
  const isOpen = expanded.has(entry.path);
  const children = childrenMap[entry.path];
  const isSelected = selectedPath === entry.path;
  const rowRef = useRef<HTMLDivElement>(null);
  // 成为选中项时（含展开后异步挂载）滚动到可见范围，最小滚动、不影响横向
  useEffect(() => {
    if (isSelected) rowRef.current?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [isSelected]);
  return (
    <div className="tree-node">
      <div
        ref={rowRef}
        className={`tree-row ${isSelected ? "selected" : ""} ${menuPath === entry.path ? "menu-open" : ""} ${
          entry.hidden ? "is-hidden-entry" : ""
        }`}
        style={{ paddingLeft: 16 + depth * 14 }}
        title={entry.name}
        onClick={() => (entry.isDir ? onToggle(entry) : onSelect(entry.path))}
        onContextMenu={(e) => onContext(e, entry)}
      >
        <span className={`tree-caret ${entry.isDir ? "" : "tree-caret-empty"}`}>{entry.isDir && <Chevron open={isOpen} />}</span>
        {entry.isDir ? <FolderIcon /> : <FileTypeIcon path={entry.path} />}
        <span className="tree-name">{entry.name}</span>
        <button
          type="button"
          className="tree-more"
          title="更多操作"
          aria-label="更多操作"
          // 行本身点了会展开/打开，这里必须拦住
          onClick={(e) => {
            e.stopPropagation();
            onMore(e, entry);
          }}
        >
          <MoreIcon />
        </button>
      </div>
      {entry.isDir && isOpen && children && (
        // --tree-depth 供 ::before 的缩进参考线定位（对齐本行的折叠箭头中线）
        <div className="tree-children" style={{ "--tree-depth": depth } as React.CSSProperties}>
          {/* 43 = 箭头 16 + 图标 15 + 两处 6px 间距，让提示文字与同级文件名左对齐 */}
          {children.length === 0 && (
            <div className="tree-empty" style={{ paddingLeft: 16 + (depth + 1) * 14 + 43 }}>
              空文件夹
            </div>
          )}
          {children.map((c) => (
            <TreeNode
              key={c.path}
              entry={c}
              depth={depth + 1}
              expanded={expanded}
              childrenMap={childrenMap}
              selectedPath={selectedPath}
              menuPath={menuPath}
              onToggle={onToggle}
              onSelect={onSelect}
              onContext={onContext}
              onMore={onMore}
            />
          ))}
        </div>
      )}
    </div>
  );
}

// 菜单项：sep=true 为分组分隔线（无 label / onClick）；disabled 项灰显不可点（如剪贴板为空时的「粘贴」）
type CtxItem =
  | { label: string; onClick: () => void; danger?: boolean; disabled?: boolean; sep?: false }
  | { sep: true };

// 右键菜单（文件树行、行尾「⋯」、标签页共用）
function ContextMenu({
  x,
  y,
  items,
  onClose,
}: {
  x: number;
  y: number;
  items: CtxItem[];
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  // 贴边翻转：菜单项变多后（目录 9 项），靠近窗口右/下缘时会被截断
  const [pos, setPos] = useState({ left: x, top: y });
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const { width, height } = el.getBoundingClientRect();
    const left = x + width > window.innerWidth - 8 ? Math.max(8, x - width) : x;
    const top = y + height > window.innerHeight - 8 ? Math.max(8, window.innerHeight - height - 8) : y;
    setPos({ left, top });
  }, [x, y, items.length]);
  useEffect(() => {
    function close() {
      onClose();
    }
    document.addEventListener("mousedown", close);
    window.addEventListener("blur", close);
    return () => {
      document.removeEventListener("mousedown", close);
      window.removeEventListener("blur", close);
    };
  }, [onClose]);
  return (
    <div ref={ref} className="ctx-menu" style={{ left: pos.left, top: pos.top }} onMouseDown={(e) => e.stopPropagation()}>
      {items.map((it, i) =>
        it.sep ? (
          <div key={i} className="ctx-sep" />
        ) : (
          <button
            key={i}
            className={`ctx-item ${it.danger ? "danger" : ""}`}
            disabled={it.disabled}
            onClick={() => {
              onClose();
              it.onClick();
            }}
          >
            {it.label}
          </button>
        ),
      )}
    </div>
  );
}

// 文本输入对话框（新建 folder / 重命名用）
function PromptDialog({
  title,
  initial,
  onOk,
  onCancel,
}: {
  title: string;
  initial: string;
  onOk: (v: string) => void;
  onCancel: () => void;
}) {
  const [v, setV] = useState(initial);
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);
  return (
    <div className="modal-mask" onMouseDown={onCancel}>
      <div className="modal" onMouseDown={(e) => e.stopPropagation()}>
        <div className="modal-title">{title}</div>
        <input
          ref={ref}
          className="modal-input"
          value={v}
          onChange={(e) => setV(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") onOk(v);
            else if (e.key === "Escape") onCancel();
          }}
        />
        <div className="modal-actions">
          <button className="btn-ghost" onClick={onCancel}>
            取消
          </button>
          <button className="btn-primary" onClick={() => onOk(v)}>
            确定
          </button>
        </div>
      </div>
    </div>
  );
}

/** 文案中被操作的对象（文件名、环境名、动作名）：以样式区分，不用书名号包裹 */
function Obj({ children }: { children: ReactNode }) {
  return <span className="q">{children}</span>;
}

/**
 * 确认对话框。取代 window.confirm——系统弹窗不跟随深色主题、按钮只能是「确定/取消」，
 * 而删除这类操作应当把动作写进按钮（「删除」而不是「确定」），破坏性的还要标红。
 */
type ConfirmOptions = {
  title: ReactNode;
  message?: ReactNode; // 副标题：补充后果，如「此操作不可撤销」
  confirmLabel?: string; // 默认「确定」；破坏性操作应写明动作
  danger?: boolean; // 主按钮转红
  onConfirm: () => void;
};

function ConfirmDialog({ title, message, confirmLabel = "确定", danger, onConfirm, onClose }: ConfirmOptions & { onClose: () => void }) {
  const okRef = useRef<HTMLButtonElement>(null);
  // 焦点直接落在主按钮上：回车即确认、Esc 取消，与系统弹窗的键盘习惯一致
  useEffect(() => okRef.current?.focus(), []);
  return (
    <div className="modal-mask" onMouseDown={onClose}>
      <div
        className="modal"
        onMouseDown={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Escape") onClose();
        }}
      >
        <div className="modal-title">{title}</div>
        {message && <div className="modal-message">{message}</div>}
        <div className="modal-actions">
          <button className="btn-ghost" onClick={onClose}>
            取消
          </button>
          <button
            ref={okRef}
            className={danger ? "btn-danger" : "btn-primary"}
            onClick={() => {
              onConfirm();
              onClose();
            }}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}

/** 返回 [要渲染的节点, 发起确认的函数]。各组件各持一份，无需全局状态。 */
function useConfirm() {
  const [opts, setOpts] = useState<ConfirmOptions | null>(null);
  const node = opts ? <ConfirmDialog {...opts} onClose={() => setOpts(null)} /> : null;
  return [node, setOpts] as const;
}

// 可视化新建 case 对话框（名称 + method + URL）
function NewCaseDialog({
  onOk,
  onCancel,
}: {
  onOk: (v: { name: string; method: string; url: string }) => void;
  onCancel: () => void;
}) {
  const [name, setName] = useState("新用例");
  const [method, setMethod] = useState("GET");
  const [url, setUrl] = useState("https://api.example.com");
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);
  const submit = () => {
    if (name.trim()) onOk({ name, method, url });
  };
  return (
    <div className="modal-mask" onMouseDown={onCancel}>
      <div className="modal modal-wide" onMouseDown={(e) => e.stopPropagation()}>
        <div className="modal-title">新建用例</div>
        <div className="field-row">
          <label>名称</label>
          <input
            ref={ref}
            value={name}
            placeholder="用例名（.yml 可省略）"
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") submit();
              else if (e.key === "Escape") onCancel();
            }}
          />
        </div>
        <div className="field-row">
          <label>请求</label>
          <Select
            className={`nc-method ${methodClass(method)}`}
            value={method}
            options={METHODS.map((m) => ({ value: m, label: m }))}
            onChange={(v) => setMethod(v)}
            ariaLabel="请求方法"
          />
          <input
            className="nc-url"
            value={url}
            placeholder="https://api.example.com/path"
            onChange={(e) => setUrl(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") submit();
              else if (e.key === "Escape") onCancel();
            }}
          />
        </div>
        <div className="modal-actions">
          <button className="btn-ghost" onClick={onCancel}>
            取消
          </button>
          <button className="btn-primary" onClick={submit}>
            创建
          </button>
        </div>
      </div>
    </div>
  );
}

// application.yml 的可视化设置页：左导航 + 右配置面板（仿 GitHub 设置页）
// 配置页「快捷键」分区：查看 + 录制重绑 + 冲突检测 + 恢复默认。
function ShortcutsSettings({
  overrides,
  onChange,
  enabled,
  onToggleEnabled,
}: {
  overrides: Overrides;
  onChange: (next: Overrides) => void;
  enabled: boolean;
  onToggleEnabled: (next: boolean) => void;
}) {
  const [recording, setRecording] = useState<ActionId | null>(null);
  const [confirmNode, askConfirm] = useConfirm();
  const bindings = resolveBindings(overrides);

  // 录制态：capture 阶段全局捕获 + stopPropagation，避免录制时触发全局快捷键分发
  useEffect(() => {
    const rec = recording;
    if (!rec) return;
    function onKey(e: KeyboardEvent) {
      if (!rec) return;
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape") {
        setRecording(null);
        return;
      }
      if (e.key === "Backspace" || e.key === "Delete") {
        onChange({ ...overrides, [rec]: "" }); // 清空 → 禁用
        setRecording(null);
        return;
      }
      const accel = eventToAccel(e);
      if (!accel) return; // 仅按了修饰键，等待主键
      const conflict = findConflict(resolveBindings(overrides), accel, rec);
      if (conflict) {
        // 确认是异步的，rec 在此闭包里捕获——setRecording(null) 不影响已取到的值
        const target = rec;
        askConfirm({
          title: `${formatAccel(accel)} 已被占用`,
          message: <>原绑定 <Obj>{ACTION_MAP[conflict].label}</Obj> 将被解绑</>,
          confirmLabel: "替换",
          onConfirm: () => onChange({ ...overrides, [conflict]: "", [target]: accelKey(accel) }),
        });
      } else {
        onChange({ ...overrides, [rec]: accelKey(accel) });
      }
      setRecording(null);
    }
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [recording, overrides, onChange]);

  const groups: { name: string; items: ActionDef[] }[] = [];
  for (const a of ACTIONS) {
    let g = groups.find((x) => x.name === a.group);
    if (!g) {
      g = { name: a.group, items: [] };
      groups.push(g);
    }
    g.items.push(a);
  }

  function restore(id: ActionId) {
    const n = { ...overrides };
    delete n[id];
    onChange(n);
  }

  return (
    <div className="settings-section">
      {/* 标题已由左导航指明，这行只剩右侧操作 */}
      <div className="sc-title-row">
        <span className="sc-title-actions">
          <button
            type="button"
            role="switch"
            aria-checked={enabled}
            title={enabled ? "停用全部快捷键" : "启用快捷键"}
            className={`sc-switch ${enabled ? "on" : ""}`}
            onClick={() => onToggleEnabled(!enabled)}
          >
            <span className="sc-switch-thumb" />
          </button>
          <button className="sc-btn sc-reset" onClick={() => onChange({})}>
            全部恢复默认
          </button>
        </span>
      </div>
      <div className={`sc-list ${enabled ? "" : "is-off"}`}>
        {groups.map((g) => (
          <div key={g.name} className="sc-group">
            <div className="sc-group-name">{g.name}</div>
            {g.items.map((a) => {
              const accel = bindings[a.id];
              const isRec = recording === a.id;
            return (
              <div key={a.id} className="sc-row">
                <span className="sc-label">{a.label}</span>
                <span className="sc-spacer" />
                {isRec ? (
                  <span className="sc-badge recording">按下快捷键…</span>
                ) : accel ? (
                  <span className="sc-keys">
                    {accelTokens(accel).map((t, i) => (
                      <kbd key={i} className="sc-key">
                        {t}
                      </kbd>
                    ))}
                  </span>
                ) : (
                  <span className="sc-badge disabled">已禁用</span>
                )}
                <button className="sc-btn" onClick={() => setRecording(isRec ? null : a.id)}>
                  {isRec ? "取消" : "修改"}
                </button>
                {!isDefaultBinding(overrides, a.id) && (
                  <button className="sc-btn ghost" onClick={() => restore(a.id)}>
                    恢复
                  </button>
                )}
              </div>
            );
          })}
          </div>
        ))}
      </div>
      {confirmNode}
    </div>
  );
}

// 应用元信息（与 package.json / tauri.conf.json 保持一致）
const APP_VERSION = "0.1.0";
/** case 文件的 schema 版本（`apicase:` 字段）。带 `v` 前缀故不是数字形态，落盘不必加引号。 */
const CASE_VERSION = "v0.1";
const APP_REPO = "https://github.com/v2hoping/apicase";

type SysInfo = { os: string; arch: string; chip: string };

// 配置页「关于」分区：应用元信息 + 一句话简介 + 系统信息 + 外链
function AboutSettings() {
  const [sys, setSys] = useState<SysInfo | null>(null);
  useEffect(() => {
    invoke<SysInfo>("system_info")
      .then(setSys)
      .catch(() => {});
  }, []);
  function openLink(url: string) {
    openUrl(url).catch(() => {});
  }
  return (
    <div className="settings-section about">
      <img className="about-logo" src="/nautilus.svg" alt="" draggable={false} />
      <div className="about-title">
        <div className="about-name">Apicase</div>
        <div className="about-ver">版本 {APP_VERSION}</div>
      </div>
      <p className="about-intro">一款 AI 原生的 API 自动化测试工具，集接口调试、接口管理、用例编排与文件即数据于一体</p>
      <div className="about-sys">
        <div className="about-sys-row">
          <span className="about-sys-k">操作系统</span>
          <span className="about-sys-v">{sys?.os || "—"}</span>
        </div>
        <div className="about-sys-row">
          <span className="about-sys-k">芯片</span>
          <span className="about-sys-v">{sys ? `${sys.chip}（${sys.arch}）` : "—"}</span>
        </div>
      </div>
      <button className="about-link" onClick={() => openLink(APP_REPO)}>
        <svg width="15" height="15" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">
          <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8Z" />
        </svg>
        项目主页 / 源码
      </button>
      <div className="about-copyright">© 2026 Apicase</div>
    </div>
  );
}

const THEME_OPTIONS: { mode: ThemeMode; label: string }[] = [
  { mode: "light", label: "浅色" },
  { mode: "dark", label: "深色" },
  { mode: "system", label: "跟随系统" },
];

const PROXY_OPTIONS: { mode: ProxyMode; label: string; desc: string }[] = [
  { mode: "system", label: "跟随系统", desc: "使用系统 / 环境变量配置的代理（默认）" },
  { mode: "none", label: "不使用代理", desc: "直连，忽略系统代理（调试本地服务推荐）" },
  { mode: "custom", label: "自定义", desc: "指定 http / https 代理地址" },
];

// 「显示位置」图标：方框 + 右上外指箭头（通用的 reveal / open-external 语义）
function RevealIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5">
      <path d="M13 9v3.5a1 1 0 0 1-1 1H3.5a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1H7" strokeLinecap="round" />
      <path d="M9.5 2.5H13.5V6.5" strokeLinecap="round" strokeLinejoin="round" />
      <path d="M13.5 2.5 8 8" strokeLinecap="round" />
    </svg>
  );
}

/**
 * 一行「位置」：标签 + 只读全路径 + 在系统文件管理器中显示。
 * exists 省略时视为存在（工作空间这类必然存在的路径不必额外查一次盘）；
 * 明确为 false 时禁用按钮——对不存在的路径调 revealItemInDir 会直接抛错。
 */
function PathRow({
  label,
  path,
  note,
  exists,
  home = "",
}: {
  label: string;
  path: string;
  note?: string;
  exists?: boolean;
  home?: string;
}) {
  const usable = path.trim() !== "" && exists !== false;
  return (
    <div className="path-row">
      <div className="field-row">
        <label>{label}</label>
        {/* 用可换行的文本块而非 input：路径很长，截断了就失去「告诉用户在哪」的意义。
            显示走 ~ 缩写，title 给出未缩写的原路径 */}
        <div className="path-value" title={path}>
          <span className="path-text">{path ? tildify(path, home) : "—"}</span>
          {/* 状态标签就地贴在路径末尾——它同时解释了右侧按钮为何禁用 */}
          {exists === false && <span className="path-tag">尚未创建</span>}
        </div>
        <button
          type="button"
          className="path-reveal"
          title={usable ? "在文件管理器中显示" : "该路径尚不存在"}
          aria-label="在文件管理器中显示"
          disabled={!usable}
          onClick={() => revealItemInDir(path).catch(() => {})}
        >
          <RevealIcon />
        </button>
      </div>
      {note && <div className="path-note">{note}</div>}
    </div>
  );
}

function SettingsPage({
  environments,
  onChange,
  workspacePath,
  configPath,
  shortcutOverrides,
  onShortcutChange,
  shortcutsEnabled,
  onShortcutsEnabledChange,
  themeMode,
  onThemeChange,
  proxyConfig,
  onProxyChange,
  wsSettings,
  onWsSettingsChange,
}: {
  environments: Record<string, Record<string, string>>;
  onChange: (next: Record<string, Record<string, string>>) => void;
  workspacePath: string;
  configPath: string;
  shortcutOverrides: Overrides;
  onShortcutChange: (next: Overrides) => void;
  shortcutsEnabled: boolean;
  onShortcutsEnabledChange: (next: boolean) => void;
  themeMode: ThemeMode;
  onThemeChange: (next: ThemeMode) => void;
  proxyConfig: ProxyConfig;
  onProxyChange: (next: ProxyConfig) => void;
  wsSettings: WorkspaceSettings;
  onWsSettingsChange: (next: WorkspaceSettings) => void;
}) {
  // 导航分两组：上＝跟随**项目**（工作空间 / application.yml），下＝跟随**应用**（settings.json）
  const NAV_PROJECT = ["通用", "环境"] as const;
  const NAV_APP = ["主题", "代理", "快捷键", "关于"] as const;
  const NAV = [...NAV_PROJECT, ...NAV_APP] as const;
  const [section, setSection] = useState<(typeof NAV)[number]>("通用");
  const envNames = Object.keys(environments);
  const [selEnv, setSelEnv] = useState(envNames[0] || "");
  const cur = envNames.includes(selEnv) ? selEnv : envNames[0] || "";
  const [envQuery, setEnvQuery] = useState("");
  const shownEnvs = envNames.filter((e) => e.toLowerCase().includes(envQuery.trim().toLowerCase()));
  // 改名走草稿：逐字改 key 会让 activeEnv 抖动，故失焦/回车才提交
  const [nameDraft, setNameDraft] = useState(cur);
  // 重名提示：就地显示在名称输入框下方，不弹模态——用户正要改这个名字
  const [renameError, setRenameError] = useState("");
  useEffect(() => {
    setNameDraft(cur);
    setRenameError("");
  }, [cur]);

  // 应用侧存储位置（配置目录 / settings.json）：由 Tauri 按 identifier 推导，各平台不同，
  // 只能问后端要。进入「通用」页时取一次，顺带拿到「是否已创建」用于禁用「显示位置」。
  const [appPaths, setAppPaths] = useState<AppPaths | null>(null);
  useEffect(() => {
    if (section !== "通用") return;
    let alive = true;
    invoke<AppPaths>("app_paths")
      .then((p) => alive && setAppPaths(p))
      .catch(() => alive && setAppPaths(null));
    return () => {
      alive = false;
    };
  }, [section]);

  // 报告目录是否已创建（首次运行前并不存在，「显示位置」要据此禁用）
  const [reportsDirExists, setReportsDirExists] = useState<boolean | undefined>(undefined);
  useEffect(() => {
    if (section !== "通用" || !workspacePath) return;
    let alive = true;
    invoke<boolean>("path_exists", { path: joinPath(workspacePath, REPORTS_REL) })
      .then((e) => alive && setReportsDirExists(e))
      .catch(() => alive && setReportsDirExists(false));
    return () => {
      alive = false;
    };
  }, [section, workspacePath]);

  const [confirmNode, askConfirm] = useConfirm();

  // 「自定义 CA」下拉的候选：工作空间内的证书文件（相对路径）。
  // 每次进入「通用」页重扫一次——用户常是刚把证书拷进工作空间就回来选。
  const [certFiles, setCertFiles] = useState<string[]>([]);
  useEffect(() => {
    if (section !== "通用" || !workspacePath) return;
    let alive = true;
    invoke<string[]>("list_cert_files", { root: workspacePath })
      .then((files) => alive && setCertFiles(files))
      .catch(() => alive && setCertFiles([]));
    return () => {
      alive = false;
    };
  }, [section, workspacePath]);
  // 已配置的证书当前扫不到（被删 / 改名 / 换了工作空间）：仍列进候选并给出提示，不静默丢弃设置
  const caCertMissing = !!wsSettings.caCert && certFiles.length > 0 && !certFiles.includes(wsSettings.caCert);
  const caCertOptions = useMemo(
    () => [
      { value: "", label: "未选择" },
      ...(caCertMissing ? [wsSettings.caCert, ...certFiles] : certFiles).map((f) => ({ value: f, label: f })),
    ],
    [certFiles, caCertMissing, wsSettings.caCert],
  );

  function setVars(env: string, rows: { name: string; value: string; enabled?: boolean }[]) {
    const m: Record<string, string> = {};
    for (const r of rows) if (r.name.trim()) m[r.name.trim()] = r.value;
    onChange({ ...environments, [env]: m });
  }
  function addEnv() {
    let n = "新环境";
    for (let i = 2; environments[n]; i++) n = `新环境 ${i}`;
    onChange({ ...environments, [n]: {} });
    setSelEnv(n);
    setEnvQuery(""); // 否则新环境可能被搜索词过滤掉
  }
  function delEnv(env: string) {
    askConfirm({
      title: <>删除环境 <Obj>{env}</Obj>？</>,
      message: "其中的变量一并移除",
      confirmLabel: "删除",
      danger: true,
      onConfirm: () => {
        const next = { ...environments };
        delete next[env];
        onChange(next);
        setSelEnv(Object.keys(next)[0] || "");
      },
    });
  }
  function commitRename() {
    const n = nameDraft.trim();
    if (!n || n === cur) {
      setNameDraft(cur);
      setRenameError("");
      return;
    }
    if (environments[n]) {
      // 保留用户输入而非回滚：他要的是改个名字，回滚等于让他从头再来
      setRenameError(`环境 ${n} 已存在`);
      return;
    }
    setRenameError("");
    const next: Record<string, Record<string, string>> = {};
    for (const [k, v] of Object.entries(environments)) next[k === cur ? n : k] = v; // 重建以保持顺序
    onChange(next);
    setSelEnv(n);
  }

  return (
    <div className="settings">
      <nav className="settings-nav">
        {NAV.map((s, i) => [
          // 项目组与应用组之间插入分割线，把「跟随项目」与「跟随应用存储」的数据分开显示
          i === NAV_PROJECT.length ? <div key="__nav-divider" className="settings-nav-divider" /> : null,
          <button key={s} className={`settings-nav-item ${section === s ? "active" : ""}`} onClick={() => setSection(s)}>
            <SettingsNavIcon name={s} />
            <span>{s}</span>
          </button>,
        ])}
      </nav>
      <div className={`settings-panel ${section === "环境" ? "is-env" : ""}`}>
        {section === "环境" && (
          <div className="settings-section env-manage">
            <div className="env-manage-body">
              <div className="env-side">
                <div className="env-side-head">
                  <div className="tree-search-wrap">
                    <span className="tree-search-icon">⌕</span>
                    <input
                      className="tree-search"
                      placeholder="搜索环境…"
                      value={envQuery}
                      onChange={(e) => setEnvQuery(e.target.value)}
                    />
                    {/* 始终占位：无文字时隐藏但保留宽度，避免出现/消失时搜索栏输入区长度跳动 */}
                    <button
                      className={`tree-search-clear ${envQuery ? "" : "is-hidden"}`}
                      title="清空"
                      onClick={() => setEnvQuery("")}
                    >
                      ×
                    </button>
                  </div>
                  <button className="tree-add env-add" title="新增环境" onClick={addEnv}>
                    ＋
                  </button>
                </div>
                <div className="env-side-list">
                  {shownEnvs.map((e) => (
                    <div key={e} className={`env-row ${e === cur ? "active" : ""}`} onClick={() => setSelEnv(e)}>
                      <span className="env-row-name">{e}</span>
                      <button
                        className="env-row-del"
                        title="删除环境"
                        onClick={(ev) => {
                          ev.stopPropagation();
                          delEnv(e);
                        }}
                      >
                        ×
                      </button>
                    </div>
                  ))}
                  {!shownEnvs.length && <div className="settings-empty">{envNames.length ? "无匹配环境" : "暂无环境，点 ＋ 新建。"}</div>}
                </div>
              </div>
              {cur ? (
                <div className="env-detail">
                  <div className="env-detail-head">
                    <input
                      className={`env-name-input ${renameError ? "is-invalid" : ""}`}
                      value={nameDraft}
                      placeholder="环境名称"
                      onChange={(e) => {
                        setNameDraft(e.target.value);
                        if (renameError) setRenameError("");
                      }}
                      onBlur={commitRename}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") e.currentTarget.blur();
                        if (e.key === "Escape") {
                          setNameDraft(cur);
                          setRenameError("");
                          e.currentTarget.blur();
                        }
                      }}
                    />
                    {renameError && <div className="field-error">{renameError}</div>}
                  </div>
                  <div className="env-detail-body">
                    <KVTable
                      rows={Object.entries(environments[cur] || {}).map(([name, value]) => ({ name, value, enabled: true }))}
                      onChange={(rows) => setVars(cur, rows)}
                      namePlaceholder="变量名"
                      valuePlaceholder="值（如 https://api.demo.com）"
                      hideEnabled
                    />
                  </div>
                </div>
              ) : (
                <div className="env-detail is-empty">
                  <div className="settings-empty">选择或新建一个环境</div>
                </div>
              )}
            </div>
          </div>
        )}
        {section === "通用" && (
          <div className="settings-section">

            <div className="settings-subtitle is-first">位置</div>
            <PathRow label="工作空间" path={workspacePath} home={appPaths?.home} />
            <PathRow label="工作空间配置" path={configPath} home={appPaths?.home} />
            {wsSettings.useCustomCa && wsSettings.caCert.trim() && (
              <PathRow
                label="CA 证书"
                path={workspacePath ? joinPath(workspacePath, wsSettings.caCert.trim()) : wsSettings.caCert}
                home={appPaths?.home}
                note={`配置中记为相对路径 ${wsSettings.caCert.trim()}`}
              />
            )}
            {/* 报告目录在 .apicase/ 下、文件树默认不显示，不在这里列出用户就无从知道它在哪。
                首次运行前目录并不存在，故 exists 交给 reportsDirExists 决定按钮可否点。 */}
            <PathRow
              label="运行报告"
              path={workspacePath ? joinPath(workspacePath, REPORTS_REL) : ""}
              exists={reportsDirExists}
              home={appPaths?.home}
            />
            <PathRow
              label="应用设置"
              path={appPaths?.settingsFile || ""}
              exists={appPaths?.settingsFileExists}
              home={appPaths?.home}
            />

            <div className="settings-subtitle">请求</div>

            {/* ① 证书验证：关闭是降安全操作，故紧跟一条常驻警示 */}
            <div className="set-row">
              <div className="set-row-main">
                <div className="set-row-label">SSL/TLS 证书验证</div>
              </div>
              <button
                type="button"
                role="switch"
                aria-checked={wsSettings.verifySsl}
                title={wsSettings.verifySsl ? "关闭证书验证" : "开启证书验证"}
                className={`sc-switch ${wsSettings.verifySsl ? "on" : ""}`}
                onClick={() => onWsSettingsChange({ ...wsSettings, verifySsl: !wsSettings.verifySsl })}
              >
                <span className="sc-switch-thumb" />
              </button>
            </div>
            {/* 只讲当下决策要用到的：风险是什么、能用在哪。
                原理（中间人如何无症状地解密篡改、为何请求照常返回 200）与「该设置随 application.yml
                提交给团队」这类背景，属于知识而非状态，待内嵌文档落地后挂「了解风险」链接过去。 */}
            {!wsSettings.verifySsl && (
              <div className="text-warn">
                已关闭证书验证：<b>无法确认对端身份</b>，仅用于本机 / 内网调试。
              </div>
            )}

            {/* ② 自定义 CA：校验关掉后它不再起作用，整项灰显并说明 */}
            <div className={`set-row ${wsSettings.verifySsl ? "" : "is-off"}`}>
              <div className="set-row-main">
                <div className="set-row-label">使用自定义 CA 证书</div>
                {/* 仅在被禁用时说明原因——否则整项灰着点不动却不给缘由 */}
                {!wsSettings.verifySsl && <div className="set-row-desc">证书验证已关闭，自定义 CA 不再起作用。</div>}
              </div>
              <button
                type="button"
                role="switch"
                aria-checked={wsSettings.useCustomCa}
                disabled={!wsSettings.verifySsl}
                title={wsSettings.useCustomCa ? "停用自定义 CA" : "启用自定义 CA"}
                className={`sc-switch ${wsSettings.useCustomCa ? "on" : ""}`}
                onClick={() => onWsSettingsChange({ ...wsSettings, useCustomCa: !wsSettings.useCustomCa })}
              >
                <span className="sc-switch-thumb" />
              </button>
            </div>
            {wsSettings.verifySsl && wsSettings.useCustomCa && (
              <>
                <div className="field-row">
                  <label>证书文件</label>
                  <Select
                    className="cert-select"
                    ariaLabel="选择 CA 证书文件"
                    value={wsSettings.caCert}
                    disabled={certFiles.length === 0}
                    placeholder="未选择"
                    options={caCertOptions}
                    onChange={(v) => onWsSettingsChange({ ...wsSettings, caCert: v })}
                  />
                </div>
                {/* 仅在两种异常态给提示：无候选、已配置项失效。正常选中时不占版面 */}
                {certFiles.length === 0 ? (
                  <div className="settings-hint">工作空间内未找到证书文件（.pem / .crt / .cer / .der / .ca-bundle）</div>
                ) : (
                  caCertMissing && <div className="settings-hint is-warn">已配置的 {wsSettings.caCert} 不在工作空间内</div>
                )}
              </>
            )}

            {/* ③ 超时：0 显示为空，「不填即不限制」比显示一个 0 更自然 */}
            <div className="set-row">
              <div className="set-row-main">
                <div className="set-row-label">请求超时时间</div>
              </div>
              <div className="set-row-input">
                <input
                  type="number"
                  min={0}
                  step={100}
                  placeholder="0（不限制）"
                  value={wsSettings.timeoutMs || ""}
                  onChange={(e) => {
                    const n = Math.floor(Number(e.target.value));
                    onWsSettingsChange({ ...wsSettings, timeoutMs: Number.isFinite(n) && n > 0 ? n : 0 });
                  }}
                />
                <span className="set-row-unit">毫秒</span>
              </div>
            </div>
          </div>
        )}
        {section === "主题" && (
          <div className="settings-section">
            <div className="theme-options">
              {THEME_OPTIONS.map((o) => (
                <button
                  key={o.mode}
                  className={`theme-card ${themeMode === o.mode ? "active" : ""}`}
                  onClick={() => onThemeChange(o.mode)}
                >
                  <span className={`theme-swatch is-${o.mode}`} aria-hidden="true" />
                  <span className="theme-card-label">{o.label}</span>
                  {themeMode === o.mode && <span className="theme-card-check">✓</span>}
                </button>
              ))}
            </div>
          </div>
        )}
        {section === "代理" && (
          <div className="settings-section">
            <div className="proxy-options">
              {PROXY_OPTIONS.map((o) => (
                <button
                  key={o.mode}
                  className={`proxy-card ${proxyConfig.mode === o.mode ? "active" : ""}`}
                  onClick={() => onProxyChange({ ...proxyConfig, mode: o.mode })}
                >
                  <span className="proxy-card-label">{o.label}</span>
                  <span className="proxy-card-desc">{o.desc}</span>
                  {proxyConfig.mode === o.mode && <span className="proxy-card-check">✓</span>}
                </button>
              ))}
            </div>
            {proxyConfig.mode === "custom" && (
              <div className="field-row proxy-url-row">
                <label>代理地址</label>
                <input
                  value={proxyConfig.url}
                  placeholder="http://127.0.0.1:7890"
                  onChange={(e) => onProxyChange({ ...proxyConfig, url: e.target.value })}
                />
              </div>
            )}
          </div>
        )}
        {section === "快捷键" && (
          <ShortcutsSettings
            overrides={shortcutOverrides}
            onChange={onShortcutChange}
            enabled={shortcutsEnabled}
            onToggleEnabled={onShortcutsEnabledChange}
          />
        )}
        {section === "关于" && <AboutSettings />}
      </div>
      {confirmNode}
    </div>
  );
}

// 标签页栏（多文件打开）：中键 / × 关闭，右键弹关闭菜单
/**
 * 运行配置对话框。
 *
 * **列出将要运行的用例是必备项**——Newman / Bruno 都没有，结果是用户经常不知道自己刚跑了什么。
 * 本期只暴露「递归」与「环境」两个选项：并发默认串行、失败继续、输出目录固定，
 * runner 侧留了参数位，加 UI 时不必改执行语义。
 */
function RunDialog({
  state,
  workspaceRoot,
  environments,
  onRecursive,
  onEnv,
  onRun,
  onCancel,
}: {
  state: RunDialogState;
  workspaceRoot: string;
  environments: Record<string, Record<string, string>>;
  onRecursive: (v: boolean) => void;
  onEnv: (v: string) => void;
  onRun: () => void;
  onCancel: () => void;
}) {
  const envNames = Object.keys(environments);
  const rel = relPath(workspaceRoot, state.target);
  const files = state.files;
  const scanning = files === null;
  const canRun = !scanning && files.length > 0;

  return (
    <div className="modal-mask" onMouseDown={onCancel}>
      <div
        className="modal run-modal"
        onMouseDown={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Escape") onCancel();
          if (e.key === "Enter" && canRun) onRun();
        }}
      >
        <div className="modal-title">运行并生成报告</div>
        <div className="modal-message">
          {state.isDir ? "目录" : "用例"} <Obj>{rel || baseName(state.target) || "工作空间根"}</Obj>
        </div>

        <div className="run-opts">
          {state.isDir && (
            <label className="run-opt">
              <span className="run-opt-label">范围</span>
              <span className="seg-radio">
                <button className={state.recursive ? "on" : ""} onClick={() => onRecursive(true)}>
                  含子目录
                </button>
                <button className={!state.recursive ? "on" : ""} onClick={() => onRecursive(false)}>
                  仅当前目录
                </button>
              </span>
            </label>
          )}
          <label className="run-opt">
            <span className="run-opt-label">环境</span>
            {envNames.length ? (
              <Select
                value={state.env}
                options={envNames.map((n) => ({ value: n, label: n }))}
                onChange={onEnv}
              />
            ) : (
              <span className="run-opt-none">未配置环境</span>
            )}
          </label>
        </div>

        <div className="run-preview">
          <div className="run-preview-head">
            {scanning ? "正在扫描用例…" : `将运行 ${files.length} 个用例`}
          </div>
          {!scanning && files.length > 0 && (
            <ul className="run-preview-list">
              {files.map((f) => (
                <li key={f} title={relPath(workspaceRoot, f)}>
                  {relPath(workspaceRoot, f)}
                </li>
              ))}
            </ul>
          )}
          {!scanning && files.length === 0 && (
            <div className="run-preview-empty">
              这里没有可运行的用例（只认 <code>.yml</code> / <code>.yaml</code>，不含 application.yml）。
            </div>
          )}
        </div>

        <div className="modal-actions">
          <button className="btn-ghost" onClick={onCancel}>
            取消
          </button>
          <button className="btn-primary" onClick={onRun} disabled={!canRun}>
            运行
          </button>
        </div>
      </div>
    </div>
  );
}

/**
 * 运行报告面板 = 原生工具栏（iframe 外）+ 报告页 iframe（iframe 内）。
 *
 * **报告内容只有一套渲染实现**：iframe 里跑的 REPORT_SHELL 与落盘 report.html 出自同一模板，
 * 因此「应用内看到的」「历史回看的」「发给同事的」三处像素级一致。若这里改用 React 再画一遍，
 * 改一次配色就要改两处，必然漂移。
 *
 * 反过来，**运行控制留在 iframe 外**——分发出去的报告里本就不该有「取消」按钮。
 *
 * 数据经 postMessage 推送而非重塞 srcdoc：整页重画会丢掉用户展开的详情与滚动位置。
 */
/** 报告空壳的进程级缓存：它不随运行数据变化，取一次就够，多个报告标签共用。 */
let reportShellCache: Promise<string> | null = null;
function loadReportShell(): Promise<string> {
  reportShellCache ??= reportShell();
  return reportShellCache;
}

function RunReportPane({
  session,
  theme,
  onCancel,
  onOpenCase,
  onOpenExternal,
  onReveal,
}: {
  session: RunSession;
  theme: "light" | "dark";
  onCancel: () => void;
  onOpenCase: (file: string) => void;
  onOpenExternal: () => void;
  onReveal: () => void;
}) {
  const frameRef = useRef<HTMLIFrameElement>(null);
  const readyRef = useRef(false);
  const { report } = session;
  // 空壳是编译期常量（与运行数据无关），取一次缓存起来即可；
  // 拿到之前不渲染 iframe——srcDoc 一变就会重新加载整个文档，握手状态会跟着作废。
  const [shell, setShell] = useState("");
  useEffect(() => {
    let alive = true;
    void loadReportShell().then((html) => {
      if (alive) setShell(html);
    });
    return () => {
      alive = false;
    };
  }, []);

  const post = (msg: unknown) => {
    // srcdoc 的 origin 是 null，只能用 "*"；反向消息在下面按 type 白名单处理。
    frameRef.current?.contentWindow?.postMessage(msg, "*");
  };

  // 报告页加载完成 → 握手（宿主专属动作据此显形）→ 推主题 → 推数据
  useEffect(() => {
    function onMessage(e: MessageEvent) {
      if (e.source !== frameRef.current?.contentWindow) return;
      const d = e.data as { type?: string; file?: string };
      if (!d || typeof d !== "object") return;
      if (d.type === "ready") {
        readyRef.current = true;
        post({ type: "host", app: "apicase" });
        post({ type: "theme", mode: theme });
        if (report) post({ type: "report", report });
      } else if (d.type === "open-case" && typeof d.file === "string") {
        onOpenCase(d.file);
      }
    }
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [report, theme, onOpenCase]);

  useEffect(() => {
    if (readyRef.current) post({ type: "theme", mode: theme });
  }, [theme]);

  useEffect(() => {
    if (readyRef.current && report) post({ type: "report", report });
  }, [report]);

  const s = report?.summary;
  const done = s ? s.passed + s.failed + s.error + s.skipped : 0;
  const total = Math.max(session.total, s?.total ?? 0, 1);
  const running = !session.readOnly && report?.status === "running";
  const pct = Math.round((done / total) * 100);

  return (
    <div className="run-pane">
      <div className="run-bar">
        {running ? (
          <>
            <span className="run-spinner" aria-hidden="true" />
            <span className="run-count">
              {done} / {total}
            </span>
            <div className="run-progress">
              <div className="run-progress-fill" style={{ width: `${pct}%` }} />
            </div>
          </>
        ) : (
          <span className={`run-verdict ${s && s.failed + s.error > 0 ? "bad" : "ok"}`}>
            {report?.status === "cancelled" ? "已取消" : s && s.failed + s.error > 0 ? "未全部通过" : "全部通过"}
          </span>
        )}
        {s && (
          <span className="run-stats">
            <span className="ok">✓ {s.passed}</span>
            {s.failed > 0 && <span className="bad">✕ {s.failed}</span>}
            {s.error > 0 && <span className="warn">! {s.error}</span>}
            {s.skipped > 0 && <span className="mute">– {s.skipped}</span>}
          </span>
        )}
        <span className="run-bar-spacer" />
        {running && (
          <button className="btn-ghost sm" onClick={onCancel} disabled={session.cancelling}>
            {session.cancelling ? "正在停止…" : "取消"}
          </button>
        )}
        <button className="btn-ghost sm" onClick={onReveal} title={session.file}>
          显示位置
        </button>
        <button className="btn-ghost sm" onClick={onOpenExternal}>
          在浏览器中打开
        </button>
      </div>
      {shell && (
        <iframe
          ref={frameRef}
          className="run-frame"
          title="运行报告"
          // 不给 allow-same-origin：给了就等于放弃隔离，报告页将能访问父窗口与存储
          sandbox="allow-scripts"
          srcDoc={shell}
        />
      )}
    </div>
  );
}

function TabBar({
  tabs,
  active,
  isDirty,
  labelOf,
  onSelect,
  onClose,
  onContext,
}: {
  tabs: string[];
  active: string;
  isDirty: (path: string) => boolean;
  labelOf: (path: string) => string;
  onSelect: (path: string) => void;
  onClose: (path: string) => void;
  onContext: (e: React.MouseEvent, path: string) => void;
}) {
  return (
    <div className="tab-bar">
      {tabs.map((path) => (
        <div
          key={path}
          className={`file-tab ${path === active ? "active" : ""}`}
          title={isRunTab(path) ? labelOf(path) : path}
          onMouseDown={(e) => {
            if (e.button === 1) {
              e.preventDefault();
              onClose(path);
            }
          }}
          onClick={() => onSelect(path)}
          onContextMenu={(e) => onContext(e, path)}
        >
          <FileTypeIcon path={path} />
          <span className="ft-name">{labelOf(path)}</span>
          <span className="ft-right">
            {isDirty(path) && <span className="ft-dirty" />}
            <button
              className="ft-close"
              title="关闭"
              onClick={(e) => {
                e.stopPropagation();
                onClose(path);
              }}
            >
              ×
            </button>
          </span>
        </div>
      ))}
    </div>
  );
}

// 三栏布局显隐标志（左文件树 / 底部终端 / 右侧 AI）。
// 用 sessionStorage 而非 localStorage：只在本次运行内记忆（含 dev 热重载/刷新），
// 应用整体关闭再启动即视为全新会话，回退到默认「三栏全关」。
interface LayoutFlags {
  left: boolean;
  bottom: boolean;
  right: boolean;
}
const LAYOUT_KEY = "apicase.layout.v1";
function loadLayout(): LayoutFlags {
  const fallback: LayoutFlags = { left: false, bottom: false, right: false };
  try {
    const raw = sessionStorage.getItem(LAYOUT_KEY);
    if (!raw) return fallback;
    const o = JSON.parse(raw);
    return {
      left: typeof o.left === "boolean" ? o.left : fallback.left,
      bottom: typeof o.bottom === "boolean" ? o.bottom : fallback.bottom,
      right: typeof o.right === "boolean" ? o.right : fallback.right,
    };
  } catch {
    return fallback;
  }
}

function App() {
  // 应用级设置（settings.json）：首帧先用 localStorage 镜像的同步值起手，挂载后再以磁盘值为准。
  // 只在挂载时取一次快照——后续各 state 自行演进，统一由下方写回 effect 落盘。
  const [cachedSettings] = useState(loadCachedSettings);
  // 确认对话框（取代 window.confirm）
  const [confirmNode, askConfirm] = useConfirm();
  // 工作空间
  const [workspace, setWorkspace] = useState("");
  // 最近打开的工作空间：持久化到应用配置目录 settings.json（见 settings.ts）
  const [recentWorkspaces, setRecentWorkspaces] = useState<string[]>([]);
  const settingsLoaded = useRef(false);
  // 磁盘上当前内容的序列化快照，用于跳过无变化的写回（见下方写回 effect 的说明）
  const lastSavedRef = useRef<string>("");
  const [wsMenuOpen, setWsMenuOpen] = useState(false);
  const wsMenuRef = useRef<HTMLDivElement>(null);
  const [isFullscreen, setIsFullscreen] = useState(false);
  // 默认 260：够放下「05-延迟与重定向」这类目录名，不用一上来就拖宽
  // （拖动范围仍是 160~480，见下方 onMove）
  const [sidebarWidth, setSidebarWidth] = useState(260);
  const resizingRef = useRef(false);
  // 三栏布局显隐（顶栏切换）：左=文件树 / 底=终端 / 右=AI 对话；仅本次运行内记忆
  const [layout, setLayout] = useState<LayoutFlags>(() => loadLayout());
  const { left: showLeft, bottom: showBottom, right: showRight } = layout;
  const toggleBottom = () => setLayout((l) => ({ ...l, bottom: !l.bottom }));
  const toggleRight = () => setLayout((l) => ({ ...l, right: !l.right }));
  // 左栏可独立开关：无工作空间时开关照常可用，仅内容显示为空态提示（引导打开工作空间）
  const effectiveShowLeft = showLeft;
  const toggleLeft = () => setLayout((l) => ({ ...l, left: !l.left }));
  // 底部终端一旦打开即常驻（隐藏而非卸载），保持 shell 会话与滚动；右侧 AI 同理
  const termEverOpened = useRef(false);
  if (showBottom) termEverOpened.current = true;
  const aiEverOpened = useRef(false);
  if (showRight) aiEverOpened.current = true;
  // 底部终端高度（px，可拖）+ 右侧 AI 宽度（px，可拖）
  const [bottomHeight, setBottomHeight] = useState(240);
  const bottomResizingRef = useRef(false);
  // 多终端（仿 VSCode/Postman）：底部栏可开多个 shell，右侧列表切换/关闭。
  // cwd 在创建时快照——切换工作空间不影响已开终端；新开的终端用当前工作空间。
  const [terminals, setTerminals] = useState<{ id: string; cwd: string; n: number }[]>([]);
  const [activeTermId, setActiveTermId] = useState("");
  const termSeqRef = useRef(0);
  function addTerminal() {
    const n = termSeqRef.current + 1;
    termSeqRef.current = n;
    const t = { id: `bterm-${n}`, cwd: workspace, n };
    setTerminals((prev) => [...prev, t]);
    setActiveTermId(t.id);
  }
  function closeTerminal(id: string) {
    const idx = terminals.findIndex((t) => t.id === id);
    if (idx < 0) return;
    const next = terminals.filter((t) => t.id !== id);
    setTerminals(next);
    if (activeTermId === id) {
      const neighbor = next[idx] || next[idx - 1];
      setActiveTermId(neighbor ? neighbor.id : "");
    }
    if (next.length === 0) setLayout((l) => ({ ...l, bottom: false })); // 关掉最后一个即收起底部栏
  }
  // 底部栏打开且尚无终端时，自动创建一个（首次开栏 / 关净后重开）
  useEffect(() => {
    if (showBottom && terminals.length === 0) addTerminal();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [showBottom]);
  const [aiWidth, setAiWidth] = useState(320);
  const aiResizingRef = useRef(false);
  const centerColRef = useRef<HTMLDivElement>(null);
  // 流程/请求分栏：流程面板宽度（px）。null → 用 CSS 默认 44%；拖动后固定为像素值
  const [flowPaneWidth, setFlowPaneWidth] = useState<number | null>(null);
  const flowResizingRef = useRef(false);
  const structuredRef = useRef<HTMLDivElement>(null);
  // environment（多套环境）：从工作空间根 application.yml 读取
  const [environments, setEnvironments] = useState<Record<string, Record<string, string>>>({});
  // 工作空间级请求设置（同一份 application.yml 的 settings: 键）：证书校验 / 自定义 CA / 超时
  const [wsSettings, setWsSettings] = useState<WorkspaceSettings>({ ...DEFAULT_WS_SETTINGS });
  const [activeEnv, setActiveEnv] = useState("");
  const [envMenuOpen, setEnvMenuOpen] = useState(false);
  const envMenuRef = useRef<HTMLDivElement>(null);
  // 文件树
  const [childrenMap, setChildrenMap] = useState<Record<string, DirEntry[]>>({});
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  // 文件系统监听：外部增删改的自愈刷新
  // 本应用刚写过的路径 → 时间戳，用于抑制监听回声（避免自身保存触发重载/覆盖）
  const selfWritesRef = useRef<Map<string, number>>(new Map());
  // 监听器每次触发时读取的最新处理闭包（避免一次性订阅捕获过期 state）
  const fsHandlerRef = useRef<(paths: string[]) => void>(() => {});
  // 活动文件被外部修改且存在未保存改动时的提示（不静默覆盖用户编辑）
  const [externalStale, setExternalStale] = useState(false);
  // 文件树/搜索的选中高亮直接以 currentCasePath 为准（当前打开的文件），无需单独状态
  // 搜索栏 / 可视化新建
  const [searchQuery, setSearchQuery] = useState("");
  const [searchResults, setSearchResults] = useState<DirEntry[]>([]);
  const [newCaseDir, setNewCaseDir] = useState<string | null>(null);
  // 文件树选中项：与「当前打开文件」分开——目录也要能被选中（粘贴要落到它里面）
  const [treeSel, setTreeSel] = useState<{ path: string; isDir: boolean } | null>(null);
  // 文件树剪贴板（应用内语义，不走系统剪贴板）：复制后可连续粘贴，粘贴不清空
  const [clip, setClip] = useState<{ path: string; isDir: boolean } | null>(null);
  // 右键菜单 / 输入对话框
  // newOnly：工具栏「+」弹的菜单，只有新建两项（与根行的「⋯」菜单区分开）
  const [ctxMenu, setCtxMenu] = useState<{ x: number; y: number; entry: DirEntry | null; newOnly?: boolean } | null>(null);
  const [promptState, setPromptState] = useState<{ title: string; initial: string; onOk: (v: string) => void } | null>(null);

  // 多标签页：打开顺序 + 非活动标签的状态快照（活动标签用下方 live state）
  const [tabOrder, setTabOrder] = useState<string[]>([]);
  const tabCacheRef = useRef<Record<string, TabSnapshot>>({});
  const [tabMenu, setTabMenu] = useState<{ x: number; y: number; path: string } | null>(null);

  // 当前打开的 case（活动标签）
  const [currentCasePath, setCurrentCasePath] = useState("");
  const [caseName, setCaseName] = useState("");
  const [caseVars, setCaseVars] = useState<Record<string, unknown> | undefined>(undefined);
  const [caseVersion, setCaseVersion] = useState(CASE_VERSION);
  const [dirty, setDirty] = useState(false);

  // 统一 requests 模型（单请求 = 长度 1）
  const [requests, setRequests] = useState<RequestDraft[]>([]);
  const [selectedRequestId, setSelectedRequestId] = useState("");
  const [uiNodes, setUiNodes] = useState<UiNodes | undefined>(undefined);

  // 视图切换：文本互斥；流程 / 请求为结构化分栏
  const [textMode, setTextMode] = useState(false);
  const [showFlow, setShowFlow] = useState(false);
  const [showRequest, setShowRequest] = useState(true);
  const [rawText, setRawText] = useState("");
  const [caseValid, setCaseValid] = useState(false);
  const [textError, setTextError] = useState<string | null>(null);
  const [binaryFile, setBinaryFile] = useState(false); // 二进制/不支持编码：显示占位提示
  const [configVisual, setConfigVisual] = useState(false); // application.yml：可视设置页 vs 文本

  // 批量运行：会话（live 或历史报告）与运行配置对话框。
  // 会话独立于 tabCacheRef——伪路径标签不是编辑态，不进快照体系。
  const [runSessions, setRunSessions] = useState<Record<string, RunSession>>({});
  const [runDialog, setRunDialog] = useState<RunDialogState | null>(null);
  const runDialogRef = useRef<RunDialogState | null>(null);
  runDialogRef.current = runDialog;
  // 「打开并运行」：openCase 是异步的，记下待运行的路径，等 requests 就绪后再发
  const pendingRunRef = useRef<string | null>(null);

  // 运行态：每个请求一份（响应区展示当前选中请求）
  const [runMap, setRunMap] = useState<Record<string, RunState>>({});
  const [outputsCtx, setOutputsCtx] = useState<Record<string, Record<string, unknown>>>({});
  const [runningAll, setRunningAll] = useState(false);
  const [respTab, setRespTab] = useState<"body" | "headers" | "assert">("body");
  // 响应体恒用 prettyBody 美化（非 JSON 自动回退原文），不再提供「美化」开关
  const [error, setError] = useState<string | null>(null);
  // 响应区高度（px，可上下拖动）+ 折叠态（拖到最下收成一行「响应」）
  const [respHeight, setRespHeight] = useState(240);
  const [respCollapsed, setRespCollapsed] = useState(false);
  const respResizingRef = useRef(false);
  const requestPaneRef = useRef<HTMLDivElement>(null);

  // 以下四项均为 app 级偏好，统一持久化到 settings.json（见 settings.ts）；
  // 初值取自首帧缓存，挂载后由下方的加载 effect 以磁盘值校正、由写回 effect 落盘。
  // 全局快捷键 override
  const [scOverrides, setScOverrides] = useState<Overrides>(cachedSettings.shortcuts);
  const onShortcutChange = setScOverrides;
  // 快捷键功能总开关：关闭时全局不分发任何快捷键
  const [scEnabled, setScEnabled] = useState<boolean>(cachedSettings.shortcutsEnabled);
  const onShortcutsEnabledChange = setScEnabled;
  // 文件树显示隐藏项（. 开头）。报告目录 .apicase/ 靠它可见，但这是通用能力——
  // 用户的 .env / .gitignore / .gitlab-ci.yml 本来也该能看到。
  const [showHidden, setShowHidden] = useState<boolean>(cachedSettings.showHiddenFiles);
  // 代理设置：控制发请求是否走系统代理
  const [proxyConfig, setProxyConfig] = useState<ProxyConfig>(cachedSettings.proxy);
  const onProxyChange = setProxyConfig;
  // 主题（浅色 / 深色 / 跟随系统）：写 data-theme（供 CSS 变量覆盖）+ 传 resolvedTheme 给终端等运行时消费者
  const [themeMode, setThemeMode] = useState<ThemeMode>(cachedSettings.theme);
  const [resolvedTheme, setResolvedTheme] = useState<"light" | "dark">(() => resolveTheme(cachedSettings.theme));
  useEffect(() => {
    setResolvedTheme(applyTheme(themeMode));
    if (themeMode !== "system") return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = () => setResolvedTheme(applyTheme("system"));
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, [themeMode]);

  // 挂载时以磁盘值校正首帧缓存，并剔除已删除 / 移动的最近工作空间。
  // 最近工作空间用去重合并而非直接覆盖：读盘完成前用户可能已经打开了工作空间。
  // 过滤结果随下方写回 effect 覆盖 settings.json，失效项从文件中一并清除——
  // 不会只在显示层剔除而磁盘数据无限累积。
  useEffect(() => {
    loadAppSettings().then(async (s) => {
      setThemeMode(s.theme);
      setProxyConfig(s.proxy);
      setScOverrides(s.shortcuts);
      setScEnabled(s.shortcutsEnabled);
      setShowHidden(s.showHiddenFiles);
      const alive = await filterExistingPaths(s.recentWorkspaces);
      setRecentWorkspaces((prev) => Array.from(new Set([...prev, ...alive])).slice(0, 10));
      lastSavedRef.current = JSON.stringify({ ...s, recentWorkspaces: alive });
      settingsLoaded.current = true;
    });
  }, []);

  // 任一偏好变化即整份写回（settings.json 是整份覆盖，分散写会互相抹掉字段，故收敛到这一个出口）。
  // 加载完成前不写，避免用首帧缓存覆盖磁盘上更新的值。
  //
  // 与磁盘内容逐字相同则跳过——这不只是省一次 IO：迁移期两种启动方式的 localStorage 按 origin
  // 分桶（dev 是 http://localhost:1420、打包是 tauri://localhost），dev 侧看不到打包侧的旧键，
  // 若挂载后无条件写一次，dev 空跑就会把默认值固化进 settings.json，反手覆盖掉打包版已有的设置。
  useEffect(() => {
    if (!settingsLoaded.current) return;
    const next: AppSettings = {
      recentWorkspaces,
      theme: themeMode,
      proxy: proxyConfig,
      shortcuts: scOverrides,
      shortcutsEnabled: scEnabled,
      showHiddenFiles: showHidden,
    };
    const serialized = JSON.stringify(next);
    if (serialized === lastSavedRef.current) return;
    lastSavedRef.current = serialized;
    saveAppSettings(next);
  }, [recentWorkspaces, themeMode, proxyConfig, scOverrides, scEnabled, showHidden]);

  const mark = () => setDirty(true);

  // 「打开并运行」的第二步：文件已打开且请求就绪，此时才发得出去
  useEffect(() => {
    const want = pendingRunRef.current;
    if (!want || want !== currentCasePath || !requests.length) return;
    pendingRunRef.current = null;
    if (requests.length > 1) void onRunAll();
    else void onSendRequest(requests[0].id);
  }, [currentCasePath, requests]);

  const selected = requests.find((s) => s.id === selectedRequestId) || requests[0];
  const isFlow = requests.length >= 2 || requests.some((s) => s.outputs.length > 0 || s.dependsOn.length > 0);
  const effectiveText = !!currentCasePath && (textMode || requests.length === 0 || (!showFlow && !showRequest));
  // 仅 .yml/.yaml（非 application.yml）可作为 case：决定是否显示流程/请求视图切换
  const caseEligible = !!currentCasePath && isYamlFile(currentCasePath) && !isAppConfig(currentCasePath);
  const isConfig = !!currentCasePath && isAppConfig(currentCasePath);
  const isMarkdown = !!currentCasePath && !binaryFile && isMarkdownFile(currentCasePath);

  // 点击菜单外部时关闭工作空间下拉
  useEffect(() => {
    if (!wsMenuOpen) return;
    function onDocClick(e: MouseEvent) {
      if (wsMenuRef.current && !wsMenuRef.current.contains(e.target as Node)) {
        setWsMenuOpen(false);
      }
    }
    document.addEventListener("mousedown", onDocClick);
    return () => document.removeEventListener("mousedown", onDocClick);
  }, [wsMenuOpen]);

  // 点击外部关闭环境下拉
  useEffect(() => {
    if (!envMenuOpen) return;
    function onDocClick(e: MouseEvent) {
      if (envMenuRef.current && !envMenuRef.current.contains(e.target as Node)) setEnvMenuOpen(false);
    }
    document.addEventListener("mousedown", onDocClick);
    return () => document.removeEventListener("mousedown", onDocClick);
  }, [envMenuOpen]);

  // 三栏布局显隐持久化（仅本次运行内，见 loadLayout 说明）
  useEffect(() => {
    try {
      sessionStorage.setItem(LAYOUT_KEY, JSON.stringify(layout));
    } catch {
      /* ignore */
    }
  }, [layout]);

  // 左侧栏拖动调宽
  useEffect(() => {
    function onMove(e: MouseEvent) {
      if (!resizingRef.current) return;
      setSidebarWidth(Math.min(480, Math.max(160, e.clientX)));
    }
    function onUp() {
      if (!resizingRef.current) return;
      resizingRef.current = false;
      document.body.classList.remove("resizing-col", "resizing-sidebar");
    }
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    return () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
    };
  }, []);

  // 底部终端栏拖动调高（向上拖增高）
  useEffect(() => {
    function onMove(e: MouseEvent) {
      if (!bottomResizingRef.current) return;
      const box = centerColRef.current?.getBoundingClientRect();
      if (!box) return;
      // 从中间列底边反推高度；上限留 120px 给主区，下限 80px
      const h = box.bottom - e.clientY;
      setBottomHeight(Math.max(80, Math.min(box.height - 120, h)));
    }
    function onUp() {
      if (!bottomResizingRef.current) return;
      bottomResizingRef.current = false;
      document.body.classList.remove("resizing-row");
    }
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    return () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
    };
  }, []);

  // 右侧 AI 栏拖动调宽（向左拖增宽）
  useEffect(() => {
    function onMove(e: MouseEvent) {
      if (!aiResizingRef.current) return;
      const w = window.innerWidth - e.clientX;
      setAiWidth(Math.max(240, Math.min(560, w)));
    }
    function onUp() {
      if (!aiResizingRef.current) return;
      aiResizingRef.current = false;
      document.body.classList.remove("resizing-col", "resizing-ai");
    }
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    return () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
    };
  }, []);

  // 流程/请求分栏的拖动分割条：调整流程面板宽度（请求面板占剩余空间）
  useEffect(() => {
    function onMove(e: MouseEvent) {
      if (!flowResizingRef.current) return;
      const box = structuredRef.current?.getBoundingClientRect();
      if (!box) return;
      // 流程面板不小于 260，且尽量给请求面板留 360（260 下限优先）
      const w = Math.max(260, Math.min(box.width - 360, e.clientX - box.left));
      setFlowPaneWidth(w);
    }
    function onUp() {
      if (!flowResizingRef.current) return;
      flowResizingRef.current = false;
      document.body.classList.remove("resizing-col", "resizing-pane");
    }
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    return () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
    };
  }, []);

  // 响应区拖动调高：拖动「响应」标题栏改变响应区高度；拖到最下（阈值以下）折叠为一行
  useEffect(() => {
    function onMove(e: MouseEvent) {
      if (!respResizingRef.current) return;
      const box = requestPaneRef.current?.getBoundingClientRect();
      if (!box) return;
      const h = box.bottom - e.clientY; // 由请求面板底边反推响应区目标高度
      if (h < 60) {
        setRespCollapsed(true); // 拖到最下：收起为一行标题
      } else {
        setRespCollapsed(false);
        setRespHeight(Math.max(120, Math.min(box.height - 150, h))); // 上限给请求编辑器留 150
      }
    }
    function onUp() {
      if (!respResizingRef.current) return;
      respResizingRef.current = false;
      document.body.classList.remove("resizing-row");
    }
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    return () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
    };
  }, []);

  // 全屏切换（用窗口尺寸变化方向判断，避免退出时 isFullscreen() 滞后）
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let lastW = 0;
    let lastH = 0;
    try {
      const appWindow = getCurrentWindow();
      appWindow.isFullscreen().then(setIsFullscreen).catch(() => {});
      appWindow
        .innerSize()
        .then((s) => {
          lastW = s.width;
          lastH = s.height;
        })
        .catch(() => {});
      appWindow
        .onResized(async ({ payload }) => {
          const { width, height } = payload;
          const shrinking = width < lastW || height < lastH;
          lastW = width;
          lastH = height;
          if (shrinking) {
            setIsFullscreen(false);
            return;
          }
          try {
            setIsFullscreen(await appWindow.isFullscreen());
          } catch {
            // 忽略
          }
        })
        .then((u) => {
          unlisten = u;
        })
        .catch(() => {});
    } catch {
      // 非 Tauri 环境忽略
    }
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  // 挂载一次：订阅后端文件系统变更事件，交给最新的处理闭包（fsHandlerRef）
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<string[]>("workspace:fs-change", (e) => {
      fsHandlerRef.current(e.payload || []);
    })
      .then((u) => {
        unlisten = u;
      })
      .catch(() => {});
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  // 全局快捷键：单一 document 监听；用 ref 取最新绑定 / 动作闭包
  const saveRef = useRef<() => void>(() => {});
  const scLookupRef = useRef<Record<string, ActionId>>({});
  const scActionsRef = useRef<Partial<Record<ActionId, () => void>>>({});
  const searchInputRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      const accel = eventToAccel(e);
      if (!accel) return;
      // 纯键（无 Mod/Alt）在输入类元素中不拦截，避免干扰打字
      if (!accel.mod && !accel.alt) {
        const t = e.target as HTMLElement | null;
        const tag = t?.tagName;
        if (tag === "INPUT" || tag === "TEXTAREA" || t?.isContentEditable) return;
      }
      const id = scLookupRef.current[accelKey(accel)];
      if (!id) return;
      e.preventDefault();
      scActionsRef.current[id]?.();
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  // 搜索：有词时后端递归扫描（debounce 200ms）；清空即恢复文件树
  useEffect(() => {
    if (!workspace) return;
    const q = searchQuery.trim();
    if (q === "") {
      setSearchResults([]);
      return;
    }
    const t = setTimeout(() => {
      invoke<DirEntry[]>("search_workspace", { root: workspace, query: q })
        .then(setSearchResults)
        .catch(() => {});
    }, 200);
    return () => clearTimeout(t);
  }, [searchQuery, workspace]);

  // 读取某目录的直接子项并缓存
  async function loadDir(path: string) {
    try {
      const entries = await invoke<DirEntry[]>("list_dir", { path, showHidden });
      setChildrenMap((prev) => ({ ...prev, [path]: entries }));
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  // 切换「显示隐藏文件」后重拉已加载的目录（保持懒加载：只刷新展开过的）
  const showHiddenRef = useRef(showHidden);
  useEffect(() => {
    if (showHiddenRef.current === showHidden) return;
    showHiddenRef.current = showHidden;
    const loaded = Object.keys(childrenMap);
    loaded.forEach((d) => void loadDir(d));
  }, [showHidden]);

  // 记录本应用自身发起的写操作路径，令监听回声可被识别并抑制
  function noteSelfWrite(...paths: string[]) {
    const now = Date.now();
    paths.forEach((p) => selfWritesRef.current.set(p, now));
  }

  // 切标签 / 从搜索结果打开时，树选中项跟随当前文件（视觉上与「选中即当前打开文件」一致）
  useEffect(() => {
    if (currentCasePath) setTreeSel({ path: currentCasePath, isDir: false });
  }, [currentCasePath]);

  function toggleDir(entry: DirEntry) {
    setTreeSel({ path: entry.path, isDir: true }); // 点目录既展开/折叠，也成为选中项（粘贴要落到它里面）
    const isOpen = expanded.has(entry.path);
    setExpanded((prev) => {
      const next = new Set(prev);
      if (isOpen) next.delete(entry.path);
      else next.add(entry.path);
      return next;
    });
    if (!isOpen && !childrenMap[entry.path]) {
      loadDir(entry.path);
    }
  }

  // 在文件树中「显露」某个文件：展开它与工作空间根之间的各级父目录，并懒加载其子项，
  // 使折叠目录里的文件（如从 Tab 切过去）能正确展开并高亮可见。
  function revealInTree(path: string) {
    if (!workspace || !path.startsWith(workspace)) return;
    const ancestors: string[] = [];
    let d = dirName(path);
    // 收集根与文件之间的所有中间目录（不含根，根在树中始终显示）
    while (d.length > workspace.length && d.startsWith(workspace)) {
      ancestors.push(d);
      const parent = dirName(d);
      if (parent === d) break; // 防御：路径已到顶，dirName 不再变化
      d = parent;
    }
    if (ancestors.length === 0) return; // 文件就在根目录下，无需展开
    setExpanded((prev) => {
      const next = new Set(prev);
      ancestors.forEach((a) => next.add(a));
      return next;
    });
    // 未加载 children 的目录先加载，否则展开后子树为空、文件仍不可见
    ancestors.forEach((a) => {
      if (!childrenMap[a]) loadDir(a);
    });
  }

  // 活动文件变化（点 Tab、关标签切邻居、新建等）时，自动在文件树中展开显露它
  useEffect(() => {
    revealInTree(currentCasePath);
    setExternalStale(false); // 切换活动文件即清除上一个文件的外部改动提示
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentCasePath, workspace]);

  function applyWorkspace(path: string) {
    setWorkspace(path);
    setRecentWorkspaces((prev) => [path, ...prev.filter((p) => p !== path)].slice(0, 10));
    setChildrenMap({});
    setExpanded(new Set());
    closeAllTabsAndReset();
    loadDir(path);
    loadEnvironments(path);
    // 打开 / 切换工作空间即显示左侧文件树（顶栏开关随之点亮）
    setLayout((l) => ({ ...l, left: true }));
    // 启动/切换文件系统监听：外部对该工作空间的增删改将实时回传
    invoke("watch_workspace", { path }).catch(() => {});
  }

  // 读取工作空间 application.yml：environment（挑选活动环境）+ settings（请求设置）
  async function loadEnvironments(root: string) {
    try {
      const text = await invoke<string>("read_text_file", { path: joinPath(root, "application.yml") });
      const { environment: envs, settings } = await parseAppConfig(text);
      setEnvironments(envs);
      setWsSettings(settings);
      const names = Object.keys(envs);
      setActiveEnv(names.includes("default") ? "default" : names[0] || "");
    } catch {
      setEnvironments({});
      setWsSettings({ ...DEFAULT_WS_SETTINGS });
      setActiveEnv("");
    }
  }

  async function openOrCreateWorkspace() {
    setWsMenuOpen(false);
    try {
      const selectedDir = await open({ directory: true, multiple: false, title: "打开工作空间" });
      if (typeof selectedDir === "string") {
        await invoke("init_workspace", { path: selectedDir });
        applyWorkspace(selectedDir);
      }
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  async function selectWorkspace(ws: string) {
    setWsMenuOpen(false);
    // TOCTOU：历史项在点击这一刻可能已被外部删除 / 移动。先校验，
    // 失效则从最近列表移除（经写回 effect 落盘清除）并提示，不再切过去、不污染数据。
    if (!(await pathExists(ws))) {
      setRecentWorkspaces((prev) => prev.filter((p) => p !== ws));
      setError(`工作空间已不存在或被移动，已从最近列表移除：${ws}`);
      return;
    }
    applyWorkspace(ws);
  }

  // 从最近列表手动移除一条记录（仅删历史记录，不删磁盘上的工作空间目录）；
  // 经写回 effect 落盘，settings.json 同步清除。
  function removeRecentWorkspace(ws: string) {
    setRecentWorkspaces((prev) => prev.filter((p) => p !== ws));
  }

  // ── case / 标签页 打开 / 关闭 ─────────────────────
  // 重置活动编辑态（不动标签列表）
  function resetCaseState() {
    setCurrentCasePath("");
    setCaseName("");
    setCaseVars(undefined);
    setRequests([]);
    setSelectedRequestId("");
    setUiNodes(undefined);
    setRawText("");
    setCaseValid(false);
    setTextError(null);
    setBinaryFile(false);
    setConfigVisual(false);
    setTextMode(false);
    setRunMap({});
    setOutputsCtx({});
    setDirty(false);
  }

  function closeAllTabsAndReset() {
    tabCacheRef.current = {};
    tabOrder.forEach(disposeRunTab); // 运行中的一并取消
    setTabOrder([]);
    resetCaseState();
  }

  // 把当前活动标签的完整状态打成快照
  function snapshotCurrent(): TabSnapshot | null {
    if (!currentCasePath) return null;
    return {
      path: currentCasePath,
      caseName,
      caseVars,
      caseVersion,
      dirty,
      requests,
      selectedRequestId,
      uiNodes,
      textMode,
      showFlow,
      showRequest,
      rawText,
      caseValid,
      textError,
      binaryFile,
      configVisual,
      runMap,
      outputsCtx,
      respTab,
      error,
    };
  }

  function restoreSnapshot(s: TabSnapshot) {
    setCurrentCasePath(s.path);
    setCaseName(s.caseName);
    setCaseVars(s.caseVars);
    setCaseVersion(s.caseVersion);
    setDirty(s.dirty);
    setRequests(s.requests);
    setSelectedRequestId(s.selectedRequestId);
    setUiNodes(s.uiNodes);
    setTextMode(s.textMode);
    setShowFlow(s.showFlow);
    setShowRequest(s.showRequest);
    setRawText(s.rawText);
    setCaseValid(s.caseValid);
    setTextError(s.textError);
    setBinaryFile(s.binaryFile);
    setConfigVisual(s.configVisual);
    setRunMap(s.runMap);
    setOutputsCtx(s.outputsCtx);
    setRespTab(s.respTab);
    setError(s.error);
  }

  // 运行报告标签恒非 dirty（它不是编辑态），关闭时不该弹「未保存」确认
  const isDirtyPath = (p: string): boolean =>
    isRunTab(p) ? false : p === currentCasePath ? dirty : tabCacheRef.current[p]?.dirty ?? false;

  /**
   * 让某个标签成为活动标签（**不做快照**，调用方负责）。
   * 运行报告是伪路径：清空编辑态即可，不读盘、不进 tabCacheRef。
   */
  function activateTab(path: string) {
    if (isRunTab(path)) {
      resetCaseState();
      setCurrentCasePath(path);
      setError(null);
      return;
    }
    const s = tabCacheRef.current[path];
    if (s) restoreSnapshot(s);
    else openCase(path); // 读取成功后再入标签，避免二进制读取失败留下空标签
  }

  // 打开一个标签（新开则从磁盘加载，已开则恢复其内存状态）
  function openTab(path: string) {
    if (path === currentCasePath) return;
    const snap = snapshotCurrent();
    if (snap) tabCacheRef.current[snap.path] = snap;
    activateTab(path);
  }

  function closeTab(path: string) {
    if (isDirtyPath(path)) {
      askConfirm({
        title: <><Obj>{baseName(path)}</Obj> 有未保存修改</>,
        message: "修改将丢失",
        confirmLabel: "不保存并关闭",
        danger: true,
        onConfirm: () => doCloseTab(path),
      });
      return;
    }
    doCloseTab(path);
  }

  function doCloseTab(path: string) {
    const wasActive = path === currentCasePath;
    const idx = tabOrder.indexOf(path);
    const rest = tabOrder.filter((p) => p !== path);
    delete tabCacheRef.current[path];
    disposeRunTab(path);
    setTabOrder(rest);
    if (wasActive) {
      if (rest.length === 0) {
        resetCaseState();
      } else {
        activateTab(rest[Math.min(idx, rest.length - 1)]);
      }
    }
  }

  /** 关闭运行报告标签：运行中的一并取消（不能留个跑着的孤儿），并释放会话。 */
  function disposeRunTab(path: string) {
    if (!isRunTab(path)) return;
    const id = runIdOf(path);
    const s = runSessions[id];
    if (s && !s.readOnly) void cancelRunIpc(id);
    setRunSessions((m) => {
      if (!(id in m)) return m;
      const next = { ...m };
      delete next[id];
      return next;
    });
  }

  // 外部删除：静默关闭指向该路径的标签并切到邻居（文件已不存在，无需确认）
  function dropOpenTab(path: string) {
    const idx = tabOrder.indexOf(path);
    if (idx === -1) return;
    const wasActive = path === currentCasePath;
    const rest = tabOrder.filter((p) => p !== path);
    delete tabCacheRef.current[path];
    setTabOrder(rest);
    if (wasActive) {
      setExternalStale(false);
      if (rest.length === 0) {
        resetCaseState();
      } else {
        activateTab(rest[Math.min(idx, rest.length - 1)]);
      }
    }
  }

  function closeOtherTabs(keep: string) {
    const others = tabOrder.filter((p) => p !== keep);
    if (others.some(isDirtyPath)) {
      askConfirm({
        title: "其它标签页有未保存修改",
        message: "修改将丢失",
        confirmLabel: "不保存并关闭",
        danger: true,
        onConfirm: () => doCloseOtherTabs(keep),
      });
      return;
    }
    doCloseOtherTabs(keep);
  }

  function doCloseOtherTabs(keep: string) {
    const others = tabOrder.filter((p) => p !== keep);
    if (currentCasePath !== keep) activateTab(keep);
    others.forEach((p) => {
      delete tabCacheRef.current[p];
      disposeRunTab(p);
    });
    delete tabCacheRef.current[keep];
    setTabOrder([keep]);
  }

  function closeAllTabs() {
    if (tabOrder.some(isDirtyPath)) {
      askConfirm({
        title: "有标签页未保存",
        message: "修改将丢失",
        confirmLabel: "不保存并关闭",
        danger: true,
        onConfirm: closeAllTabsAndReset,
      });
      return;
    }
    closeAllTabsAndReset();
  }

  // 把一个已解析 Case 应用到结构化编辑态（保持已选请求）
  function applyCase(c: Case) {
    const { requests: rd, ui } = caseToRequests(c);
    setRequests(rd);
    setUiNodes(ui);
    setCaseName(c.name || "");
    setCaseVars(c.vars);
    setCaseVersion(c.version || CASE_VERSION);
    setSelectedRequestId((prev) => (rd.some((s) => s.id === prev) ? prev : rd[0].id));
    setCaseValid(true);
  }

  function onSelectFile(path: string) {
    setTreeSel({ path, isDir: false });
    openTab(path); // 任意文件都打开：case 渲染结构、其余落文本、二进制读取失败给提示
  }

  // 打开一个二进制/不支持编码的文件（像 VSCode 一样开标签 + 占位提示，不渲染编辑器）
  function openBinaryTab(path: string) {
    setTabOrder((prev) => (prev.includes(path) ? prev : [...prev, path]));
    setCurrentCasePath(path);
    setBinaryFile(true);
    setConfigVisual(false);
    setCaseName("");
    setCaseVars(undefined);
    setRequests([]);
    setSelectedRequestId("");
    setUiNodes(undefined);
    setRawText("");
    setCaseValid(false);
    setTextError(null);
    setTextMode(false);
    setRunMap({});
    setOutputsCtx({});
    setDirty(false);
    setError(null);
  }

  async function openCase(path: string) {
    // 已知二进制/媒体扩展名：直接占位，连 invoke 都省
    if (isBinaryExt(path)) {
      openBinaryTab(path);
      return;
    }
    try {
      // 后端判定文本/二进制（NUL 嗅探 + UTF-8 校验），不再靠错误串匹配
      const fc = await invoke<{ binary: boolean; text: string | null }>("read_file_smart", { path });
      if (fc.binary || fc.text === null) {
        openBinaryTab(path);
        return;
      }
      const text = fc.text;
      // apicase 自己生成的运行报告 → 报告视图（与刚跑完时看到的完全一致）。
      // **只认自己的报告**：提取不到结构化数据的普通 HTML 一律落纯文本，
      // 把任意 HTML 塞进 iframe 既无产品价值，又等于在应用里跑一份来源不明的页面。
      if (isHtmlFile(path)) {
        const parsed = await parseReport(text);
        if (parsed) {
          openReportTab(path, parsed);
          return;
        }
      }
      setTabOrder((prev) => (prev.includes(path) ? prev : [...prev, path]));
      setBinaryFile(false);
      setCurrentCasePath(path);
      setDirty(false);
      setError(null);
      setRunMap({});
      setOutputsCtx({});
      setRawText(text);
      // application.yml：默认进可视设置页，并按文件内容同步环境与请求设置
      setConfigVisual(isAppConfig(path));
      if (isAppConfig(path)) {
        const cfg = await parseAppConfig(text);
        setEnvironments(cfg.environment);
        setWsSettings(cfg.settings);
      }
      // 仅 .yml/.yaml（非 application.yml）才按 case 解析渲染；其余一律纯文本——
      // 避免把恰好符合格式的 .txt/.json 误渲染成结构化编辑器（保存会用 YAML 覆盖、丢内容）
      const canBeCase = isYamlFile(path) && !isAppConfig(path);
      const res = canBeCase ? await analyzeCase(text) : null;
      if (!res || !res.valid || !res.case) {
        // 非 case 或校验不通过 → 纯文本兜底（非 .yml 文件不挂"不是有效用例"提示）
        setRequests([]);
        setSelectedRequestId("");
        setUiNodes(undefined);
        setCaseValid(false);
        setTextError(res ? res.error || "不是有效的用例" : null);
        setTextMode(true);
        setShowFlow(false);
        setShowRequest(true);
      } else {
        applyCase(res.case);
        setTextError(null);
        setTextMode(false);
        // 内容驱动默认视图：多请求 → 流程+请求；单请求 → 请求
        const list = res.case.requests;
        const multi = list.length >= 2 || list.some((r: Request) => r.outputs.length || r.dependsOn.length);
        setShowFlow(multi);
        setShowRequest(true);
      }
    } catch (e) {
      // 到这里都是真实 IO 错误（找不到/无权限）；二进制判定已在后端完成
      setError(typeof e === "string" ? e : String(e));
    }
  }

  // ── 内部状态 → Case（保存 / 文本 dump 的公共路径）──
  function stateToCase(): { case?: Case; error?: string } {
    const out: Request[] = [];
    for (const rd of requests) {
      const { request, error: err } = draftToRequest(rd.req);
      if (err || !request) return { error: `请求 ${rd.id}：${err || "请求非法"}` };
      out.push({
        id: rd.id,
        protocol: rd.protocol || "http",
        http: request,
        dependsOn: rd.dependsOn,
        outputs: rd.outputs,
        assertions: rd.assertions,
        docs: rd.docs ? rd.docs : undefined,
        ui: uiNodes?.[rd.id], // 坐标跟着 step 走：改 id / 删 step 不会在别处留下孤儿坐标
      });
    }
    if (out.length === 0) return { error: "无请求" };
    const c: Case = {
      version: caseVersion || CASE_VERSION,
      name: caseName || undefined,
      vars: caseVars,
      requests: out,
    };
    return { case: c };
  }

  async function currentDump(): Promise<{ text?: string; error?: string }> {
    const { case: c, error: err } = stateToCase();
    if (err || !c) return { error: err };
    return { text: await dumpCase(c) };
  }

  // ── 视图切换 ────────────────────────────────────
  async function enterText() {
    // 未修改：保留原始文件文本（含注释/格式，忠实展示）；
    // 有结构化改动：从结构态重新 dump 以反映编辑（注释不可避免地丢失）。
    if (dirty) {
      const { text, error: err } = await currentDump();
      if (!err && text !== undefined) setRawText(text);
      else if (err) setError(err);
    }
    setTextMode(true);
  }

  async function commitText(): Promise<Case | null> {
    const res = await analyzeCase(rawText);
    if (!res.valid || !res.case) {
      // 复用页面顶部的错误条，而不是弹模态打断——用户正要回去改这段 YAML
      setError(`YAML 无效，无法切换到结构视图：${res.error || "未知错误"}`);
      return null;
    }
    applyCase(res.case);
    setTextError(null);
    return res.case;
  }

  const onClickText = () => void enterText();

  // 流程/请求切换：关掉当前唯一在显的面板 → 切到文本
  function onClickFlow() {
    if (showFlow && !showRequest) {
      setShowFlow(false);
      void enterText();
      return;
    }
    setShowFlow((v) => !v);
  }

  function onClickRequest() {
    if (showRequest && !showFlow) {
      setShowRequest(false);
      void enterText();
      return;
    }
    setShowRequest((v) => !v);
  }

  // 用例：点「可视」进结构视图（文本先提交回结构）；两面板都关时按内容驱动默认
  async function onClickVisual() {
    if (!effectiveText) return;
    let multi = isFlow;
    if (textMode) {
      const c = await commitText();
      if (!c) return;
      // 用刚解析的 case 判断多请求，避免 setRequests 异步导致 isFlow 滞后
      multi = c.requests.length >= 2 || c.requests.some((s) => s.outputs.length > 0 || s.dependsOn.length > 0);
    }
    setTextMode(false);
    if (!showFlow && !showRequest) {
      // 多请求 → 流程 + 请求；单请求 → 请求
      setShowRequest(true);
      if (multi) setShowFlow(true);
    }
  }

  // application.yml：文本 ↔ 可视设置页
  async function enterConfigVisual() {
    if (configVisual) return;
    // 以文本为准同步到可视（环境与请求设置同处一份文件）
    const cfg = await parseAppConfig(rawText);
    setEnvironments(cfg.environment);
    setWsSettings(cfg.settings);
    setConfigVisual(true);
  }
  async function exitConfigVisual() {
    if (!configVisual) return;
    // 有编辑才回写文本（保留原注释除非改过）
    if (dirty) setRawText(await dumpAppConfig(rawText, environments, wsSettings));
    setConfigVisual(false);
  }
  // 可视设置页编辑环境：更新全局 environments + 保持 activeEnv 有效 + 标脏
  function onEnvChange(next: Environments) {
    setEnvironments(next);
    const names = Object.keys(next);
    if (activeEnv && !names.includes(activeEnv)) setActiveEnv(names.includes("default") ? "default" : names[0] || "");
    mark();
  }
  // 可视设置页编辑请求设置（证书校验 / 自定义 CA / 超时）：同 onEnvChange，改完标脏待保存
  function onWsSettingsChange(next: WorkspaceSettings) {
    setWsSettings(next);
    mark();
  }

  // ── 保存 ────────────────────────────────────────
  async function saveCase() {
    if (!currentCasePath) return;
    noteSelfWrite(currentCasePath); // 抑制本次保存的监听回声
    try {
      if (isAppConfig(currentCasePath) && configVisual) {
        // 可视设置页：把 environments 与请求设置一并序列化进 application.yml
        const content = await dumpAppConfig(rawText, environments, wsSettings);
        await invoke("write_text_file", { path: currentCasePath, content });
        setRawText(content);
        const names = Object.keys(environments);
        if (!names.includes(activeEnv)) setActiveEnv(names.includes("default") ? "default" : names[0] || "");
      } else if (effectiveText) {
        await invoke("write_text_file", { path: currentCasePath, content: rawText });
        // application.yml：保存后重载 environment / 请求设置，使切换与发请求即时生效
        if (isAppConfig(currentCasePath)) {
          const cfg = await parseAppConfig(rawText);
          const envs = cfg.environment;
          setEnvironments(envs);
          setWsSettings(cfg.settings);
          const names = Object.keys(envs);
          if (!names.includes(activeEnv)) setActiveEnv(names.includes("default") ? "default" : names[0] || "");
        }
        // 仅 .yml/.yaml：文本此时有效则回填结构态；非 case 文件（.txt/.json）不解析、保持纯文本
        if (isYamlFile(currentCasePath) && !isAppConfig(currentCasePath)) {
          const res = await analyzeCase(rawText);
          if (res.valid && res.case) {
            applyCase(res.case);
            setTextError(null);
          } else {
            setCaseValid(false);
            setTextError(res.error || null);
          }
        }
      } else {
        const { text, error: err } = await currentDump();
        if (err || text === undefined) {
          setError(err || "序列化失败");
          return;
        }
        await invoke("write_text_file", { path: currentCasePath, content: text });
        setRawText(text);
      }
      setDirty(false);
      setError(null);
      setExternalStale(false);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }
  saveRef.current = () => {
    if (currentCasePath && dirty) saveCase();
  };

  // 快捷键：反查表 + 动作分发闭包（每次 render 取最新 state / handler）
  const scBindings = resolveBindings(scOverrides);
  // 总开关关闭 → 反查表置空，任何键都查不到动作，等于全局停用快捷键
  scLookupRef.current = scEnabled ? buildLookup(scBindings) : {};
  scActionsRef.current = {
    "new-case": () => {
      if (workspace) newCaseIn(workspace);
    },
    "open-workspace": () => openOrCreateWorkspace(),
    save: () => saveRef.current(),
    "close-tab": () => {
      if (currentCasePath) closeTab(currentCasePath);
    },
    search: () => {
      searchInputRef.current?.focus();
      searchInputRef.current?.select();
    },
    "open-settings": () => {
      if (!workspace) return;
      openTab(joinPath(workspace, "application.yml"));
      setConfigVisual(true);
    },
    "send-request": () => {
      if (selected) onSendRequest(selected.id);
    },
  };

  // 文件系统变更处理（监听器每次触发时经 fsHandlerRef 读取此最新闭包）
  fsHandlerRef.current = (paths: string[]) => {
    if (!workspace) return;
    const under = (p: string) => p === workspace || p.startsWith(workspace + "/") || p.startsWith(workspace + "\\");
    const inWs = paths.filter(under);
    if (inWs.length === 0) return;

    const now = Date.now();
    const isEcho = (p: string) => {
      const t = selfWritesRef.current.get(p);
      return t !== undefined && now - t < 2500; // 本应用刚写过：忽略回声
    };

    // 1) 目录树：刷新受影响且「已加载」的目录（懒加载一致——不主动展开新目录）
    const dirs = new Set<string>();
    for (const p of inWs) {
      const parent = dirName(p);
      if (parent === workspace || childrenMap[parent] !== undefined) dirs.add(parent); // 增/删/改名改变父目录列表
      if (childrenMap[p] !== undefined) dirs.add(p); // 受影响路径本身是已展开目录
    }
    dirs.forEach((d) => loadDir(d));

    // 2) application.yml 外部改动 → 重载环境（非活动文件时；活动文件走下方重载）
    const cfg = joinPath(workspace, "application.yml");
    if (inWs.includes(cfg) && !isEcho(cfg) && currentCasePath !== cfg) {
      loadEnvironments(workspace);
    }

    // 3) 已打开标签受影响：核对存在性——删除→关标签；活动文件被改→净态重载 / 脏态提示
    const affected = tabOrder.filter((p) => inWs.includes(p) && !isEcho(p));
    affected.forEach((p) => {
      invoke<boolean>("path_exists", { path: p })
        .then((exists) => {
          if (!exists) {
            dropOpenTab(p);
          } else if (p === currentCasePath && !binaryFile) {
            if (dirty) setExternalStale(true); // 有未保存改动：提示，绝不静默覆盖
            else openCase(p); // 净态：直接加载最新内容
          }
        })
        .catch(() => {});
    });
  };

  // ── 请求编辑 ────────────────────────────────────
  function updateReq(next: ReqDraft) {
    setRequests((prev) => prev.map((s) => (s.id === selectedRequestId ? { ...s, req: next } : s)));
    mark();
  }

  function setOutputs(list: RequestOutput[]) {
    setRequests((prev) => prev.map((s) => (s.id === selectedRequestId ? { ...s, outputs: list } : s)));
    mark();
  }

  function setAssertions(list: Assertion[]) {
    setRequests((prev) => prev.map((s) => (s.id === selectedRequestId ? { ...s, assertions: list } : s)));
    mark();
  }

  function setDocs(text: string) {
    setRequests((prev) => prev.map((s) => (s.id === selectedRequestId ? { ...s, docs: text } : s)));
    mark();
  }

  function setProtocol(p: string) {
    setRequests((prev) => prev.map((s) => (s.id === selectedRequestId ? { ...s, protocol: p } : s)));
    mark();
  }

  function renameRequest(oldId: string, newId: string) {
    if (requests.some((s) => s.id === newId)) {
      setError(`请求 ID ${newId} 已存在`);
      return;
    }
    setRequests((prev) =>
      prev.map((s) => ({
        ...s,
        id: s.id === oldId ? newId : s.id,
        dependsOn: s.dependsOn.map((d) => (d === oldId ? newId : d)),
      })),
    );
    if (selectedRequestId === oldId) setSelectedRequestId(newId);
    setUiNodes((prev) => {
      if (!prev || !prev[oldId]) return prev;
      const next = { ...prev };
      next[newId] = next[oldId];
      delete next[oldId];
      return next;
    });
    setRunMap((prev) => {
      if (!prev[oldId]) return prev;
      const next = { ...prev };
      next[newId] = next[oldId];
      delete next[oldId];
      return next;
    });
    mark();
  }

  function addRequest() {
    const existing = new Set(requests.map((s) => s.id));
    let i = requests.length + 1;
    let id = `req${i}`;
    while (existing.has(id)) {
      i++;
      id = `req${i}`;
    }
    const dependsOn = selectedRequestId ? [selectedRequestId] : [];
    setRequests((prev) => [
      ...prev,
      { id, protocol: "http", dependsOn, outputs: [], assertions: [], docs: "", req: emptyDraft("GET", "") },
    ]);
    setSelectedRequestId(id);
    setShowFlow(true);
    setShowRequest(true);
    mark();
  }

  function deleteRequest(id: string) {
    if (requests.length <= 1) return;
    const next = requests.filter((s) => s.id !== id).map((s) => ({ ...s, dependsOn: s.dependsOn.filter((d) => d !== id) }));
    setRequests(next);
    if (selectedRequestId === id) setSelectedRequestId(next[0].id);
    setRunMap((prev) => {
      const n = { ...prev };
      delete n[id];
      return n;
    });
    // 清掉被删节点的手动坐标
    setUiNodes((prev) => {
      if (!prev || !prev[id]) return prev;
      const nx = { ...prev };
      delete nx[id];
      return nx;
    });
    mark();
  }

  // 拖动节点：把坐标写入 uiNodes（画布视图态，随 case 一并保存）
  function moveNode(id: string, x: number, y: number) {
    setUiNodes((prev) => ({ ...(prev || {}), [id]: { x, y } }));
    mark();
  }

  // 端口连线建依赖：edge from→to 表示「to 依赖 from」；防自连、防重复、防成环
  function addDependency(fromId: string, toId: string) {
    if (fromId === toId) return;
    const byId = new Map(requests.map((r) => [r.id, r]));
    // from 若已（间接）依赖 to，则再让 to 依赖 from 会成环
    const reaches = (start: string, target: string): boolean => {
      const seen = new Set<string>();
      const stack = [start];
      while (stack.length) {
        const cur = stack.pop() as string;
        if (cur === target) return true;
        if (seen.has(cur)) continue;
        seen.add(cur);
        const n = byId.get(cur);
        if (n) stack.push(...n.dependsOn);
      }
      return false;
    };
    if (reaches(fromId, toId)) {
      setError("无法建立该依赖：会形成环");
      return;
    }
    let changed = false;
    setRequests((prev) =>
      prev.map((s) => {
        if (s.id !== toId || s.dependsOn.includes(fromId)) return s;
        changed = true;
        return { ...s, dependsOn: [...s.dependsOn, fromId] };
      }),
    );
    if (changed) mark();
  }

  // 解除依赖：从 toId.dependsOn 移除 fromId
  function removeDependency(fromId: string, toId: string) {
    let changed = false;
    setRequests((prev) =>
      prev.map((s) => {
        if (s.id !== toId || !s.dependsOn.includes(fromId)) return s;
        changed = true;
        return { ...s, dependsOn: s.dependsOn.filter((d) => d !== fromId) };
      }),
    );
    if (changed) mark();
  }

  // 规整：清除全部手动坐标，恢复自动分层布局
  function resetLayout() {
    if (!uiNodes || Object.keys(uiNodes).length === 0) return;
    setUiNodes(undefined);
    mark();
  }

  // ── 运行 ────────────────────────────────────────
  //
  // 前端在这条链路上只做两件事：把配置递给执行内核、把回来的结果画出来。
  // 变量透传、请求组装、认证、断言、脱敏全在 Rust（core/src/runner.rs）。

  /**
   * 客户端级配置 = 代理（应用级偏好）+ 请求设置（工作空间 application.yml）。
   * CA 的相对路径在此还原为绝对路径——存盘用相对是为了随 git 走、换机器仍有效。
   * 只传偏离默认值的项，让后端的缺省语义（校验开启 / 无 CA / 不限超时）自然兜底。
   */
  const clientConfig = useMemo<ClientConfig>(() => {
    const options: { verifySsl?: boolean; caCertPath?: string; timeoutMs?: number } = {};
    if (!wsSettings.verifySsl) options.verifySsl = false;
    if (wsSettings.useCustomCa && wsSettings.caCert.trim() && workspace) {
      options.caCertPath = joinPath(workspace, wsSettings.caCert.trim());
    }
    if (wsSettings.timeoutMs > 0) options.timeoutMs = wsSettings.timeoutMs;
    return { proxy: proxyPayload(proxyConfig), options };
  }, [wsSettings, workspace, proxyConfig]);

  /** 当前活动环境（运行时注入的变量来源）。 */
  const activeEnvInfo = useMemo(
    () => ({ name: activeEnv, vars: environments[activeEnv] || {} }),
    [activeEnv, environments],
  );

  /**
   * 调试运行的执行参数。**不脱敏、不截断**——响应区要看的就是真实内容；
   * 脱敏与截断只在写进报告时才有意义（报告会被转发、归档）。
   */
  const debugOpts = useMemo<RunOpts>(
    () => makeDebugOpts(activeEnvInfo, clientConfig),
    [activeEnvInfo, clientConfig],
  );

  /** 编辑态 → 可执行的 step。请求非法（如 JSON body 写坏了）时返回错误而不是发一个残缺请求。 */
  function stepOf(sd: RequestDraft): { step?: Request; error?: string } {
    const { request, error } = draftToRequest(sd.req);
    if (error || !request) return { error: error || "请求非法" };
    return {
      step: {
        id: sd.id,
        protocol: sd.protocol || "http",
        http: request,
        dependsOn: sd.dependsOn,
        outputs: sd.outputs,
        assertions: sd.assertions,
        docs: sd.docs || undefined,
      },
    };
  }

  /**
   * 单个请求执行 —— 交给执行内核，把结构化的 StepResult 折回响应区要的 RunState。
   * **调试运行与批量运行是同一份执行语义**，只差脱敏与截断两个开关。
   */
  async function runOneStep(sd: RequestDraft, ctx: RunContext): Promise<{ state: RunState; outputs: Record<string, unknown> }> {
    const { step: spec, error: buildErr } = stepOf(sd);
    if (!spec) return { state: { status: "err", error: buildErr }, outputs: {} };
    try {
      const { step, outputs } = await runStep(spec, ctx, debugOpts);
      if (step.status === "error") {
        return { state: { status: "err", error: step.error || "请求失败" }, outputs };
      }
      return {
        state: { status: step.status === "passed" ? "ok" : "err", resp: respViewOf(step), asserts: step.assertions },
        outputs,
      };
    } catch (e) {
      // IPC 本身失败（后端崩了 / 参数不合法）——与"请求失败"分开报，指向完全不同
      return { state: { status: "err", error: typeof e === "string" ? e : String(e) }, outputs: {} };
    }
  }

  async function onSendRequest(reqId: string) {
    const sd = requests.find((s) => s.id === reqId);
    if (!sd) return;
    if (!sd.req.url.trim()) {
      setRunMap((m) => ({ ...m, [reqId]: { status: "err", error: "请先填写 URL" } }));
      return;
    }
    // 变量优先级：case 级 vars 覆盖 environment（case-local 更具体）
    const ctx: RunContext = { vars: { ...(environments[activeEnv] || {}), ...(caseVars || {}) }, steps: outputsCtx };
    setRunMap((m) => ({ ...m, [reqId]: { status: "running" } }));
    const { state, outputs } = await runOneStep(sd, ctx);
    setRunMap((m) => ({ ...m, [reqId]: state }));
    setOutputsCtx((prev) => ({ ...prev, [reqId]: outputs }));
    setRespTab("body");
  }

  async function onRunAll() {
    setRunningAll(true);
    // 本地上下文在 await 间同步透传 outputs（不依赖异步 state）
    const local: RunContext = { vars: { ...(environments[activeEnv] || {}), ...(caseVars || {}) }, steps: {} };
    setOutputsCtx({});
    try {
      // 顺序由执行内核排（成环兜底等边界只在那里有一份实现）；
      // 循环留在前端是为了每跑完一步就刷新界面。
      const specs = requests.map((sd) => stepOf(sd).step).filter((s): s is Request => !!s);
      const order = await topoOrder(specs);
      // 下标是按 specs 排的，而 specs 可能因组装失败少了几个 —— 用 id 回查才对得上
      const byId = new Map(requests.map((r) => [r.id, r]));
      for (const i of order) {
        const sd = byId.get(specs[i].id);
        if (!sd) continue;
        setRunMap((m) => ({ ...m, [sd.id]: { status: "running" } }));
        const { state, outputs } = await runOneStep(sd, local);
        local.steps[sd.id] = outputs;
        setOutputsCtx({ ...local.steps });
        setRunMap((m) => ({ ...m, [sd.id]: state }));
      }
    } finally {
      // 中途抛错也要把「运行中」的旗子放下，否则按钮永远转下去
      setRunningAll(false);
    }
  }

  // ── 批量运行（目录 / 单用例 → 报告）──────────────

  /**
   * 递归发现目录下的用例文件。
   * 过滤规则：仅 `.yml`/`.yaml`、排除 `application.yml`、跳过隐藏项与大目录。
   * **按路径排序**——执行顺序要可预期、可控（用 `01-` / `02-` 前缀即可编排）。
   */
  async function discoverCases(target: string, isDir: boolean, recursive: boolean): Promise<string[]> {
    if (!isDir) return isYamlFile(target) && !isAppConfig(target) ? [target] : [];
    const out: string[] = [];
    const walk = async (dir: string, depth: number) => {
      let entries: DirEntry[] = [];
      try {
        entries = await invoke<DirEntry[]>("list_dir", { path: dir, showHidden: false });
      } catch {
        return; // 单个目录读不动不该中断整轮发现
      }
      for (const e of entries) {
        if (e.isDir) {
          if (recursive) await walk(e.path, depth + 1);
        } else if (isYamlFile(e.path) && !isAppConfig(e.path)) {
          out.push(e.path);
        }
      }
    };
    await walk(target, 0);
    out.sort();
    return out;
  }

  /** 打开运行配置对话框（右键「运行并生成报告…」）。 */
  async function openRunDialog(target: string, isDir: boolean) {
    if (!workspace) return;
    setRunDialog({ target, isDir, recursive: true, env: activeEnv, files: null });
    const files = await discoverCases(target, isDir, true);
    setRunDialog((d) => (d && d.target === target ? { ...d, files } : d));
  }

  /** 对话框里改「递归 / 仅当前目录」后重扫——预览列表必须与实际要跑的一致。 */
  async function setRunRecursive(recursive: boolean) {
    setRunDialog((d) => (d ? { ...d, recursive, files: null } : d));
    const d = runDialogRef.current;
    if (!d) return;
    const files = await discoverCases(d.target, d.isDir, recursive);
    setRunDialog((cur) => (cur && cur.target === d.target ? { ...cur, recursive, files } : cur));
  }

  /** 报告输出目录：`<workspace>/.apicase/reports/<YYYYMMDD-HHmmss>/` */
  function reportDirFor(at: Date): string {
    const p = (n: number) => String(n).padStart(2, "0");
    const stamp =
      `${at.getFullYear()}${p(at.getMonth() + 1)}${p(at.getDate())}` +
      `-${p(at.getHours())}${p(at.getMinutes())}${p(at.getSeconds())}`;
    return joinPath(joinPath(workspace, REPORTS_REL), stamp);
  }

  /**
   * 把 `.apicase/` 写进工作空间 `.gitignore`（报告是产物，不该进版本库）。
   * 已有该行则不动；读不到 `.gitignore` 就新建。失败静默——写不进去不该挡住运行。
   */
  async function ensureGitignore() {
    const gi = joinPath(workspace, ".gitignore");
    try {
      let text = "";
      try {
        text = await invoke<string>("read_text_file", { path: gi });
      } catch {
        text = "";
      }
      if (text.split(/\r?\n/).some((l) => l.trim() === ".apicase/" || l.trim() === ".apicase")) return;
      const next = text && !text.endsWith("\n") ? `${text}\n.apicase/\n` : `${text}.apicase/\n`;
      noteSelfWrite(gi);
      await invoke("write_text_file", { path: gi, content: next });
    } catch {
      /* 写不进 .gitignore 不影响运行本身 */
    }
  }

  /**
   * 从对话框启动一次批量运行：开标签页 → 交给执行内核 → 订阅进度事件。
   *
   * 执行、报告渲染、周期写盘全在 Rust；前端只负责开标签、订阅事件、把 case
   * 逐个追加进本地报告对象。进度按**增量**推送而不是每次整份重发——
   * 一份跑了 200 个用例的报告可达数 MB，每完成一个就整份过一次 IPC 会把界面拖卡。
   */
  async function startRun(d: RunDialogState) {
    const files = d.files || (await discoverCases(d.target, d.isDir, d.recursive));
    setRunDialog(null);
    if (!files.length) {
      setError("没有找到可运行的用例");
      return;
    }
    const at = new Date();
    const runId = String(at.getTime());
    const tabPath = RUN_TAB_PREFIX + runId;
    const dir = reportDirFor(at);
    const file = joinPath(dir, "report.html");

    const targets = files.map((p) => ({ file: relPath(workspace, p), path: p }));
    const opts = makeBatchOpts({ name: d.env, vars: environments[d.env] || {} }, clientConfig);
    // 报告头里的运行参数**从 opts 派生**，不并列写第二遍——
    // 半年后回看一份失败报告，"当时脱没脱敏、截断阈值多少"直接决定结论能不能信，
    // 两处各写一份迟早会对不上。
    const options = {
      targets: [d.isDir ? relPath(workspace, d.target) || "（工作空间根）" : relPath(workspace, d.target)],
      recursive: d.recursive,
      environment: d.env,
      concurrency: opts.concurrency,
      stopOnFailure: opts.stopOnFailure,
      redact: opts.redact,
      maxBodyBytes: opts.maxBodyBytes,
    };

    setRunSessions((m) => ({ ...m, [runId]: { runId, report: null, file, dir, total: files.length } }));
    setTabOrder((prev) => (prev.includes(tabPath) ? prev : [...prev, tabPath]));
    openTab(tabPath);
    await ensureGitignore();
    // 报告目录由后端周期覆写，fs 监听会看到——先打个招呼免得触发误刷新
    noteSelfWrite(file);

    // 增量事件 → 本地报告对象。case 一经产出就不再变，故直接追加即可。
    const unlisten = await listenRun(runId, (e) => {
      setRunSessions((m) => {
        const s = m[runId];
        if (!s) return m;
        if (e.kind === "start") return { ...m, [runId]: { ...s, report: e.report } };
        if (!s.report) return m;
        const report =
          e.kind === "case"
            ? { ...s.report, cases: [...s.report.cases, e.case], summary: e.summary, durationMs: e.durationMs }
            : { ...s.report, status: e.status, finishedAt: e.finishedAt, summary: e.summary, durationMs: e.durationMs };
        return { ...m, [runId]: { ...s, report } };
      });
      if (e.kind === "case") noteSelfWrite(file);
    });

    try {
      const report = await runBatch({
        runId,
        targets,
        meta: { workspace: { name: baseName(workspace), root: workspace }, toolVersion: APP_VERSION, options },
        opts,
        reportFile: file,
      });
      // 以返回的完整报告为准收尾：增量事件可能有漏网（窗口切换时的事件积压）
      setRunSessions((m) => (m[runId] ? { ...m, [runId]: { ...m[runId], report } } : m));
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      unlisten();
    }

    // 报告落在隐藏目录里，而 fs 监听刻意跳过 `.` 开头的路径（否则周期覆写会把文件树刷爆）。
    // 故运行结束后精准刷新一次已加载的相关目录——开着「显示隐藏文件」时新报告能立刻出现。
    if (showHidden) {
      const apicaseDir = joinPath(workspace, ".apicase");
      for (const d of [workspace, apicaseDir, joinPath(workspace, REPORTS_REL)]) {
        if (childrenMap[d]) void loadDir(d);
      }
    }
  }

  /** 取消一次运行（在 case 边界生效；已发出的 HTTP 不中断，避免服务端收到半截请求）。 */
  function cancelRun(runId: string) {
    if (!runSessions[runId]) return;
    void cancelRunIpc(runId);
    setRunSessions((m) => (m[runId] ? { ...m, [runId]: { ...m[runId], cancelling: true } } : m));
  }

  /**
   * 「打开并运行」：打开标签后自动发送。
   * 走 pendingRunRef 而非直接调 onSendRequest——openCase 是异步的，requests 此刻还没就绪。
   */
  function openAndRun(path: string) {
    pendingRunRef.current = path;
    openTab(path);
  }

  /** 打开一份历史报告（点 report.html 时走这里）。 */
  function openReportTab(path: string, report: RunReport) {
    const tabPath = RUN_TAB_PREFIX + reportKey(path);
    setRunSessions((m) => ({
      ...m,
      [reportKey(path)]: {
        runId: reportKey(path),
        report,
        file: path,
        dir: dirName(path),
        total: report.summary.total,
        readOnly: true,
      },
    }));
    setTabOrder((prev) => (prev.includes(tabPath) ? prev : [...prev, tabPath]));
    openTab(tabPath);
  }

  // ── 文件管理（右键菜单触发）─────────────────────
  function newCaseIn(dir: string) {
    setNewCaseDir(dir);
  }

  async function createCaseFile(dir: string, name: string, method: string, url: string) {
    let fname = name.trim() || "新用例";
    if (!/\.(yml|yaml)$/i.test(fname)) fname += ".yml";
    const path = joinPath(dir, fname);
    const split = splitQueryFromUrl(url.trim());
    const c: Case = {
      version: CASE_VERSION,
      requests: [
        {
          id: "step1",
          protocol: "http",
          http: { method, url: split.base, query: split.query, headers: [], auth: { type: "none" }, body: { type: "none" } },
          dependsOn: [],
          outputs: [],
          assertions: [],
        },
      ],
    };
    try {
      noteSelfWrite(path);
      await invoke("create_file", { path, content: await dumpCase(c) });
      await loadDir(dir);
      setExpanded((prev) => new Set(prev).add(dir));
      openTab(path);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  function newFolderIn(dir: string) {
    setPromptState({
      title: "新建文件夹名称",
      initial: "新文件夹",
      onOk: async (name) => {
        if (!name.trim()) return;
        const path = joinPath(dir, name.trim());
        try {
          noteSelfWrite(path);
          await invoke("create_dir", { path });
          await loadDir(dir);
          setExpanded((prev) => new Set(prev).add(dir));
        } catch (e) {
          setError(typeof e === "string" ? e : String(e));
        }
      },
    });
  }

  /**
   * from 改名 / 移动到 to 后，把界面里记着旧路径的地方一并迁移：
   * 标签（含目录之下的所有已打开文件）、当前文件、树选中项、展开态与已加载的子项缓存。
   */
  function retargetPaths(from: string, to: string) {
    const cache = tabCacheRef.current;
    for (const p of Object.keys(cache)) {
      const np = retargetPath(p, from, to);
      if (np !== p) {
        cache[np] = { ...cache[p], path: np };
        delete cache[p];
      }
    }
    setTabOrder((prev) => prev.map((p) => retargetPath(p, from, to)));
    setCurrentCasePath((prev) => retargetPath(prev, from, to));
    setTreeSel((prev) => (prev ? { ...prev, path: retargetPath(prev.path, from, to) } : prev));
    setExpanded((prev) => new Set(Array.from(prev, (p) => retargetPath(p, from, to))));
    setChildrenMap((prev) => {
      const next: Record<string, DirEntry[]> = {};
      for (const [k, v] of Object.entries(prev)) {
        const nk = retargetPath(k, from, to);
        next[nk] = nk === k ? v : v.map((e) => ({ ...e, path: retargetPath(e.path, from, to) }));
      }
      return next;
    });
  }

  function renameEntry(entry: DirEntry) {
    setPromptState({
      title: "重命名",
      initial: entry.name,
      onOk: async (name) => {
        if (!name.trim() || name.trim() === entry.name) return;
        const dir = dirName(entry.path);
        const to = joinPath(dir, name.trim());
        try {
          noteSelfWrite(entry.path, to);
          await invoke("rename_path", { from: entry.path, to });
          await loadDir(dir);
          // 目录改名时其下已打开文件的标签路径也一并跟随（否则会指向失效路径）
          retargetPaths(entry.path, to);
        } catch (e) {
          setError(typeof e === "string" ? e : String(e));
        }
      },
    });
  }

  function deleteEntry(entry: DirEntry) {
    askConfirm({
      title: <>删除 <Obj>{entry.name}</Obj>？</>,
      message: entry.isDir ? "连同其中的全部内容，不可撤销" : "不可撤销",
      confirmLabel: "删除",
      danger: true,
      onConfirm: () => void doDeleteEntry(entry),
    });
  }

  async function doDeleteEntry(entry: DirEntry) {
    const dir = dirName(entry.path);
    try {
      noteSelfWrite(entry.path);
      await invoke("delete_path", { path: entry.path });
      await loadDir(dir);
      // 选中项 / 剪贴板若指向已删除的路径，一并作废
      if (treeSel && isUnder(entry.path, treeSel.path)) setTreeSel(null);
      if (clip && isUnder(entry.path, clip.path)) setClip(null);
      // 关闭被删文件/目录下的所有标签
      const under = (p: string) => isUnder(entry.path, p);
      const affected = tabOrder.filter(under);
      if (affected.length) {
        affected.forEach((p) => delete tabCacheRef.current[p]);
        const rest = tabOrder.filter((p) => !under(p));
        setTabOrder(rest);
        if (under(currentCasePath)) {
          if (rest.length) {
            const s = tabCacheRef.current[rest[0]];
            if (s) restoreSnapshot(s);
            else openCase(rest[0]);
          } else {
            resetCaseState();
          }
        }
      }
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  // ── 文件树：克隆 / 复制 / 剪切 / 粘贴 ─────────────
  /** 目标目录已占用的名称：直接问后端，不用可能过期的 childrenMap。 */
  async function takenNamesIn(dir: string): Promise<Set<string>> {
    const entries = await invoke<DirEntry[]>("list_dir", { path: dir });
    return new Set(entries.map((e) => e.name));
  }

  /** 克隆：同目录复制一份，重名自动排号（用例.yml → 用例 副本.yml → 用例 副本 2.yml）。 */
  async function cloneEntry(entry: DirEntry) {
    const dir = dirName(entry.path);
    try {
      const taken = await takenNamesIn(dir);
      const to = joinPath(dir, uniqueName(entry.name, (n) => taken.has(n)));
      noteSelfWrite(to);
      await invoke("copy_path", { from: entry.path, to });
      await loadDir(dir);
      setExpanded((prev) => new Set(prev).add(dir));
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  /** 粘贴目标目录：选中目录 → 其自身；选中文件 → 其所在目录；没有选中 → 工作空间根。 */
  function pasteTargetDir(): string {
    if (treeSel) return treeSel.isDir ? treeSel.path : dirName(treeSel.path);
    return workspace;
  }

  async function pasteInto(targetDir: string) {
    if (!clip || !targetDir) return;
    const { path: from, isDir } = clip;
    try {
      if (!(await pathExists(from))) {
        setError(`剪贴板中的 ${baseName(from)} 已不存在`);
        setClip(null);
        return;
      }
      // 目录粘进自身或自己的子目录：会无限递归
      if (isDir && isUnder(from, targetDir)) {
        setError("不能把目录粘贴到它自己或它的子目录中");
        return;
      }
      const taken = await takenNamesIn(targetDir);
      const to = joinPath(targetDir, uniqueName(baseName(from), (n) => taken.has(n)));
      noteSelfWrite(to);
      await invoke("copy_path", { from, to });
      await loadDir(targetDir);
      setExpanded((prev) => new Set(prev).add(targetDir));
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  /** 在系统文件管理器中定位该文件 / 目录（打开父目录并选中它）。 */
  async function revealEntry(path: string) {
    try {
      await revealItemInDir(path);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  /** 工具栏「+」的菜单：只管新建，不掺粘贴 / 显示位置那些针对具体项的操作。 */
  function newItems(dir: string): CtxItem[] {
    return [
      { label: "新建用例", onClick: () => newCaseIn(dir) },
      { label: "新建文件夹", onClick: () => newFolderIn(dir) },
    ];
  }

  function ctxItems(entry: DirEntry | null): CtxItem[] {
    const dir = entry ? (entry.isDir ? entry.path : dirName(entry.path)) : workspace;
    const items: CtxItem[] = [];
    // 新建只对目录 / 工作空间根（文件行新建到它所在目录反而容易误解）
    if (!entry || entry.isDir) items.push(...newItems(dir));

    // ── 运行 ──
    // 「打开并运行」= 调试（打开标签 + 自动发送）；「运行并生成报告…」= 回归（出一份可归档的报告）。
    // 两者分成两项而非共用一个「运行」：同一个词做两件事，用户点第二次就会困惑。
    const runTarget = entry ? entry.path : workspace;
    const runIsDir = entry ? entry.isDir : true;
    const canRun = runIsDir || (isYamlFile(runTarget) && !isAppConfig(runTarget));
    if (canRun) {
      if (items.length) items.push({ sep: true });
      if (!runIsDir) items.push({ label: "打开并运行", onClick: () => openAndRun(runTarget) });
      items.push({ label: "运行并生成报告…", onClick: () => openRunDialog(runTarget, runIsDir) });
    }

    if (entry) {
      if (items.length) items.push({ sep: true });
      items.push({ label: "克隆", onClick: () => cloneEntry(entry) });
      items.push({ label: "复制", onClick: () => setClip({ path: entry.path, isDir: entry.isDir }) });
    }
    // 粘贴常驻在能收东西的位置（目录 / 根），剪贴板为空时禁用而非隐藏——固定位置比忽隐忽现好找
    if (!entry || entry.isDir) {
      items.push({
        label: clip ? `粘贴「${baseName(clip.path)}」` : "粘贴",
        onClick: () => pasteInto(dir),
        disabled: !clip,
      });
    }
    if (entry) {
      items.push({ sep: true });
      items.push({ label: "重命名", onClick: () => renameEntry(entry) });
    } else {
      items.push({ sep: true });
    }
    items.push({ label: "显示文件所在位置", onClick: () => revealEntry(entry ? entry.path : workspace) });
    if (entry) {
      items.push({ sep: true });
      items.push({ label: "删除", onClick: () => deleteEntry(entry), danger: true });
    }
    return items;
  }

  /** 选中工作空间根（根行点击 / 右键 / 「⋯」都落到它，粘贴目标即工作空间根）。 */
  function selectRoot() {
    if (workspace) setTreeSel({ path: workspace, isDir: true });
  }

  function openContext(e: React.MouseEvent, entry: DirEntry | null) {
    e.preventDefault();
    e.stopPropagation();
    // 菜单与选中态始终一致（同 VSCode）：entry=null 即针对工作空间根
    if (entry) setTreeSel({ path: entry.path, isDir: entry.isDir });
    else selectRoot();
    setCtxMenu({ x: e.clientX, y: e.clientY, entry });
  }

  /** 行尾「⋯」：与右键同一份菜单，锚定按钮左下角。 */
  function openMoreMenu(e: React.MouseEvent, entry: DirEntry) {
    e.preventDefault();
    e.stopPropagation();
    setTreeSel({ path: entry.path, isDir: entry.isDir });
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    setCtxMenu({ x: r.left, y: r.bottom + 4, entry });
  }

  /** 文件树内的 Ctrl/⌘+C、Ctrl/⌘+V（只在侧栏有焦点时生效，不劫持编辑区的复制粘贴）。 */
  function onTreeKeyDown(e: React.KeyboardEvent) {
    if (!workspace || !(e.metaKey || e.ctrlKey) || e.altKey) return;
    const t = e.target as HTMLElement;
    if (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable) return; // 搜索框等照常
    const k = e.key.toLowerCase();
    if (k === "c") {
      if (!treeSel) return;
      e.preventDefault();
      setClip({ path: treeSel.path, isDir: treeSel.isDir });
    } else if (k === "v") {
      if (!clip) return;
      e.preventDefault();
      pasteInto(pasteTargetDir());
    }
  }

  // 画布节点数据
  const flowNodes: FlowNode[] = requests.map((s) => ({
    id: s.id,
    method: s.req.method,
    dependsOn: s.dependsOn,
    status: runMap[s.id]?.status ?? "idle",
  }));

  const activeRunSession = isRunTab(currentCasePath) ? runSessions[runIdOf(currentCasePath)] : undefined;

  /** 标签页显示名：普通文件用文件名，运行报告用「运行报告 · 起始时刻」（多次运行可并存对比）。 */
  function tabLabel(path: string): string {
    if (!isRunTab(path)) return baseName(path);
    const s = runSessions[runIdOf(path)];
    const iso = s?.report?.startedAt;
    if (!iso) return "运行报告";
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return "运行报告";
    const p = (n: number) => String(n).padStart(2, "0");
    return `运行报告 · ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
  }

  const run = selected ? runMap[selected.id] : undefined;
  const resp = run?.resp || null;
  const runErr = run?.error || null;
  const sending = run?.status === "running";
  // 响应区「断言」栏数据：结果按序对齐已填目标的断言（evalAssertions 过滤空目标），补上期望值
  const assertResults = run?.asserts ?? [];
  const assertPass = assertResults.filter((r) => r.ok).length;
  const assertDefs = (selected?.assertions || []).filter((a) => a.target.trim() !== "");

  // URL 里 ${{变量}} 的高亮判定：当前环境变量 + case 级 vars（case 覆盖 env），值非空即视为「已设值」
  const definedVars: Record<string, unknown> = { ...(environments[activeEnv] || {}), ...(caseVars || {}) };
  const isVarSet = (expr: string): boolean => {
    const name = expr.trim();
    if (!name) return false; // 空占位 {{}}
    if (/^(?:requests|steps)\./.test(name)) return true; // 运行期步骤输出，编辑期不判定
    const key = name.startsWith("vars.") ? name.slice(5) : name;
    const v = definedVars[key];
    return v !== undefined && v !== null && String(v).trim() !== "";
  };

  // Tab 行右侧固定控件：视图切换（文本|可视/流程/请求）+ 保存。始终完整显示，不随 Tab 滚动
  const headControls =
    !currentCasePath || binaryFile ? null : isConfig ? (
      <div className="tab-controls">
        <div className="view-switch">
          <button className={`vs-btn ${!configVisual ? "active" : ""}`} onClick={exitConfigVisual} title="原始 YAML">
            文本
          </button>
          <button className={`vs-btn ${configVisual ? "active" : ""}`} onClick={enterConfigVisual} title="可视化设置">
            可视
          </button>
        </div>
        <button className="save-btn ghost" onClick={saveCase} disabled={!dirty}>
          保存
        </button>
      </div>
    ) : (
      <div className="tab-controls">
        {caseEligible && (
          <div className="view-switch">
            <button className={`vs-btn ${effectiveText ? "active" : ""}`} onClick={onClickText} title="原始 YAML（互斥）">
              文本
            </button>
            {effectiveText ? (
              <button className="vs-btn" onClick={onClickVisual} title="可视化编辑">
                可视
              </button>
            ) : (
              <>
                <button className={`vs-btn ${showFlow ? "active" : ""}`} onClick={onClickFlow} title="DAG 流程画布">
                  流程
                </button>
                <button className={`vs-btn ${showRequest ? "active" : ""}`} onClick={onClickRequest} title="请求编辑器">
                  请求
                </button>
              </>
            )}
          </div>
        )}
        <button className="save-btn ghost" onClick={saveCase} disabled={!dirty}>
          保存
        </button>
      </div>
    );

  return (
    <div className="app">
      <header className={`topbar ${isFullscreen ? "is-fullscreen" : ""}`} data-tauri-drag-region>
        <div className="workspace-menu" ref={wsMenuRef}>
          <button
            className={`workspace-trigger ${workspace ? "" : "is-placeholder"} ${wsMenuOpen ? "is-open" : ""}`}
            onClick={() => setWsMenuOpen((v) => !v)}
          >
            <FolderIcon className="ws-glyph" size={14} />
            <span className="workspace-label" title={workspace || undefined}>
              {workspace ? baseName(workspace) : "选择工作空间"}
            </span>
            <CaretDown open={wsMenuOpen} />
          </button>
          {wsMenuOpen && (
            <div className="workspace-dropdown">
              <button className="ws-item" onClick={openOrCreateWorkspace}>
                <FolderPlusIcon className="ws-item-glyph" size={15} />
                打开工作空间
              </button>
              <div className="ws-divider" />
              <div className="ws-section-title">最近</div>
              <div className="ws-recent-list">
                {recentWorkspaces.length === 0 ? (
                  <div className="ws-empty">暂无最近工作空间</div>
                ) : (
                  recentWorkspaces.map((ws) => (
                    <div key={ws} className="ws-recent-row">
                      <button className="ws-item ws-recent" title={ws} onClick={() => selectWorkspace(ws)}>
                        <FolderIcon className="ws-item-glyph" size={15} />
                        <span className="ws-recent-text">
                          <span className="ws-recent-name">{baseName(ws)}</span>
                          <span className="ws-recent-path">{ws}</span>
                        </span>
                      </button>
                      <button
                        className="ws-recent-del"
                        title="从最近列表移除"
                        onClick={(e) => {
                          e.stopPropagation();
                          removeRecentWorkspace(ws);
                        }}
                      >
                        ×
                      </button>
                    </div>
                  ))
                )}
              </div>
            </div>
          )}
        </div>

        {/* 右侧集群：环境 + 配置 + 三面板切换，整体靠最右 */}
        <div className="topbar-right">
        {workspace && (
          <div className="environment-menu" ref={envMenuRef}>
            <button className={`env-trigger ${envMenuOpen ? "is-open" : ""}`} onClick={() => setEnvMenuOpen((v) => !v)} title="切换环境">
              <SettingsNavIcon name="环境" className="env-glyph" size={14} />
              <span className="env-label">{activeEnv || "无环境"}</span>
              <CaretDown open={envMenuOpen} />
            </button>
            {envMenuOpen && (
              <div className="workspace-dropdown env-dropdown">
                <div className="ws-section-title">环境</div>
                {Object.keys(environments).length === 0 ? (
                  <div className="ws-empty">application.yml 未配置环境</div>
                ) : (
                  Object.keys(environments).map((name) => (
                    <button
                      key={name}
                      className={`ws-item env-item ${name === activeEnv ? "active" : ""}`}
                      onClick={() => {
                        setActiveEnv(name);
                        setEnvMenuOpen(false);
                      }}
                    >
                      <span className="env-check">{name === activeEnv ? "✓" : ""}</span>
                      <span className="env-name">{name}</span>
                      <span className="env-count">{Object.keys(environments[name]).length} 变量</span>
                    </button>
                  ))
                )}
                <div className="ws-divider" />
                <button
                  className="ws-item"
                  onClick={() => {
                    setEnvMenuOpen(false);
                    const p = joinPath(workspace, "application.yml");
                    openTab(p);
                  }}
                >
                  编辑环境（application.yml）
                </button>
              </div>
            )}
          </div>
        )}

        {workspace && (
          <button
            className="topbar-config"
            title="工作空间配置（application.yml）"
            onClick={() => openTab(joinPath(workspace, "application.yml"))}
          >
            <ConfigIcon className="topbar-config-ico" size={18} />
          </button>
        )}

        {/* 右上角：三面板显隐切换（仿 VSCode） */}
        <div className="layout-toggles">
          <button
            className={`layout-toggle ${effectiveShowLeft ? "is-on" : ""}`}
            title="切换左侧边栏（文件树）"
            aria-pressed={effectiveShowLeft}
            onClick={toggleLeft}
          >
            <PanelIcon side="left" />
          </button>
          <button
            className={`layout-toggle ${showBottom ? "is-on" : ""}`}
            title="切换底部栏（终端）"
            aria-pressed={showBottom}
            onClick={toggleBottom}
          >
            <PanelIcon side="bottom" />
          </button>
          <button
            className={`layout-toggle ${showRight ? "is-on" : ""}`}
            title="切换右侧边栏（AI 对话）"
            aria-pressed={showRight}
            onClick={toggleRight}
          >
            <PanelIcon side="right" />
          </button>
        </div>
        </div>
      </header>

      <div className="body-layout">
        {/* 左侧栏：开启左栏即显示；有工作空间显示文件树，否则显示空态引导 */}
        {effectiveShowLeft && (
        <aside
          className="sidebar"
          style={{ width: sidebarWidth }}
          // tabIndex=-1：点击可聚焦（Ctrl/⌘+C·V 只在树内生效），但不打乱 Tab 顺序
          tabIndex={-1}
          onKeyDown={onTreeKeyDown}
          onContextMenu={(e) => {
            if (workspace) openContext(e, null);
          }}
        >
          {workspace ? (
            <>
              <div className="tree-toolbar">
                <div className="tree-search-wrap">
                  <span className="tree-search-icon">⌕</span>
                  <input
                    ref={searchInputRef}
                    className="tree-search"
                    placeholder="搜索用例…"
                    value={searchQuery}
                    onChange={(e) => setSearchQuery(e.target.value)}
                  />
                  {searchQuery && (
                    <button className="tree-search-clear" title="清空" onClick={() => setSearchQuery("")}>
                      ×
                    </button>
                  )}
                </div>
                <button
                  className={`tree-icon-btn ${showHidden ? "is-on" : ""}`}
                  title={showHidden ? "隐藏「.」开头的文件与目录" : "显示「.」开头的文件与目录"}
                  onClick={() => setShowHidden((v) => !v)}
                >
                  {showHidden ? <EyeIcon /> : <EyeOffIcon />}
                </button>
                <button
                  className="tree-add"
                  title="新建"
                  onClick={(e) => {
                    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
                    setCtxMenu({ x: r.left, y: r.bottom + 4, entry: null, newOnly: true });
                  }}
                >
                  +
                </button>
              </div>

              {searchQuery.trim() ? (
                <div className="search-results">
                  {searchResults.length === 0 ? (
                    <div className="search-empty">无匹配</div>
                  ) : (
                    searchResults.map((r) => {
                      const rel = relPath(workspace, dirName(r.path));
                      return (
                        <div
                          key={r.path}
                          className={`search-row ${currentCasePath === r.path ? "selected" : ""} ${r.isDir ? "is-dir" : ""}`}
                          title={r.path}
                          onClick={() => {
                            if (!r.isDir) onSelectFile(r.path);
                          }}
                          onContextMenu={(e) => openContext(e, r)}
                        >
                          {r.isDir ? <FolderIcon /> : <FileTypeIcon path={r.path} />}
                          <span className="search-name">{r.name}</span>
                          {rel && rel !== r.name && <span className="search-path">{rel}</span>}
                        </div>
                      );
                    })
                  )}
                </div>
              ) : (
                <div className="tree">
                  {/* 工作空间根：与普通行同样可点选、可悬浮出「⋯」，菜单走 entry=null 那套（新建 / 粘贴 / 显示位置） */}
                  <div
                    className={`tree-root-name ${treeSel?.path === workspace ? "selected" : ""}`}
                    title={workspace}
                    onClick={() => selectRoot()}
                    onContextMenu={(e) => openContext(e, null)}
                  >
                    <FolderIcon />
                    <span className="tree-name">{baseName(workspace)}</span>
                    <button
                      type="button"
                      className="tree-more"
                      title="更多操作"
                      aria-label="更多操作"
                      onClick={(e) => {
                        e.stopPropagation();
                        const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
                        selectRoot();
                        setCtxMenu({ x: r.left, y: r.bottom + 4, entry: null });
                      }}
                    >
                      <MoreIcon />
                    </button>
                  </div>
                  {(childrenMap[workspace] || []).map((c) => (
                    <TreeNode
                      key={c.path}
                      entry={c}
                      depth={0}
                      expanded={expanded}
                      childrenMap={childrenMap}
                      selectedPath={treeSel?.path || ""}
                      menuPath={ctxMenu?.entry?.path || ""}
                      onToggle={toggleDir}
                      onSelect={onSelectFile}
                      onContext={openContext}
                      onMore={openMoreMenu}
                    />
                  ))}
                </div>
              )}
            </>
          ) : (
            <div className="sidebar-empty">
              <div>未打开工作空间</div>
            </div>
          )}
        </aside>
        )}

        {effectiveShowLeft && (
          <div
            className="sidebar-resizer"
            onMouseDown={(e) => {
              e.preventDefault();
              resizingRef.current = true;
              document.body.classList.add("resizing-col", "resizing-sidebar");
            }}
          />
        )}

        {/* 中间列：主工作区 + 底部终端栏 */}
        <div className="center-col" ref={centerColRef}>
        {/* 主工作区（配置可视设置页铺满，去四周留白）*/}
        <main className={`workspace ${isConfig && configVisual ? "is-flush" : ""}`}>
          {tabOrder.length > 0 && (
            <div className="tab-row">
              <TabBar
                tabs={tabOrder}
                active={currentCasePath}
                isDirty={isDirtyPath}
                labelOf={tabLabel}
                onSelect={openTab}
                onClose={closeTab}
                onContext={(e, p) => {
                  e.preventDefault();
                  e.stopPropagation();
                  setTabMenu({ x: e.clientX, y: e.clientY, path: p });
                }}
              />
              {headControls}
            </div>
          )}
          {!currentCasePath ? (
            <div className="workspace-empty">
              <img className="empty-logo" src="/nautilus.svg" alt="" draggable={false} />
              <div className="empty-shortcuts">
                {(workspace ? ACTIONS : ACTIONS.filter((a) => a.id === "open-workspace")).map((a) => {
                  const accel = scBindings[a.id];
                  if (!accel) return null;
                  return (
                    <div
                      key={a.id}
                      className="empty-sc-row"
                      role="button"
                      onClick={() => scActionsRef.current[a.id]?.()}
                    >
                      <span className="empty-sc-label">{a.label}</span>
                      <span className="empty-sc-keys">
                        {accelTokens(accel).map((t, i) => (
                          <kbd key={i} className="empty-sc-key">
                            {t}
                          </kbd>
                        ))}
                      </span>
                    </div>
                  );
                })}
              </div>
            </div>
          ) : isRunTab(currentCasePath) ? (
            activeRunSession ? (
              <RunReportPane
                key={activeRunSession.runId}
                session={activeRunSession}
                theme={resolvedTheme}
                onCancel={() => cancelRun(activeRunSession.runId)}
                onOpenCase={(f) => {
                  // 报告页来的路径不可信：必须落在当前工作空间内才打开
                  const abs = joinPath(activeRunSession.report?.workspace.root || workspace, f);
                  if (workspace && isUnder(abs, workspace)) openTab(abs);
                }}
                onOpenExternal={() => void openUrl(`file://${activeRunSession.file}`)}
                onReveal={() => void revealItemInDir(activeRunSession.file)}
              />
            ) : (
              <div className="workspace-empty">
                <div className="empty-note">此次运行的会话已结束。</div>
              </div>
            )
          ) : binaryFile ? (
            <div className="binary-view">
              <svg className="binary-ico" viewBox="0 0 24 24" width="44" height="44" aria-hidden="true">
                <path fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinejoin="round" d="M6 3h8l4 4v14H6z" />
                <path fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinejoin="round" d="M14 3v4h4" />
                <path fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round" d="M9.5 12.5 7.5 15l2 2.5M14.5 12.5l2 2.5-2 2.5" />
              </svg>
              <div className="binary-msg">此文件是二进制文件或使用了不受支持的文本编码，所以无法在文本编辑器中显示。</div>
            </div>
          ) : isMarkdown ? (
            <>
              {error && <div className="error-box">⚠ {error}</div>}
              {externalStale && (
                <div className="stale-box">
                  <span>⚠ 此文件已在外部被修改，而你有未保存的改动。</span>
                  <span className="stale-actions">
                    <button className="stale-btn reload" onClick={() => { setExternalStale(false); openCase(currentCasePath); }}>
                      重新加载
                    </button>
                    <button className="stale-btn" onClick={() => setExternalStale(false)}>
                      忽略
                    </button>
                  </span>
                </div>
              )}
              <div className="md-file-view">
                <MarkdownEditor
                  value={rawText}
                  onChange={(v) => {
                    setRawText(v);
                    mark();
                  }}
                />
              </div>
            </>
          ) : isConfig ? (
            <>
              {error && <div className="error-box">⚠ {error}</div>}
              {externalStale && (
                <div className="stale-box">
                  <span>⚠ 此文件已在外部被修改，而你有未保存的改动。</span>
                  <span className="stale-actions">
                    <button className="stale-btn reload" onClick={() => { setExternalStale(false); openCase(currentCasePath); }}>
                      重新加载
                    </button>
                    <button className="stale-btn" onClick={() => setExternalStale(false)}>
                      忽略
                    </button>
                  </span>
                </div>
              )}

              {configVisual ? (
                <SettingsPage
                  environments={environments}
                  onChange={onEnvChange}
                  workspacePath={workspace}
                  configPath={currentCasePath}
                  shortcutOverrides={scOverrides}
                  onShortcutChange={onShortcutChange}
                  shortcutsEnabled={scEnabled}
                  onShortcutsEnabledChange={onShortcutsEnabledChange}
                  themeMode={themeMode}
                  onThemeChange={setThemeMode}
                  proxyConfig={proxyConfig}
                  onProxyChange={onProxyChange}
                  wsSettings={wsSettings}
                  onWsSettingsChange={onWsSettingsChange}
                />
              ) : (
                <div className="text-view">
                  <textarea
                    className="raw-editor"
                    value={rawText}
                    spellCheck={false}
                    onChange={(e) => {
                      setRawText(e.target.value);
                      mark();
                    }}
                  />
                </div>
              )}
            </>
          ) : (
            <>
              {error && <div className="error-box">⚠ {error}</div>}
              {externalStale && (
                <div className="stale-box">
                  <span>⚠ 此文件已在外部被修改，而你有未保存的改动。</span>
                  <span className="stale-actions">
                    <button className="stale-btn reload" onClick={() => { setExternalStale(false); openCase(currentCasePath); }}>
                      重新加载
                    </button>
                    <button className="stale-btn" onClick={() => setExternalStale(false)}>
                      忽略
                    </button>
                  </span>
                </div>
              )}

              {effectiveText ? (
                <div className="text-view">
                  <textarea
                    className="raw-editor"
                    value={rawText}
                    spellCheck={false}
                    onChange={(e) => {
                      setRawText(e.target.value);
                      mark();
                    }}
                  />
                </div>
              ) : (
                <div ref={structuredRef} className={`structured ${showFlow && showRequest ? "split" : showFlow ? "only-flow" : "only-request"}`}>
                  {showFlow && (
                    <div
                      className="flow-pane"
                      style={
                        showFlow && showRequest && flowPaneWidth != null
                          ? { flex: `0 0 ${flowPaneWidth}px`, maxWidth: "none", minWidth: 0 }
                          : undefined
                      }
                    >
                      <FlowCanvas
                        nodes={flowNodes}
                        selectedId={selectedRequestId}
                        ui={uiNodes}
                        onSelect={setSelectedRequestId}
                        onAddStep={addRequest}
                        onDeleteStep={deleteRequest}
                        onRunAll={onRunAll}
                        running={runningAll}
                        onMoveNode={moveNode}
                        onConnect={addDependency}
                        onDisconnect={removeDependency}
                        onResetLayout={resetLayout}
                      />
                    </div>
                  )}
                  {showFlow && showRequest && selected && (
                    <div
                      className="pane-resizer"
                      onMouseDown={(e) => {
                        e.preventDefault();
                        flowResizingRef.current = true;
                        document.body.classList.add("resizing-col", "resizing-pane");
                      }}
                      onDoubleClick={() => setFlowPaneWidth(null)}
                      title="拖动调整宽度（双击恢复默认）"
                    />
                  )}
                  {showRequest && selected && (
                    <div className="request-pane" ref={requestPaneRef}>
                      <div className="req-scroll">
                        <RequestEditor
                          key={currentCasePath + "/" + selectedRequestId}
                          value={selected.req}
                          onChange={updateReq}
                          onSend={() => onSendRequest(selected.id)}
                          sending={sending}
                          sendLabel="发送"
                          assertions={selected.assertions}
                          onAssertions={setAssertions}
                          outputs={isFlow ? selected.outputs : undefined}
                          onOutputs={isFlow ? setOutputs : undefined}
                          docs={selected.docs}
                          onDocs={setDocs}
                          stepId={selected.id}
                          onRenameId={(v) => renameRequest(selected.id, v)}
                          protocol={selected.protocol}
                          onProtocol={setProtocol}
                          isVarSet={isVarSet}
                          resp={run?.resp ?? undefined}
                        />
                      </div>

                      {/* 响应区顶边拖动条：细命中区（6px）+ 悬停/拖动显示蓝线，参照 sidebar-resizer；折叠态隐藏 */}
                      {!respCollapsed && (
                        <div
                          className="panel-resizer-h"
                          title="拖动调整响应区高度"
                          onMouseDown={(e) => {
                            e.preventDefault();
                            respResizingRef.current = true;
                            document.body.classList.add("resizing-row");
                          }}
                        />
                      )}

                      {/* 响应区：与请求编辑器上下分栏；顶边拖动条调高，拖到最下折叠为一行 */}
                      <div
                        className={`response ${respCollapsed ? "is-collapsed" : ""}`}
                        style={respCollapsed ? undefined : { height: respHeight }}
                      >
                        <div
                          className="response-head"
                          onClick={respCollapsed ? () => setRespCollapsed(false) : undefined}
                          title={respCollapsed ? "点击展开响应" : undefined}
                        >
                          <button
                            className="response-toggle"
                            title={respCollapsed ? "展开" : "折叠"}
                            onMouseDown={(e) => e.stopPropagation()}
                            onClick={(e) => {
                              e.stopPropagation();
                              setRespCollapsed((v) => !v);
                            }}
                          >
                            <Chevron open={!respCollapsed} />
                          </button>
                          {resp && (
                            <div className="resp-tabs">
                              <button
                                className={`tab ${respTab === "body" ? "active" : ""}`}
                                onMouseDown={(e) => e.stopPropagation()}
                                onClick={(e) => {
                                  e.stopPropagation();
                                  setRespTab("body");
                                  setRespCollapsed(false);
                                }}
                              >
                                响应体
                              </button>
                              <button
                                className={`tab ${respTab === "headers" ? "active" : ""}`}
                                onMouseDown={(e) => e.stopPropagation()}
                                onClick={(e) => {
                                  e.stopPropagation();
                                  setRespTab("headers");
                                  setRespCollapsed(false);
                                }}
                              >
                                响应头 ({resp.headers.length})
                              </button>
                              <button
                                className={`tab ${respTab === "assert" ? "active" : ""}`}
                                onMouseDown={(e) => e.stopPropagation()}
                                onClick={(e) => {
                                  e.stopPropagation();
                                  setRespTab("assert");
                                  setRespCollapsed(false);
                                }}
                              >
                                断言
                                {assertResults.length > 0 && (
                                  <span className={`tab-count ${assertPass === assertResults.length ? "all-ok" : "has-bad"}`}>
                                    {assertPass}/{assertResults.length}
                                  </span>
                                )}
                              </button>
                            </div>
                          )}
                          {resp && (
                            <span className="response-meta response-head-meta">
                              <span className={`status-badge ${statusClass(resp.status)}`}>
                                {resp.status} {resp.statusText}
                              </span>
                              <span className="meta-item">{resp.elapsedMs} ms</span>
                              <span className="meta-item">{byteSize(resp.body)}</span>
                            </span>
                          )}
                        </div>

                        {!respCollapsed && (
                          <div className="response-content">
                            {runErr && <div className="error-box">⚠ {runErr}</div>}

                            {resp && (
                                <div className={`tab-panel${respTab === "body" ? " body" : ""}`}>
                                  {respTab === "body" ? (
                                    <pre className="response-body">{prettyBody(resp.body)}</pre>
                                  ) : respTab === "headers" ? (
                                    <table className="kv-table grid readonly">
                                      <thead>
                                        <tr>
                                          <th>名称</th>
                                          <th>值</th>
                                        </tr>
                                      </thead>
                                      <tbody>
                                        {resp.headers.map((h, i) => (
                                          <tr key={i}>
                                            <td className="hk">{h.key}</td>
                                            <td className="hv">{h.value}</td>
                                          </tr>
                                        ))}
                                      </tbody>
                                    </table>
                                  ) : assertResults.length ? (
                                    <table className="kv-table grid readonly assert-result-table">
                                      <thead>
                                        <tr>
                                          <th className="res-col"></th>
                                          <th>目标</th>
                                          <th>断言</th>
                                          <th>期望值</th>
                                          <th>实际</th>
                                        </tr>
                                      </thead>
                                      <tbody>
                                        {assertResults.map((r, i) => {
                                          const noVal = r.op === "exists" || r.op === "notExists";
                                          return (
                                            <tr key={i}>
                                              <td className="res-col">
                                                <span className={`assert-badge ${r.ok ? "ok" : "bad"}`}>{r.ok ? "✓" : "✗"}</span>
                                              </td>
                                              <td className="ak">{r.target}</td>
                                              <td className="ao">{OP_LABELS[r.op as AssertOp] ?? r.op}</td>
                                              <td className={`av${noVal ? " na-cell" : ""}`}>{noVal ? "—" : assertDefs[i]?.value || ""}</td>
                                              <td className="aa">{r.actual}</td>
                                            </tr>
                                          );
                                        })}
                                      </tbody>
                                    </table>
                                  ) : (
                                    <div className="response-empty">未设置断言</div>
                                  )}
                                </div>
                            )}

                            {sending && <div className="response-empty">请求发送中…</div>}
                          </div>
                        )}
                      </div>
                    </div>
                  )}
                </div>
              )}
            </>
          )}
        </main>

          {/* 底部终端栏：首次打开后常驻，隐藏用 display:none 保留 shell 会话与滚动 */}
          {termEverOpened.current && (
            <>
              <div
                className="panel-resizer-h"
                style={{ display: showBottom ? "block" : "none" }}
                onMouseDown={(e) => {
                  e.preventDefault();
                  bottomResizingRef.current = true;
                  document.body.classList.add("resizing-row");
                }}
              />
              <div className="bottom-panel" style={{ height: bottomHeight, display: showBottom ? "flex" : "none" }}>
                <div className="panel-head">
                  <span className="panel-head-title">
                    <span className="panel-head-glyph">›_</span> 终端
                  </span>
                  <span className="panel-head-actions">
                    <button className="panel-add" title="新建终端" onClick={addTerminal}>
                      +
                    </button>
                    <button className="panel-close" title="隐藏终端栏" onClick={() => setLayout((l) => ({ ...l, bottom: false }))}>
                      ×
                    </button>
                  </span>
                </div>
                <div className="bottom-panel-body">
                  <div className="term-stack">
                    {terminals.map((t) => {
                      const on = showBottom && activeTermId === t.id;
                      return (
                        <div key={t.id} className="term-pane-wrap" style={{ display: on ? "flex" : "none" }}>
                          <TerminalPane cwd={t.cwd} active={on} theme={resolvedTheme} />
                        </div>
                      );
                    })}
                  </div>
                  {terminals.length > 0 && (
                    <div className="term-tabs">
                      {terminals.map((t) => (
                        <div
                          key={t.id}
                          className={`term-tab ${activeTermId === t.id ? "active" : ""}`}
                          title={`终端 ${t.n}`}
                          onClick={() => setActiveTermId(t.id)}
                        >
                          <span className="term-tab-glyph">›_</span>
                          <span className="term-tab-label">终端 {t.n}</span>
                          <button
                            className="term-tab-close"
                            title="关闭此终端"
                            onClick={(e) => {
                              e.stopPropagation();
                              closeTerminal(t.id);
                            }}
                          >
                            ×
                          </button>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              </div>
            </>
          )}
        </div>

        {/* 右侧 AI 对话栏：可拖宽；首次打开后常驻，隐藏保留对话历史 */}
        {showRight && (
          <div
            className="ai-resizer"
            onMouseDown={(e) => {
              e.preventDefault();
              aiResizingRef.current = true;
              document.body.classList.add("resizing-col", "resizing-ai");
            }}
          />
        )}
        {aiEverOpened.current && (
          <aside className="ai-panel" style={{ width: aiWidth, display: showRight ? "flex" : "none" }}>
            <AiChat />
          </aside>
        )}
      </div>

      {ctxMenu && (
        <ContextMenu
          x={ctxMenu.x}
          y={ctxMenu.y}
          items={ctxMenu.newOnly ? newItems(workspace) : ctxItems(ctxMenu.entry)}
          onClose={() => setCtxMenu(null)}
        />
      )}
      {tabMenu && (
        <ContextMenu
          x={tabMenu.x}
          y={tabMenu.y}
          items={[
            { label: "关闭当前标签页", onClick: () => closeTab(tabMenu.path) },
            { label: "关闭其他标签页", onClick: () => closeOtherTabs(tabMenu.path) },
            { label: "关闭全部标签页", onClick: () => closeAllTabs() },
          ]}
          onClose={() => setTabMenu(null)}
        />
      )}
      {runDialog && (
        <RunDialog
          state={runDialog}
          workspaceRoot={workspace}
          environments={environments}
          onRecursive={(v) => void setRunRecursive(v)}
          onEnv={(v) => setRunDialog((d) => (d ? { ...d, env: v } : d))}
          onRun={() => void startRun(runDialog)}
          onCancel={() => setRunDialog(null)}
        />
      )}
      {promptState && (
        <PromptDialog
          title={promptState.title}
          initial={promptState.initial}
          onCancel={() => setPromptState(null)}
          onOk={(v) => {
            const fn = promptState.onOk;
            setPromptState(null);
            fn(v);
          }}
        />
      )}
      {newCaseDir !== null && (
        <NewCaseDialog
          onCancel={() => setNewCaseDir(null)}
          onOk={(v) => {
            const dir = newCaseDir;
            setNewCaseDir(null);
            createCaseFile(dir, v.name, v.method, v.url);
          }}
        />
      )}
      {confirmNode}
    </div>
  );
}

export default App;
