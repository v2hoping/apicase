// DAG 画布：按 dependsOn 分层自动布局（左→右），SVG 贝塞尔连线 + 节点卡片。
// 交互：平移 / 缩放 / 适应 / 规整；拖拽节点（持久化到 uiNodes）；从 out 端口拖到
// 目标节点建立依赖（防环、防重复）；边悬停删除解除依赖；双击空白快速加节点。
// 拓扑是真相（dependsOn），坐标是视图：有 uiNodes 覆盖则用之，否则自动布局。
import { useEffect, useRef, useState } from "react";
import { UiNodes } from "./case";
import { methodClass } from "./RequestEditor";

export type RunStatus = "idle" | "running" | "ok" | "err";

export interface FlowNode {
  id: string;
  method: string;
  dependsOn: string[];
  status: RunStatus;
}

const NODE_W = 172;
const NODE_H = 58;
const COL_W = 232; // 列间距（含节点宽）
const ROW_H = 96; // 行间距（含节点高）
const PAD = 28;
const MIN_ZOOM = 0.3;
const MAX_ZOOM = 2;
const MM_W = 168; // 小地图尺寸
const MM_H = 116;

type XY = { x: number; y: number };
const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));

// 每个节点的层 = 最长依赖链深度；带环保护（DAG 正常无环）
function computeLayers(nodes: FlowNode[]): Map<string, number> {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const cache = new Map<string, number>();
  const walk = (id: string, stack: Set<string>): number => {
    const cached = cache.get(id);
    if (cached !== undefined) return cached;
    if (stack.has(id)) return 0; // 环：就地截断
    const n = byId.get(id);
    if (!n || n.dependsOn.length === 0) {
      cache.set(id, 0);
      return 0;
    }
    stack.add(id);
    let m = 0;
    for (const dep of n.dependsOn) {
      if (byId.has(dep)) m = Math.max(m, walk(dep, stack) + 1);
    }
    stack.delete(id);
    cache.set(id, m);
    return m;
  };
  const out = new Map<string, number>();
  for (const n of nodes) out.set(n.id, walk(n.id, new Set()));
  return out;
}

function layout(nodes: FlowNode[], ui?: UiNodes): Record<string, XY> {
  const layers = computeLayers(nodes);
  const rowByLayer = new Map<number, number>();
  const pos: Record<string, XY> = {};
  for (const n of nodes) {
    const l = layers.get(n.id) ?? 0;
    const r = rowByLayer.get(l) ?? 0;
    rowByLayer.set(l, r + 1);
    const override = ui?.[n.id];
    pos[n.id] = override ? { x: override.x, y: override.y } : { x: PAD + l * COL_W, y: PAD + r * ROW_H };
  }
  return pos;
}

// 两点间水平朝向的三次贝塞尔
function bezier(sx: number, sy: number, tx: number, ty: number): string {
  const dx = Math.max(36, Math.abs(tx - sx) * 0.5);
  return `M ${sx} ${sy} C ${sx + dx} ${sy}, ${tx - dx} ${ty}, ${tx} ${ty}`;
}
function edgePath(s: XY, t: XY): string {
  return bezier(s.x + NODE_W, s.y + NODE_H / 2, t.x, t.y + NODE_H / 2);
}

