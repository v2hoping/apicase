import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { applyTheme, loadThemeMode } from "./theme";

// 首帧前应用主题，避免深色下加载闪白
applyTheme(loadThemeMode());

// 禁用 WebView 自带右键菜单（重新载入 / 检查元素 / 查询 / 翻译 / 用谷歌翻译等浏览器行为）。
// 应用自有的右键菜单（文件树、标签页等）靠 onContextMenu + setState 渲染，自身已 preventDefault，
// 因此不受影响；这里在 document 兜底拦截其余区域，避免 WebView 默认菜单弹出。
document.addEventListener("contextmenu", (e) => e.preventDefault());

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
