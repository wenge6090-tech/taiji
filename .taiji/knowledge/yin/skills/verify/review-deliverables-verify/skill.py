"""review-deliverables-verify —— 审查交付物一致性机械判据（归藏 skill 资产层执行体）。

功能本质：判据类。输入「报告路径 + 期望引用」，输出「passed 布尔 + detail + 逐项 checks」，
机械判定审查任务交付物是否齐备一致，供阴验证 / 人读消费。

stdin 输入 params（JSON 对象）：
  {
    "base_dir": ".",                                  # 判据根目录（默认当前目录）
    "report_path": "deliverables/review-B.md",        # 审查报告相对路径
    "output_refs": ["deliverables/optimized"],        # 期望存在的引用产物（front matter 缺失时兜底）
    "require_status": "complete"                      # front matter 期望状态
  }

stdout 输出（JSON 对象）：
  {"passed": bool, "detail": str, "checks": 逐项明细对象}

纯 Python 标准库实现：不写文件、不触网、不调 LLM、无死循环，30s 内必然返回。
"""

import sys
import json
import os

REQUIRED_FM_FIELDS = ("task", "result", "status")


def parse_front_matter(text):
    """简化 YAML front matter 解析：首行 --- 起，第二段 --- 止。"""
    if not text.startswith("---"):
        return None
    end = text.find("\n---", 3)
    if end == -1:
        return None
    block = text[3:end].strip()
    fm = {}
    for line in block.splitlines():
        if ":" not in line:
            continue
        key, _, val = line.partition(":")
        key = key.strip()
        val = val.strip()
        if val.startswith("[") and val.endswith("]"):
            items = [x.strip().strip('"').strip("'")
                     for x in val[1:-1].split(",") if x.strip()]
            fm[key] = items
        elif val == "":
            fm[key] = None
        else:
            fm[key] = val.strip('"').strip("'")
    return fm


def execute(params):
    base = params.get("base_dir", ".")
    report = params.get("report_path", "deliverables/review-B.md")
    fallback_refs = params.get("output_refs", [])
    if isinstance(fallback_refs, str):
        fallback_refs = [fallback_refs]
    require_status = params.get("require_status", "complete")

    checks = {}

    # 1) 审查报告存在
    report_abs = os.path.join(base, report)
    checks["report_exists"] = os.path.isfile(report_abs)

    # 2) front matter 合法（首行 --- + 必需字段齐全）
    fm = None
    missing = []
    if checks["report_exists"]:
        try:
            with open(report_abs, "r", encoding="utf-8") as f:
                text = f.read(65536)
            fm = parse_front_matter(text)
            if fm is not None:
                missing = [k for k in REQUIRED_FM_FIELDS if k not in fm]
        except OSError:
            fm = None
    checks["front_matter_valid"] = fm is not None and not missing
    checks["front_matter_missing_fields"] = missing

    # 3) status 与期望一致
    checks["status_matches"] = bool(
        fm is not None and fm.get("status") == require_status
    )

    # 4) output_refs 逐一可解析（优先取 front matter 自身引用，缺省用参数兜底）
    refs_to_check = fallback_refs
    if isinstance(fm, dict) and fm.get("output_refs"):
        refs_to_check = fm["output_refs"]
    ref_detail = []
    refs_all_ok = True
    for ref in refs_to_check:
        ok = os.path.isfile(os.path.join(base, ref))
        ref_detail.append({"ref": ref, "exists": ok})
        refs_all_ok = refs_all_ok and ok
    checks["output_refs_resolve"] = refs_all_ok
    checks["output_refs_detail"] = ref_detail

    # 5) trace 一致性：全部关键判据汇聚
    passed = bool(
        checks["report_exists"]
        and checks["front_matter_valid"]
        and checks["status_matches"]
        and checks["output_refs_resolve"]
    )

    if passed:
        detail = ("PASS: 审查交付物一致性满足——报告存在、front matter 合法、"
                  "status=%s、output_refs 全部可解析") % require_status
    else:
        failed = [k for k, v in {
            "report_exists": checks["report_exists"],
            "front_matter_valid": checks["front_matter_valid"],
            "status_matches": checks["status_matches"],
            "output_refs_resolve": checks["output_refs_resolve"],
        }.items() if not v]
        detail = "FAIL: 判据未满足 -> " + "; ".join(failed)

    return {"passed": passed, "detail": detail, "checks": checks}


if __name__ == "__main__":
    print(json.dumps(execute(json.loads(sys.stdin.read())), ensure_ascii=False))
