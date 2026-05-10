// Here, we modify each function item by appending rustdoc
// containing information about the Verus signature that we want to appear
// in auto-generated rustdoc. For example, we add information about
// 'requires' and 'ensures' clauses, and we also add 'mode' information.
// This information would be absent if we tried to run rustdoc without any
// processing.
//
// The flow is:
//  - the verus! macro (this file) adds extra information into a rustdoc comment
//  - rustdoc (unmodified) generates the rustdoc HTML
//  - we run a postprocessor (verusdoc crate) to present the information in a nice way.
//
// Specifically, we add "attributes" in a kinda-silly format that the post-processing
// step can recognize.  The format is:
//
//     ```rust
//     // verusdoc_special_attr $ATTR_NAME
//     $ATTR_VALUE
//     ```
//
// The ATTR_NAME can be requires, ensures, returns, recommends or modes.
//
// The reason we use a codeblock here is that so rustdoc will perform syntax highlighting
// on the value which is applicable if it's an expression. For example, if it's a
// requires, ensures, or recommends attribute then ATTR_VALUE will be a (pretty-printed)
// boolean expression.
//
// The other type, 'modes', is a bit more complicated: the value is a JSON blob with
// some data explaining the function mode, param modes, and return mode.

use proc_macro2::Span;
use quote::ToTokens;
use std::iter::FromIterator;
use verus_syn::punctuated::Punctuated;
use verus_syn::spanned::Spanned;
use verus_syn::token;
use verus_syn::{
    AssumeSpecification, AttrStyle, Attribute, Block, Expr, ExprBlock, ExprPath, FnArg, FnMode,
    Ident, ImplItemFn, ItemFn, Pat, PatIdent, Path, PathArguments, PathSegment, Publish, QSelf,
    ReturnType, Signature, TraitItemFn, Type, TypeGroup, TypePath,
};

/// Returns true when the macro should inject `verusdoc_special_attr`
/// doc markers for `verusdoc` to post-process.
///
/// Activation is *intentionally per-rustdoc-invocation*: the same
/// crate's rustc compile (e.g. vstd's rmeta build under
/// `cargo verus doc`) must NOT see the markers, because the
/// `assume_specification` shape they're paired with isn't valid
/// input to the VIR translation that runs during the rustc compile.
///
/// `cargo verus doc` arranges for `VERUSDOC=1` to be set only in the
/// rustdoc subprocess by routing rustdoc through the
/// `verus-rustdoc-shim` binary (cargo's `RUSTDOC` env var points at
/// the shim; the shim sets `VERUSDOC=1` and execs the real rustdoc).
/// The wrapped rustc invocations that cargo also runs to build dep
/// rmetas inherit the parent cargo process's env, which does NOT have
/// `VERUSDOC` set, so `env_rustdoc()` returns false there.
///
/// The upstream `tools/docs.sh` standalone-rustdoc path also exports
/// `VERUSDOC=1` directly and continues to work.
#[cfg(verus_keep_ghost)]
pub fn env_rustdoc() -> bool {
    match proc_macro::tracked::env_var("VERUSDOC") {
        Err(_) => false, // VERUSDOC key not present in environment
        Ok(s) => s == "1",
    }
}

/// Check if VERUSDOC=1.
#[cfg(not(verus_keep_ghost))]
pub fn env_rustdoc() -> bool {
    false
}

// Main hooks for the verus! macro to manipulate ItemFn, etc.

pub fn process_item_fn(item: &mut ItemFn) {
    match attr_for_sig(&item.sig, Some(&item.block), None) {
        Some(attr) => item.attrs.insert(0, attr),
        None => {}
    }
}

pub fn process_item_fn_assume_specification(item: &mut ItemFn, as_spec: &AssumeSpecification) {
    match attr_for_sig(&item.sig, Some(&item.block), Some(as_spec)) {
        Some(attr) => item.attrs.insert(0, attr),
        None => {}
    }
}

pub fn process_item_fn_broadcast_group(item: &mut ItemFn) {
    match attr_for_broadcast_group(&item.sig) {
        Some(attr) => item.attrs.insert(0, attr),
        None => {}
    }
}

pub fn process_impl_item_method(item: &mut ImplItemFn) {
    match attr_for_sig(&item.sig, Some(&item.block), None) {
        Some(attr) => item.attrs.insert(0, attr),
        None => {}
    }
}

