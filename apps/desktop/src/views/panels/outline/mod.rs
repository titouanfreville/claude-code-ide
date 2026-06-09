//! Code-structure (outline) extraction for the Structure panel.
//!
//! A provider chain with graceful degradation — each tier refines the one below:
//!
//! 1. **LSP** (`textDocument/documentSymbol`) — richest, when a server is connected
//!    for the file's language. Async, so the panel overlays it when it arrives; this
//!    module computes the synchronous baseline the panel shows immediately.
//! 2. **Tree-sitter** ([`treesitter`]) — the universal baseline: a real parse of the
//!    buffer for every bundled grammar (priority languages have curated rules).
//! 3. **Heuristic** ([`heuristic`]) — today's dependency-free keyword scan, the floor
//!    for languages without a grammar rule set.
//!
//! Symbols are **hierarchical** (methods nest under their class/impl) so the panel can
//! render a collapsible tree. All extraction is pure and unit-testable; the panel owns
//! the gpui rendering.

mod heuristic;
mod treesitter;

use std::path::Path;

/// The kind of a declaration, for the outline's icon/label and grouping.
///
/// Some variants (`Field`, `Variable`, `Other`) aren't produced by the tree-sitter /
/// heuristic tiers yet — they round out the mapping from LSP `documentSymbol` kinds
/// added by the LSP tier (Stage 2), so the model is complete up front.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Trait,
    Impl,
    Class,
    Interface,
    Module,
    Type,
    Constant,
    Field,
    Macro,
    Variable,
    Other,
}

impl SymbolKind {
    /// Short lowercase tag shown next to the symbol name (e.g. `fn`, `struct`).
    pub fn label(self) -> &'static str {
        match self {
            SymbolKind::Function => "fn",
            SymbolKind::Method => "method",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Trait => "trait",
            SymbolKind::Impl => "impl",
            SymbolKind::Class => "class",
            SymbolKind::Interface => "interface",
            SymbolKind::Module => "mod",
            SymbolKind::Type => "type",
            SymbolKind::Constant => "const",
            SymbolKind::Field => "field",
            SymbolKind::Macro => "macro",
            SymbolKind::Variable => "var",
            SymbolKind::Other => "·",
        }
    }
}

/// One outline entry. `line` is 0-based (for the editor cursor jump); `children` are
/// the symbols syntactically nested inside this one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub line: usize,
    pub children: Vec<Symbol>,
}

impl Symbol {
    pub fn leaf(name: impl Into<String>, kind: SymbolKind, line: usize) -> Self {
        Self {
            name: name.into(),
            kind,
            line,
            children: Vec::new(),
        }
    }
}

/// The languages the outline providers understand. Drives both the tree-sitter rule
/// set and the heuristic keyword tables; unknown extensions are [`OutlineLang::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutlineLang {
    Rust,
    Go,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    C,
    Cpp,
    Java,
    Other,
}

/// Map a path's extension to an [`OutlineLang`]. Mirrors the editor's `lang_from_ext`
/// for the languages we extract structure for.
pub fn outline_lang(path: &Path) -> OutlineLang {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" => OutlineLang::Rust,
        "go" => OutlineLang::Go,
        "py" | "pyi" => OutlineLang::Python,
        "js" | "jsx" | "mjs" | "cjs" => OutlineLang::JavaScript,
        "ts" | "mts" | "cts" => OutlineLang::TypeScript,
        "tsx" => OutlineLang::Tsx,
        "c" | "h" => OutlineLang::C,
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => OutlineLang::Cpp,
        "java" => OutlineLang::Java,
        _ => OutlineLang::Other,
    }
}

/// The synchronous outline baseline for `text`: tree-sitter when the language has a
/// grammar rule set, else the keyword heuristic. (The LSP tier, when present, is
/// overlaid by the panel — see the module docs.)
pub fn symbols(lang: OutlineLang, text: &str) -> Vec<Symbol> {
    treesitter::symbols(lang, text).unwrap_or_else(|| heuristic::symbols(lang, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_language_falls_back_to_empty_heuristic() {
        assert!(symbols(OutlineLang::Other, "whatever\n").is_empty());
    }

    #[test]
    fn rust_uses_tree_sitter_and_nests_methods_under_impl() {
        let src = "struct Foo;\nimpl Foo {\n    fn bar(&self) {}\n}\n";
        let syms = symbols(OutlineLang::Rust, src);
        // Two top-level symbols: the struct and the impl; bar nests under the impl.
        let impl_sym = syms.iter().find(|s| s.kind == SymbolKind::Impl).unwrap();
        assert_eq!(impl_sym.name, "Foo");
        assert_eq!(impl_sym.children.len(), 1);
        assert_eq!(impl_sym.children[0].name, "bar");
        assert_eq!(impl_sym.children[0].kind, SymbolKind::Method);
    }
}
