# ─────────────────────────────────────────────
# 当前任务：全量代码审查（活跃实例）
# ─────────────────────────────────────────────

> **状态**：🔄 进行中。每批次审查完更新下方进度表与问题清单。
> **审查范围**：Rust 75 文件（31096 行）+ 前端 15 文件（1704 行）= **90 文件 / 32800 行**。

## 目标

逐文件审查全部 90 个文件，对照 `AGENTS.md` 19 条避坑规则 + `Blueprint.md` 设计定论，输出分级问题清单（P0 契约违背 / P1 缺陷 / P2 改进）。

## 依赖顺序

**自底向上**（先审被依赖方，后审依赖方）：`L0 类型 → L1 基础设施 → L2 钩子 → L3 Agent → L4 编排 → L5 MCP → L6 WS/入口 → L7 前端`。

## 接口签名（审查检查清单）

**每文件通用 8 项**：

| # | 检查项 | 来源规则 |
|---|---|---|
| A | 契约一致性：字段/签名 vs Blueprint 图 + AGENTS.md 索引 | §0 |
| B | 错误处理：`TaijiError` 带 `context`；async 无 `panic!/unwrap()` | §5 |
| C | LLM 解析一律走 `parse_llm_json<T>`，禁止裸 `serde_json::from_str` | §6 |
| D | 周易防护：round/cycle 上限、depth<max_depth、max_subtasks 截断、child_token、JoinSet abort | §2 |
| E | 带工具必有 SafetyHook；多 hook 经 HookSet 组合（单槽覆盖） | §4 |
| F | 无降级原则：身份册/会盟/归藏 I/O 失败上抛带路径 | §8 |
| G | 归藏写路径：tmp+rename+git commit | §11 |
| H | 测试质量：tmp_dir 清理、并行唯一（AtomicUsize） | §5 |

## 模块清单（20 批进度表）

> 图例：⬜ 待审 · 🔄 审查中 · ✅ 通过 · ⚠️ 有问题

| 批 | 层 | 文件（行数） | 状态 | 结论 |
|---|---|---|---|---|
| 1 | L0 | mod(9) · task_spec(23) · execution(107) · plan(49) · manifold(77) | ✅ | 5/5 通过，3 条 P2 |
| 2 | L0 | task(149) · ontology(172) · frontend(166) | ✅ | 3/3 通过，2 条 P2 |
| 3 | L0 | verification(624) · agent(544) | ✅ | 2/2 通过，3 条 P2 |
| 4 | L1 | error(62) · json_util(111) · config(296) · task_id(155) · task_spec(230) | ✅ | 5/5 通过，3 条 P2 |
| 5 | L1 | trace(247) · handoff(417) · provider(300) · migrate(188) | ✅ | 4/4 通过，4 条 P2 |
| 6 | L1 | git_backend(394) · meta_skills(422) · skill_catalog(158) | ✅ | 3/3 通过，3 条 P2 |
| 7 | L1 | knowledge(3003，分 3 轮) | ✅ | 通过，6 条 P2 |
| 8 | L2 | safety(847) · trace(574) | ✅ | 2/2 通过，3 条 P2 |
| 9 | L2 | context_limiter(185) · chat_history_snapshot(114) · yang_hook_set(500) · yin_hook_set(209) · test_support(44) | ✅ | 5/5 通过，0 问题 |
| 10 | L3 | skills/common(268) · skills/mod(688) · bash/read/write/search/webfetch(961) | ✅ | 7/7 通过，2 条 P1 + 4 条 P2 |
| 11 | L3 | text_call(148) · yin_verify(107) · recursive_decompose(1099) | ✅ | 3/3 通过，3 条 P2 |
| 12 | L3 | factory(721) · meta(1027) | ✅ | 2/2 通过，1 条 P1 + 2 条 P2 |
| 13 | L3 | yang(1333) · yin(1308) | ✅ | 2/2 通过，4 条 P2 |
| 14 | L3 | plan(322) · chat(357) | ✅ | 2/2 通过，2 条 P1 + 2 条 P2 |
| 15 | L4 | event_bus(45) · worker_pool(256) · task_tree_builder(306) · trigger_engine(402) · constraint_engine(558) | ✅ | 5/5 通过，4 条 P2 |
| 16 | L4 | skill_engine(1151) · model_router(412) · active_learning(437) | ✅ | 3/3 通过，2 条 P2 |
| 17 | L4 | cognition_evolver(2042，分 2 轮) · manifold(337) · ontology_miner(320) · compile(535) | ✅ | 4/4 通过，4 条 P2 |
| 18 | L4 | lianshan(994) · zhouyi(1633，分 2 轮) · runner(352) | ✅ | 3/3 通过，1 条 P1 + 2 条 P2 |
| 19 | L5/L6 | mcp/client(328) · mcp/server(781) · ws/types(203) · ws/handler(226) · ws/server(370) · lib(26) · main(608) | ✅ | 7/7 通过，5 条 P2 |
| 20 | L7 | 前端 15 文件(1704) | ✅ | 15/15 通过，2 条 P2 |

