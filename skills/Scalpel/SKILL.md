---
name: Scalpel
description: 扫描代码仓库并输出面向 agent 的 AST 结构化事实，包括文件语言、解析状态、顶层节点、Vue template 节点、符号、导入、导出、调用和诊断。用户提到代码库分析、AST、依赖关系、模块结构、Vue 组件结构、符号分布、调用关系、代码体检、重构前摸底时，都应该使用这个 skill；即使用户没有明确说“AST”或“skill”，只要目标是先理解代码库再决定怎么改，也要触发。
compatibility:
  tools: [exec_command, apply_patch]
---

# Scalpel

先采集代码库事实，再基于这些事实回答问题或修改代码。不要在没有证据的情况下直接删除、合并或重构代码。

## 目标

把这个 skill 当成“代码库 AST 分析后端”来用：

1. 扫描目标目录中的代码文件。
2. 为已支持语言提取结构化事实。
3. 把 `derived` 视图当成附加信息，而不是默认结论。

## 何时使用

优先用于这些场景：

- 用户要先理解代码库结构，再决定怎么改
- 用户想看模块关系、符号、导入导出、调用分布
- 用户在分析 Vue、TypeScript、Rust、Go、Python 等代码库
- 用户提到“先扫描一下”“先摸底”“先做 AST 分析”“先看依赖/调用/组件结构”

如果用户的目标只是“找重复代码”，也可以用，但重复检测只是 `derived` 视图的一部分。

## 工作流

### 1. 确认范围

优先确认或推断：

- 目标路径
- 是否限制扩展名
- 是否需要追加排除目录

如果用户没有给细节，默认扫描当前仓库，并排除 `.git`、`node_modules`、`target`、`dist`、`build`、`vendor`、`.next`、`.turbo`。

### 2. 运行分析器

可执行文件位于 `scripts/scalpel`。

默认：

```bash
scripts/scalpel \
  --path <target-path> \
  --format markdown
```

如果需要机器可读输出：

```bash
scripts/scalpel \
  --path <target-path> \
  --format json
```

需要修改分析器实现时，再进入 `tools/scalpel-cli`。

### 3. 读取结果

优先读取顶层 `files`，这是主结果。重点关注：

- `path`
- `language`
- `parse_status`
- `summary`
- `top_level_nodes`
- `template_nodes`
- `symbols`
- `imports`
- `exports`
- `calls`
- `diagnostics`

`derived` 只作为附加视图读取：

- `exact_duplicate_files`
- `clone_candidates`

不要把 `derived` 当成默认主结论。

### 4. 根据用户目标解释结果

按任务类型组织输出：

- 架构/模块分析：优先总结顶层节点、符号、导入导出、调用分布
- Vue 分析：优先总结 `template_nodes`、script 中的符号与调用
- 重构前摸底：先给文件事实，再指出高价值的 `derived` 观察
- 重复代码分析：说明这些候选来自 `derived`，是否值得处理由后续判断决定

### 5. 真正修改代码时

如果用户要基于扫描结果继续改代码：

- 先指出你依据了哪些文件事实
- 说明修改目标和影响范围
- 修改后做最小验证
- 汇报哪些判断是基于 AST 事实，哪些是你的推断

## 输出要求

默认输出应包含：

1. 扫描范围
2. 与用户目标最相关的代码库事实
3. 必要时引用 `derived` 中的派生观察
4. 如果已经改代码，说明验证结果

## 结果解读原则

- `files` 是主数据，`derived` 是派生视图。
- `parse_status=unsupported` 或 `parse_error` 时，结论要降权。
- Vue 文件要把 `template` 和 `script` 分开看。
- 发现候选重复并不等于必须合并。
- 工具输出的是证据，不是最终架构判断。

## 当前实现边界

- 已接入 AST 的语言：`rs`、`go`、`js`、`jsx`、`ts`、`tsx`、`py`、`vue`
- Vue 当前会细化 `script/script setup` 的 AST，以及 `template` 中的元素、组件、指令、插值节点
- 其他扩展名会被扫描，但会标记为未支持 AST

## 示例

**示例 1**

用户请求：`先分析一下这个前端仓库的组件结构和调用关系`

动作：

- 运行扫描器扫描仓库
- 先汇总 `files` 中的 Vue/TS 文件事实
- 按组件、符号、导入和调用关系组织结果

**示例 2**

用户请求：`帮我看看这个 Vue 项目 template 层是不是太复杂`

动作：

- 扫描项目
- 优先读取 `.vue` 文件的 `template_nodes`
- 结合 script 中的符号和调用给出复杂度观察

**示例 3**

用户请求：`扫一下这个 services 目录有没有明显重复逻辑`

动作：

- 扫描 `services`
- 先读取相关文件事实
- 把 `derived.clone_candidates` 作为候选证据提供给用户
