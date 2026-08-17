#!/usr/bin/env python3
"""Skill: check-file-exists —— 文件存在性检查。

编译自迹拓扑「编写一个-Python-脚本-接收一个文件路径参-20260815-193552」：
该任务产出 check_file.py 并以 file-exists 等检查机械验证。
原子可复用能力 = 接收文件路径参数，判定文件是否存在。
契约：stdin JSON 入参，stdout JSON 返回；30s 内确定性完成，不调 LLM。
"""
import sys, json, os, subprocess


def _file_exists(path):
    """优先经 taiji builtin read 原语机械验证；原语不可用时回退标准库 os.path。"""
    try:
        r = subprocess.run(
            ["taiji", "builtin", "read", "--args", json.dumps({"input": path})],
            capture_output=True, text=True, timeout=10,
        )
        if r.returncode == 0:
            return True, "taiji builtin read 原语确认存在"
        return False, (r.stderr.strip() or "taiji builtin read 返回非零")
    except FileNotFoundError:
        # taiji 原语不在 PATH 时回退标准库（无副作用、确定性、即时完成）
        return os.path.isfile(path), "标准库 os.path.isfile 判定"


def execute(params):
    """params: LLM 工具调用参数（JSON 对象）。支持 path / file / input 键传入文件路径。"""
    if not isinstance(params, dict):
        return {"passed": False, "detail": "params 必须是 JSON 对象", "path": None, "exists": False}
    path = params.get("path") or params.get("file") or params.get("input")
    if not path:
        return {"passed": False, "detail": "缺少文件路径参数（path/file/input）", "path": None, "exists": False}
    exists, how = _file_exists(str(path))
    return {
        "passed": bool(exists),
        "detail": f"文件 {path} 存在性={bool(exists)}（{how}）",
        "path": str(path),
        "exists": bool(exists),
    }


if __name__ == "__main__":
    print(json.dumps(execute(json.loads(sys.stdin.read())), ensure_ascii=False))
