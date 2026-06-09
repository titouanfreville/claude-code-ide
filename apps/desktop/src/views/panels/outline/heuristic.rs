//! Dependency-free keyword-scan outline — the floor of the provider chain.
//!
//! Used for files with no tree-sitter rule set (the ~26 non-priority grammars, and
//! truly unknown types). It is a **generic, language-agnostic** scanner: it matches a
//! leading declaration keyword (`fn`/`func`/`function`/`def`/`class`/`struct`/…) after
//! stripping visibility/modifier prefixes, and nests by indentation. Good enough for
//! navigation in Ruby/PHP/Swift/Kotlin/etc.; a tree-sitter rule set is the accurate
//! upgrade per language.

use super::{OutlineLang, Symbol, SymbolKind};

/// Leading modifier/visibility words stripped before keyword matching.
const PREFIXES: &[&str] = &[
    "pub ",
    "pub(crate) ",
    "pub(super) ",
    "public ",
    "private ",
    "protected ",
    "internal ",
    "export ",
    "default ",
    "static ",
    "async ",
    "abstract ",
    "final ",
    "open ",
    "override ",
    "suspend ",
    "unsafe ",
    "const ",
    "inline ",
    "virtual ",
    "data ",
];

/// Declaration keyword → kind. Order matters only for keywords that share a prefix
/// (none do here, since each ends in a space).
const KEYWORDS: &[(&str, SymbolKind)] = &[
    ("fn ", SymbolKind::Function),
    ("fun ", SymbolKind::Function),
    ("func ", SymbolKind::Function),
    ("function ", SymbolKind::Function),
    ("def ", SymbolKind::Function),
    ("sub ", SymbolKind::Function),
    ("class ", SymbolKind::Class),
    ("object ", SymbolKind::Class),
    ("struct ", SymbolKind::Struct),
    ("interface ", SymbolKind::Interface),
    ("protocol ", SymbolKind::Interface),
    ("trait ", SymbolKind::Trait),
    ("impl ", SymbolKind::Impl),
    ("extension ", SymbolKind::Impl),
    ("enum ", SymbolKind::Enum),
    ("module ", SymbolKind::Module),
    ("namespace ", SymbolKind::Module),
    ("mod ", SymbolKind::Module),
    ("package ", SymbolKind::Module),
    ("type ", SymbolKind::Type),
    ("typedef ", SymbolKind::Type),
];

/// Extract a hierarchical outline by keyword scan. `_lang` is currently unused (the
/// scan is generic) but kept so a future per-language table can specialize.
pub fn symbols(_lang: OutlineLang, text: &str) -> Vec<Symbol> {
    let flat: Vec<(usize, usize, SymbolKind, String)> = text
        .lines()
        .enumerate()
        .filter_map(|(line, raw)| {
            let (kind, name) = match_decl(raw.trim_start())?;
            Some((line, indent_cols(raw), kind, name))
        })
        .collect();
    nest(flat)
}

/// Raw leading-indent width in columns (a tab counts as 4). Nesting compares these
/// **relatively** (deeper-indented = nested), so any indent unit — 2-space, 4-space,
/// or tabs — works without assuming a fixed step.
fn indent_cols(line: &str) -> usize {
    let mut cols = 0usize;
    for ch in line.chars() {
        match ch {
            ' ' => cols += 1,
            '\t' => cols += 4,
            _ => break,
        }
    }
    cols
}

/// The identifier following a keyword: chars up to a delimiter.
fn ident(rest: &str) -> String {
    rest.trim()
        .chars()
        .take_while(|c| !matches!(c, '(' | '<' | '{' | ' ' | '\t' | ':' | ';' | '=' | ','))
        .collect()
}

/// Match a trimmed line against [`KEYWORDS`] after stripping [`PREFIXES`]. For `impl`,
/// the remainder is kept (e.g. `Foo for Bar`); otherwise the first identifier.
fn match_decl(trimmed: &str) -> Option<(SymbolKind, String)> {
    let mut s = trimmed;
    loop {
        let mut stripped = false;
        for p in PREFIXES {
            if let Some(rest) = s.strip_prefix(p) {
                s = rest;
                stripped = true;
            }
        }
        if !stripped {
            break;
        }
    }
    for (kw, kind) in KEYWORDS {
        if let Some(rest) = s.strip_prefix(kw) {
            let name = if *kind == SymbolKind::Impl {
                rest.trim().trim_end_matches('{').trim().to_string()
            } else {
                ident(rest)
            };
            if !name.is_empty() {
                return Some((*kind, name));
            }
        }
    }
    None
}

/// Build a tree from the source-ordered, depth-tagged flat list via an indentation
/// stack. A symbol's children are the deeper-indented symbols that follow it.
fn nest(flat: Vec<(usize, usize, SymbolKind, String)>) -> Vec<Symbol> {
    let mut roots: Vec<Symbol> = Vec::new();
    let mut stack: Vec<Symbol> = Vec::new();
    let mut depths: Vec<usize> = Vec::new();

    for (line, depth, kind, name) in flat {
        while depths.last().is_some_and(|&d| d >= depth) {
            let sym = stack.pop().unwrap();
            depths.pop();
            push_into(&mut stack, &mut roots, sym);
        }
        stack.push(Symbol::leaf(name, kind, line));
        depths.push(depth);
    }
    while let Some(sym) = stack.pop() {
        depths.pop();
        push_into(&mut stack, &mut roots, sym);
    }
    roots
}

/// Attach a completed subtree to the current parent (new stack top) or, if none, to
/// the roots.
fn push_into(stack: &mut [Symbol], roots: &mut Vec<Symbol>, sym: Symbol) {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(sym);
    } else {
        roots.push(sym);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruby_style_def_class_nest() {
        let src = "class Foo\n  def bar\n  end\n  def baz\n  end\nend\nmodule M\nend\n";
        let syms = symbols(OutlineLang::Other, src);
        assert_eq!(syms[0].name, "Foo");
        assert_eq!(syms[0].kind, SymbolKind::Class);
        let names: Vec<_> = syms[0].children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["bar", "baz"]); // both methods, in source order, nested
        assert_eq!(syms[1].name, "M");
        assert_eq!(syms[1].kind, SymbolKind::Module);
    }

    #[test]
    fn strips_modifiers_and_keeps_impl_target() {
        let src = "pub async fn run() {}\nimpl Foo for Bar {\n}\n";
        let syms = symbols(OutlineLang::Other, src);
        assert_eq!(syms[0], Symbol::leaf("run", SymbolKind::Function, 0));
        assert_eq!(syms[1].name, "Foo for Bar");
        assert_eq!(syms[1].kind, SymbolKind::Impl);
    }

    #[test]
    fn unknown_lines_produce_nothing() {
        assert!(symbols(OutlineLang::Other, "x = 1\nprint(x)\n").is_empty());
    }
}
