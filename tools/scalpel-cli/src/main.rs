use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tree_sitter::{Language, Node, Parser as TsParser};
use walkdir::{DirEntry, WalkDir};

const DEFAULT_EXTENSIONS: &[&str] = &[
    "rs", "go", "js", "jsx", "ts", "tsx", "py", "vue", "java", "kt", "swift", "rb", "php", "c",
    "cc", "cpp", "h", "hpp", "cs", "scala", "sql", "sh", "lua",
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

const MAX_PREVIEW_LINES: usize = 3;
const MAX_SUMMARY_NODES: usize = 12;
const MAX_DERIVED_ITEMS: usize = 40;

#[derive(Debug, Parser)]
#[command(name = "scalpel", about = "面向 agent 的代码库 AST 分析器")]
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

#[derive(Clone, Copy, Debug)]
enum LanguageKind {
    Rust,
    JavaScript,
    TypeScript,
    Tsx,
    Python,
    Go,
    Vue,
}

impl PartialEq for LanguageKind {
    fn eq(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

impl Eq for LanguageKind {}

impl LanguageKind {
    fn from_extension(path: &Path) -> Option<Self> {
        match path.extension().and_then(|ext| ext.to_str())? {
            "rs" => Some(Self::Rust),
            "js" | "jsx" => Some(Self::JavaScript),
            "ts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "py" => Some(Self::Python),
            "go" => Some(Self::Go),
            "vue" => Some(Self::Vue),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::Python => "python",
            Self::Go => "go",
            Self::Vue => "vue",
        }
    }

    fn parser_language(self) -> Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::Vue => unreachable!("vue 由 SFC 分块分析，不直接走单一 parser"),
        }
    }
}

#[derive(Debug)]
struct FileRecord {
    path: PathBuf,
    content_hash: String,
    normalized_lines: Vec<String>,
    ast_spans: Vec<AstSpanRecord>,
    analysis: FileAnalysis,
}

#[derive(Debug)]
struct AstSpanRecord {
    path: String,
    language: String,
    node_kind: String,
    start_line: usize,
    end_line: usize,
    line_count: usize,
    structural_hash: String,
    token_hash: String,
    structural_kinds: Vec<String>,
    preview: Vec<String>,
}

#[derive(Debug)]
struct FileAnalysis {
    parse_status: ParseStatus,
    language: Option<String>,
    summary: FileSummary,
    top_level_nodes: Vec<NodeSummary>,
    template_nodes: Vec<TemplateNodeFact>,
    symbols: Vec<SymbolFact>,
    imports: Vec<ImportFact>,
    exports: Vec<ExportFact>,
    calls: Vec<CallFact>,
    diagnostics: Vec<AnalysisDiagnostic>,
}

#[derive(Debug, Clone, Copy)]
enum ParseStatus {
    Parsed,
    Unsupported,
    ParseError,
}

impl ParseStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Parsed => "parsed",
            Self::Unsupported => "unsupported",
            Self::ParseError => "parse_error",
        }
    }
}

#[derive(Debug, Default)]
struct FileSummary {
    total_named_nodes: usize,
    max_depth: usize,
}

#[derive(Debug)]
struct VueBlock {
    tag: String,
    lang: Option<String>,
    start_line: usize,
    end_line: usize,
    content_start_line: usize,
    content: String,
}

#[derive(Debug, Serialize)]
struct CodebaseAnalysisReport {
    root: String,
    scanned_files: usize,
    parsed_files: usize,
    unsupported_files: usize,
    parse_error_files: usize,
    files: Vec<FileFact>,
    derived: DerivedViews,
    notes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct FileFact {
    path: String,
    language: Option<String>,
    parse_status: String,
    content_hash: String,
    summary: FileSummaryFact,
    top_level_nodes: Vec<NodeSummary>,
    template_nodes: Vec<TemplateNodeFact>,
    symbols: Vec<SymbolFact>,
    imports: Vec<ImportFact>,
    exports: Vec<ExportFact>,
    calls: Vec<CallFact>,
    diagnostics: Vec<AnalysisDiagnostic>,
}

#[derive(Debug, Serialize)]
struct FileSummaryFact {
    total_named_nodes: usize,
    max_depth: usize,
    template_node_count: usize,
    symbol_count: usize,
    import_count: usize,
    export_count: usize,
    call_count: usize,
}

#[derive(Debug, Clone, Serialize)]
struct NodeSummary {
    kind: String,
    start_line: usize,
    end_line: usize,
}

#[derive(Debug, Clone, Serialize)]
struct TemplateNodeFact {
    kind: String,
    name: Option<String>,
    directives: Vec<String>,
    attributes: Vec<String>,
    expression: Option<String>,
    depth: usize,
    span: Span,
}

#[derive(Debug, Clone, Serialize)]
struct SymbolFact {
    name: String,
    kind: String,
    visibility: String,
    span: Span,
}

#[derive(Debug, Clone, Serialize)]
struct ImportFact {
    source: Option<String>,
    imported_names: Vec<String>,
    span: Span,
}

#[derive(Debug, Clone, Serialize)]
struct ExportFact {
    name: Option<String>,
    kind: String,
    span: Span,
}

#[derive(Debug, Clone, Serialize)]
struct CallFact {
    callee: Option<String>,
    kind: String,
    span: Span,
}

#[derive(Debug, Clone, Serialize)]
struct AnalysisDiagnostic {
    level: String,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
struct Span {
    start_line: usize,
    end_line: usize,
}

#[derive(Debug, Serialize)]
struct DerivedViews {
    exact_duplicate_files: Vec<ExactFileGroup>,
    clone_candidates: Vec<CloneCandidate>,
}

#[derive(Debug, Serialize)]
struct ExactFileGroup {
    content_hash: String,
    paths: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CloneCandidate {
    candidate_kind: String,
    match_basis: String,
    language: String,
    occurrence_count: usize,
    scores: MatchScores,
    fingerprint: FingerprintEvidence,
    occurrences: Vec<SourceSpan>,
    preview: Vec<String>,
}

#[derive(Debug, Serialize)]
struct MatchScores {
    confidence: f64,
    structural_similarity: f64,
    token_similarity: f64,
    text_similarity: f64,
}

#[derive(Debug, Serialize)]
struct FingerprintEvidence {
    structural_hash: String,
    token_hash: String,
    sample_node_kinds: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SourceSpan {
    path: String,
    start_line: usize,
    end_line: usize,
    node_kind: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let report = scan(&cli)?;

    match cli.format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        OutputFormat::Markdown => println!("{}", render_markdown(&report)),
    }

