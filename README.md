# Scalpel

`Scalpel` 是一个面向 agent 的代码库 AST 分析 skill。

仓库结构按常见 skill 约定组织在 `skills/Scalpel` 下。

仓库现在分成两层：

- [skills/Scalpel/SKILL.md](/Users/c.chen/dev/scalpel/skills/Scalpel/SKILL.md)，定义何时触发以及如何使用
- `skills/Scalpel/scripts/scalpel`：已编译的 Rust 分析器，可直接执行
- `tools/scalpel-cli`：Rust 源码项目，仅用于维护和构建，不跟随 skill 一起安装

## 能力范围

- 扫描指定目录中的常见代码文件
- 对已支持语言做 AST 解析
- 输出文件级基础事实：语言、解析状态、顶层节点、template 节点、符号、导入、导出、调用、诊断
- 输出适合 agent 消费的 Markdown 或 JSON 结构化报告
- 在 `derived` 视图中附带完全重复文件和重复候选，供上层按需使用

## 本地运行

```bash
./skills/Scalpel/scripts/scalpel --path . --format markdown
```

输出 JSON：

```bash
./skills/Scalpel/scripts/scalpel --path . --format json
```

## 重新打包

如果修改了 Rust 源码，重新生成二进制：

```bash
make build
```

静态检查：

```bash
make check
```

## 常用参数

- `--path <dir>`: 扫描目录
- `--format <markdown|json>`: 输出格式
- `--min-lines <n>`: 判定重复片段的最小行数，默认 `8`
- `--min-chars <n>`: 判定重复片段的最小字符数，默认 `160`
- `--extensions rs,ts,tsx`: 只扫描指定扩展名
- `--exclude coverage,tmp`: 追加排除目录名

## 说明

当前版本的中心职责是“采集代码库 AST 事实”，而不是直接替 agent 做结论判断。

- 已接入 AST 的语言：`rs`、`go`、`js`、`jsx`、`ts`、`tsx`、`py`、`vue`
- Vue 当前会细化 `script/script setup` 的 AST，以及 `template` 中的元素、组件、指令、插值节点
- 其他扩展名目前仍会被扫描，但会标记为未支持 AST
- `derived` 字段只是派生视图；是否用它做重复检测、架构分析或重构决策，由上层 agent 决定
