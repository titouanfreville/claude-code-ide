//! Tree-sitter outline provider — the universal baseline.
//!
//! Rather than compile per-language `.scm` queries (which hard-fail if a node name
//! drifts between grammar versions), we walk the parse tree and match **node kinds**
//! against a small per-language rule table. This degrades gracefully (an unmatched
//! node is simply skipped, never an error) and yields nesting for free: a definition's
//! `children` are the matching definitions found in its subtree.
//!
//! Priority languages (Rust/Go/Python/JS/TS/C/C++/Java) have curated rules; everything
//! else returns `None` so the caller falls back to the keyword heuristic.

use tree_sitter::{Language, Node, Parser};

use super::{OutlineLang, Symbol, SymbolKind};

/// A node kind that introduces a symbol, and how to name + classify it.
struct Rule {
    /// The tree-sitter node kind (e.g. `function_item`).
    kind: &'static str,
    sym: SymbolKind,
    /// The field holding the name node (default `"name"`; e.g. Rust `impl` uses `type`).
    name_field: &'static str,
}

const fn r(kind: &'static str, sym: SymbolKind) -> Rule {
    Rule {
        kind,
        sym,
        name_field: "name",
    }
}

const fn rf(kind: &'static str, sym: SymbolKind, name_field: &'static str) -> Rule {
    Rule {
        kind,
        sym,
        name_field,
    }
}

/// Extract a hierarchical outline, or `None` when the language has no rule set.
pub fn symbols(lang: OutlineLang, text: &str) -> Option<Vec<Symbol>> {
    let (language, rules) = lang_spec(lang)?;
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(text, None)?;
    Some(collect(tree.root_node(), rules, lang, text.as_bytes()))
}

fn lang_spec(lang: OutlineLang) -> Option<(Language, &'static [Rule])> {
    let pair: (Language, &'static [Rule]) = match lang {
        OutlineLang::Rust => (tree_sitter_rust::LANGUAGE.into(), RUST),
        OutlineLang::Go => (tree_sitter_go::LANGUAGE.into(), GO),
        OutlineLang::Python => (tree_sitter_python::LANGUAGE.into(), PYTHON),
        OutlineLang::JavaScript => (tree_sitter_javascript::LANGUAGE.into(), JS),
        OutlineLang::TypeScript => (tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), TS),
        OutlineLang::Tsx => (tree_sitter_typescript::LANGUAGE_TSX.into(), TS),
        OutlineLang::C => (tree_sitter_c::LANGUAGE.into(), C),
        OutlineLang::Cpp => (tree_sitter_cpp::LANGUAGE.into(), CPP),
        OutlineLang::Java => (tree_sitter_java::LANGUAGE.into(), JAVA),
        OutlineLang::Other => return None,
    };
    Some(pair)
}

/// Recursively collect symbols under `node`. A matched definition keeps its subtree's
/// symbols as `children`; an unmatched node is transparent (its symbols bubble up).
fn collect(node: Node, rules: &[Rule], lang: OutlineLang, src: &[u8]) -> Vec<Symbol> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(rule) = rules.iter().find(|r| r.kind == child.kind()) {
            if let Some(name) = symbol_name(child, rule, lang, src) {
                let mut children = collect(child, rules, lang, src);
                // A free function nested inside an impl/trait/class/interface is a method.
                if matches!(
                    rule.sym,
                    SymbolKind::Impl
                        | SymbolKind::Trait
                        | SymbolKind::Class
                        | SymbolKind::Interface
                ) {
                    for c in &mut children {
                        if c.kind == SymbolKind::Function {
                            c.kind = SymbolKind::Method;
                        }
                    }
                }
                out.push(Symbol {
                    name,
                    kind: rule.sym,
                    line: child.start_position().row,
                    children,
                });
                continue;
            }
        }
        // No rule (or anonymous): descend so nested definitions aren't lost.
        out.extend(collect(child, rules, lang, src));
    }
    out
}

