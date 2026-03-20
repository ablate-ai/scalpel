use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use serde::Serialize;
use sha2::{Digest, Sha256};
use walkdir::{DirEntry, WalkDir};

const DEFAULT_EXTENSIONS: &[&str] = &[
    "rs", "go", "js", "jsx", "ts", "tsx", "py", "java", "kt", "swift", "rb", "php", "c", "cc",
    "cpp", "h", "hpp", "cs", "scala", "sql", "sh", "lua",
];

const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    ".next",
    ".turbo",
];

#[derive(Debug, Parser)]
#[command(name = "scalpel", about = "扫描代码重复与冗余热点")]
struct Cli {
    #[arg(long, default_value = ".")]
    path: PathBuf,

    #[arg(long, value_enum, default_value_t = OutputFormat::Markdown)]
    format: OutputFormat,

    #[arg(long, default_value_t = 8)]
    min_lines: usize,

    #[arg(long, default_value_t = 160)]
    min_chars: usize,

    #[arg(long, value_delimiter = ',')]
    extensions: Vec<String>,

    #[arg(long, value_delimiter = ',')]
    exclude: Vec<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum OutputFormat {
    Json,
    Markdown,
}

#[derive(Debug)]
struct FileRecord {
    path: PathBuf,
    normalized_lines: Vec<String>,
    content_hash: String,
    normalized_hash: String,
}

#[derive(Debug, Serialize)]
struct ScanReport {
    root: String,
    scanned_files: usize,
    duplicate_files: Vec<DuplicateFileGroup>,
    duplicate_snippets: Vec<DuplicateSnippetGroup>,
    boilerplate_hotspots: Vec<BoilerplateHotspot>,
    suggestions: Vec<String>,
}

#[derive(Debug, Serialize)]
struct DuplicateFileGroup {
    kind: String,
    paths: Vec<String>,
}

#[derive(Debug, Serialize)]
struct DuplicateSnippetGroup {
    occurrences: Vec<SnippetOccurrence>,
    lines: usize,
    normalized_preview: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SnippetOccurrence {
    path: String,
    start_line: usize,
    end_line: usize,
}

#[derive(Debug, Serialize)]
struct BoilerplateHotspot {
    path: String,
    repeated_lines: usize,
    total_lines: usize,
    ratio: f64,
    sample_lines: Vec<String>,
}

#[derive(Clone, Debug)]
struct WindowOccurrence {
    file_idx: usize,
    start: usize,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let report = scan(&cli)?;

    match cli.format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        OutputFormat::Markdown => {
            println!("{}", render_markdown(&report));
        }
    }

    Ok(())
}

fn scan(cli: &Cli) -> Result<ScanReport> {
    let excludes: HashSet<String> = DEFAULT_EXCLUDES
        .iter()
        .copied()
        .chain(cli.exclude.iter().map(String::as_str))
        .map(str::to_string)
        .collect();

    let extensions: HashSet<String> = if cli.extensions.is_empty() {
        DEFAULT_EXTENSIONS.iter().map(|s| s.to_string()).collect()
    } else {
        cli.extensions
            .iter()
            .map(|s| s.trim_start_matches('.').to_string())
            .collect()
    };

    let root = cli
        .path
        .canonicalize()
        .with_context(|| format!("无法访问路径 {}", cli.path.display()))?;

    let mut files = Vec::new();
    for entry in WalkDir::new(&root)
        .into_iter()
        .filter_entry(|entry| keep_entry(entry, &excludes))
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        if !is_code_file(path, &extensions) {
            continue;
        }

        if let Some(record) = load_file(path, &root)? {
            files.push(record);
        }
    }

    let duplicate_files = find_duplicate_files(&files);
    let duplicate_snippets = find_duplicate_snippets(&files, cli.min_lines, cli.min_chars);
    let boilerplate_hotspots = find_boilerplate_hotspots(&files);
    let suggestions =
        build_suggestions(&duplicate_files, &duplicate_snippets, &boilerplate_hotspots);

    Ok(ScanReport {
        root: root.display().to_string(),
        scanned_files: files.len(),
        duplicate_files,
        duplicate_snippets,
        boilerplate_hotspots,
        suggestions,
    })
}

fn keep_entry(entry: &DirEntry, excludes: &HashSet<String>) -> bool {
    if entry.depth() == 0 {
        return true;
    }

    let name = entry.file_name().to_string_lossy();
    !excludes.contains(name.as_ref())
}

fn is_code_file(path: &Path, extensions: &HashSet<String>) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| extensions.contains(ext))
        .unwrap_or(false)
}

fn load_file(path: &Path, root: &Path) -> Result<Option<FileRecord>> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return Ok(None),
    };

    let relative = path.strip_prefix(root).unwrap_or(path).to_path_buf();
    let lines: Vec<String> = content.lines().map(|line| line.to_string()).collect();
    let normalized_lines: Vec<String> = lines
        .iter()
        .map(|line| normalize_line(line))
        .filter(|line| !line.is_empty())
        .collect();

    if normalized_lines.is_empty() {
        return Ok(None);
    }

    Ok(Some(FileRecord {
        path: relative,
        normalized_lines: normalized_lines.clone(),
        content_hash: hash_string(&content),
        normalized_hash: hash_string(&normalized_lines.join("\n")),
    }))
}