pub fn process_trait_item_method(item: &mut TraitItemFn) {
    match attr_for_sig(&item.sig, item.default.as_ref(), None) {
        Some(attr) => item.attrs.insert(0, attr),
        None => {}
    }
}

/// Process a signature to get all the information, apply the codeblock
/// formatting tricks, and then package it all up into a #[doc = "..."] attribute
/// (as a verus_syn::Attribute object) that we can apply to the item.
fn attr_for_sig(
    sig: &Signature,
    block: Option<&Block>,
    as_spec: Option<&AssumeSpecification>,
) -> Option<Attribute> {
    let mut v = vec![];

    v.push(encoded_sig_info(sig));

    if let Some(with_spec) = &sig.spec.with {
        v.push(encoded_str("with", &format_with_spec(with_spec)));
    }

    // Collect linkable paths from every clause expression so we can
    // emit a single intra-doc link map (resolved by rustdoc to hrefs)
    // that the post-processor uses to wrap matching identifiers inside
    // rendered clauses in `<a>` tags.
    let mut link_paths: Vec<String> = vec![];

    match &sig.spec.requires {
        Some(es) => {
            for expr in es.exprs.exprs.iter() {
                v.push(encoded_expr("requires", expr));
                collect_link_paths(expr, &mut link_paths);
            }
        }
        None => {}
    }
    match &sig.spec.recommends {
        Some(es) => {
            for expr in es.exprs.exprs.iter() {
                v.push(encoded_expr("recommends", expr));
                collect_link_paths(expr, &mut link_paths);
            }
        }
        None => {}
    }
    match &sig.spec.ensures {
        Some(es) => {
            for expr in es.exprs.exprs.iter() {
                v.push(encoded_expr("ensures", expr));
                collect_link_paths(expr, &mut link_paths);
            }
        }
        None => {}
    }
    match &sig.spec.returns {
        Some(rs) => {
            for expr in rs.exprs.exprs.iter() {
                v.push(encoded_expr("returns", expr));
                collect_link_paths(expr, &mut link_paths);
            }
        }
        None => {}
    }

    match block {
        Some(block) => {
            if is_spec(&sig) {
                if show_body(sig) {
                    let b =
                        Expr::Block(ExprBlock { attrs: vec![], label: None, block: block.clone() });
                    v.push(encoded_body("body", &b));
                    collect_link_paths(&b, &mut link_paths);
                }
            }
        }
        None => {}
    }

    if let Some(as_spec) = as_spec {
        let e = Expr::Path(ExprPath {
            attrs: vec![],
            qself: as_spec.qself.clone(),
            path: as_spec.path.clone(),
        });
        v.push(encoded_expr("assume_specification", &e));
        v.push(assume_specification_link_line(&e));
    }

    if let Some(s) = encoded_link_map(&link_paths) {
        v.push(s);
    }

    if v.len() == 0 { None } else { Some(doc_attr_from_string(&v.join("\n\n"), sig.span())) }
}

fn attr_for_broadcast_group(sig: &Signature) -> Option<Attribute> {
    let mut v = vec![];

    v.push(encoded_str("broadcast_group", ""));

    if v.len() == 0 { None } else { Some(doc_attr_from_string(&v.join("\n\n"), sig.span())) }
}

fn is_spec(sig: &Signature) -> bool {
    match &sig.mode {
        FnMode::Spec(_) | FnMode::SpecChecked(_) => true,
        FnMode::Proof(_) | FnMode::ProofAxiom(_) | FnMode::Exec(_) | FnMode::Default => false,
    }
}

/// Do we want to show the body for the given spec function?
/// If it's 'open', then yes
fn show_body(sig: &Signature) -> bool {
    matches!(sig.publish, Publish::Open(_))
}

fn fn_mode_to_string(mode: &FnMode, publish: &Publish) -> String {
    match mode {
        FnMode::Spec(_) | FnMode::SpecChecked(_) => match publish {
            Publish::Closed(_) => "closed spec".to_string(),
            Publish::Open(_) => "open spec".to_string(),
            Publish::OpenRestricted(res) => {
                "open(".to_string() + &module_path_to_string(&res.path) + ") spec"
            }
            Publish::Uninterp(_) => "uninterp".to_string(),
            Publish::Default => "spec".to_string(),
        },
        FnMode::Proof(_) | FnMode::ProofAxiom(_) => "proof".to_string(),
        FnMode::Exec(_) => "exec".to_string(),
        FnMode::Default => "exec".to_string(),
    }
}

