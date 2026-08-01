# 实现计划 — 生产就绪安全整改（三项阻断风险）

## 目标
消除 taiji 仓库生产就绪的三项 🔴 阻断风险：① API key 泄露进 git 历史；② 敏感路径未被 .gitignore 覆盖；③ 无安全的本地密钥存放位。**范围仅限安全项**，不动业务代码、不启动 DMN、不补 CI（均属后续工程化阶段）。

## 背景事实（已审计确认）
- `taiji.config.json` 含真实 key `sk-REVOKED-3d9f…(脱敏)`，已被 git 跟踪（`HEAD:taiji.config.json` 可检出），历史 7 个 commit 中均存在。
- 根 `.gitignore` 仅 1 行 `/target`；`taiji-web/.gitignore` 已正确忽略 node_modules/dist。
- `.taiji/knowledge/prompts/*.yaml` 6 个 seed 资产**有意跟踪**（勿误删）；`.taiji/chat/`、`.taiji/tasks/`、`.taiji/knowledge/index.yaml` 当前未跟踪（防误提交需忽略）。
- 无 git remote（本地仓库），历史清洗无推送冲突，但仍需备份防数据丢失。
- 配置搜索顺序（main.rs `load_config`）：`.taiji/config.json` → `taiji.config.json`；api_key 为空时跳过该路径继续找下一个。

## 任务清单

### 阶段 1：备份（不可跳过）
- [ ] 镜像备份仓库：`git clone --mirror . /tmp/taiji-backup-<date>.git`（或 `cp -r .git`），确认备份存在后再进行任何历史重写。

### 阶段 2：密钥迁移与模板化
- [ ] 创建 `.taiji/config.json`（新文件，将被忽略）：从 `taiji.config.json` 复制完整内容，**保留真实 api_key**，作为本地运行配置。
- [ ] 将 `taiji.config.json` 改造为**仓库内模板**：
  - `api_key` → 占位符 `"sk-REPLACE_WITH_REAL_KEY"`（非空，避免硬错误语义混乱；运行时会走 `.taiji/config.json` 优先路径）
  - `workspace` → 占位符说明（机器特定路径不适合提交）
  - `mcp_servers[].args` 中 `/home/vingo/mimo-mcp` → 说明为机器特定路径（保留示例，注明按环境调整）
  - 文件头注释说明"这是模板，真实配置放 .taiji/config.json"
- [ ] 验证：`load_config` 逻辑确认 `.taiji/config.json` 存在时优先于模板（读 main.rs 确认逻辑不变）。

### 阶段 3：.gitignore 补充（根目录）
- [ ] 追加到根 `.gitignore`：
  ```
  # 敏感配置（含 API key）
  /taiji.config.json
  /.taiji/config.json
  # 运行时数据
  /.taiji/chat/
  /.taiji/tasks/
  /.taiji/knowledge/index.yaml
  ```
- [ ] **不得**忽略 `.taiji/knowledge/prompts/`（seed 资产有意跟踪）。

### 阶段 4：git 停止跟踪 + 收尾提交
- [ ] `git rm --cached taiji.config.json`（保留工作区文件，仅解除跟踪）。
- [ ] 提交当前全部工作区改动（40+ 文件，V24 遗留）：`git add -A && git commit -m "chore: 生产安全整改 — 密钥移出版本控制 + .gitignore 补全"`。
  - ⚠️ 此步是 filter-repo 的前提（工作区必须干净），同时也解决"重构半途未提交"风险。

### 阶段 5：git 历史清洗（破坏性操作，备份完成后执行）
- [ ] 安装工具：`pip install git-filter-repo`（如不可用则备选 `git filter-branch --index-filter 'git rm --cached --ignore-unmatch taiji.config.json' -- --all`）。
- [ ] 执行：`git filter-repo --path taiji.config.json --invert-paths --force`，从全部历史 commit 中移除该文件。
- [ ] 彻底清除残留对象：`git reflog expire --expire=now --all && git gc --prune=now --aggressive`。
- [ ] 验证历史已清洗：`git log --all --oneline -- taiji.config.json` 无输出；`git rev-list --all | xargs git grep -l 'sk-xxxx'` 无匹配。

### 阶段 6：密钥轮换（人工步骤，需要用户操作）
- [ ] 用户登录 DeepSeek 平台，**吊销**旧 key `sk-REVOKED-3d9f…(脱敏)`（已泄露进过 git 历史，轮换不可省）。
- [ ] 生成新 key，写入 `.taiji/config.json` 的 `llm.api_key`（该文件已被 git 忽略，安全）。
- [ ] 旧 key 即使历史已清洗，也因"可能曾被他人接触"必须轮换——清洗是防扩散，轮换是根治。

### 阶段 7：验证（verify agent 执行）
- [ ] `git ls-files | grep -E 'taiji\.config|\.taiji/(chat|tasks)'` 输出为空。
- [ ] `git status --short` 干净（除被忽略文件）。
- [ ] `git log --all --oneline -- taiji.config.json` 无输出。
- [ ] `git grep` 全历史无 `sk-xxxx` 匹配。
- [ ] `cargo test --lib` 仍 142 passed / 0 failed / 9 ignored（无回归）。
- [ ] 新 key 配置可被 `load_config` 正常加载（`.taiji/config.json` 优先）。

## 依赖顺序
1. 阶段 1 备份 → 2. 阶段 2 密钥迁移（依赖 .gitignore 规划，先做无妨）→ 3. 阶段 3 .gitignore → 4. 阶段 4 git rm --cached + 收尾 commit（**filter-repo 前置**）→ 5. 阶段 5 历史清洗（依赖阶段 1 备份 + 阶段 4 干净工作区）→ 6. 阶段 6 用户人工轮换 key（可与阶段 5 并行）→ 7. 阶段 7 验证（全部完成后）。

## 明确不做（本次范围外）
- 不补 CI / Dockerfile / README / LICENSE（后续工程化阶段）
- 不动 3 条编译警告、不改 AGENTS.md 基线数字（非安全项）
- 不启动 DMN Consumer、不触碰业务代码

## 验收标准
- [ ] 真实 API key 不再存在于 git 跟踪文件与全部历史 commit 中
- [ ] `.gitignore` 覆盖 `taiji.config.json` / `.taiji/config.json` / `.taiji/chat/` / `.taiji/tasks/`，且 prompts seed 资产仍被跟踪
- [ ] 本地运行配置（`.taiji/config.json`）含新轮换后的 key，`taiji` 可正常加载
- [ ] `taiji.config.json` 成为无敏感信息的仓库模板
- [ ] 历史清洗后所有 commit 可正常 checkout、无 dangling 大对象残留
- [ ] `cargo test --lib` 基线无回归（142/0/9）