    Ok(())
}

fn scan(cli: &Cli) -> Result<CodebaseAnalysisReport> {
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

        if let Some(record) = load_file(path, &root, cli.min_lines, cli.min_chars)? {
            files.push(record);
        }
    }

    let parsed_files = files
        .iter()
        .filter(|file| matches!(file.analysis.parse_status, ParseStatus::Parsed))
        .count();
    let unsupported_files = files
        .iter()
        .filter(|file| matches!(file.analysis.parse_status, ParseStatus::Unsupported))
        .count();
    let parse_error_files = files
        .iter()
        .filter(|file| matches!(file.analysis.parse_status, ParseStatus::ParseError))
        .count();

    let files_view = files.iter().map(to_file_fact).collect::<Vec<_>>();
    let derived = DerivedViews {
        exact_duplicate_files: find_exact_duplicate_files(&files),
        clone_candidates: find_clone_candidates(&files, cli.min_lines, cli.min_chars),
    };
    let notes = build_notes(parsed_files, unsupported_files, parse_error_files, &derived);

    Ok(CodebaseAnalysisReport {
        root: root.display().to_string(),
        scanned_files: files.len(),
        parsed_files,
        unsupported_files,
        parse_error_files,
        files: files_view,
        derived,
        notes,
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

fn load_file(
    path: &Path,
    root: &Path,
    min_lines: usize,
    min_chars: usize,
) -> Result<Option<FileRecord>> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return Ok(None),
    };

    let normalized_lines = content
        .lines()
        .map(normalize_line)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if normalized_lines.is_empty() {
        return Ok(None);
    }

    let relative = if root.is_file() {
        path.file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| path.to_path_buf())
    } else {
        path.strip_prefix(root).unwrap_or(path).to_path_buf()
    };
    let language = LanguageKind::from_extension(path);
    let (analysis, ast_spans) = analyze_file(&content, &relative, language, min_lines, min_chars);

