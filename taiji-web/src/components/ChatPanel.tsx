import { useCallback, useEffect, useRef, useState } from "react";
import { wsClient } from "../lib/wsClient";
import { CHAT_TIMEOUT_MS } from "../lib/wsClient";
import type { WsStatus } from "../lib/wsClient";

interface ChatMessage {
  id: number;
  role: "user" | "ai";
  content: string;
}

const WS_DOT: Record<WsStatus, string> = {
  connecting: "bg-yellow-400",
  open: "bg-green-400",
  closed: "bg-red-400",
};

const WS_LABEL: Record<WsStatus, string> = {
  connecting: "连接中",
  open: "已连接",
  closed: "已断开",
};

export default function ChatPanel({
  onRunTask,
  wsStatus = "open",
  selectedTaskId = null,
}: {
  onRunTask?: (desc: string) => void;
  wsStatus?: WsStatus;
  /** 当前选中的任务节点,随聊天消息作为 contextTaskId 注入后端。 */
  selectedTaskId?: string | null;
}) {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const nextId = useRef(0);
  const listRef = useRef<HTMLDivElement | null>(null);
  const typeTimer = useRef<ReturnType<typeof setInterval> | null>(null);
  const delayTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const typing = useRef<{ id: number; full: string } | null>(null);
  const sessionId = useRef<string>(crypto.randomUUID());

  const finalizeTyping = useCallback(() => {
    if (typeTimer.current) {
      clearInterval(typeTimer.current);
      typeTimer.current = null;
    }
    if (typing.current) {
      const t = typing.current;
      setMessages((prev) =>
        prev.map((m) => (m.id === t.id ? { ...m, content: t.full } : m))
      );
      typing.current = null;
    }
  }, []);

  const typewriter = useCallback(
    (full: string, delayMs = 0) => {
      if (delayTimer.current) clearTimeout(delayTimer.current);
      delayTimer.current = setTimeout(() => {
        delayTimer.current = null;
        finalizeTyping();
        const id = nextId.current++;
        setMessages((prev) => [...prev, { id, role: "ai", content: "" }]);
        typing.current = { id, full };
        let i = 0;
        typeTimer.current = setInterval(() => {
          i = Math.min(full.length, i + 1 + (Math.random() < 0.5 ? 1 : 0));
          setMessages((prev) =>
            prev.map((m) => (m.id === id ? { ...m, content: full.slice(0, i) } : m))
          );
          if (i >= full.length) {
            if (typeTimer.current) clearInterval(typeTimer.current);
            typeTimer.current = null;
            typing.current = null;
          }
        }, 12);
      }, delayMs);
    },
    [finalizeTyping]
  );

  const handleSend = useCallback(async () => {
    const text = input.trim();
    if (!text || sending) return;
    setInput("");
    setMessages((prev) => [...prev, { id: nextId.current++, role: "user", content: text }]);

    if (text.startsWith("/run")) {
      const desc = text.slice(4).trim();
      const run = async () => {
        if (onRunTask) {
          onRunTask(desc);
          return;
        }
        await wsClient.send("ExecuteTask", { description: desc });
      };
      run()
        .then(() => typewriter(`⚡ 已创建任务:${desc}，正在启动 Zhouyi 递归循环…`, 400))
        .catch((e) => typewriter(`任务创建失败:${String(e)}`, 400));
      return;
    }

    if (text.startsWith("/plan")) {
      typewriter("规划模式即将上线,当前可直接发送 /run 描述 执行任务", 400);
      return;
    }

    if (text.startsWith("/clear")) {
      sessionId.current = crypto.randomUUID();
      setMessages([]);
      return;
    }

    setSending(true);
    const msgId = nextId.current++;
    setMessages((prev) => [...prev, { id: msgId, role: "ai", content: "" }]);
    let acc = "";
    try {
      const final = await wsClient.sendStreaming(
        "ChatMessage",
        {
          message: text,
          sessionId: sessionId.current,
          contextTaskId: selectedTaskId,
        },
        (_delta, accumulated) => {
          acc = accumulated;
          setMessages((prev) =>
            prev.map((m) => (m.id === msgId ? { ...m, content: accumulated } : m))
          );
        },
        CHAT_TIMEOUT_MS
      );
      // 兜底:若服务端未推任何 chunk,用最终文本补齐
      setMessages((prev) =>
        prev.map((m) => (m.id === msgId ? { ...m, content: acc || final } : m))
      );
    } catch (e) {
      setMessages((prev) =>
        prev.map((m) =>
          m.id === msgId ? { ...m, content: `智灵调用失败:${String(e)}` } : m
        )
      );
    } finally {
      setSending(false);
    }
  }, [input, sending, onRunTask, selectedTaskId]);

  useEffect(() => {
    const el = listRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages]);

  useEffect(
    () => () => {
      if (typeTimer.current) clearInterval(typeTimer.current);
      if (delayTimer.current) clearTimeout(delayTimer.current);
    },
    []
  );

  return (
    <aside className="flex h-full w-[300px] shrink-0 flex-col border-r border-slate-800 bg-bg-deep">
      <div className="flex items-center justify-between border-b border-slate-800 px-4 py-3">
        <h2 className="text-sm font-semibold text-slate-200">内嵌智灵</h2>
        <span className="flex items-center gap-1.5 text-xs text-slate-500">
          <span
            className={`h-2 w-2 rounded-full transition-colors duration-300 ${WS_DOT[wsStatus]}`}
            title={WS_LABEL[wsStatus]}
          />
          {WS_LABEL[wsStatus]}
        </span>
      </div>

      <div ref={listRef} className="flex-1 space-y-3 overflow-y-auto p-4">
        {messages.map((m) => (
          <div
            key={m.id}
            className={`flex ${m.role === "user" ? "justify-end" : "justify-start"}`}
          >
            <div
              className={`max-w-[85%] rounded-xl px-3 py-2 text-sm leading-relaxed transition-colors duration-300 ${
                m.role === "user"
                  ? "bg-yang text-slate-900"
                  : "border border-slate-800 bg-slate-900 text-slate-200"
              }`}
            >
              {m.content}
            </div>
          </div>
        ))}
        {sending && (
          <div className="flex justify-start">
            <div className="max-w-[85%] rounded-xl border border-slate-800 bg-slate-900 px-3 py-2 text-sm text-slate-500">
              思考中…
            </div>
          </div>
        )}
      </div>

      <div className="flex items-end gap-2 border-t border-slate-800 p-3">
        <textarea
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              void handleSend();
            }
          }}
          disabled={sending}
          placeholder="输入消息,或 /run 描述 执行任务"
          rows={2}
          className="flex-1 resize-none rounded-lg border border-slate-700 bg-slate-900 px-3 py-2 text-sm text-slate-200 placeholder-slate-500 outline-none transition-colors duration-300 focus:border-yang disabled:opacity-50"
        />
        <button
          onClick={() => void handleSend()}
          disabled={sending}
          className="rounded-lg bg-yang px-3 py-2 text-sm font-medium text-slate-900 transition-colors duration-300 hover:bg-amber-300 disabled:opacity-50"
        >
          {sending ? "发送中…" : "发送"}
        </button>
      </div>
    </aside>
  );
}
