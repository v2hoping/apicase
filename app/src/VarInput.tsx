// {{var}} 高亮输入框：透明 <input> 叠在一层「高亮背板」之上——
// 背板与输入框共用同一 className（排版完全一致），并随输入横向滚动同步。
// 变量 token 依据是否已设值着色：已设值→蓝色(--accent)，未设值/空 {{}}→警告色。
import { useRef, type InputHTMLAttributes, type ReactNode } from "react";

// 匹配 {{ 变量名 }}（含空占位 {{}}）；变量名内不含花括号
const VAR_RE = /\{\{([^{}]*?)\}\}/g;

/** 将含 {{var}} 的字符串拆成高亮片段。 */
function highlight(value: string, isVarSet: (name: string) => boolean): ReactNode[] {
  const nodes: ReactNode[] = [];
  let last = 0;
  let key = 0;
  let m: RegExpExecArray | null;
  VAR_RE.lastIndex = 0;
  while ((m = VAR_RE.exec(value)) !== null) {
    if (m.index > last) nodes.push(value.slice(last, m.index));
    const ok = isVarSet(m[1].trim());
    nodes.push(
      <span key={key++} className={`var-token ${ok ? "ok" : "warn"}`}>
        {m[0]}
      </span>,
    );
    last = m.index + m[0].length;
  }
  if (last < value.length) nodes.push(value.slice(last));
  return nodes;
}

export function VarInput({
  value,
  onChange,
  isVarSet,
  className = "",
  wrapClassName = "",
  ...rest
}: {
  value: string;
  onChange: (v: string) => void;
  isVarSet: (name: string) => boolean;
  className?: string; // 同时作用于输入框与背板，保证两层排版一致
  wrapClassName?: string; // 作用于外层容器，负责布局（如 flex:1）
} & Omit<InputHTMLAttributes<HTMLInputElement>, "value" | "onChange" | "className">) {
  const inputRef = useRef<HTMLInputElement>(null);
  const backRef = useRef<HTMLDivElement>(null);
  const syncScroll = () => {
    if (inputRef.current && backRef.current) backRef.current.scrollLeft = inputRef.current.scrollLeft;
  };
  return (
    <div className={`var-input ${wrapClassName}`}>
      <div ref={backRef} className={`var-input-back ${className}`} aria-hidden="true">
        {highlight(value, isVarSet)}
      </div>
      <input
        ref={inputRef}
        className={`var-input-field ${className}`}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onScroll={syncScroll}
        {...rest}
      />
    </div>
  );
}