### 模块特定检查点（审查时对照）

| 文件 | 特定检查点 |
|---|---|
| `types/manifold.rs` | 拓扑契约：serde YAML、节点/边 kind |
| `types/ontology.rs` | OntologyEdge type→type、evidence 审计支撑 |
| `types/verification.rs` | SkillAsset 渐进披露三字段、SkillRef.summary、VerificationAsset env_tags/safe_for_exploration |
| `infra/json_util.rs` | parse_llm_json 四级容错 |
| `infra/config.rs` | ContextLimits 单次窗口语义、lianshan 命名 |
| `infra/git_backend.rs` | commit 排除 .history、快照式 rollback |
| `infra/meta_skills.rs`+`skill_catalog.rs` | 双轨加载同 id 资产优先 |
| `infra/knowledge.rs` | 双类型禁令、load_skill_assets 兼容旧单文件、本体 YAML 存取、asset_type_map |
| `hooks/context_limiter.rs` | limiter.triggered() 时序（agent 返回后 parse 前） |
| `hooks/yang_hook_set.rs` | HookSet 组合顺序 safety→trace→snapshot |
| `tools/skills/mod.rs` | ToolProfile 路由、弱模型双通道 normalize_args 三级展开 |
| `tools/skills/write.rs` | write 路径基准 task_dir、enforce_cwd_scope |
| `tools/text_call.rs` | 文本调用块 fallback 提取 |
| `tools/recursive_decompose.rs` | depth/max_subtasks/cancellation 约束 |
| `factory.rs` | model_class 指纹、profile_for_model |
| `meta.rs` | MetaOutcome 双出口 match、短路判断先于模式决策、env_tags、ontology 消费 |
| `yang.rs`/`yin.rs` | 溢出时序、阴保守裁决 30%/35% 两阈值 |
| `plan.rs`/`chat.rs` | MetaOutcome match；chat 只读工具挂 SafetyHook |
| `constraint_engine.rs` | 约束升级 ontology: 前缀、rule_matches |
| `skill_engine.rs` | run_checks_assets 跳过阳面 |
| `active_learning.rs` | exploration_score 冷启动先验、env_tags 降权、safe_for_exploration 过滤 |
| `cognition_evolver.rs` | fork 打 env_tags 线程 |
| `ontology_miner.rs` | 纯函数零 LLM、常量阈值 ONTOLOGY_MIN_SAMPLES=50 |
| `compile.rs` | 单写者幂等、不写 model_stats、删 pending、机械判据 |
| `lianshan.rs` | enqueue_lianshan_pending 带 task_dir、compile 入队、ontology hook |
| `zhouyi.rs` | YangOutcome 三态 match、rerun_meta 共用、context_overflow 三路分流 |
| `mcp/server.rs` | 工具暴露面安全 |
| `main.rs` | --with-lianshan spawn_compiler |
| 前端 | 类型与 ws/types 对齐、ZhouyiPopup/useZhouyiState 重命名后引用完整性 |

## 验收标准

- [x] 90 文件全部审完，进度表无 ⬜（20/20 批 ✅）
- [x] 每文件结论：✅ 通过 或 ⚠️ 问题 N 条（进度表结论栏）
- [x] 问题分档：🔴 P0（0）/ 🟡 P1（6）/ 🟢 P2（61）
- [ ] 审查发现按三文件回流规则归档：避坑 → `AGENTS.md`；设计定论 → `Blueprint.md`（待用户批准）

---

## 问题清单（审查中动态追加）

### 🔴 P0（契约违背，立即修）

（空）

### 🟡 P1（缺陷，已全部修复 ✅，339 tests 通过）