fn module_path_to_string(p: &Path) -> String {
    // path is for a module; we can ignore type arguments

    let lead = if p.leading_colon.is_some() { "::" } else { "" };
    let main = p
        .segments
        .iter()
        .map(|path_seg| path_seg.ident.to_string())
        .collect::<Vec<String>>()
        .join("::");
    lead.to_string() + &main
}

fn encoded_sig_info(sig: &Signature) -> String {
    let fn_mode = fn_mode_to_string(&sig.mode, &sig.publish);
    let (ret_mode, ret_name) = match &sig.output {
        ReturnType::Default => ("Default", "".to_string()),
        ReturnType::Type(_, tracked_token, opt_name, _) => {
            let mode = if tracked_token.is_some() { "Tracked" } else { "Default" };

            let name = match opt_name {
                None => "".to_string(),
                Some(b) => match &b.1 {
                    Pat::Ident(PatIdent { ident, .. }) => ident.to_string(),
                    _ => "".to_string(),
                },
            };

            (mode, name)
        }
    };

    let param_modes = sig
        .inputs
        .iter()
        .map(|fn_arg| if fn_arg.tracked.is_some() { "\"Tracked\"" } else { "\"Default\"" })
        .collect::<Vec<&str>>();
    let param_modes = param_modes.join(",");

    let broadcast = sig.broadcast.is_some();

    // JSON blob is parsed by the verusdoc post-processor into a `DocModeInfo` object.
    // I decided not to pull in serde as a dependency for verus_builtin_macros,
    // but if serialization gets too complicated, we should probably do that instead.

    // We put it in a comment to avoid extra syntax highlighting or anything that would
    // complicate the post-processing.

    let info = format!(
        r#"// {{ "fn_mode": "{fn_mode:}", "ret_mode": "{ret_mode:}", "param_modes": [{param_modes:}], "broadcast": {broadcast:}, "ret_name": "{ret_name:}" }}"#
    );

    encoded_str("modes", &info)
}

/// Get the assume_specification line
fn assume_specification_link_line(e: &Expr) -> String {
    // This function applies a series of heuristics to try to get the doc links to work
    // 1. Change <A>::B to A::B
    // 2. Not link things we know cannot be linked:
    //  - Pointer types
    //  - <Type as Trait>::trait_method constructions
    let mut can_link = true;
    let e = match e {
        Expr::Path(ExprPath {
            attrs,
            qself: Some(QSelf { lt_token: _, ty, position: 0, as_token: None, gt_token: _ }),
            path: Path { leading_colon: Some(leading_colon), segments },
        }) => {
            let mut ty = ty;

            if let Type::Group(TypeGroup { group_token: _, elem }) = &**ty {
                ty = elem;
            }

            match &**ty {
                Type::Ptr(_) => {
                    // Cannot link to pointer types in rustdoc
                    can_link = false;
                }
                _ => {}
            }

            if let Type::Path(TypePath { qself: None, path: inner_path }) = &**ty {
                if !inner_path.segments.trailing_punct() && !segments.trailing_punct() {
                    let mut new_path = inner_path.clone();
                    new_path.segments.push_punct(leading_colon.clone());
                    for (i, value) in segments.iter().enumerate() {
                        new_path.segments.push_value(value.clone());
                        if i + 1 < segments.len() {
                            new_path.segments.push_punct(leading_colon.clone());
                        }
                    }
                    &Expr::Path(ExprPath { attrs: attrs.clone(), qself: None, path: new_path })
                } else {
                    e
                }
            } else {
                e
            }
        }
        Expr::Path(ExprPath { qself: Some(QSelf { as_token: Some(_), .. }), .. }) => {
            // Cannot link to implementations of trait methods
            // https://github.com/rust-lang/rust/issues/74563
            // FIXME: we could instead link for both the trait method and the implementing type
            can_link = false;
            e
        }
        _ => e,
    };

    let s = verus_prettyplease::unparse_expr(&e).replace("\n", " ");
    if can_link {
        format!("**Specification for [`{:}`]**", s)
    } else {
        format!("**Specification for `{:}`**", s)
    }
}

/// Pretty print the expression, then wrap in a code block.
fn encoded_expr(kind: &str, code: &Expr) -> String {
    let s = verus_prettyplease::unparse_expr(&code);
    let s = format!("{s:},");
    encoded_str(kind, &s)
}