/// Resolve a definition node's display name: its name field, then a declarator dig for
/// C/C++ functions, then the first identifier-ish descendant.
fn symbol_name(node: Node, rule: &Rule, lang: OutlineLang, src: &[u8]) -> Option<String> {
    if let Some(n) = node.child_by_field_name(rule.name_field) {
        return node_text(n, src);
    }
    if matches!(rule.sym, SymbolKind::Function | SymbolKind::Method)
        && matches!(lang, OutlineLang::C | OutlineLang::Cpp)
    {
        if let Some(decl) = node.child_by_field_name("declarator") {
            if let Some(id) = first_identifier(decl, src) {
                return Some(id);
            }
        }
    }
    first_identifier(node, src)
}

/// First identifier-ish node in `node`'s subtree (preorder).
fn first_identifier(node: Node, src: &[u8]) -> Option<String> {
    if matches!(
        node.kind(),
        "identifier"
            | "type_identifier"
            | "field_identifier"
            | "property_identifier"
            | "namespace_identifier"
    ) {
        return node_text(node, src);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(found) = first_identifier(child, src) {
            return Some(found);
        }
    }
    None
}

fn node_text(node: Node, src: &[u8]) -> Option<String> {
    std::str::from_utf8(&src[node.byte_range()])
        .ok()
        .map(|s| s.to_string())
}

const RUST: &[Rule] = &[
    r("function_item", SymbolKind::Function),
    r("struct_item", SymbolKind::Struct),
    r("union_item", SymbolKind::Struct),
    r("enum_item", SymbolKind::Enum),
    r("trait_item", SymbolKind::Trait),
    rf("impl_item", SymbolKind::Impl, "type"),
    r("mod_item", SymbolKind::Module),
    r("macro_definition", SymbolKind::Macro),
    r("const_item", SymbolKind::Constant),
    r("static_item", SymbolKind::Constant),
    r("type_item", SymbolKind::Type),
];

const GO: &[Rule] = &[
    r("function_declaration", SymbolKind::Function),
    r("method_declaration", SymbolKind::Method),
    r("type_spec", SymbolKind::Type),
];

const PYTHON: &[Rule] = &[
    r("function_definition", SymbolKind::Function),
    r("class_definition", SymbolKind::Class),
];

const JS: &[Rule] = &[
    r("function_declaration", SymbolKind::Function),
    r("generator_function_declaration", SymbolKind::Function),
    r("class_declaration", SymbolKind::Class),
    r("method_definition", SymbolKind::Method),
];

const TS: &[Rule] = &[
    r("function_declaration", SymbolKind::Function),
    r("class_declaration", SymbolKind::Class),
    r("abstract_class_declaration", SymbolKind::Class),
    r("method_definition", SymbolKind::Method),
    r("interface_declaration", SymbolKind::Interface),
    r("enum_declaration", SymbolKind::Enum),
    r("type_alias_declaration", SymbolKind::Type),
];

const C: &[Rule] = &[
    r("function_definition", SymbolKind::Function),
    r("struct_specifier", SymbolKind::Struct),
    r("union_specifier", SymbolKind::Struct),
    r("enum_specifier", SymbolKind::Enum),
    rf("type_definition", SymbolKind::Type, "declarator"),
];

const CPP: &[Rule] = &[
    r("function_definition", SymbolKind::Function),
    r("struct_specifier", SymbolKind::Struct),
    r("class_specifier", SymbolKind::Class),
    r("union_specifier", SymbolKind::Struct),
    r("enum_specifier", SymbolKind::Enum),
    r("namespace_definition", SymbolKind::Module),
    rf("type_definition", SymbolKind::Type, "declarator"),
];