- **[批10] `tools/skills/webfetch.rs`** — `check_ssrf` 十进制/十六进制私网 IP 绕过：字符串层只匹配点分十进制字面量，url 层只查 `localhost`/`127.`/`[::1]` 三种（漏 10.x/172.16-31/192.168/169.254 段）。`http://167772160`（=10.0.0.0）字符串层不含 "10."、url 层 host_str="10.0.0.0" 不匹配三检查 → 放行 → 内网 SSRF。与 `hooks/safety.rs` 的 `check_web_url`（有十进制/十六进制 + 全私网段检测）双实现漂移，建议复用 safety 单一事实源。→ ✅ 已修复：webfetch 委托 `safety::check_web_url_static`（单一事实源，覆盖十进制/十六进制 + 全私网段）。
- **[批10] `tools/skills/webfetch.rs`** — 重定向 SSRF 绕过：`reqwest::redirect::Policy::limited(5)` 跟随重定向但对每跳目标不做 `check_ssrf`，初始公网 URL 302→内网地址即绕过；建议 `Policy::none()` 或 `Policy::custom` 逐跳检查 host。→ ✅ 已修复：`Policy::custom` 逐跳 `check_ssrf`（最多 5 跳）。
- **[批12] `agents/meta.rs`** — `run()` 硬编码 `self.provider.client("deepseek")`，而 `factory::create_meta_agent` 丢弃了 `agent_llm_config("meta")` 解析出的 provider（`let (_provider, model)`，MetaAgentBuilder 无 provider_name 字段）；配置 meta 用非 deepseek provider（或路由到异 provider 模型）时模型名与客户端不匹配 → 调用失败。对比 `create_yang_agent` 正确传了 `.provider_name(&provider)`，建议 meta 同构补 provider_name。→ ✅ 已修复：MetaAgentBuilder 补 `provider_name` 字段 + factory 传入。
- **[批14] `agents/plan.rs`** — `build_plan_prompt` 的 `&prompt[..max_chars]`（max_chars=200）按**字节**切片：`yang_system_prompt` 常含中文（多字节 UTF-8），200 字节处非 char boundary 时 panic（§5 禁止）；变量名 max_chars 误导（实际是 max_bytes），建议用 `char_indices` 找安全边界或 `.chars().take(n)`。→ ✅ 已修复：`prompt.chars().take(200)` 按 char 截断。
- **[批14] `agents/plan.rs`** — `compose_plan` 硬编码 `self.provider.client("deepseek")`，且 `factory::create_plan_agent` 丢弃 provider（`let (_provider, model)`）；与批12 meta.rs 同类（provider 硬编码 + 无 provider_name 字段），建议同构补 provider_name。→ ✅ 已修复：PlanBuilder 补 `provider_name` 字段 + factory 传入。
- **[批18] `orchestration/zhouyi.rs`** — PASS 分支用 `engine_ctx.task_dir.parent().and_then(|p| p.parent())` 推导 data_root——**只对根任务正确**（`{data_root}/tasks/{id}`）；子任务（`{data_root}/tasks/{root}/children/{idx}`）推导出根任务目录，`enqueue_lianshan_pending` 把子任务 pending 写到 `{根任务目录}/pending/`，而 Lianshan Consumer 扫的是 `{data_root}/pending/` → **子任务学习信号（checks 统计/model_stats/迹拓扑/编译/本体挖掘）全部丢失**，只有根任务学习闭环生效。`ZhouyiCycle` 已有 `factory.data_root` 字段可直接用，建议改用 `self.factory.data_root` 替代路径推导（这与 §15「MVP 边界：仅根级归因」很可能是同一根因）。→ ✅ 已修复：PASS 分支直接取 `self.factory.data_root`。

### 🟢 P2（改进，仅记录）