fn encoded_body(kind: &str, code: &Expr) -> String {
    let s = verus_prettyplease::unparse_expr(&code);
    let s = format!("{s:}");
    encoded_str(kind, &s)
}

/// Wrap the given string into a code block,
/// into the format that the postprocessor will recognize.
fn encoded_str(kind: &str, data: &str) -> String {
    "```rust\n// verusdoc_special_attr ".to_string() + kind + "\n" + data + "\n```"
}

fn format_with_spec(with_spec: &verus_syn::WithSpecOnFn) -> String {
    let mut lines: Vec<String> = vec![];

    let inputs = format_fn_args(&with_spec.inputs);
    for input in inputs {
        let input = normalize_ws(input.trim());
        lines.push(format!("{input},"));
    }

    if let Some((_, outputs)) = &with_spec.outputs {
        lines.push("->".to_string());
        let outputs = format_pat_types(outputs);
        for output in outputs {
            let output = normalize_ws(output.trim());
            lines.push(format!("{output},"));
        }
    }

    lines.join("\n")
}

fn format_pat_types(outputs: &Punctuated<verus_syn::PatType, verus_syn::Token![,]>) -> Vec<String> {
    if outputs.is_empty() {
        return vec![];
    }

    outputs.iter().map(format_pat_type).collect()
}

fn format_fn_args(inputs: &Punctuated<FnArg, verus_syn::Token![,]>) -> Vec<String> {
    if inputs.is_empty() {
        return vec![];
    }
    inputs.iter().map(format_fn_arg).collect()
}

fn format_fn_arg(arg: &FnArg) -> String {
    let tracked = if arg.tracked.is_some() { "tracked " } else { "" };
    match &arg.kind {
        verus_syn::FnArgKind::Receiver(receiver) => {
            let s = normalize_ws(&receiver.to_token_stream().to_string());
            format!("{tracked}{s}")
        }
        verus_syn::FnArgKind::Typed(pt) => {
            let s = format_pat_type(pt);
            format!("{tracked}{s}")
        }
    }
}

fn format_pat_type(pt: &verus_syn::PatType) -> String {
    let pat = normalize_ws(&verus_prettyplease::unparse_pat(&pt.pat));
    let ty = normalize_ws(&verus_prettyplease::unparse_ty(&pt.ty));
    format!("{pat}: {ty}")
}

fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<&str>>().join(" ")
}

/// Create an attr that looks like #[doc = "doc_str"]
fn doc_attr_from_string(doc_str: &str, span: Span) -> Attribute {
    let path = Path {
        leading_colon: None,
        segments: Punctuated::from_iter(vec![PathSegment {
            ident: Ident::new("doc", span),
            arguments: PathArguments::None,
        }]),
    };
    let lit = verus_syn::Lit::Str(verus_syn::LitStr::new(doc_str, span));
    let name_value = verus_syn::MetaNameValue {
        path,
        eq_token: token::Eq { spans: [span] },
        value: Expr::Lit(verus_syn::ExprLit { attrs: vec![], lit }),
    };
    Attribute {
        pound_token: token::Pound { spans: [span] },
        style: AttrStyle::Outer,
        bracket_token: token::Bracket { span: crate::syntax::into_spans(span) },
        meta: verus_syn::Meta::NameValue(name_value),
    }
}