fn normalize_line(line: &str) -> String {
    let no_comment = line
        .split("//")
        .next()
        .unwrap_or("")
        .split('#')
        .next()
        .unwrap_or("")
        .trim();

    if no_comment.is_empty() {
        return String::new();
    }

    let mut out = String::with_capacity(no_comment.len());
    let mut last_space = false;

    for ch in no_comment.chars() {
        let mapped = if ch.is_ascii_digit() {
            '0'
        } else if ch.is_ascii_whitespace() {
            ' '
        } else {
            ch
        };

        if mapped == ' ' {
            if !last_space {
                out.push(mapped);
            }
            last_space = true;
        } else {
            out.push(mapped);
            last_space = false;
        }
    }

    out.trim().to_string()
}

fn hash_string(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn find_duplicate_files(files: &[FileRecord]) -> Vec<DuplicateFileGroup> {
    let mut exact: HashMap<&str, Vec<String>> = HashMap::new();
    let mut normalized: HashMap<&str, Vec<String>> = HashMap::new();

    for file in files {
        let path = file.path.display().to_string();
        exact
            .entry(&file.content_hash)
            .or_default()
            .push(path.clone());
        normalized
            .entry(&file.normalized_hash)
            .or_default()
            .push(path);
    }

    let mut groups = Vec::new();

    for paths in exact.into_values().filter(|paths| paths.len() > 1) {
        groups.push(DuplicateFileGroup {
            kind: "exact".to_string(),
            paths: sorted(paths),
        });
    }

    for paths in normalized.into_values().filter(|paths| paths.len() > 1) {
        let set: HashSet<_> = paths.iter().collect();
        if set.len() < 2 {
            continue;
        }

        groups.push(DuplicateFileGroup {
            kind: "normalized".to_string(),
            paths: sorted(paths),
        });
    }

    groups.sort_by(|a, b| {
        b.paths
            .len()
            .cmp(&a.paths.len())
            .then_with(|| a.kind.cmp(&b.kind))
    });
    groups
}

fn find_duplicate_snippets(
    files: &[FileRecord],
    min_lines: usize,
    min_chars: usize,
) -> Vec<DuplicateSnippetGroup> {
    let mut windows: HashMap<String, Vec<WindowOccurrence>> = HashMap::new();

    for (file_idx, file) in files.iter().enumerate() {
        if file.normalized_lines.len() < min_lines {
            continue;
        }

        for start in 0..=file.normalized_lines.len() - min_lines {
            let snippet = file.normalized_lines[start..start + min_lines].join("\n");
            if snippet.len() < min_chars {
                continue;
            }
            windows
                .entry(hash_string(&snippet))
                .or_default()
                .push(WindowOccurrence { file_idx, start });
        }
    }

    let mut groups = Vec::new();
    let mut seen_keys = HashSet::new();

    for occurrences in windows.into_values() {
        if occurrences.len() < 2 {
            continue;
        }

        let representative = &occurrences[0];
        let base = &files[representative.file_idx].normalized_lines;
        let base_slice = &base[representative.start..representative.start + min_lines];
        let key = format!(
            "{}:{}",
            files[representative.file_idx].path.display(),
            representative.start
        );
        if seen_keys.contains(&key) {
            continue;
        }

        let mut merged = Vec::new();
        for occ in &occurrences {
            let current = &files[occ.file_idx].normalized_lines;
            let max_len = base
                .len()
                .saturating_sub(representative.start)
                .min(current.len().saturating_sub(occ.start));
            let mut len = 0usize;
            while len < max_len && base[representative.start + len] == current[occ.start + len] {
                len += 1;
            }

            if len < min_lines {
                continue;
            }

            let preview = current[occ.start..occ.start + len].join("\n");
            if preview.len() < min_chars {
                continue;
            }

            merged.push((occ.file_idx, occ.start, len));
        }

        merged.sort();
        merged.dedup();
        if merged.len() < 2 {
            continue;
        }

        for (file_idx, start, _) in &merged {
            seen_keys.insert(format!("{}:{}", files[*file_idx].path.display(), start));
        }

        let lines = merged
            .iter()
            .map(|(_, _, len)| *len)
            .min()
            .unwrap_or(min_lines);
        let occurrences = merged
            .into_iter()
            .map(|(file_idx, start, len)| SnippetOccurrence {
                path: files[file_idx].path.display().to_string(),
                start_line: start + 1,
                end_line: start + len,
            })
            .collect::<Vec<_>>();

        let preview = base_slice
            .iter()
            .take(3)
            .map(|line| line.to_string())
            .collect::<Vec<_>>();

        groups.push(DuplicateSnippetGroup {
            occurrences,
            lines,
            normalized_preview: preview,
        });
    }

    groups.sort_by(|a, b| {
        b.occurrences
            .len()
            .cmp(&a.occurrences.len())
            .then_with(|| b.lines.cmp(&a.lines))
    });
    groups.truncate(20);
    groups
}

fn find_boilerplate_hotspots(files: &[FileRecord]) -> Vec<BoilerplateHotspot> {
    let mut hotspots = Vec::new();

    for file in files {
        if file.normalized_lines.len() < 20 {
            continue;
        }

        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for line in &file.normalized_lines {
            *counts.entry(line).or_default() += 1;
        }

        let repeated: Vec<_> = counts
            .into_iter()
            .filter(|(_, count)| *count >= 3)
            .collect();
        let repeated_lines = repeated.iter().map(|(_, count)| *count).sum::<usize>();
        let ratio = repeated_lines as f64 / file.normalized_lines.len() as f64;

        if ratio < 0.35 {
            continue;
        }

        hotspots.push(BoilerplateHotspot {
            path: file.path.display().to_string(),
            repeated_lines,
            total_lines: file.normalized_lines.len(),
            ratio,
            sample_lines: repeated
                .into_iter()
                .take(5)
                .map(|(line, _)| line.to_string())
                .collect(),
        });
    }

    hotspots.sort_by(|a, b| b.ratio.total_cmp(&a.ratio));
    hotspots.truncate(20);
    hotspots
}

fn build_suggestions(
    duplicate_files: &[DuplicateFileGroup],
    duplicate_snippets: &[DuplicateSnippetGroup],
    hotspots: &[BoilerplateHotspot],
) -> Vec<String> {
    let mut suggestions = Vec::new();

    if let Some(group) = duplicate_files.first() {
        suggestions.push(format!(
            "先处理重复文件：{} 个文件内容相同或归一化后相同，优先收敛为单一实现。",
            group.paths.len()
        ));
    }

    if let Some(group) = duplicate_snippets.first() {
        suggestions.push(format!(
            "优先抽取公共逻辑：发现一组长度约 {} 行、出现 {} 次的重复片段，适合提升为共享函数或模块。",
            group.lines,
            group.occurrences.len()
        ));
    }

    if let Some(hotspot) = hotspots.first() {
        suggestions.push(format!(
            "关注样板热点：{} 中约 {:.0}% 的归一化代码行属于重复样板，可考虑表驱动或模板收敛。",
            hotspot.path,
            hotspot.ratio * 100.0
        ));
    }

    if suggestions.is_empty() {
        suggestions.push(
            "未发现高置信度重复热点；可以降低阈值后重扫，或限制到特定目录做更细粒度分析。"
                .to_string(),
        );
    }

    suggestions
}

fn render_markdown(report: &ScanReport) -> String {
    let mut out = String::new();
    out.push_str("# Scalpel 扫描报告\n\n");
    out.push_str(&format!("- 扫描根目录: `{}`\n", report.root));
    out.push_str(&format!("- 扫描文件数: `{}`\n\n", report.scanned_files));

    out.push_str("## 重复文件\n\n");
    if report.duplicate_files.is_empty() {
        out.push_str("未发现重复文件。\n\n");
    } else {
        for group in &report.duplicate_files {
            out.push_str(&format!(
                "- 类型: `{}`，文件数: `{}`\n",
                group.kind,
                group.paths.len()
            ));
            for path in &group.paths {
                out.push_str(&format!("  - `{}`\n", path));
            }
        }
        out.push('\n');
    }

    out.push_str("## 重复代码片段\n\n");
    if report.duplicate_snippets.is_empty() {
        out.push_str("未发现达到阈值的重复代码片段。\n\n");
    } else {
        for group in &report.duplicate_snippets {
            out.push_str(&format!(
                "- 重复长度: 约 `{}` 行，出现次数: `{}`\n",
                group.lines,
                group.occurrences.len()
            ));
            for occ in &group.occurrences {
                out.push_str(&format!(
                    "  - `{}`:{}-{}\n",
                    occ.path, occ.start_line, occ.end_line
                ));
            }
            if !group.normalized_preview.is_empty() {
                out.push_str("  - 片段预览:\n");
                for line in &group.normalized_preview {
                    out.push_str(&format!("    - `{}`\n", line));
                }
            }
        }
        out.push('\n');
    }

    out.push_str("## 样板冗余热点\n\n");
    if report.boilerplate_hotspots.is_empty() {
        out.push_str("未发现明显样板热点。\n\n");
    } else {
        for hotspot in &report.boilerplate_hotspots {
            out.push_str(&format!(
                "- `{}`: 重复样板 `{}` / `{}` 行，比例 `{:.0}%`\n",
                hotspot.path,
                hotspot.repeated_lines,
                hotspot.total_lines,
                hotspot.ratio * 100.0
            ));
            for line in &hotspot.sample_lines {
                out.push_str(&format!("  - `{}`\n", line));
            }
        }
        out.push('\n');
    }

    out.push_str("## 瘦身建议\n\n");
    for suggestion in &report.suggestions {
        out.push_str(&format!("- {}\n", suggestion));
    }

    out
}

fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}
