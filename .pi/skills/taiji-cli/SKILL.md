---
name: taiji-cli
description: 通过 CLI 驱动和管理 taiji 认知内核项目（TPN-DMN 递归任务引擎）。覆盖：初始化工作区、执行/恢复任务（taiji run/--resume）、任务列表与状态查看（list/status）、失败任务诊断（trace + 任务目录检查）、产物读取、Web 前端启动。pi 无 MCP 时的官方 CLI 操作手册——当用户提到"跑任务/执行 taiji/看任务状态/诊断任务/恢复任务/taiji 前端"时使用。
---

# taiji CLI 操作手册

taiji 是一个 Rust 认知内核：`taiji run <描述>` 把任务交给 TPN 三阶段循环（Meta 权重编排 → Fitting 概率执行 → Causal 因果验证），可递归拆解子任务，产物写入任务目录的 `deliverables/`。本 skill 教你通过 CLI 正确驱动与诊断它。

## 0. 前置条件（每次先检查）

```bash
cd /home/vingo/vingo/taiji
test -f .taiji/config.json && echo "config OK" || echo "缺配置"
grep -q '"api_key": "[^"]' .taiji/config.json && echo "api_key OK" || echo "api_key 空（硬错误）"
ls target/debug/taiji >/dev/null 2>&1 && echo "二进制 OK" || cargo build 2>/dev/null | tail -1
```

- 配置**仅来自配置文件**（`.taiji/config.json` → `taiji.config.json`），不读环境变量；`api_key` 为空是硬错误。
- 用 `target/debug/taiji`（开发构建）或 `cargo run -- <命令>`。改过代码先 `cargo build`。

## 1. 命令速查

| 命令 | 用途 | 关键输出 |
|------|------|---------|
| `taiji run <描述...>` | 执行任务（描述多词自动空格拼接） | `✓ Task completed: <task_id>` + Content + Tools used |
| `taiji run --resume <task_id>` | 恢复失败/中断任务（复用目录 + 恢复链） | 同上 |
| `taiji init` | 初始化工作区 + 归藏知识库 | `✓ taiji workspace initialized` |
| `taiji list` | 任务列表与状态 | `Tasks (N total):` + `<id> [Status]` |
| `taiji status` | DMN/认知状态摘要 | Workspace/Data root/provider/depth/rounds/计数 |
| `taiji trace <task_id> [--tree] [--tail N]` | 查看 trace 记录（JSONL 逐条） | 不存在时 stderr 报错 + exit 1 |
| `taiji serve [--port 1420] [--no-open]` | Web 前端（HTTP 1420 + WS 17890，阻塞） | 需先 `cd taiji-web && npm run build` |

## 2. 跑任务（核心操作）

```bash
# 新任务（task_id 是自动生成的 UUID，从输出捕获）
timeout 620 ./target/debug/taiji run "任务描述" 2>&1 | tee /tmp/taiji_run.log
```

**输出解析**：
- 成功：`✓ Task completed: <uuid>` 后跟 Content 与 `Tools used: read, write, ...`。
- 失败：进程非零退出，stderr 含 `Error:` 或 `panic` 字样；任务状态已原子写入 `meta.json`（Failed/Cancelled）。

**任务描述要点**（直接影响质量与安全）：
- 明确产物路径与格式：`在 deliverables/ 下写 src/xxx.md`（LLM 只在该任务目录内自由读写，区外受 SafetyHook 拦截）。
- 明确规模与预算：小任务直接执行；大任务提示"可拆解"；要求"不要拆解"可强制单层。
- 涉及代码库读取时用相对路径（如 `src/types/task.rs`），agent 的 read 工具按项目根解析。

**超时处置**：bash 超时杀掉进程后，任务可能停在 `Running`（checkpoint 已写）。此时：
1. `taiji list` 看状态；2. `taiji run --resume <id>` 从失败点增量续跑（Fitting 阶段从 `chat_history.json` 快照继续，不会重跑整个任务）。

## 3. 任务状态语义

`taiji list` 显示的 `[Status]` 来自 `meta.json`：`Running`（进行中/被中断）/ `Completed` / `Failed` / `Cancelled`。恢复优先级：`--resume` 显式历史 > `decompose_result.json`（已完成）> `checkpoint.json`（崩溃恢复）。

## 4. 任务诊断（失败/超时/异常时）

```bash
ID=<task_id>
# 1. 状态与元数据
cat .taiji/tasks/$ID/meta.json          # status/depth/description/parent_id
# 2. trace 尾部（最近 LLM 调用，找错误/驳回原因）
./target/debug/taiji trace $ID --tail 10
# 3. 全树 trace（含子任务）
./target/debug/taiji trace $ID --tree | tail -40
# 4. 产物（成功任务在此）
find .taiji/tasks/$ID/deliverables -type f 2>/dev/null
# 5. 子任务状态（递归分解时）
for d in .taiji/tasks/$ID/children/*/; do echo "$d: $(python3 -c "import json;print(json.load(open('$d/meta.json'))['status'])" 2>/dev/null)"; done
```

**判断规则**：
- 有 `decompose_result.json` + status=Completed → 真完成。
- status=Failed 且 trace 尾部是 `MaxTurnsError`/`LLMCallFailed` → LLM 轮次或调用问题，`--resume` 或升级描述后重跑。
- `children/` 下大量 Running 残留 → 父任务被中止的旧版本行为（V26.3 起已修复为 Failed 落盘），可安全忽略或 `--resume`。
- trace 记录无密钥明文（脱敏生效）；若见长字符串被整段遮蔽属旧版本行为，V26.3 起仅前缀密钥模式脱敏。

## 5. 任务目录结构（9 项持久化文件清单，BCP §8.1）

`{data_root}/tasks/{id}/` 下：`meta.json`（元数据+状态）、`checkpoint.json`（循环进度，PASS 后删除）、`meta_ctx.json`（Meta 编排上下文）、`chat_history.json`（Fitting 对话快照）、`verify_state.json`（Causal 验证缓存）、`decompose_result.json`（完成标记+结果缓存）、`deliverables/`（产物）、`children/`（子任务树）、`trace.jsonl`（审计）。**只读诊断，不要手动改写**（运行时数据，写者只有引擎）。

## 6. 常见工作流

- **初始化**：`taiji init` → 检查 `taiji status`。
- **跑新任务**：§2。完成后读产物：`find deliverables -type f` + `cat` 关键文件，向用户汇报任务 ID、状态、Tools used、产物路径。
- **恢复**：`taiji list` 找 Failed/Running → `taiji run --resume <id>`。
- **诊断**：§4 流程走一遍，给出结论（真失败/超时/LLM 问题）与建议动作。
- **前端**：`cd taiji-web && npm run build && cd .. && ./target/debug/taiji serve`（浏览器自动打开 http://127.0.0.1:1420，WS 17890）。

## 7. 陷阱清单

- `taiji run` 是长任务：bash 一律 `timeout 620`，不要默认 30s。
- 新任务 ID 是 UUID：从 `run` 输出捕获，不要猜测。
- 不要用 `taiji mcp`（阻塞式 MCP 服务器，pi 场景用本 skill 的 CLI 路径替代）。
- 诊断前先 `cargo build` 确保二进制与源码一致。
- 任务描述模糊会导致 LLM 反复试错烧预算：描述里给足路径、格式、规模约束。
- 改 `src/` 后跑 `cargo test` 回归（基线 172 passed / 9 ignored），BCP/AGENTS.md 是唯一事实，CLI 行为变更需同步文档。