/// Walk a clause expression and collect path identifiers that are
/// candidates for intra-doc link resolution. The post-processor uses
/// these to wrap identifier occurrences inside rendered clauses in
/// `<a>` tags pointing at the resolved item pages.
///
/// We collect:
///   - Single-segment path that appears as the function of a Call
///     (e.g. `agree_up_to(a, b, k)` → `agree_up_to`).
///   - Multi-segment paths in any position (e.g. `Log::EMPTY`,
///     `Log::append`, `core::option::Option::Some`).
///
/// We deliberately skip single-segment paths in non-call position
/// because they're usually local-variable references that would emit
/// `unresolved link` warnings from rustdoc.
fn collect_link_paths(expr: &Expr, out: &mut Vec<String>) {
    use verus_syn::Expr as E;

    let push = |path: &Path, out: &mut Vec<String>, allow_single: bool| {
        if !path_is_link_candidate(path, allow_single) {
            return;
        }
        let s = simple_path_to_string(path);
        if !out.iter().any(|x| x == &s) {
            out.push(s);
        }
    };

    match expr {
        E::Array(a) => {
            for e in &a.elems {
                collect_link_paths(e, out);
            }
        }
        E::Assign(a) => {
            collect_link_paths(&a.left, out);
            collect_link_paths(&a.right, out);
        }
        E::Await(a) => collect_link_paths(&a.base, out),
        E::Binary(b) => {
            collect_link_paths(&b.left, out);
            collect_link_paths(&b.right, out);
        }
        E::Block(b) => {
            for stmt in &b.block.stmts {
                if let verus_syn::Stmt::Expr(e, _) = stmt {
                    collect_link_paths(e, out);
                }
            }
        }
        E::Break(b) => {
            if let Some(e) = &b.expr {
                collect_link_paths(e, out);
            }
        }
        E::Call(c) => {
            if let E::Path(p) = &*c.func {
                if p.qself.is_none() {
                    push(&p.path, out, true);
                } else {
                    push(&p.path, out, false);
                }
                collect_link_paths_in_generic_args(&p.path, out);
            } else {
                collect_link_paths(&c.func, out);
            }
            for a in &c.args {
                collect_link_paths(a, out);
            }
        }
        E::Cast(c) => collect_link_paths(&c.expr, out),
        E::Closure(c) => collect_link_paths(&c.body, out),
        E::Field(f) => collect_link_paths(&f.base, out),
        E::ForLoop(f) => {
            collect_link_paths(&f.expr, out);
            for stmt in &f.body.stmts {
                if let verus_syn::Stmt::Expr(e, _) = stmt {
                    collect_link_paths(e, out);
                }
            }
        }
        E::Group(g) => collect_link_paths(&g.expr, out),
        E::If(i) => {
            collect_link_paths(&i.cond, out);
            for stmt in &i.then_branch.stmts {
                if let verus_syn::Stmt::Expr(e, _) = stmt {
                    collect_link_paths(e, out);
                }
            }
            if let Some((_, else_expr)) = &i.else_branch {
                collect_link_paths(else_expr, out);
            }
        }
        E::Index(i) => {
            collect_link_paths(&i.expr, out);
            collect_link_paths(&i.index, out);
        }
        E::Let(l) => collect_link_paths(&l.expr, out),
        E::Loop(l) => {
            for stmt in &l.body.stmts {
                if let verus_syn::Stmt::Expr(e, _) = stmt {
                    collect_link_paths(e, out);
                }
            }
        }
        E::Match(m) => {
            collect_link_paths(&m.expr, out);
            for arm in &m.arms {
                if let Some((_, g)) = &arm.guard {
                    collect_link_paths(g, out);
                }
                collect_link_paths(&arm.body, out);
            }
        }
        E::MethodCall(mc) => {
            collect_link_paths(&mc.receiver, out);
            for a in &mc.args {
                collect_link_paths(a, out);
            }
        }
        E::Paren(p) => collect_link_paths(&p.expr, out),
        E::Path(p) => {
            // Multi-segment paths only here (single-segment would
            // typically be a local variable).
            push(&p.path, out, false);
            collect_link_paths_in_generic_args(&p.path, out);
        }
        E::Range(r) => {
            if let Some(s) = &r.start {
                collect_link_paths(s, out);
            }
            if let Some(e) = &r.end {
                collect_link_paths(e, out);
            }
        }
        E::Reference(r) => collect_link_paths(&r.expr, out),
        E::Repeat(r) => collect_link_paths(&r.expr, out),
        E::Return(r) => {
            if let Some(e) = &r.expr {
                collect_link_paths(e, out);
            }
        }
        E::Struct(s) => {
            push(&s.path, out, false);
            collect_link_paths_in_generic_args(&s.path, out);
            for f in &s.fields {
                collect_link_paths(&f.expr, out);
            }
            if let Some(r) = &s.rest {
                collect_link_paths(r, out);
            }
        }
        E::Try(t) => collect_link_paths(&t.expr, out),
        E::Tuple(t) => {
            for e in &t.elems {
                collect_link_paths(e, out);
            }
        }
        E::Unary(u) => collect_link_paths(&u.expr, out),
        E::While(w) => {
            collect_link_paths(&w.cond, out);
            for stmt in &w.body.stmts {
                if let verus_syn::Stmt::Expr(e, _) = stmt {
                    collect_link_paths(e, out);
                }
            }
        }
        E::Yield(y) => {
            if let Some(e) = &y.expr {
                collect_link_paths(e, out);
            }
        }
        // Verus-specific expression nodes that hold sub-exprs.
        E::Assume(a) => collect_link_paths(&a.expr, out),
        E::Assert(a) => collect_link_paths(&a.expr, out),
        E::AssertForall(a) => collect_link_paths(&a.expr, out),
        E::View(v) => collect_link_paths(&v.expr, out),
        E::BigAnd(ba) => {
            for x in &ba.exprs {
                collect_link_paths(&x.expr, out);
            }
        }
        E::BigOr(bo) => {
            for x in &bo.exprs {
                collect_link_paths(&x.expr, out);
            }
        }
        E::Is(i) => collect_link_paths(&i.base, out),
        E::IsNot(i) => collect_link_paths(&i.base, out),
        E::Has(h) => {
            collect_link_paths(&h.lhs, out);
            collect_link_paths(&h.rhs, out);
        }
        E::HasNot(h) => {
            collect_link_paths(&h.lhs, out);
            collect_link_paths(&h.rhs, out);
        }
        E::Matches(m) => collect_link_paths(&m.lhs, out),
        E::GetField(g) => collect_link_paths(&g.base, out),
        E::Final(f) => collect_link_paths(&f.arg, out),
        // Leaf or non-recursive variants we skip:
        // Lit, Infer, Continue, RawAddr, TryBlock, Async, Unsafe,
        // Const, Macro, Verbatim, RevealHide.
        _ => {}
    }
}

