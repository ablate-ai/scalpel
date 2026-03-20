# Scalpel

`Scalpel` 是一个用于代码瘦身的 skill。

仓库结构按常见 skill 约定组织在 `skills/Scalpel` 下。

仓库现在分成两层：

- [skills/Scalpel/SKILL.md](/Users/c.chen/dev/scalpel/skills/Scalpel/SKILL.md)，定义何时触发以及如何使用
- `skills/Scalpel/scripts/scalpel`：已编译的 Rust 扫描器，可直接执行
- `tools/scalpel-cli`：Rust 源码项目，仅用于维护和构建，不跟随 skill 一起安装

## 能力范围

- 扫描指定目录中的常见代码文件
- 识别完全重复或归一化后重复的文件
- 识别达到阈值的重复代码片段
- 识别文件内部的样板冗余热点
- 生成 Markdown 或 JSON 报告

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

当前版本优先做高置信度检测，不会直接判定“未使用代码”。它更适合先定位 copy-paste、重复模块和样板热点，再由人或 agent 执行安全重构。