export function FlowCanvas({
  nodes,
  selectedId,
  ui,
  onSelect,
  onAddStep,
  onDeleteStep,
  onRunAll,
  running,
  onMoveNode,
  onConnect,
  onDisconnect,
  onResetLayout,
}: {
  nodes: FlowNode[];
  selectedId: string;
  ui?: UiNodes;
  onSelect: (id: string) => void;
  onAddStep: () => void;
  onDeleteStep: (id: string) => void;
  onRunAll: () => void;
  running: boolean;
  onMoveNode: (id: string, x: number, y: number) => void;
  onConnect: (fromId: string, toId: string) => void;
  onDisconnect: (fromId: string, toId: string) => void;
  onResetLayout: () => void;
}) {
  const basePos = layout(nodes, ui);

  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState<XY>({ x: 0, y: 0 });
  const [panning, setPanning] = useState(false);
  const [drag, setDrag] = useState<{ id: string; x: number; y: number } | null>(null);
  const [connect, setConnect] = useState<{ from: string; x: number; y: number; overId?: string } | null>(null);
  // 拖拽对齐参考线（图坐标下的中心线；null 表示当前无对齐）
  const [guides, setGuides] = useState<{ x: number | null; y: number | null }>({ x: null, y: null });

  // 拖拽中的节点用本地坐标覆盖，令连线实时跟随
  const pos: Record<string, XY> = drag ? { ...basePos, [drag.id]: { x: drag.x, y: drag.y } } : basePos;

  // 供 document 级拖拽 / 缩放闭包读取最新值
  const viewportRef = useRef<HTMLDivElement>(null);
  const panRef = useRef(pan);
  panRef.current = pan;
  const zoomRef = useRef(zoom);
  zoomRef.current = zoom;
  const posRef = useRef(basePos);
  posRef.current = basePos;
  const nodesRef = useRef(nodes);
  nodesRef.current = nodes;

  // 屏幕坐标 → 画布（图）坐标
  const toGraph = (clientX: number, clientY: number): XY => {
    const r = viewportRef.current?.getBoundingClientRect();
    const rx = r?.left ?? 0;
    const ry = r?.top ?? 0;
    return { x: (clientX - rx - panRef.current.x) / zoomRef.current, y: (clientY - ry - panRef.current.y) / zoomRef.current };
  };

  // 以某屏幕点为锚缩放（保持该点下的图坐标不动）
  function zoomAt(clientX: number, clientY: number, factor: number) {
    const vp = viewportRef.current;
    if (!vp) return;
    const r = vp.getBoundingClientRect();
    const cx = clientX - r.left;
    const cy = clientY - r.top;
    const z = zoomRef.current;
    const nz = clamp(z * factor, MIN_ZOOM, MAX_ZOOM);
    const f = nz / z;
    const p = panRef.current;
    setPan({ x: cx - (cx - p.x) * f, y: cy - (cy - p.y) * f });
    setZoom(nz);
  }
  const zoomByCenter = (factor: number) => {
    const vp = viewportRef.current;
    if (!vp) return;
    const r = vp.getBoundingClientRect();
    zoomAt(r.left + r.width / 2, r.top + r.height / 2, factor);
  };
  const resetZoom = () => {
    const vp = viewportRef.current;
    if (!vp) return;
    const r = vp.getBoundingClientRect();
    zoomAt(r.left + r.width / 2, r.top + r.height / 2, 1 / zoomRef.current);
  };

  // 适应视图：把全部节点缩放平移到视口内居中
  function fitView() {
    const vp = viewportRef.current;
    const ns = nodesRef.current;
    if (!vp || ns.length === 0) return;
    const p = posRef.current;
    let minX = Infinity,
      minY = Infinity,
      maxX = -Infinity,
      maxY = -Infinity;
    for (const n of ns) {
      const q = p[n.id];
      if (!q) continue;
      minX = Math.min(minX, q.x);
      minY = Math.min(minY, q.y);
      maxX = Math.max(maxX, q.x + NODE_W);
      maxY = Math.max(maxY, q.y + NODE_H);
    }
    const w = Math.max(1, maxX - minX);
    const h = Math.max(1, maxY - minY);
    const vw = vp.clientWidth;
    const vh = vp.clientHeight;
    if (vw <= 0 || vh <= 0) return; // 视口尚未布局，跳过
    const p2 = 48;
    const z = clamp(Math.min((vw - 2 * p2) / w, (vh - 2 * p2) / h), MIN_ZOOM, 1);
    setZoom(z);
    setPan({ x: (vw - w * z) / 2 - minX * z, y: (vh - h * z) / 2 - minY * z });
  }

  // 挂载后自动适应一次
  useEffect(() => {
    fitView();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 滚轮：Ctrl/⌘ 缩放（锚定光标），否则平移；非被动以便 preventDefault
  useEffect(() => {
    const vp = viewportRef.current;
    if (!vp) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      if (e.ctrlKey || e.metaKey) {
        zoomAt(e.clientX, e.clientY, Math.exp(-e.deltaY * 0.0015));
      } else {
        setPan((p) => ({ x: p.x - e.deltaX, y: p.y - e.deltaY }));
      }
    };
    vp.addEventListener("wheel", onWheel, { passive: false });
    return () => vp.removeEventListener("wheel", onWheel);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 与其它节点的列（同 x）/ 行（同 y）对齐吸附：阈值内贴齐并返回中心参考线
  function snapPos(nx: number, ny: number, id: string): { x: number; y: number; gx: number | null; gy: number | null } {
    const SNAP = 6; // 图坐标阈值
    let x = nx,
      y = ny,
      gx: number | null = null,
      gy: number | null = null;
    let dxBest = SNAP + 1,
      dyBest = SNAP + 1;
    for (const n of nodesRef.current) {
      if (n.id === id) continue;
      const p = posRef.current[n.id];
      if (!p) continue;
      const dx = Math.abs(nx - p.x);
      if (dx <= SNAP && dx < dxBest) {
        dxBest = dx;
        x = p.x;
        gx = p.x + NODE_W / 2;
      }
      const dy = Math.abs(ny - p.y);
      if (dy <= SNAP && dy < dyBest) {
        dyBest = dy;
        y = p.y;
        gy = p.y + NODE_H / 2;
      }
    }
    return { x, y, gx, gy };
  }

  // 拖拽节点：移动超过阈值视为拖动（提交 uiNodes），否则视为点击（选中）
  function startNodeDrag(e: React.MouseEvent, id: string) {
    if (e.button !== 0) return;
    e.stopPropagation();
    const g0 = toGraph(e.clientX, e.clientY);
    const base = posRef.current[id];
    let moved = false;
    const raw = (ev: MouseEvent) => {
      const g = toGraph(ev.clientX, ev.clientY);
      // 不设坐标下限：画布可平移，节点可自由拖到任意位置（含负坐标）
      return { x: base.x + (g.x - g0.x), y: base.y + (g.y - g0.y) };
    };
    const onMove = (ev: MouseEvent) => {
      const g = toGraph(ev.clientX, ev.clientY);
      if (!moved && Math.hypot(g.x - g0.x, g.y - g0.y) * zoomRef.current > 3) moved = true;
      if (moved) {
        const q = raw(ev);
        const s = snapPos(q.x, q.y, id);
        setDrag({ id, x: s.x, y: s.y });
        setGuides({ x: s.gx, y: s.gy });
      }
    };
    const onUp = (ev: MouseEvent) => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      if (moved) {
        const q = raw(ev);
        const s = snapPos(q.x, q.y, id);
        onMoveNode(id, Math.round(s.x), Math.round(s.y));
      } else {
        onSelect(id);
      }
      setDrag(null);
      setGuides({ x: null, y: null });
    };
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  }

  // 从 out 端口拉出连线：拖到目标节点松开即建依赖
  function startConnect(e: React.MouseEvent, id: string) {
    if (e.button !== 0) return;
    e.stopPropagation();
    const g = toGraph(e.clientX, e.clientY);
    setConnect({ from: id, x: g.x, y: g.y });
    const onMove = (ev: MouseEvent) => {
      const gg = toGraph(ev.clientX, ev.clientY);
      let over: string | undefined;
      for (const n of nodesRef.current) {
        if (n.id === id) continue;
        const p = posRef.current[n.id];
        if (p && gg.x >= p.x && gg.x <= p.x + NODE_W && gg.y >= p.y && gg.y <= p.y + NODE_H) {
          over = n.id;
          break;
        }
      }
      setConnect({ from: id, x: gg.x, y: gg.y, overId: over });
    };
    const onUp = () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      setConnect((c) => {
        if (c && c.overId && c.overId !== c.from) onConnect(c.from, c.overId);
        return null;
      });
    };
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  }

  // 拖拽空白背景平移画布
  function startPan(e: React.MouseEvent) {
    if (e.button !== 0) return;
    const sx = e.clientX;
    const sy = e.clientY;
    const base = panRef.current;
    setPanning(true);
    const onMove = (ev: MouseEvent) => setPan({ x: base.x + (ev.clientX - sx), y: base.y + (ev.clientY - sy) });
    const onUp = () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      setPanning(false);
    };
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  }

  const maxX = Math.max(NODE_W + PAD * 2, ...nodes.map((n) => pos[n.id].x + NODE_W + PAD));
  const maxY = Math.max(NODE_H + PAD * 2, ...nodes.map((n) => pos[n.id].y + NODE_H + PAD));

  const edges: { from: string; to: string }[] = [];
  for (const n of nodes) for (const dep of n.dependsOn) if (pos[dep]) edges.push({ from: dep, to: n.id });

  // 小地图：按真实包围盒（含负坐标）等比缩放 + 居中留白；视口框用当前 pan/zoom 反推到图坐标
  const xs = nodes.map((n) => pos[n.id].x);
  const ys = nodes.map((n) => pos[n.id].y);
  const bx0 = Math.min(0, ...xs);
  const by0 = Math.min(0, ...ys);
  const bx1 = Math.max(NODE_W, ...xs.map((x) => x + NODE_W));
  const by1 = Math.max(NODE_H, ...ys.map((y) => y + NODE_H));
  const contentW = Math.max(1, bx1 - bx0);
  const contentH = Math.max(1, by1 - by0);
  const mmScale = Math.min(MM_W / contentW, MM_H / contentH);
  const mmOffX = (MM_W - contentW * mmScale) / 2;
  const mmOffY = (MM_H - contentH * mmScale) / 2;
  // 缩略图坐标 = 屏幕内 = mmOffX + (图坐标 - bx0) * scale
  const mmTX = mmOffX - bx0 * mmScale;
  const mmTY = mmOffY - by0 * mmScale;
  const vpW = viewportRef.current?.clientWidth ?? 0;
  const vpH = viewportRef.current?.clientHeight ?? 0;
  const mmView = { x: -pan.x / zoom, y: -pan.y / zoom, w: vpW / zoom, h: vpH / zoom };

  // 点击 / 拖拽小地图：把对应内容点居中到视口
  function startMinimapNav(e: React.MouseEvent) {
    e.stopPropagation();
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const nav = (clientX: number, clientY: number) => {
      const vp = viewportRef.current;
      if (!vp) return;
      const cx = (clientX - r.left - mmTX) / mmScale;
      const cy = (clientY - r.top - mmTY) / mmScale;
      setPan({ x: vp.clientWidth / 2 - cx * zoomRef.current, y: vp.clientHeight / 2 - cy * zoomRef.current });
    };
    nav(e.clientX, e.clientY);
    const onMove = (ev: MouseEvent) => nav(ev.clientX, ev.clientY);
    const onUp = () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
    };
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  }

  return (
    <div className="flow-canvas">
      <div className="flow-toolbar">
        <button className="flow-tool-btn primary" onClick={onAddStep}>
          ＋ 添加
        </button>
        <button className="flow-tool-btn" onClick={onRunAll} disabled={running}>
          {running ? "运行中…" : "▶ 运行"}
        </button>
        <span className="flow-tool-sep" />
        <button className="flow-tool-btn icon" title="缩小" onClick={() => zoomByCenter(1 / 1.2)}>
          －
        </button>
        <button className="flow-tool-btn zoom-label" title="重置为 100%" onClick={resetZoom}>
          {Math.round(zoom * 100)}%
        </button>
        <button className="flow-tool-btn icon" title="放大" onClick={() => zoomByCenter(1.2)}>
          ＋
        </button>
        <button className="flow-tool-btn" title="适应视图（定位全部节点）" onClick={fitView}>
          适应
        </button>
        <button className="flow-tool-btn" title="规整：恢复自动布局（清除手动位置）" onClick={onResetLayout}>
          规整
        </button>
      </div>

      <div
        ref={viewportRef}
        className={`flow-viewport ${panning ? "is-panning" : ""} ${connect ? "is-connecting" : ""}`}
        style={{ backgroundPosition: `${pan.x}px ${pan.y}px`, backgroundSize: `${18 * zoom}px ${18 * zoom}px` }}
        onMouseDown={startPan}
        onDoubleClick={(e) => {
          // 双击空白（非节点/非边）快速加节点
          const t = e.target as Element;
          if (!t.closest(".flow-node") && !t.closest(".flow-edge-group")) onAddStep();
        }}
      >
        <div className="flow-world" style={{ width: maxX, height: maxY, transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom})` }}>
          <svg
            className="flow-edges"
            width={contentW}
            height={contentH}
            viewBox={`${bx0} ${by0} ${contentW} ${contentH}`}
            style={{ left: bx0, top: by0 }}
          >
            <defs>
              <marker id="arrow" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
                <path d="M 0 0 L 10 5 L 0 10 z" style={{ fill: "var(--edge)" }} />
              </marker>
            </defs>
            {edges.map((e) => {
              const s = pos[e.from];
              const t = pos[e.to];
              const d = edgePath(s, t);
              const mx = (s.x + NODE_W + t.x) / 2;
              const my = (s.y + NODE_H / 2 + t.y + NODE_H / 2) / 2;
              const hot = e.to === selectedId || e.from === selectedId;
              return (
                <g key={`${e.from}->${e.to}`} className={`flow-edge-group ${hot ? "hot" : ""}`}>
                  <path className="flow-edge-hit" d={d} />
                  <path className="flow-edge" d={d} markerEnd="url(#arrow)" />
                  <g
                    className="flow-edge-del"
                    transform={`translate(${mx} ${my})`}
                    onMouseDown={(ev) => ev.stopPropagation()}
                    onClick={(ev) => {
                      ev.stopPropagation();
                      onDisconnect(e.from, e.to);
                    }}
                  >
                    <title>删除依赖</title>
                    <circle r="9" />
                    <path d="M -3.2 -3.2 L 3.2 3.2 M 3.2 -3.2 L -3.2 3.2" />
                  </g>
                </g>
              );
            })}
            {connect && (
              <path className="flow-edge ghost" d={bezier(pos[connect.from].x + NODE_W, pos[connect.from].y + NODE_H / 2, connect.x, connect.y)} />
            )}
          </svg>

          {nodes.map((n) => {
            const p = pos[n.id];
            return (
              <div
                key={n.id}
                className={`flow-node status-${n.status} ${n.id === selectedId ? "selected" : ""} ${drag?.id === n.id ? "dragging" : ""} ${
                  connect && connect.overId === n.id ? "drop-target" : ""
                }`}
                style={{ left: p.x, top: p.y, width: NODE_W, height: NODE_H }}
                onMouseDown={(e) => startNodeDrag(e, n.id)}
                title={n.id}
              >
                <span className="fn-port in" title="上游依赖接入" />
                <span className={`fn-method ${methodClass(n.method)}`}>{n.method}</span>
                <span className="fn-id">{n.id}</span>
                <span className={`fn-status dot-${n.status}`} />
                {nodes.length > 1 && (
                  <button
                    className="fn-del"
                    title="删除请求"
                    onMouseDown={(e) => e.stopPropagation()}
                    onClick={(e) => {
                      e.stopPropagation();
                      onDeleteStep(n.id);
                    }}
                  >
                    ×
                  </button>
                )}
                <span className="fn-port out" title="拖拽以连接到下游请求" onMouseDown={(e) => startConnect(e, n.id)} />
              </div>
            );
          })}

          {/* 拖拽对齐参考线（覆盖在节点之上） */}
          <svg
            className="flow-overlay"
            width={contentW}
            height={contentH}
            viewBox={`${bx0} ${by0} ${contentW} ${contentH}`}
            style={{ left: bx0, top: by0 }}
          >
            {guides.x != null && <line className="align-guide" x1={guides.x} y1={by0} x2={guides.x} y2={by1} />}
            {guides.y != null && <line className="align-guide" x1={bx0} y1={guides.y} x2={bx1} y2={guides.y} />}
          </svg>
        </div>

        {nodes.length > 1 && (
          <div className="flow-minimap" title="概览：点击 / 拖拽以导航" onMouseDown={startMinimapNav} onDoubleClick={(e) => e.stopPropagation()}>
            <svg width={MM_W} height={MM_H}>
              <g transform={`translate(${mmTX} ${mmTY}) scale(${mmScale})`}>
                {nodes.map((n) => {
                  const p = pos[n.id];
                  return <rect key={n.id} className={`mm-node ${n.id === selectedId ? "selected" : ""}`} x={p.x} y={p.y} width={NODE_W} height={NODE_H} rx={8} />;
                })}
                <rect className="mm-view" x={mmView.x} y={mmView.y} width={mmView.w} height={mmView.h} />
              </g>
            </svg>
          </div>
        )}
      </div>
    </div>
  );
}
