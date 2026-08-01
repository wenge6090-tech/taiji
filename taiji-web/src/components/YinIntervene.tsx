import { useCallback, useEffect, useRef, useState } from "react";
import { wsClient } from "../lib/wsClient";
import type { InterventionAction } from "../types";

const ACTIONS: Array<{
  action: InterventionAction;
  label: string;
  className: string;
}> = [
  {
    action: "Approve",
    label: "通过 ✓",
    className: "border-green-400 text-green-400 hover:bg-green-400/10",
  },
  {
    action: "RejectRetry",
    label: "驳回重试",
    className: "border-yellow-400 text-yellow-400 hover:bg-yellow-400/10",
  },
  {
    action: "RejectReroute",
    label: "驳回改道",
    className: "border-red-400 text-red-400 hover:bg-red-400/10",
  },
];

export default function YinIntervene({
  taskId,
  onSubmitted,
}: {
  taskId: string;
  onSubmitted?: () => void;
}) {
  const [suggestion, setSuggestion] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [submittedAction, setSubmittedAction] = useState<InterventionAction | null>(null);
  const [error, setError] = useState<string | null>(null);
  const restoreTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(
    () => () => {
      if (restoreTimer.current) clearTimeout(restoreTimer.current);
    },
    []
  );

  const handleAction = useCallback(
    async (action: InterventionAction) => {
      if (submitting) return;
      const suggestionText = suggestion.trim();
      if (action !== "Approve" && suggestionText === "") return;
      setSubmitting(true);
      setError(null);
      try {
        await wsClient.send("SubmitReview", {
          intervention: { taskId, action, suggestion: suggestionText },
        });
        setSubmittedAction(action);
        if (restoreTimer.current) clearTimeout(restoreTimer.current);
        restoreTimer.current = setTimeout(() => {
          setSubmittedAction(null);
          restoreTimer.current = null;
        }, 2000);
        onSubmitted?.();
      } catch (e) {
        setError(`提交失败:${String(e)}`);
      } finally {
        setSubmitting(false);
      }
    },
    [submitting, suggestion, taskId, onSubmitted]
  );

  return (
    <div className="space-y-2">
      <p className="text-xs font-semibold uppercase tracking-wide text-orange-400">
        阴极审批
      </p>
      <input
        value={suggestion}
        onChange={(e) => setSuggestion(e.target.value)}
        disabled={submitting}
        placeholder="给阴的建议…如:改为搜索实现方案后再分解"
        className="w-full rounded-lg border border-slate-700 bg-slate-800 px-3 py-2 text-sm text-slate-200 placeholder-slate-500 outline-none transition-colors duration-300 focus:border-orange-400 disabled:opacity-50"
      />
      <div className="flex flex-wrap gap-2">
        {ACTIONS.map(({ action, label, className }) => {
          const needsSuggestion = action !== "Approve";
          const disabled =
            submitting || (needsSuggestion && suggestion.trim() === "");
          const done = submittedAction === action;
          return (
            <button
              key={action}
              onClick={() => void handleAction(action)}
              disabled={disabled}
              className={`rounded-lg border px-3 py-1.5 text-xs font-medium transition-colors duration-300 ${className} ${
                done ? "bg-green-400/10" : ""
              } ${disabled ? "cursor-not-allowed opacity-50" : ""}`}
            >
              {done ? "已提交 ✓" : label}
            </button>
          );
        })}
      </div>
      {error && <p className="text-xs text-red-400">{error}</p>}
    </div>
  );
}