- **[批1] `types/plan.rs`** — 注释含旧分层术语（"L1 skills"/"L4 Truth constraints"/"L5 Prompt 匹配摘要"/"V22 renamed"），V44 去分区化后已过时，建议改为去分层措辞（skills/truths/prompts）。→ ✅ 已清理
- **[批1] `types/execution.rs`** — `PhaseSummary.phase` 注释列出 4 个中文值（权重更新|概率拟合|因果验证|收敛判定），但实际 `mcp/server.rs` explain 只写入 "概率拟合" 一个值；注释取值域与实际不符，建议收敛注释或补全实现。→ ✅ 已清理
- **[批1] `types/manifold.rs`** — 无 serde roundtrip 测试锁契约（对照 `types/ontology.rs` 有 `semantic_type_yaml_roundtrip`），建议补 YAML roundtrip + kind 枚举 snake_case 序列化断言。→ ✅ 已补
- **[批2] `types/task.rs`** — `SubtaskSpec` 结构体注释第 18 行「V26 起无 `mode` 字段」与第 33 行实际存在的 `mode: AgentMode`（V27 阴阳配对）矛盾，V26 注释未随 V27 删除，建议删/改该句。→ ✅ 已清理
- **[批2] `types/ontology.rs`** — `OntologyEdge.strength` 字段注释写「P(pass | a∧b) − P(pass | a)（lift）」，但实际实现 `ontology_miner.rs:112` 是联合通过率 `pass/co`（MVP 非 lift，AGENTS.md §19 遗留③）；注释与实现矛盾，建议改注释为「联合通过率（MVP）」。→ ✅ 已清理
- **[批3] `types/verification.rs`** — 注释残留旧分层术语「L4 Truth」（`TruthStatus`/`TruthConstraint`）、「L0/L1/L2」（`CheckKind` 注释「前四种机械可判定（L0/L1）…L2 兜底」），V44 去分区化后应改为无分层措辞。→ ✅ 已清理
- **[批3] `types/agent.rs`** — 多处旧分层注释（`SkillRef`「L1 Skill」、`PromptAsset.layer`「Cognitive layer (1 = Skill, matching L1)」、`VerificationAsset.layer`「0 占位」）；`AssetRef.asset_type` 注释列「workflow」类型（全仓无引用，已废弃）；`PromptAsset` 的 Directory layout 注释写 `{data_dir}/prompts/` 旧布局，实际为 `yang/prompts` + `yin/prompts`。→ ✅ 已清理
- **[批3] `types/verification.rs`** — `ContractReport` 兼容别名已自标记「待全仓引用迁移后删除」，建议排期清理（grep 确认迁移完成后删）。→ ✅ 已删除。
- **[批4] `infra/error.rs` + AGENTS.md §5** — 规则「`TaijiError` 变体必须携带 `context: String`」与代码实际不一致：`ContextOverflow`/`HardCutoff` 带 `threshold`、`MaxDepthExceeded` 等带 `max`、`SafetyViolation` 带 `reason`（结构化字段同样携带上下文）；建议更新 AGENTS.md 措辞为「携带上下文信息（`context: String` 或结构化字段）」。→ ✅ 已清理
- **[批4] `infra/json_util.rs`** — ` ```json ` 围栏提取仅匹配小写无空格 ` ```json `，LLM 常见的 ` ```JSON ` / ` ``` json ` 变体未覆盖，建议 case-insensitive + 允许 ` ``` ` 与 `json` 间空格。→ ✅ 已修（find_json_fence + 测试）
- **[批4] `infra/config.rs`** — 无测试锁定 `ContextLimits::effective_handoff/hard_cutoff` 的 30%/35% 推导、`LianshanConfig` 默认值（`compile_enabled=false`、`bayesian_enabled=true` 等），建议补默认值 + 阈值推导测试。→ ✅ 已补
- **[批5] `infra/trace.rs`** — `TraceRecord.phase` 注释列中文值（「权重更新|概率拟合·turn|工具调用|因果验证|收敛判定」）与实际写入值（`yang`/`tool_call::*`）漂移；且 trace.rs 无测试——`redact_sensitive`（脱敏）与 `rotate`（轮转）是关键安全/持久化逻辑，缺覆盖。→ ✅ 已补测试
- **[批5] `infra/handoff.rs`** — `write_handoff` 上方两行注释重复且第一行过时（「V28 第一版不调 LLM…留待增强」已被 V29 实现推翻）。→ ✅ 已清理
- **[批5] `infra/provider.rs`** — `llm.providers` 中 `base_url` 为空且 `name≠deepseek` 的条目被静默按 deepseek 处理（语义模糊，建议 warn）；provider.rs 无测试（`model_candidates` 路由候选、`resolve_model` 查表、`client` 回退）。→ ✅ 已补测试
- **[批5] `infra/migrate.rs`** — `migrate_all` 的 `read_dir` 失败静默吞错（返回 `Ok(0)` 但实际未迁移任何任务），运维工具建议 warn 提示。→ ✅ 已修（warn）
- **[批6] `infra/git_backend.rs`** — commit id = `{ts_millis:x}-{hash:msg}`：同毫秒 + 同 msg 的两次 commit 产生相同 id，覆盖历史快照（注释称「单调 + 唯一」不成立）；建议加纳秒时间戳或原子计数器。→ ✅ 已修复（id 用纳秒时间戳）。
- **[批6] `infra/meta_skills.rs`** — 注释「阴 8 元 skill」「阳 7 + 阴 8」与实际的阴 9 个（Verify 6 + Converge 3，总 16）不符（文件头对偶表实列 9 个阴）。→ ✅ 已清理
- **[批6] `infra/skill_catalog.rs`** — `test_load_skill_catalog_meta_only_when_empty` 与 `test_load_skill_catalog_minimal_keeps_recursive_decompose` 两个测试未末尾清理临时目录（违反 §5；虽 `tempfile_dir` 有「先删旧」缓解，后续 `_cleaned` 测试才补 TmpGuard）。→ ✅ 已修（TmpGuard）
- **[批7] `infra/knowledge.rs`** — 文件头 doc 注释过时：仍写「`{data_dir}/{type}s/{id}.yaml`」旧布局（实际 yang/yin 对偶目录），Directory layout 含「L1/L2」旧分层术语。→ ✅ 已清理
- **[批7] `infra/knowledge.rs`** — `scan_assets` 是 `pub` 但返回 private `IndexData`（编译警告 `private_interfaces`），建议 `IndexData` 改 `pub(crate)` 或 `scan_assets` 降可见性。→ ✅ 已修（scan_assets 降 private）
- **[批7] `infra/knowledge.rs`** — `save_model_stats` 只 tmp+rename 不 `git.commit`，与 `save_asset`/`save_skill`/`save_topology`/`save_ontology_yaml` 的「每次写 = commit」不一致（有意：高频统计衍生，不进版本历史）。→ ✅ 已决策（+ 注释说明）→ ✅ 决策：有意不 commit（高频衍生非契约，避免快照爆炸）+ 注释说明
- **[批7] `infra/knowledge.rs`** — `e.to_string().contains("failed to read asset")` 字符串匹配错误类型（反模式，`load_prompt`/`load_model`/`load_verification` 多处）：文案变更即失效，且权限错误会被误判为「不存在」静默降级；建议加专门 NotFound 变体或 error kind。→ ✅ 已修（KnowledgeAssetNotFound 变体 + matches!）→ ✅ 已修（TaijiError::KnowledgeAssetNotFound + matches!）
- **[批7] `infra/knowledge.rs`** — `dual_for_check_kind` 的 `LlmJudgement → "recursive-decompose"` 推导对 `semantic-coherence`（元层 dual 实为 "yin-verify"）推错（迁移期 kind 级歧义，3/4 正确）。→ ✅ 已决策（多数派默认 + 注释歧义边界）→ ✅ 决策：取多数派递归分解 + 注释歧义边界（文件夹格式已存精确 dual）
- **[批7] `infra/knowledge.rs`** — `migrate_to_yang_yin` doc 注释两段重复且第一段过时（「YangAgent→yang」vs 实际「其余→yang」）；测试仍用 `#[deprecated] SkillAsset` 别名（触发 warning）。（doc 重复 → ✅ 已清理；测试 warning → ✅ 已清（裸 `SkillAsset` 别名改 `LegacyToolSkillAsset`）
- **[批8] `hooks/safety.rs`** — `check_exec_command` 的 `contains("eval")` 过宽，误杀含 "eval" 的正常命令（如 `cargo test evaluation`、"retrieval"），建议改为 `eval `（带空格边界）匹配。→ ✅ 已修复（空格边界 + 整词）。
- **[批8] `hooks/trace.rs`** — 测试临时目录用 `std::process::id()` + 固定前缀且开头不 `remove_dir_all` 残留（§5 轻微违规：测试 panic 残留会污染下次运行；`write_record_preserves_long_plain_strings_end_to_end` 已用 AtomicUsize，其余未统一）。→ ✅ 已修（计数器 + remove_dir_all）
- **[批8] `hooks/trace.rs`** — `on_tool_result` 在 Mutex 锁内做 `elapsed()` + JSON 解析（延长临界区），建议锁内只取 `(Instant, String)`、锁外计算。→ ✅ 已修（锁外 elapsed + parse）
- **[批10] `tools/skills/common.rs`** — `truncate_tail` 字节兜底从头 `truncate`（tail 语义矛盾：末尾行超字节时丢末尾保开头），与 bash 用它「capture errors at the end」的目的相悖，应改为保留末尾 max_bytes 字节。→ ✅ 已修复 + 回归测试。
- **[批10] `tools/skills/common.rs`** — `safe_canonicalize` 在「已存在祖先经 symlink 解析后与原始路径词法前缀不一致」+「目标不存在」时 `strip_prefix` 失败降级为原路径（丢失 canonicalize），`enforce_cwd_scope` 的 starts_with 退化为词法级，symlink 逃逸可能漏检。→ ✅ 已修（strip_prefix 用原始祖先 + symlink 逃逸回归测试）
- **[批10] `tools/skills/read.rs`** — 先 `tokio::fs::read` 全文件进内存再检查 100MB 上限（超大文件内存风险），应先 `tokio::fs::metadata` 查大小再决定是否读。→ ✅ 已修复（先 metadata 拦截）。
- **[批10] `tools/skills/mod.rs`** — `test_builtin_skills_all_executable` 写 `target/taiji_test_write.txt` 末尾不清理（残留，违反 §5 临时文件清理）。→ ✅ 已修（末尾清理）
- **[批11] `tools/yin_verify.rs`** — `definition()` 描述残留旧分层术语「L4 Truth constraints」「L1 skill tools」（V44 去分区化后过时，同类：批1/批3）。→ ✅ 已清理
- **[批11] `tools/recursive_decompose.rs` + AGENTS.md §2** — 规则「任何子任务失败时 abort_all() 清理」与 V31 实现矛盾：代码对**任务级失败不 abort**（记 Diverged 条目进 prior_results、成功兄弟继续收集），仅取消/panic 才 abort_all；应更新 §2 措辞为「取消/panic 时 abort_all；任务级失败记录 Diverged 继续」。→ ✅ 已清理
- **[批11] `tools/recursive_decompose.rs`** — `rerun_of` 指向不存在的索引时 `create_dir_all` 静默新建空目录（而非报错），LLM 传错 index 会被掩盖为「全新子任务」；建议校验 old_idx 存在性（不存在则报错）。→ ✅ 已修复（不存在则报错）。
- **[批12] `agents/factory.rs`** — `profile_for_model` doc 注释过时：仍写「Minimal（隐藏 recursive-decompose/webfetch 等高代价工具）」，但 V47 已不再隐藏 recursive-decompose（拆解正是弱模型小上下文规避超限的核心手段），实际仅隐藏 webfetch；建议改注释。→ ✅ 已清理
- **[批12] `agents/meta.rs`** — `MetaComposeResult.temperature` 死字段：LLM 输出的温度覆盖被解析但从未消费（`MetaContext` 无 temperature 字段，yang/yin 温度来自静态 `agent_overrides.temperature`，且 prompt 输出格式也未含 temperature）；建议接线进 MetaContext 或删字段。→ ✅ 已接线（MetaContext.temperature）
- **[批13] `agents/yang.rs` + AGENTS.md §3** — 四象温度默认值未接线：yang 温度单一来自 `agent_overrides["yang"].temperature`（无 YangOrch 0.8 / YangExec 0.5 按 mode 区分），yin（verify/converge）完全未设温度（YinVerify 0.2 / YinConverge 0.2 未实现）；§3「四象温度默认值」是纸面规则，建议按 mode 接入或修订 §3。→ ✅ 已接线（四象温度）
- **[批13] `agents/yin.rs`** — `verify()` 的 `ConstraintEngine::load_truths(&[], &rules)` 传空 tags：`MetaContext` 无 task_type_tags 字段，任务类型标签未从元传递到阴，验证检查**全部** truth 而非任务相关 truth；待批15确认 `load_truths` 空 tags 语义后定夺（全查是宁多勿漏 vs 标签漏接）。→ ✅ 已修（load_truths(&meta_ctx.task_type_tags)）
- **[批13] `agents/yang.rs`** — `compress_history_to_handoff` 开头两行注释重复（「压缩输入 = 磁盘快照…不用内存 history」×2，V29 历史残留）。→ ✅ 已清理
- **[批13] `agents/yang.rs` + `agents/yin.rs`** — doc 注释「Production wiring (pinned for Rig API verification)」ignore 代码块过时：含「Current state (TODO) stubbed with todo!」、`serde_json::from_str` 直接解析（违反 §6 parse_llm_json）、`client("deepseek")` 硬编码；实际已接 YangHookSet/YinHookSet + client_for + parse_llm_json，建议删/更新为实际接线。→ ✅ 已清理
- **[批14] `agents/plan.rs`** — `run_meta_agent` 用 `self.model`（plan 的 override 模型）跑 MetaAgent，而非 meta 自己的 override 模型——plan 路径下 meta 的模型配置被 plan 覆盖（预演路径语义混淆）。→ ✅ 已修（PlanBuilder.meta_llm 解耦 meta 模型）→ ✅ 已修（PlanBuilder.meta_llm 解耦 meta 模型）
- **[批14] `agents/chat.rs`** — `test_build_system_prompt_with_task_context` 弱断言：`contains("任务-123") || !contains("正在查看任务")` 恒真（context_task_id=Some 但 meta.json 不存在 → load_task_meta 返回 None → 后半恒真），未真正覆盖任务上下文注入分支；建议建临时 task meta.json 后断言 prompt 含真实 description。→ ✅ 已修
- **[批15] `orchestration/task_tree_builder.rs`** — `total_siblings` 计算 bug：`task_dir.parent().unwrap_or(&task_dir).join("children")` 在子任务 task_dir=`父/children/idx` 上多套一层 → 扫 `父/children/children`（不存在）→ count_dirs=0 → `.max(1)` 恒 1（前端纺锤树兄弟布局数据错误）；应为 `task_dir.parent()` 直接扫父的 children 目录。→ ✅ 已修复 + 回归测试。
- **[批15] `orchestration/task_tree_builder.rs`** — 零测试（L4 前端纺锤树核心模块无 `#[cfg(test)]`，total_siblings bug 正是无测试漏掉的）。→ ✅ 已补测试
- **[批15] `orchestration/constraint_engine.rs`** — `check_constraints`（MetaContext 版）死代码：grep 确认生产零调用（仅测试）；且 V32 起 `MetaContext.constraints` 恒空（meta.rs 组装 `constraints: vec![]`），即使调用也会恒违反 no-fabrication/evidence-based。建议删除或接入实际检查点。→ ✅ 已删除（+ check_single_constraint）
- **[批15] `orchestration/constraint_engine.rs`** — `truth:code-safety` 死代码：`load_truths` 唯一生产调用（`yin.rs:233`）传 `&[]` 空 tags，`code` tag 永不命中 → code-safety 约束永不加载，`check_yin_output` 的 code-safety 分支永不触发；与批13「task_type_tags 未传递」同根源。（✅ 已修：task_type_tags 链打通——zhouyi classify_task_tags → MetaContext → 阴 load_truths）
- **[批16] `orchestration/active_learning.rs`** — `enqueue_exploration_task` 的 `for _ in 0..max_per_window` 循环体末尾无条件 `break` → `max_per_window > 1` 时也恒入 1 个（doc「每窗口限量 max_per_window」与实现不符）；要么去掉 break 并排除已选资产，要么改 doc 为「每窗口 1 个」。→ ✅ 已修（去 break + exclude 已选）
- **[批16] `orchestration/skill_engine.rs`** — `impl_to_check_spec` 的 `_ => CheckKind::FileExists` 非 exhaustive 兑底：未来新增阴面 `SkillKind` 而忘更新此 match 时会静默映射为 FileExists（错误机械执行）；建议显式匹配全部阴面 kind（无 `_` 兑底），编译期暴露遗漏。→ ✅ 已修复（显式列出阳面 kind）。
- **[批17] `orchestration/manifold.rs`** — `verify` 边只建边不建节点（`checks` 循环仅 push 边，to 端 check_id 无对应 `TopologyNode`），拓扑图不完整（check 未建模为节点）；若前端/后续消费按节点遍历会缺失 verify 对象，建议补 check 节点或文档明确「verify 边 to 端为悬空标签」。→ ✅ 已修（TopologyNodeKind::Check + 建节点）→ ✅ 已修（TopologyNodeKind::Check 节点 + 测试）
- **[批17] `orchestration/ontology_miner.rs`** — check_kind 字符串格式不一致：生产 `check_kind_name` 用 serde snake_case（下划线 `command_succeeds`），测试/约定用连字符（`command-succeeds`）；rules.yaml 消费端（constraint_engine `require: check:{kind}`）需对齐格式，建议统一 snake_case。→ ✅ 已修（测试统一下划线）
- **[批17] `orchestration/cognition_evolver.rs`** — `evolve()` 的 δ₀ `prune_low_confidence` / δ₁ `tune_skill` 是 no-op 占位（仅 log 返 0），δ₂ 用 `record.task_id` 当 asset_id 的 placeholder 逻辑（`record.phase.contains("yang")` 匹配且 task_id 非真实资产 id）；实际演化走 `evolve_contracts`/`evolve_prompts`。若 `evolve()` 仍被 lianshan 调用则「剪枝/调优」实际空转，建议确认调用点后废弃或补实现。→ ✅ 已废弃（删除 no-op 方法）
- **[批17] `orchestration/cognition_evolver.rs`** — `merge_variants` 直接 `status = "pruned".into()` 字符串魔法值（与 `merge_prompts` 的 `status_mark_merged()` 方法不一致）；建议统一用语义化方法/枚举。→ ✅ 已修（VerificationAsset::status_mark_merged）
- **[批18] `orchestration/zhouyi.rs`** — `needs_meta` 与 `rerun_meta` 调 `meta_agent.run(description, &["general"], ...)` 硬编码固定 `["general"]` 标签（任务类型标签未从描述动态提取），归藏检索恒用 general 而非任务相关标签——与批13/批15「task_type_tags 未传递」同根源（标签链在 Zhouyi→Meta 层就断了）。→ ✅ 已修（classify_task_tags 纯符号提取替代硬编码 general）
- **[批18] `orchestration/lianshan.rs`** — `test_spawn_and_cancel` 泄漏后台任务：spawn consumer 后不 cancel，`timeout(500ms, handle)` 超时后 JoinHandle 被 drop（detached），consumer 无限跑（注释自认「runs forever」）；建议用外部 token cancel 后 join。→ ✅ 已修
- **[批19] `mcp/client.rs`** — 无 SafetyHook 集成：`McpClientConnection.call_tool` 直接调用，非 trusted 服务器的工具调用未经过 `SafetyHook`（trusted bypass 契约「非 trusted 受 SafetyHook 检查」依赖上游注入，本层未验证）；且 mcp/client 零测试（connect 超时/call_tool 错误/connect_all 收集错误均未覆盖）。（⏸ 暂缓：需 mock rmcp transport）
- **[批19] `mcp/server.rs`** — `handle_run` 的 `max_depth` override 只改 config 副本（`config.runtime.max_depth = depth` 后 `RecursiveRunner::new(factory.clone(), config)`），`factory.config` 未同步 → `ZhouyiCycle`（用副本）与 `RecursiveDecomposeTool`（用 `factory.config.runtime.max_depth`）的 max_depth 来源不一致，override 半生效（叶节点判断与 decompose 深度检查用不同值）。→ ✅ 已修（AgentFactory::with_config 同步 factory.config）
- **[批19] `ws/handler.rs`** — `handle_execute_task` 的 `_max_depth: Option<u32>` 被忽略（MVP 注释），与 MCP `taiji_run` 的 max_depth override 能力不一致；`handle_get_zhouyi_state` 的 trace_preview `summary` 取 `v["output"].as_str()`（对象 output 恒 None）→ 预览 summary 恒空。（summary 恒空 → ✅ 已修复；ExecuteTask max_depth → ✅ 已修：with_config 同步）
- **[批19] `ws/server.rs`** — 无背压 + 零测试：每连接 `mpsc::unbounded_channel`（注释「events cheap never block」但 DeliverableCreated/ChildCompleted 可能带大 payload，慢客户端可堆积）；`handle_connection`/`broadcast`/`process_request` 均无测试覆盖。（⏸ 暂缓：需 mock websocket）
- **[批19] `main.rs`** — `cmd_trace` 的 `--tail` 用 `records.into_iter().rev().take(n)` 输出**倒序**（最新在前），与 `mcp::handle_trace` 的 `split_off(len-n)` 正序不一致（注释「keep last N」暗示应保序）；`cmd_seed` doc 注释仍写 V39 旧分区语义（「复制到目标模型分区」），V44 去分区化后已改为恢复到知识根。（cmd_trace 倒序 → ✅ 已修复；cmd_seed 注释待清理）
- **[批20] `taiji-web/src/hooks/useTaskTree.ts`** — `refresh` 无请求序号/取消：快速切换 root（或事件并发触发）时，旧 root 的慢响应可覆盖新 root 快照（out-of-order）；建议加请求序号或 ignore-stale 检查。
- **[批20] `taiji-web/src/components/SpindleTree.tsx`** — 纺锤布局 `spread` 实现为「层整体平移」而非「水平散布」：`offsetX = PADDING + layerWidth/2 + spread` 使中间层整体右移 sin(π·d/max)·MAX_SPREAD（最大 380px），且 x 坐标可溢出 width（x_max ≈ 1.5·layerWidth + spread + PADDING > width = layerWidth + 2·PADDING），中间层节点被父容器 `overflow-hidden` 裁剪；注释「中间层散布最大」与实现不符（应为每层水平散布范围随 depth 变化）。