    Ok(Some(FileRecord {
        path: relative,
        content_hash: hash_string(&content),
        normalized_lines,
        ast_spans,
        analysis,
    }))
}

fn analyze_file(
    content: &str,
    relative: &Path,
    language: Option<LanguageKind>,
    min_lines: usize,
    min_chars: usize,
) -> (FileAnalysis, Vec<AstSpanRecord>) {
    let analysis_language = language.map(|lang| lang.as_str().to_string());

    match language {
        None => (
            FileAnalysis {
                parse_status: ParseStatus::Unsupported,
                language: analysis_language,
                summary: FileSummary::default(),
                top_level_nodes: Vec::new(),
                template_nodes: Vec::new(),
                symbols: Vec::new(),
                imports: Vec::new(),
                exports: Vec::new(),
                calls: Vec::new(),
                diagnostics: vec![AnalysisDiagnostic {
                    level: "info".to_string(),
                    message: "当前版本未接入该语言的 tree-sitter parser".to_string(),
                }],
            },
            Vec::new(),
        ),
        Some(LanguageKind::Vue) => analyze_vue_file(content, relative, min_lines, min_chars),
        Some(language) => analyze_with_parser(
            content,
            relative,
            language,
            language.as_str().to_string(),
            0,
            min_lines,
            min_chars,
        ),
    }
}

fn analyze_with_parser(
    content: &str,
    relative: &Path,
    parser_language: LanguageKind,
    reported_language: String,
    line_offset: usize,
    min_lines: usize,
    min_chars: usize,
) -> (FileAnalysis, Vec<AstSpanRecord>) {
    let mut diagnostics = Vec::new();
    let mut parser = TsParser::new();
    if let Err(err) = parser.set_language(&parser_language.parser_language()) {
        diagnostics.push(AnalysisDiagnostic {
            level: "error".to_string(),
            message: format!("parser 初始化失败: {err}"),
        });
        return (
            FileAnalysis {
                parse_status: ParseStatus::ParseError,
                language: Some(reported_language),
                summary: FileSummary::default(),
                top_level_nodes: Vec::new(),
                template_nodes: Vec::new(),
                symbols: Vec::new(),
                imports: Vec::new(),
                exports: Vec::new(),
                calls: Vec::new(),
                diagnostics,
            },
            Vec::new(),
        );
    }

    let Some(tree) = parser.parse(content, None) else {
        diagnostics.push(AnalysisDiagnostic {
            level: "error".to_string(),
            message: "parser 未返回语法树".to_string(),
        });
        return (
            FileAnalysis {
                parse_status: ParseStatus::ParseError,
                language: Some(reported_language),
                summary: FileSummary::default(),
                top_level_nodes: Vec::new(),
                template_nodes: Vec::new(),
                symbols: Vec::new(),
                imports: Vec::new(),
                exports: Vec::new(),
                calls: Vec::new(),
                diagnostics,
            },
            Vec::new(),
        );
    };

    let root = tree.root_node();
    if root.has_error() {
        diagnostics.push(AnalysisDiagnostic {
            level: "warning".to_string(),
            message: "语法树包含 error 节点，结果可能不完整".to_string(),
        });
    }

    let mut top_level_nodes = Vec::new();
    let template_nodes = Vec::new();
    let mut symbols = Vec::new();
    let mut imports = Vec::new();
    let mut exports = Vec::new();
    let mut calls = Vec::new();
    let mut summary = FileSummary::default();
    let mut ast_spans = Vec::new();

    collect_file_facts(
        root,
        content.as_bytes(),
        relative,
        parser_language,
        min_lines,
        min_chars,
        0,
        &mut summary,
        &mut top_level_nodes,
        &mut symbols,
        &mut imports,
        &mut exports,
        &mut calls,
        &mut ast_spans,
    );

    if line_offset > 0 {
        offset_node_summaries(&mut top_level_nodes, line_offset);
        offset_symbols(&mut symbols, line_offset);
        offset_imports(&mut imports, line_offset);
        offset_exports(&mut exports, line_offset);
        offset_calls(&mut calls, line_offset);
        offset_ast_spans(&mut ast_spans, line_offset);
    }

    (
        FileAnalysis {
            parse_status: ParseStatus::Parsed,
            language: Some(reported_language),
            summary,
            top_level_nodes,
            template_nodes,
            symbols,
            imports,
            exports,
            calls,
            diagnostics,
        },
        ast_spans,
    )
}

fn analyze_vue_file(
    content: &str,
    relative: &Path,
    min_lines: usize,
    min_chars: usize,
) -> (FileAnalysis, Vec<AstSpanRecord>) {
    let mut diagnostics = Vec::new();
    let blocks = extract_vue_blocks(content);
    if blocks.is_empty() {
        diagnostics.push(AnalysisDiagnostic {
            level: "warning".to_string(),
            message: "未识别到任何 Vue SFC block，已作为空文件处理".to_string(),
        });
        return (
            FileAnalysis {
                parse_status: ParseStatus::ParseError,
                language: Some("vue".to_string()),
                summary: FileSummary::default(),
                top_level_nodes: Vec::new(),
                template_nodes: Vec::new(),
                symbols: Vec::new(),
                imports: Vec::new(),
                exports: Vec::new(),
                calls: Vec::new(),
                diagnostics,
            },
            Vec::new(),
        );
    }

    let mut summary = FileSummary::default();
    let mut top_level_nodes = Vec::new();
    let mut template_nodes = Vec::new();
    let mut symbols = Vec::new();
    let mut imports = Vec::new();
    let mut exports = Vec::new();
    let mut calls = Vec::new();
    let mut ast_spans = Vec::new();
    let mut parsed_any_script = false;

    for block in blocks {
        top_level_nodes.push(NodeSummary {
            kind: format!("vue_{}", block.tag),
            start_line: block.start_line,
            end_line: block.end_line,
        });

        match block.tag.as_str() {
            "script" => {
                let script_lang = match block.lang.as_deref() {
                    Some("ts") | Some("tsx") => LanguageKind::TypeScript,
                    Some("jsx") => LanguageKind::JavaScript,
                    _ => LanguageKind::JavaScript,
                };
                let (block_analysis, block_spans) = analyze_with_parser(
                    &block.content,
                    relative,
                    script_lang,
                    "vue".to_string(),
                    block.content_start_line.saturating_sub(1),
                    min_lines,
                    min_chars,
                );
                parsed_any_script = true;
                merge_summary(&mut summary, &block_analysis.summary);
                top_level_nodes.extend(block_analysis.top_level_nodes);
                symbols.extend(block_analysis.symbols);
                imports.extend(block_analysis.imports);
                exports.extend(block_analysis.exports);
                calls.extend(block_analysis.calls);
                diagnostics.extend(block_analysis.diagnostics);
                ast_spans.extend(block_spans);
            }
            "template" => {
                template_nodes.extend(analyze_vue_template_block(&block));
            }
            "style" => {
                diagnostics.push(AnalysisDiagnostic {
                    level: "info".to_string(),
                    message: format!(
                        "style block 已识别，当前版本不做样式 AST 提取 ({}-{})",
                        block.start_line, block.end_line
                    ),
                });
            }
            _ => {}
        }
    }

    if !parsed_any_script {
        diagnostics.push(AnalysisDiagnostic {
            level: "warning".to_string(),
            message: "Vue 文件未包含可解析的 script/script setup block".to_string(),
        });
    }

    (
        FileAnalysis {
            parse_status: ParseStatus::Parsed,
            language: Some("vue".to_string()),
            summary,
            top_level_nodes,
            template_nodes,
            symbols,
            imports,
            exports,
            calls,
            diagnostics,
        },
        ast_spans,
    )
}

#[allow(clippy::too_many_arguments)]
fn collect_file_facts(
    node: Node<'_>,
    source: &[u8],
    relative: &Path,
    language: LanguageKind,
    min_lines: usize,
    min_chars: usize,
    depth: usize,
    summary: &mut FileSummary,
    top_level_nodes: &mut Vec<NodeSummary>,
    symbols: &mut Vec<SymbolFact>,
    imports: &mut Vec<ImportFact>,
    exports: &mut Vec<ExportFact>,
    calls: &mut Vec<CallFact>,
    ast_spans: &mut Vec<AstSpanRecord>,
) {
    if node.is_named() {
        summary.total_named_nodes += 1;
        summary.max_depth = summary.max_depth.max(depth);
    }

    if is_candidate_node(node) {
        let line_count = node
            .end_position()
            .row
            .saturating_sub(node.start_position().row)
            + 1;
        let char_count = node.end_byte().saturating_sub(node.start_byte());
        if line_count >= min_lines && char_count >= min_chars {
            ast_spans.push(build_ast_span(node, source, relative, language, line_count));
        }
    }

    if node
        .parent()
        .map(|parent| parent.parent().is_none())
        .unwrap_or(false)
        && node.is_named()
    {
        top_level_nodes.push(NodeSummary {
            kind: node.kind().to_string(),
            start_line: node.start_position().row + 1,
            end_line: node.end_position().row + 1,
        });
    }

    if let Some(symbol) = extract_symbol(node, source) {
        symbols.push(symbol);
    }
    if let Some(import) = extract_import(node, source) {
        imports.push(import);
    }
    if let Some(export) = extract_export(node, source) {
        exports.push(export);
    }
    if let Some(call) = extract_call(node, source) {
        calls.push(call);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_file_facts(
            child,
            source,
            relative,
            language,
            min_lines,
            min_chars,
            depth + 1,
            summary,
            top_level_nodes,
            symbols,
            imports,
            exports,
            calls,
            ast_spans,
        );
    }
}

fn extract_symbol(node: Node<'_>, source: &[u8]) -> Option<SymbolFact> {
    let kind = node.kind();
    if !matches_symbol_kind(kind) {
        return None;
    }

    Some(SymbolFact {
        name: extract_name(node, source).unwrap_or_else(|| "<anonymous>".to_string()),
        kind: classify_symbol_kind(kind),
        visibility: classify_visibility(node, source),
        span: span_of(node),
    })
}

fn extract_import(node: Node<'_>, source: &[u8]) -> Option<ImportFact> {
    let kind = node.kind();
    if !matches!(
        kind,
        "use_declaration"
            | "import_statement"
            | "import_declaration"
            | "import_from_statement"
            | "namespace_import"
    ) {
        return None;
    }

    let names = collect_named_descendants(node, source, |child| {
        let child_kind = child.kind();
        child_kind.contains("identifier")
            || child_kind == "namespace_import"
            || child_kind == "import_specifier"
    });

    Some(ImportFact {
        source: extract_string_like(node, source),
        imported_names: dedup_vec(names),
        span: span_of(node),
    })
}

fn extract_export(node: Node<'_>, source: &[u8]) -> Option<ExportFact> {
    let kind = node.kind();
    if !(kind.contains("export")
        || kind == "public_field_definition"
        || kind == "visibility_modifier")
    {
        return None;
    }

    Some(ExportFact {
        name: extract_name(node, source),
        kind: kind.to_string(),
        span: span_of(node),
    })
}

fn extract_call(node: Node<'_>, source: &[u8]) -> Option<CallFact> {
    let kind = node.kind();
    if !matches!(kind, "call_expression" | "call" | "invocation_expression") {
        return None;
    }

    Some(CallFact {
        callee: extract_callee_name(node, source),
        kind: kind.to_string(),
        span: span_of(node),
    })
}

fn matches_symbol_kind(kind: &str) -> bool {
    matches!(
        kind,
        "function_item"
            | "function_declaration"
            | "function_definition"
            | "function"
            | "method_definition"
            | "method_declaration"
            | "generator_function_declaration"
            | "arrow_function"
            | "class_declaration"
            | "class_definition"
            | "struct_item"
            | "enum_item"
            | "trait_item"
            | "impl_item"
            | "interface_declaration"
            | "type_alias_declaration"
            | "lexical_declaration"
            | "variable_declaration"
            | "const_item"
            | "static_item"
    )
}

fn classify_symbol_kind(kind: &str) -> String {
    if kind.contains("function") || kind.contains("method") || kind == "arrow_function" {
        "function".to_string()
    } else if kind.contains("class") {
        "class".to_string()
    } else if kind.contains("struct") {
        "struct".to_string()
    } else if kind.contains("enum") {
        "enum".to_string()
    } else if kind.contains("trait") || kind.contains("interface") {
        "interface".to_string()
    } else if kind.contains("impl") {
        "impl".to_string()
    } else if kind.contains("type_alias") {
        "type_alias".to_string()
    } else if kind.contains("const") || kind.contains("static") {
        "constant".to_string()
    } else if kind.contains("declaration") {
        "declaration".to_string()
    } else {
        kind.to_string()
    }
}

fn classify_visibility(node: Node<'_>, source: &[u8]) -> String {
    let text = node.utf8_text(source).unwrap_or("");
    if text.contains("pub ") || text.contains("public ") || text.starts_with("export ") {
        "public".to_string()
    } else {
        "local".to_string()
    }
}

fn extract_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    if let Some(by_field) = node.child_by_field_name("name") {
        return node_text(by_field, source);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();
        if kind.contains("identifier") || kind == "type_identifier" || kind == "property_identifier"
        {
            if let Some(name) = node_text(child, source) {
                return Some(name);
            }
        }
    }
    None
}

fn extract_string_like(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();
        if kind.contains("string")
            || kind == "interpreted_string_literal"
            || kind == "raw_string_literal"
        {
            return node_text(child, source).map(|text| strip_quotes(&text));
        }
    }
    None
}