const JAVA: &[Rule] = &[
    r("class_declaration", SymbolKind::Class),
    r("record_declaration", SymbolKind::Class),
    r("interface_declaration", SymbolKind::Interface),
    r("annotation_type_declaration", SymbolKind::Interface),
    r("enum_declaration", SymbolKind::Enum),
    r("method_declaration", SymbolKind::Method),
    r("constructor_declaration", SymbolKind::Method),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Flatten to `(name, kind, depth)` for concise assertions.
    fn flat(syms: &[Symbol], depth: usize, out: &mut Vec<(String, SymbolKind, usize)>) {
        for s in syms {
            out.push((s.name.clone(), s.kind, depth));
            flat(&s.children, depth + 1, out);
        }
    }
    fn outline(lang: OutlineLang, src: &str) -> Vec<(String, SymbolKind, usize)> {
        let mut v = Vec::new();
        flat(&symbols(lang, src).unwrap(), 0, &mut v);
        v
    }
    fn has(v: &[(String, SymbolKind, usize)], name: &str, kind: SymbolKind, depth: usize) -> bool {
        v.iter()
            .any(|(n, k, d)| n == name && *k == kind && *d == depth)
    }

    #[test]
    fn rust_decls_and_impl_methods() {
        let v = outline(
            OutlineLang::Rust,
            "pub struct Foo;\nenum E { A }\ntrait T {}\nimpl Foo {\n  pub fn bar(&self) {}\n}\nfn free() {}\n",
        );
        assert!(has(&v, "Foo", SymbolKind::Struct, 0));
        assert!(has(&v, "E", SymbolKind::Enum, 0));
        assert!(has(&v, "T", SymbolKind::Trait, 0));
        assert!(has(&v, "Foo", SymbolKind::Impl, 0));
        assert!(has(&v, "bar", SymbolKind::Method, 1)); // nested under impl, relabeled
        assert!(has(&v, "free", SymbolKind::Function, 0));
    }

    #[test]
    fn go_funcs_methods_types() {
        let v = outline(
            OutlineLang::Go,
            "package x\nfunc Top() {}\ntype S struct{}\nfunc (s S) M() {}\n",
        );
        assert!(has(&v, "Top", SymbolKind::Function, 0));
        assert!(has(&v, "S", SymbolKind::Type, 0));
        assert!(has(&v, "M", SymbolKind::Method, 0));
    }

    #[test]
    fn python_class_methods_nest() {
        let v = outline(
            OutlineLang::Python,
            "class A:\n    def m(self):\n        pass\ndef top():\n    pass\n",
        );
        assert!(has(&v, "A", SymbolKind::Class, 0));
        assert!(has(&v, "m", SymbolKind::Method, 1)); // function inside class → method
        assert!(has(&v, "top", SymbolKind::Function, 0));
    }

    #[test]
    fn typescript_decls() {
        let v = outline(
            OutlineLang::TypeScript,
            "export class W { build() {} }\ninterface I { x: number }\ntype T = number;\nfunction f() {}\n",
        );
        assert!(has(&v, "W", SymbolKind::Class, 0));
        assert!(has(&v, "build", SymbolKind::Method, 1));
        assert!(has(&v, "I", SymbolKind::Interface, 0));
        assert!(has(&v, "T", SymbolKind::Type, 0));
        assert!(has(&v, "f", SymbolKind::Function, 0));
    }

    #[test]
    fn javascript_class_and_function() {
        let v = outline(
            OutlineLang::JavaScript,
            "export class Widget { render() {} }\nfunction build() {}\n",
        );
        assert!(has(&v, "Widget", SymbolKind::Class, 0));
        assert!(has(&v, "render", SymbolKind::Method, 1));
        assert!(has(&v, "build", SymbolKind::Function, 0));
    }

    #[test]
    fn c_function_name_from_declarator() {
        let v = outline(
            OutlineLang::C,
            "int main(void) { return 0; }\nstruct Pt { int x; };\ntypedef int Handle;\n",
        );
        assert!(has(&v, "main", SymbolKind::Function, 0));
        assert!(has(&v, "Pt", SymbolKind::Struct, 0));
        assert!(has(&v, "Handle", SymbolKind::Type, 0));
    }

    #[test]
    fn cpp_class_methods_and_namespace() {
        let v = outline(
            OutlineLang::Cpp,
            "namespace ns { class C { public: void run(); }; }\nvoid C::run() {}\n",
        );
        assert!(has(&v, "ns", SymbolKind::Module, 0));
        assert!(has(&v, "C", SymbolKind::Class, 1));
    }

    #[test]
    fn java_class_methods() {
        let v = outline(
            OutlineLang::Java,
            "class A {\n  int f() { return 1; }\n  A() {}\n}\ninterface I {}\n",
        );
        assert!(has(&v, "A", SymbolKind::Class, 0));
        assert!(has(&v, "f", SymbolKind::Method, 1));
        assert!(has(&v, "I", SymbolKind::Interface, 0));
    }

    #[test]
    fn unsupported_language_is_none() {
        assert!(symbols(OutlineLang::Other, "x").is_none());
    }
}