fn collect_link_paths_in_generic_args(path: &Path, out: &mut Vec<String>) {
    use verus_syn::{GenericArgument, PathArguments};
    for seg in &path.segments {
        match &seg.arguments {
            PathArguments::AngleBracketed(ab) => {
                for arg in &ab.args {
                    match arg {
                        GenericArgument::Type(Type::Path(TypePath { qself: None, path })) => {
                            if path_is_link_candidate(path, false) {
                                let s = simple_path_to_string(path);
                                if !out.iter().any(|x| x == &s) {
                                    out.push(s);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// Decide whether `path` is worth emitting as an intra-doc link.
///
/// Filters out things that would produce `unresolved link` warnings
/// from rustdoc:
///   - keywords like `Self` (rustdoc accepts `Self::method` but not
///     bare `Self`),
///   - very short single-segment lowercase names (likely a local
///     variable when `allow_single` is false).
fn path_is_link_candidate(path: &Path, allow_single: bool) -> bool {
    if path.segments.is_empty() {
        return false;
    }
    let multi = path.segments.len() > 1;
    if !multi && !allow_single {
        return false;
    }
    let first = path.segments.first().expect("non-empty checked above");
    let first_name = first.ident.to_string();
    // Skip path expressions starting with `Self::` — rustdoc *can*
    // resolve `[`Self::foo`]`, but only inside an impl block context
    // that we can't reliably detect here.
    if first_name == "Self" {
        return false;
    }
    // Skip macro-style bare keywords just in case.
    if matches!(first_name.as_str(), "true" | "false" | "self") {
        return false;
    }
    true
}

fn simple_path_to_string(path: &Path) -> String {
    let lead = if path.leading_colon.is_some() { "::" } else { "" };
    let body = path
        .segments
        .iter()
        .map(|seg| seg.ident.to_string())
        .collect::<Vec<_>>()
        .join("::");
    format!("{lead}{body}")
}

/// Emit a markdown link map for the post-processor. The map is a
/// rustdoc-comment-rendered shortcut intra-doc link list wrapped in
/// HTML comment markers that the post-processor recognizes.
///
/// CommonMark parses HTML comments as raw HTML inline, so the markers
/// survive the markdown pass and end up as `<!-- … -->` nodes in the
/// rendered output. The bullet list between them is processed as
/// normal markdown, which triggers rustdoc's intra-doc-link pass:
/// each `[`path`]` becomes `<a href="…/fn.path.html"><code>path</code></a>`
/// (or whatever the resolved item is).
fn encoded_link_map(paths: &[String]) -> Option<String> {
    if paths.is_empty() {
        return None;
    }
    let mut s = String::new();
    s.push_str("<!--verusdoc_link_map_start-->\n\n");
    for p in paths {
        // Wrap in backticks so the rendered anchor text is rendered
        // as inline code (matches how assume_specification_link_line
        // formats its anchor and how rustdoc-resolved intra-doc
        // links normally render).
        s.push_str(&format!("- [`{p}`]\n"));
    }
    s.push_str("\n<!--verusdoc_link_map_end-->");
    Some(s)
}