fn extract_callee_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    if let Some(function) = node.child_by_field_name("function") {
        return node_text(function, source);
    }
    if let Some(callee) = node.child_by_field_name("callee") {
        return node_text(callee, source);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();
        if kind.contains("identifier")
            || kind == "member_expression"
            || kind == "field_expression"
            || kind == "selector_expression"
        {
            return node_text(child, source);
        }
    }
    None
}

fn node_text(node: Node<'_>, source: &[u8]) -> Option<String> {
    let text = node.utf8_text(source).ok()?.trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn collect_named_descendants<F>(node: Node<'_>, source: &[u8], matcher: F) -> Vec<String>
where
    F: Fn(Node<'_>) -> bool + Copy,
{
    let mut out = Vec::new();
    collect_named_descendants_inner(node, source, matcher, &mut out);
    out
}

fn collect_named_descendants_inner<F>(
    node: Node<'_>,
    source: &[u8],
    matcher: F,
    out: &mut Vec<String>,
) where
    F: Fn(Node<'_>) -> bool + Copy,
{
    if matcher(node) {
        if let Some(text) = node_text(node, source) {
            out.push(text);
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_named_descendants_inner(child, source, matcher, out);
    }
}

fn merge_summary(target: &mut FileSummary, source: &FileSummary) {
    target.total_named_nodes += source.total_named_nodes;
    target.max_depth = target.max_depth.max(source.max_depth);
}

fn offset_node_summaries(items: &mut [NodeSummary], line_offset: usize) {
    for item in items {
        item.start_line += line_offset;
        item.end_line += line_offset;
    }
}

fn offset_symbols(items: &mut [SymbolFact], line_offset: usize) {
    for item in items {
        item.span.start_line += line_offset;
        item.span.end_line += line_offset;
    }
}

fn offset_imports(items: &mut [ImportFact], line_offset: usize) {
    for item in items {
        item.span.start_line += line_offset;
        item.span.end_line += line_offset;
    }
}

fn offset_exports(items: &mut [ExportFact], line_offset: usize) {
    for item in items {
        item.span.start_line += line_offset;
        item.span.end_line += line_offset;
    }
}

fn offset_calls(items: &mut [CallFact], line_offset: usize) {
    for item in items {
        item.span.start_line += line_offset;
        item.span.end_line += line_offset;
    }
}

fn offset_ast_spans(items: &mut [AstSpanRecord], line_offset: usize) {
    for item in items {
        item.start_line += line_offset;
        item.end_line += line_offset;
    }
}

fn extract_vue_blocks(content: &str) -> Vec<VueBlock> {
    let mut blocks = Vec::new();
    let lines = content.lines().collect::<Vec<_>>();
    let mut idx = 0usize;

    while idx < lines.len() {
        let trimmed = lines[idx].trim();
        if let Some((tag, lang)) = parse_vue_open_tag(trimmed) {
            let start_line = idx + 1;
            let mut content_lines = Vec::new();
            let mut end_line = start_line;
            idx += 1;

            while idx < lines.len() {
                let line = lines[idx];
                if line.trim().starts_with(&format!("</{tag}>")) {
                    end_line = idx + 1;
                    break;
                }
                content_lines.push(line);
                idx += 1;
            }

            blocks.push(VueBlock {
                tag,
                lang,
                start_line,
                end_line,
                content_start_line: start_line + 1,
                content: content_lines.join("\n"),
            });
        }
        idx += 1;
    }

    blocks
}

fn analyze_vue_template_block(block: &VueBlock) -> Vec<TemplateNodeFact> {
    let mut nodes = Vec::new();
    let mut stack: Vec<String> = Vec::new();

    for (idx, raw_line) in block.content.lines().enumerate() {
        let line_no = block.content_start_line + idx;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let interpolations = extract_interpolations(raw_line);
        for expr in interpolations {
            nodes.push(TemplateNodeFact {
                kind: "interpolation".to_string(),
                name: None,
                directives: Vec::new(),
                attributes: Vec::new(),
                expression: Some(expr),
                depth: stack.len(),
                span: Span {
                    start_line: line_no,
                    end_line: line_no,
                },
            });
        }

        let mut cursor = 0usize;
        while let Some(start) = raw_line[cursor..].find('<') {
            let start_idx = cursor + start;
            let rest = &raw_line[start_idx..];
            let Some(end_rel) = rest.find('>') else {
                break;
            };
            let tag_text = &rest[..=end_rel];
            cursor = start_idx + end_rel + 1;

            if tag_text.starts_with("</") {
                if !stack.is_empty() {
                    stack.pop();
                }
                continue;
            }
            if tag_text.starts_with("<!--") || tag_text.starts_with("<!") {
                continue;
            }

            if let Some(tag) = parse_template_tag(tag_text) {
                let depth = stack.len();
                let node_kind = if is_component_name(&tag.name) {
                    "component"
                } else {
                    "element"
                };
                nodes.push(TemplateNodeFact {
                    kind: node_kind.to_string(),
                    name: Some(tag.name.clone()),
                    directives: tag.directives,
                    attributes: tag.attributes,
                    expression: None,
                    depth,
                    span: Span {
                        start_line: line_no,
                        end_line: line_no,
                    },
                });
                if !tag.self_closing {
                    stack.push(tag.name);
                }
            }
        }
    }

    nodes
}

#[derive(Debug)]
struct ParsedTemplateTag {
    name: String,
    directives: Vec<String>,
    attributes: Vec<String>,
    self_closing: bool,
}

fn parse_template_tag(tag_text: &str) -> Option<ParsedTemplateTag> {
    let inner = tag_text
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim_end_matches('/')
        .trim();
    if inner.is_empty() {
        return None;
    }

    let mut parts = inner.split_whitespace();
    let name = parts.next()?.trim_matches('/').to_string();
    if name.is_empty() {
        return None;
    }

    let mut directives = Vec::new();
    let mut attributes = Vec::new();
    for token in parts {
        let attr = token
            .trim_end_matches('/')
            .split('=')
            .next()
            .unwrap_or("")
            .trim();
        if attr.is_empty() {
            continue;
        }
        if is_template_directive(attr) {
            directives.push(attr.to_string());
        } else {
            attributes.push(attr.to_string());
        }
    }

    Some(ParsedTemplateTag {
        name,
        directives,
        attributes,
        self_closing: tag_text.trim_end().ends_with("/>"),
    })
}

fn is_component_name(name: &str) -> bool {
    name.contains('-')
        || name
            .chars()
            .next()
            .map(|ch| ch.is_ascii_uppercase())
            .unwrap_or(false)
}

fn is_template_directive(attr: &str) -> bool {
    attr.starts_with("v-")
        || attr.starts_with(':')
        || attr.starts_with('@')
        || attr.starts_with('#')
}

fn extract_interpolations(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(start_rel) = line[cursor..].find("{{") {
        let start = cursor + start_rel + 2;
        let Some(end_rel) = line[start..].find("}}") else {
            break;
        };
        let end = start + end_rel;
        let expr = line[start..end].trim();
        if !expr.is_empty() {
            out.push(expr.to_string());
        }
        cursor = end + 2;
    }
    out
}

fn parse_vue_open_tag(line: &str) -> Option<(String, Option<String>)> {
    let tag = if line.starts_with("<template") {
        "template"
    } else if line.starts_with("<script") {
        "script"
    } else if line.starts_with("<style") {
        "style"
    } else {
        return None;
    };

    Some((tag.to_string(), extract_lang_attr(line)))
}

fn extract_lang_attr(line: &str) -> Option<String> {
    let marker = "lang=";
    let start = line.find(marker)? + marker.len();
    let rest = &line[start..];
    let mut chars = rest.chars();
    let quote = chars.next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }

    let value = chars.take_while(|ch| *ch != quote).collect::<String>();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn span_of(node: Node<'_>) -> Span {
    Span {
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
    }
}

fn is_candidate_node(node: Node<'_>) -> bool {
    if !node.is_named() || node.parent().is_none() {
        return false;
    }

    !matches!(
        node.kind(),
        "source_file" | "program" | "module" | "script" | "chunk" | "statement_block" | "block"
    )
}

fn build_ast_span(
    node: Node<'_>,
    source: &[u8],
    relative: &Path,
    language: LanguageKind,
    line_count: usize,
) -> AstSpanRecord {
    let structural = collect_structural_kinds(node);
    let token_stream = collect_token_classes(node, source);
    let preview = node
        .utf8_text(source)
        .unwrap_or("")
        .lines()
        .map(normalize_line)
        .filter(|line| !line.is_empty())
        .take(MAX_PREVIEW_LINES)
        .collect::<Vec<_>>();

    AstSpanRecord {
        path: relative.display().to_string(),
        language: language.as_str().to_string(),
        node_kind: node.kind().to_string(),
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        line_count,
        structural_hash: hash_string(&structural.join("|")),
        token_hash: hash_string(&token_stream.join("|")),
        structural_kinds: structural,
        preview,
    }
}

fn collect_structural_kinds(node: Node<'_>) -> Vec<String> {
    let mut out = Vec::new();
    collect_structural_kinds_inner(node, &mut out);
    out
}

fn collect_structural_kinds_inner(node: Node<'_>, out: &mut Vec<String>) {
    if !node.is_named() {
        return;
    }

    out.push(node.kind().to_string());
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_structural_kinds_inner(child, out);
    }
}

fn collect_token_classes(node: Node<'_>, source: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    collect_token_classes_inner(node, source, &mut out);
    out
}

fn collect_token_classes_inner(node: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    if !node.is_named() {
        return;
    }

    if node.child_count() == 0 {
        let text = node.utf8_text(source).unwrap_or("").trim();
        if !text.is_empty() {
            out.push(classify_leaf(node.kind(), text));
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_token_classes_inner(child, source, out);
    }
}

fn classify_leaf(kind: &str, text: &str) -> String {
    let lowered = kind.to_ascii_lowercase();
    if lowered.contains("identifier")
        || lowered == "property_identifier"
        || lowered == "field_identifier"
    {
        "ID".to_string()
    } else if lowered.contains("string") {
        "STR".to_string()
    } else if lowered.contains("number") || text.chars().all(|ch| ch.is_ascii_digit()) {
        "NUM".to_string()
    } else if lowered.contains("comment") {
        "COMMENT".to_string()
    } else {
        kind.to_string()
    }
}

fn to_file_fact(file: &FileRecord) -> FileFact {
    FileFact {
        path: file.path.display().to_string(),
        language: file.analysis.language.clone(),
        parse_status: file.analysis.parse_status.as_str().to_string(),
        content_hash: file.content_hash.clone(),
        summary: FileSummaryFact {
            total_named_nodes: file.analysis.summary.total_named_nodes,
            max_depth: file.analysis.summary.max_depth,
            template_node_count: file.analysis.template_nodes.len(),
            symbol_count: file.analysis.symbols.len(),
            import_count: file.analysis.imports.len(),
            export_count: file.analysis.exports.len(),
            call_count: file.analysis.calls.len(),
        },
        top_level_nodes: file.analysis.top_level_nodes.clone(),
        template_nodes: file.analysis.template_nodes.clone(),
        symbols: file.analysis.symbols.clone(),
        imports: file.analysis.imports.clone(),
        exports: file.analysis.exports.clone(),
        calls: file.analysis.calls.clone(),
        diagnostics: file.analysis.diagnostics.clone(),
    }
}

fn find_exact_duplicate_files(files: &[FileRecord]) -> Vec<ExactFileGroup> {
    let mut groups: HashMap<&str, Vec<String>> = HashMap::new();
    for file in files {
        groups
            .entry(&file.content_hash)
            .or_default()
            .push(file.path.display().to_string());
    }

    let mut out = groups
        .into_iter()
        .filter_map(|(content_hash, paths)| {
            if paths.len() < 2 {
                None
            } else {
                Some(ExactFileGroup {
                    content_hash: content_hash.to_string(),
                    paths: sorted(paths),
                })
            }
        })
        .collect::<Vec<_>>();

    out.sort_by(|a, b| b.paths.len().cmp(&a.paths.len()));
    out.truncate(MAX_DERIVED_ITEMS);
    out
}

fn find_clone_candidates(
    files: &[FileRecord],
    min_lines: usize,
    min_chars: usize,
) -> Vec<CloneCandidate> {
    let mut out = find_ast_clone_candidates(files);
    out.extend(find_text_clone_candidates(files, min_lines, min_chars));
    out.sort_by(|a, b| {
        b.occurrence_count
            .cmp(&a.occurrence_count)
            .then_with(|| b.scores.confidence.total_cmp(&a.scores.confidence))
    });
    out.truncate(MAX_DERIVED_ITEMS);
    out
}

fn find_ast_clone_candidates(files: &[FileRecord]) -> Vec<CloneCandidate> {
    let mut groups: HashMap<(String, String), Vec<&AstSpanRecord>> = HashMap::new();
    for file in files {
        for span in &file.ast_spans {
            groups
                .entry((span.language.clone(), span.structural_hash.clone()))
                .or_default()
                .push(span);
        }
    }

    let mut out = Vec::new();
    for ((language, structural_hash), spans) in groups {
        if spans.len() < 2 {
            continue;
        }
        let distinct_paths = spans
            .iter()
            .map(|span| span.path.as_str())
            .collect::<HashSet<_>>();
        if distinct_paths.len() < 2 {
            continue;
        }

        let representative = spans[0];
        let token_matches = spans
            .iter()
            .filter(|span| span.token_hash == representative.token_hash)
            .count();
        let token_similarity = token_matches as f64 / spans.len() as f64;
        let confidence = 0.65
            + 0.25 * token_similarity
            + 0.10 * normalized_size_score(representative.line_count);

        out.push(CloneCandidate {
            candidate_kind: "structural_clone".to_string(),
            match_basis: "ast_shape".to_string(),
            language,
            occurrence_count: spans.len(),
            scores: MatchScores {
                confidence: confidence.min(0.99),
                structural_similarity: 1.0,
                token_similarity,
                text_similarity: token_similarity,
            },
            fingerprint: FingerprintEvidence {
                structural_hash,
                token_hash: representative.token_hash.clone(),
                sample_node_kinds: representative
                    .structural_kinds
                    .iter()
                    .take(MAX_SUMMARY_NODES)
                    .cloned()
                    .collect(),
            },
            occurrences: spans
                .iter()
                .map(|span| SourceSpan {
                    path: span.path.clone(),
                    start_line: span.start_line,
                    end_line: span.end_line,
                    node_kind: Some(span.node_kind.clone()),
                })
                .collect(),
            preview: representative.preview.clone(),
        });
    }

    out
}

fn find_text_clone_candidates(
    files: &[FileRecord],
    min_lines: usize,
    min_chars: usize,
) -> Vec<CloneCandidate> {
    let mut windows: HashMap<String, Vec<SourceSpan>> = HashMap::new();
    let mut previews: HashMap<String, Vec<String>> = HashMap::new();

    for file in files {
        if matches!(file.analysis.parse_status, ParseStatus::Parsed) {
            continue;
        }
        if file.normalized_lines.len() < min_lines {
            continue;
        }

        for start in 0..=file.normalized_lines.len() - min_lines {
            let snippet = file.normalized_lines[start..start + min_lines].join("\n");
            if snippet.len() < min_chars {
                continue;
            }

            let hash = hash_string(&snippet);
            windows.entry(hash.clone()).or_default().push(SourceSpan {
                path: file.path.display().to_string(),
                start_line: start + 1,
                end_line: start + min_lines,
                node_kind: None,
            });
            previews.entry(hash).or_insert_with(|| {
                file.normalized_lines[start..start + min_lines]
                    .iter()
                    .take(MAX_PREVIEW_LINES)
                    .cloned()
                    .collect()
            });
        }
    }

    let mut out = Vec::new();
    for (token_hash, spans) in windows {
        if spans.len() < 2 {
            continue;
        }
        let distinct_paths = spans
            .iter()
            .map(|span| span.path.as_str())
            .collect::<HashSet<_>>();
        if distinct_paths.len() < 2 {
            continue;
        }

        out.push(CloneCandidate {
            candidate_kind: "fallback_clone".to_string(),
            match_basis: "normalized_text_window".to_string(),
            language: "fallback".to_string(),
            occurrence_count: spans.len(),
            scores: MatchScores {
                confidence: 0.55,
                structural_similarity: 0.0,
                token_similarity: 0.0,
                text_similarity: 1.0,
            },
            fingerprint: FingerprintEvidence {
                structural_hash: "n/a".to_string(),
                token_hash: token_hash.clone(),
                sample_node_kinds: Vec::new(),
            },
            occurrences: spans,
            preview: previews.remove(&token_hash).unwrap_or_default(),
        });
    }

    out
}

fn render_markdown(report: &CodebaseAnalysisReport) -> String {
    let mut out = String::new();
    out.push_str("# Scalpel 代码库 AST 报告\n\n");
    out.push_str(&format!("- 扫描根目录: `{}`\n", report.root));
    out.push_str(&format!("- 扫描文件数: `{}`\n", report.scanned_files));
    out.push_str(&format!("- AST 成功解析: `{}`\n", report.parsed_files));
    out.push_str(&format!("- AST 未支持: `{}`\n", report.unsupported_files));
    out.push_str(&format!(
        "- AST 解析失败: `{}`\n\n",
        report.parse_error_files
    ));

    out.push_str("## 文件事实\n\n");
    if report.files.is_empty() {
        out.push_str("未发现可分析文件。\n\n");
    } else {
        for file in &report.files {
            out.push_str(&format!(
                "- `{}`: language=`{}` status=`{}` template_nodes=`{}` symbols=`{}` imports=`{}` exports=`{}` calls=`{}` nodes=`{}`\n",
                file.path,
                file.language.as_deref().unwrap_or("unknown"),
                file.parse_status,
                file.summary.template_node_count,
                file.summary.symbol_count,
                file.summary.import_count,
                file.summary.export_count,
                file.summary.call_count,
                file.summary.total_named_nodes,
            ));
            if !file.top_level_nodes.is_empty() {
                out.push_str("  - 顶层节点:\n");
                for node in file.top_level_nodes.iter().take(6) {
                    out.push_str(&format!(
                        "    - `{}`:{}-{}\n",
                        node.kind, node.start_line, node.end_line
                    ));
                }
            }
            if !file.symbols.is_empty() {
                out.push_str("  - 符号样本:\n");
                for symbol in file.symbols.iter().take(6) {
                    out.push_str(&format!(
                        "    - `{}` `{}` {}-{}\n",
                        symbol.kind, symbol.name, symbol.span.start_line, symbol.span.end_line
                    ));
                }
            }
            if !file.template_nodes.is_empty() {
                out.push_str("  - Template 节点样本:\n");
                for node in file.template_nodes.iter().take(6) {
                    out.push_str(&format!(
                        "    - `{}` name=`{}` depth=`{}` directives=`{}` expr=`{}` {}-{}\n",
                        node.kind,
                        node.name.as_deref().unwrap_or("-"),
                        node.depth,
                        node.directives.join(", "),
                        node.expression.as_deref().unwrap_or("-"),
                        node.span.start_line,
                        node.span.end_line
                    ));
                }
            }
            if !file.imports.is_empty() {
                out.push_str("  - 导入样本:\n");
                for import in file.imports.iter().take(4) {
                    out.push_str(&format!(
                        "    - source=`{}` names=`{}`\n",
                        import.source.as_deref().unwrap_or("unknown"),
                        import.imported_names.join(", ")
                    ));
                }
            }
            if !file.calls.is_empty() {
                out.push_str("  - 调用样本:\n");
                for call in file.calls.iter().take(4) {
                    out.push_str(&format!(
                        "    - `{}` {}-{}\n",
                        call.callee.as_deref().unwrap_or("<unknown>"),
                        call.span.start_line,
                        call.span.end_line
                    ));
                }
            }
            if !file.diagnostics.is_empty() {
                out.push_str("  - 诊断:\n");
                for diagnostic in &file.diagnostics {
                    out.push_str(&format!(
                        "    - [{}] {}\n",
                        diagnostic.level, diagnostic.message
                    ));
                }
            }
        }
        out.push('\n');
    }

    out.push_str("## 派生视图\n\n");
    if report.derived.exact_duplicate_files.is_empty() {
        out.push_str("- 完全重复文件: 无\n");
    } else {
        out.push_str("- 完全重复文件:\n");
        for group in &report.derived.exact_duplicate_files {
            out.push_str(&format!(
                "  - `{}` -> {}\n",
                shorten_hash(&group.content_hash),
                group.paths.join(", ")
            ));
        }
    }

    if report.derived.clone_candidates.is_empty() {
        out.push_str("- 重复候选: 无\n\n");
    } else {
        out.push_str("- 重复候选:\n");
        for candidate in report.derived.clone_candidates.iter().take(8) {
            out.push_str(&format!(
                "  - `{}` `{}` occurrences=`{}` confidence=`{:.2}`\n",
                candidate.language,
                candidate.match_basis,
                candidate.occurrence_count,
                candidate.scores.confidence
            ));
        }
        out.push('\n');
    }

    out.push_str("## 说明\n\n");
    for note in &report.notes {
        out.push_str(&format!("- {}\n", note));
    }

    out
}

fn build_notes(
    parsed_files: usize,
    unsupported_files: usize,
    parse_error_files: usize,
    derived: &DerivedViews,
) -> Vec<String> {
    let mut notes = Vec::new();
    notes.push(format!(
        "工具当前以 AST 事实采集为主，重复候选仅作为 derived 视图附带输出。"
    ));
    notes.push(format!(
        "已解析 {} 个文件，未支持 {} 个文件，解析失败 {} 个文件。",
        parsed_files, unsupported_files, parse_error_files
    ));
    notes.push(format!(
        "当前 derived 中包含 {} 组完全重复文件、{} 组重复候选。",
        derived.exact_duplicate_files.len(),
        derived.clone_candidates.len()
    ));
    notes
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

fn strip_quotes(input: &str) -> String {
    input
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches('`')
        .to_string()
}

fn hash_string(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn shorten_hash(hash: &str) -> &str {
    if hash.len() > 12 {
        &hash[..12]
    } else {
        hash
    }
}

fn dedup_vec(values: Vec<String>) -> Vec<String> {
    let mut set = HashSet::new();
    let mut out = Vec::new();
    for value in values {
        if set.insert(value.clone()) {
            out.push(value);
        }
    }
    out
}

fn normalized_size_score(line_count: usize) -> f64 {
    (line_count.min(30) as f64) / 30.0
}

fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}
