import { useCallback, useState } from "react";
import type { SpindleNode, TpnPhaseState } from "../types";
import { wsClient } from "../lib/wsClient";

/**
 * TPN 弹窗状态管理:
 * - 选中的节点(点开节点弹窗)
 * - 弹窗内当前相位详情(实时从磁盘构建)
 */
export function useTpnState() {
  const [selectedNode, setSelectedNode] = useState<SpindleNode | null>(null);
  const [phaseState, setPhaseState] = useState<TpnPhaseState | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [detailError, setDetailError] = useState<string | null>(null);

  /** 打开节点弹窗并拉取相位详情。 */
  const openNode = useCallback(async (node: SpindleNode) => {
    setSelectedNode(node);
    setDetailLoading(true);
    setDetailError(null);
    try {
      const resp = await wsClient.send("GetTpnState", { taskId: node.taskId });
      setPhaseState(resp.data as TpnPhaseState);
    } catch (e) {
      setDetailError(String(e));
      setPhaseState(null);
    } finally {
      setDetailLoading(false);
    }
  }, []);

  const closeNode = useCallback(() => {
    setSelectedNode(null);
    setPhaseState(null);
    setDetailError(null);
  }, []);

  return {
    selectedNode,
    phaseState,
    detailLoading,
    detailError,
    openNode,
    closeNode,
  };
}
