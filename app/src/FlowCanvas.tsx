// DAG 画布：按 dependsOn 分层自动布局（左→右），SVG 贝塞尔连线 + 节点卡片。
// 只负责「看/连拓扑 + 选择/增删节点 + 叠加运行状态」；节点内容编辑在右侧 RequestEditor。
// 拓扑是真相（dependsOn），坐标是视图：有 uiNodes 覆盖则用之，否则自动布局。
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

function layout(nodes: FlowNode[], ui?: UiNodes): Record<string, { x: number; y: number }> {
  const layers = computeLayers(nodes);
  const rowByLayer = new Map<number, number>();
  const pos: Record<string, { x: number; y: number }> = {};
  for (const n of nodes) {
    const l = layers.get(n.id) ?? 0;
    const r = rowByLayer.get(l) ?? 0;
    rowByLayer.set(l, r + 1);
    const override = ui?.[n.id];
    pos[n.id] = override ? { x: override.x, y: override.y } : { x: PAD + l * COL_W, y: PAD + r * ROW_H };
  }
  return pos;
}

function edgePath(s: { x: number; y: number }, t: { x: number; y: number }): string {
  const sx = s.x + NODE_W;
  const sy = s.y + NODE_H / 2;
  const tx = t.x;
  const ty = t.y + NODE_H / 2;
  const dx = Math.max(36, Math.abs(tx - sx) * 0.5);
  return `M ${sx} ${sy} C ${sx + dx} ${sy}, ${tx - dx} ${ty}, ${tx} ${ty}`;
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
}: {
  nodes: FlowNode[];
  selectedId: string;
  ui?: UiNodes;
  onSelect: (id: string) => void;
  onAddStep: () => void;
  onDeleteStep: (id: string) => void;
  onRunAll: () => void;
  running: boolean;
}) {
  const pos = layout(nodes, ui);
  const maxX = Math.max(NODE_W + PAD * 2, ...nodes.map((n) => pos[n.id].x + NODE_W + PAD));
  const maxY = Math.max(NODE_H + PAD * 2, ...nodes.map((n) => pos[n.id].y + NODE_H + PAD));

  const edges: { from: string; to: string }[] = [];
  for (const n of nodes) for (const dep of n.dependsOn) if (pos[dep]) edges.push({ from: dep, to: n.id });

  return (
    <div className="flow-canvas">
      <div className="flow-toolbar">
        <button className="flow-tool-btn primary" onClick={onAddStep}>
          ＋ 添加
        </button>
        <button className="flow-tool-btn" onClick={onRunAll} disabled={running}>
          {running ? "运行中…" : "▶ 运行"}
        </button>
      </div>

      <div className="flow-scroll">
        <div className="flow-stage" style={{ width: maxX, height: maxY }}>
          <svg className="flow-edges" width={maxX} height={maxY}>
            <defs>
              <marker id="arrow" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
                <path d="M 0 0 L 10 5 L 0 10 z" fill="#c2c2c6" />
              </marker>
            </defs>
            {edges.map((e, i) => (
              <path
                key={i}
                className={`flow-edge ${e.to === selectedId || e.from === selectedId ? "hot" : ""}`}
                d={edgePath(pos[e.from], pos[e.to])}
                markerEnd="url(#arrow)"
              />
            ))}
          </svg>

          {nodes.map((n) => {
            const p = pos[n.id];
            return (
              <div
                key={n.id}
                className={`flow-node status-${n.status} ${n.id === selectedId ? "selected" : ""}`}
                style={{ left: p.x, top: p.y, width: NODE_W, height: NODE_H }}
                onClick={() => onSelect(n.id)}
                title={n.id}
              >
                <span className="fn-port in" />
                <span className={`fn-method ${methodClass(n.method)}`}>{n.method}</span>
                <span className="fn-id">{n.id}</span>
                <span className={`fn-status dot-${n.status}`} />
                {nodes.length > 1 && (
                  <button
                    className="fn-del"
                    title="删除请求"
                    onClick={(e) => {
                      e.stopPropagation();
                      onDeleteStep(n.id);
                    }}
                  >
                    ×
                  </button>
                )}
                <span className="fn-port out" />
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
