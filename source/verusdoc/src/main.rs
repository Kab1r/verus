// HTML Postprocessor for verusdoc.
// The 'other half' to this mechanism is in verus_builtin_macros/src/rustdoc.rs
// which has more high-level details.

use html5ever::{QualName, local_name, namespace_url, ns};
use kuchiki::NodeRef;
use kuchiki::traits::TendrilSink;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use walkdir::WalkDir;

#[derive(Serialize, Deserialize)]
pub enum ParamMode {
    Tracked,
    Default,
}

#[derive(Serialize, Deserialize)]
pub struct DocSigInfo {
    pub fn_mode: String,
    pub ret_mode: ParamMode,
    pub ret_name: String,
    pub param_modes: Vec<ParamMode>,
    pub broadcast: bool,
}

enum VerusDocAttr {
    ModeInfo(DocSigInfo),
    Specification(String, NodeRef),
    BroadcastGroup,
    AssumeSpecification,
}

// Types of spec clauses we handle.
static SPEC_NAMES: [&str; 6] = ["with", "requires", "ensures", "returns", "recommends", "body"];

fn main() {
    // Manipulate the auto-generated files in `doc/` to clean them up to make
    // the more presentable.

    // First modify the main .css file to add the CSS rules for the classes we're going to use

    write_css(Path::new("doc"));

    // Process every documentation HTML file in the directory.

    for entry in WalkDir::new("doc").into_iter().filter_map(|e| e.ok()) {
        if entry.path().extension().map(|s| s == "html").unwrap_or(false) {
            process_file(entry.path())
        }
    }
}

fn process_file(path: &Path) {
    let html = std::fs::read_to_string(path).expect("read file");

    if !html.contains("verusdoc_special_attr") {
        return;
    }

    let document = kuchiki::parse_html().one(html);

    let opt_trait_info = get_opt_trait_info(path, &document);

    // Rustdoc generates HTML for an ImplItem that looks like this:
    //
    //   <summary>signature here (fn foo(...) -> ...)</summary>
    //   <div class="docblock">
    //
    //     <div class="example-wrap">     // This encapsulates a single "verusdoc attribute"
    //       <pre class="rust rust-example-rendered">
    //         <code>
    //           <span class="comment">// verusdoc_special_attr modes</span>
    //           ... additional content here
    //         </code>
    //       </pre>
    //     </div>
    //
    //     ... possibly more blocks here
    //   </div>
    //
    // Our plan is:
    //
    //   1. Collect all <div class="docblock"> elements
    //
    //   2. Check for <pre> elements that use our special format.
    //      Parse the information inside and remove them.
    //
    //   3. For the 'modes' information, we parse the modes information, then annotate
    //      the signature line with the mode information
    //
    //   4. For the requires/ensures/reocmmends information, we collect the
    //      syntax-highlighted HTML and add that HTML back in a nicely-formatted way.

    for docblock_elem in document.select(".docblock").expect("code selector") {
        let docblock_elem: &NodeRef = docblock_elem.as_node();

        // Iterate over elements. For each one, check if it is a
        // verusdoc_special_attr block. If so, remove it.

        let mut child_opt = docblock_elem.first_child();
        let mut attrs: Vec<VerusDocAttr> = vec![];
        while child_opt.is_some() {
            let child = child_opt.unwrap();

            match interpret_as_verusdoc_attribute(&child) {
                None => {
                    child_opt = child.next_sibling();
                }
                Some(attr) => {
                    attrs.push(attr);
                    child_opt = child.next_sibling();
                    child.detach();
                }
            }
        }

        // Now add content based on the data we just collected.

        update_docblock(&docblock_elem, &attrs, &opt_trait_info);
    }

    document.serialize_to_file(path).expect("serialize_to_file");
}

fn interpret_as_verusdoc_attribute(node: &NodeRef) -> Option<VerusDocAttr> {
    let pre_node = get_single_child(node)?;
    if !is_element(&pre_node, "pre") {
        return None;
    }

    let code_node = get_single_child(&pre_node)?;
    if !is_element(&code_node, "code") {
        return None;
    }

    let span_node = code_node.first_child()?;
    if !is_element(&span_node, "span") {
        return None;
    }

    let text_node = get_single_child(&span_node)?;
    let text_ref = text_node.as_text()?;
    let text: &String = &text_ref.borrow();

    let attr_data = text.strip_prefix("// verusdoc_special_attr ")?;
    let attr_name = attr_data.split_whitespace().next().unwrap();

    if attr_name == "modes" {
        let comment_idx = attr_data.find("// ").unwrap();
        let json_text = &attr_data[comment_idx + 3..];

        let doc_mode_info: DocSigInfo = serde_json::from_str(json_text).unwrap();

        Some(VerusDocAttr::ModeInfo(doc_mode_info))
    } else if SPEC_NAMES.contains(&attr_name) {
        span_node.detach();
        pre_node.detach();

        Some(VerusDocAttr::Specification(attr_name.to_string(), pre_node))
    } else if attr_name == "broadcast_group" {
        Some(VerusDocAttr::BroadcastGroup)
    } else {
        Some(VerusDocAttr::AssumeSpecification)
    }
}

/// If this node has a single child, return it. Otherwise, None.
fn get_single_child(node: &NodeRef) -> Option<NodeRef> {
    let child = node.first_child()?;
    if child.next_sibling().is_some() {
        return None;
    }
    Some(child)
}

/// Move the children of the `<code>` inside `pre_node` into `target`.
///
/// The clause expressions arrive as `<pre><code>…</code></pre>` blocks
/// (rustdoc rendered them that way from the original code-fence in a
/// doc attribute). When we splice them into the item-decl `<pre>` we
/// don't want a `<pre>` inside another `<pre>` — the surrounding
/// item-decl pre already preserves whitespace. So strip the wrapping
/// pre/code and inline the syntax-highlighted spans directly, the
/// same way rustdoc renders the constraint list inside
/// `<div class="where">`.
fn inline_pre_contents_into(target: &NodeRef, pre_node: &NodeRef) {
    let mut child = pre_node.first_child();
    while let Some(c) = child {
        let next = c.next_sibling();
        if let Some(elem) = c.as_element() {
            if &*elem.name.local == "code" {
                let mut grand = c.first_child();
                while let Some(g) = grand {
                    let g_next = g.next_sibling();
                    g.detach();
                    target.append(g);
                    grand = g_next;
                }
            }
        }
        child = next;
    }
}

/// Walk up from `node` looking for a preceding-sibling
/// `<pre class="rust item-decl">` at any ancestor level. Returns the
/// `<code>` child of that `<pre>` so callers can append directly to
/// the signature contents.
///
/// Used so the verus spec block can render *inside* the same item-decl
/// `<pre>` as the function signature (mirroring how rustdoc places
/// `<div class="where">` for ordinary `where` clauses), instead of as
/// a sibling block above the docblock.
fn find_item_decl_code(node: &NodeRef) -> Option<NodeRef> {
    let mut current = node.clone();
    loop {
        let mut sibling = current.previous_sibling();
        while let Some(sib) = sibling {
            if let Some(elem) = sib.as_element() {
                if &*elem.name.local == "pre" {
                    let attrs = elem.attributes.borrow();
                    if let Some(class) = attrs.get(local_name!("class")) {
                        if class.split_whitespace().any(|c| c == "item-decl") {
                            // Found the item-decl <pre>; descend to its
                            // <code> child (rustdoc always wraps the
                            // signature in <code> inside <pre>).
                            drop(attrs);
                            let mut child = sib.first_child();
                            while let Some(c) = child {
                                if let Some(e) = c.as_element() {
                                    if &*e.name.local == "code" {
                                        return Some(c);
                                    }
                                }
                                child = c.next_sibling();
                            }
                            return None;
                        }
                    }
                }
            }
            sibling = sib.previous_sibling();
        }
        current = current.parent()?;
    }
}

/// Check if the node is an element with the given tag name
fn is_element(node: &NodeRef, name: &str) -> bool {
    match node.as_element() {
        None => false,
        Some(data) => *data.name.local == *name,
    }
}

/// Returns node that looks like
///     <span class="verus-spec-keyword">requires</span>
/// (the inner text is given by `spec_name`
fn mk_spec_keyword_node(spec_name: &str) -> NodeRef {
    let t = NodeRef::new_text(spec_name);

    let qual_name = QualName::new(None, ns!(html), local_name!("span"));
    let nr = NodeRef::new_element(qual_name, vec![]);
    set_class_attribute(&nr, "verus-spec-keyword");
    nr.append(t);

    nr
}

/// <span class="verus-sig-keyword">{spec name}</span>
fn mk_sig_keyword_node(spec_name: &str) -> NodeRef {
    let t = NodeRef::new_text(spec_name);

    let qual_name = QualName::new(None, ns!(html), local_name!("span"));
    let nr = NodeRef::new_element(qual_name, vec![]);
    set_class_attribute(&nr, "verus-sig-keyword");
    nr.append(t);

    nr
}

/// <span class="verus-ret-name">{spec name}</span>
fn mk_ret_name_node(spec_name: &str) -> NodeRef {
    let t = NodeRef::new_text(spec_name);

    let qual_name = QualName::new(None, ns!(html), local_name!("span"));
    let nr = NodeRef::new_element(qual_name, vec![]);
    set_class_attribute(&nr, "verus-ret-name");
    nr.append(t);

    nr
}

/// <div class="verus-spec"></div>
fn mk_spec_node() -> NodeRef {
    let qual_name = QualName::new(None, ns!(html), local_name!("div"));
    let nr = NodeRef::new_element(qual_name, vec![]);
    set_class_attribute(&nr, "verus-spec");

    nr
}

fn set_class_attribute(nr: &NodeRef, value: &str) {
    let el = nr.as_element().unwrap();
    let mut attr_ref = el.attributes.borrow_mut();
    attr_ref.insert(local_name!("class"), value.to_string());
}

fn update_docblock(
    docblock_elem: &NodeRef,
    attrs: &Vec<VerusDocAttr>,
    opt_trait_info: &Option<TraitInfo>,
) {
    // The output mirrors the rustdoc `where`-clause shape:
    //
    //     <div class="verus-spec">requires
    //         <pre1>
    //         <pre2>
    //     ensures
    //         <pre3>
    //     </div>
    //
    // injected directly into the function's item-decl `<pre>` when one
    // is available (so the clauses read as a continuation of the
    // signature). On the fallback path (trait-method pages etc.) the
    // spec block is prepended to the docblock as before.

    if attrs.iter().find(|x| matches!(x, VerusDocAttr::BroadcastGroup)).is_some() {
        docblock_elem.append(mk_spec_keyword_node("broadcast group"));
    }

    // Group spec entries (requires / ensures / recommends / returns
    // / body) by keyword, preserving the canonical SPEC_NAMES order.
    let mut grouped: Vec<(&'static str, Vec<NodeRef>)> = vec![];
    for spec_name in SPEC_NAMES.iter() {
        let mut code_blocks: Vec<NodeRef> = attrs
            .iter()
            .filter_map(|a| match a {
                VerusDocAttr::Specification(s, nr) if s == spec_name => Some(nr.clone()),
                _ => None,
            })
            .collect();

        // De-duplicate identical spec blocks (can happen with `#[verus_spec]` + rustdoc pass).
        let mut seen: Vec<String> = Vec::new();
        code_blocks.retain(|nr| {
            let text = nr.text_contents();
            if seen.iter().any(|t| t == &text) {
                false
            } else {
                seen.push(text);
                true
            }
        });

        if !code_blocks.is_empty() {
            grouped.push((spec_name, code_blocks));
        }
    }

    if !grouped.is_empty() {
        // Try to inject the spec block into the function signature's
        // item-decl `<pre>` (where rustdoc puts ordinary `where`
        // clauses). If we find one, render the spec as a flat
        // sequence of literal-whitespace continuation lines —
        // matching the `<div class="where">where\n    K: …,</div>`
        // shape — so it visually flows from the closing paren of
        // the signature.
        //
        // If no item-decl pre is available (trait-method pages,
        // etc.) we fall back to the legacy block layout in the
        // docblock.
        if let Some(item_decl_code) = find_item_decl_code(docblock_elem) {
            // Build the spec block as a flat sequence inside the
            // item-decl <pre>:
            //   <newline before div>
            //   <div class="verus-spec">
            //     [keyword]
            //     "\n    " <inlined clause spans> for each clause
            //     "\n" [next keyword]
            //     ...
            //     <inlined body spans>   (body has no keyword and no
            //                             leading newline — the body's
            //                             content already starts at the
            //                             column it should render at,
            //                             i.e. "{" flush-left)
            //   </div>
            //
            // The literal `\n` we add *before* the div places the div
            // on its own line inside the surrounding <pre>; the CSS
            // keeps `.verus-spec` itself flowing inline so we don't
            // pick up an extra blank line on top of it.
            let spec_node = mk_spec_node();
            let mut first_chunk = true;
            for (spec_name, code_blocks) in grouped.iter() {
                let is_body = *spec_name == "body";
                if !is_body {
                    if !first_chunk {
                        spec_node.append(NodeRef::new_text("\n"));
                    }
                    spec_node.append(mk_spec_keyword_node(spec_name));
                    first_chunk = false;
                }
                for code_block in code_blocks.iter() {
                    if is_body {
                        if !first_chunk {
                            spec_node.append(NodeRef::new_text("\n"));
                        }
                    } else {
                        spec_node.append(NodeRef::new_text("\n    "));
                    }
                    inline_pre_contents_into(&spec_node, code_block);
                    first_chunk = false;
                }
            }
            item_decl_code.append(NodeRef::new_text("\n"));
            item_decl_code.append(spec_node);
        } else {
            // Fallback: keep the legacy block layout — keyword span
            // followed by `<pre class="verus-spec-code">` blocks.
            let mut elems: Vec<NodeRef> = vec![];
            for (spec_name, code_blocks) in grouped.into_iter() {
                let is_body = spec_name == "body";
                if !is_body {
                    elems.push(mk_spec_keyword_node(spec_name));
                }
                for code_block in code_blocks.into_iter() {
                    if is_body {
                        set_class_attribute(
                            &code_block,
                            "rust rest-example-rendered verus-body-code",
                        );
                    } else {
                        set_class_attribute(
                            &code_block,
                            "rust rest-example-rendered verus-spec-code",
                        );
                    }
                    elems.push(code_block);
                }
            }
            let spec_node = mk_spec_node();
            for elem in elems.into_iter() {
                spec_node.append(elem);
            }
            docblock_elem.prepend(spec_node);
        }
    }

    // Add mode info to the signature
    // If there are multiple ModeInfo attrs (caused by the `#[verus_spec]` macro expansion),
    // choose the one with a non-empty `ret_name`.
    let mode_infos: Vec<&DocSigInfo> = attrs
        .iter()
        .filter_map(|a| match a {
            VerusDocAttr::ModeInfo(info) => Some(info),
            _ => None,
        })
        .collect();
    let info_to_use =
        mode_infos.iter().find(|info| !info.ret_name.is_empty()).or_else(|| mode_infos.first());

    if let Some(info) = info_to_use {
        update_sig_info(docblock_elem, UpdateSigMode::DocSigInfo(info), opt_trait_info);
    } else {
        for attr in attrs.iter() {
            match attr {
                VerusDocAttr::BroadcastGroup => {
                    update_sig_info(docblock_elem, UpdateSigMode::BroadcastGroup, opt_trait_info);
                    break;
                }
                _ => {}
            }
        }
    }
}

enum UpdateSigMode<'a> {
    DocSigInfo(&'a DocSigInfo),
    BroadcastGroup,
}

struct TraitInfo {
    node: NodeRef,
}

fn update_sig_info(
    docblock_elem: &NodeRef,
    info: UpdateSigMode<'_>,
    opt_trait_info: &Option<TraitInfo>,
) {
    let Some((node, full_text, fn_idx)) = get_node_to_modify(docblock_elem) else {
        return;
    };

    do_splices_for_info(&node, &full_text, fn_idx, &info);

    if let Some(trait_info) = opt_trait_info {
        if matches!(info, UpdateSigMode::DocSigInfo(..)) {
            if let Some(fn_name) = get_fn_name(&full_text, fn_idx) {
                let text = trait_info.node.text_contents();
                if let Some(fn_idx) = get_fn_idx_in_trait(&trait_info.node, fn_name) {
                    do_splices_for_info(&trait_info.node, &text, fn_idx, &info);
                }
            }
        }
    }
}

fn do_splices_for_info(node: &NodeRef, full_text: &str, fn_idx: usize, info: &UpdateSigMode<'_>) {
    // The signature is a dom tree with special formatting for syntax highlighting and such.
    // We first collect it as pure text, to get a string like
    //    pub fn foo<...>(...) -> ...
    // We identify the locations where content needs to be inserted,
    // collected all in the 'splices' vec.
    // Then we add the content.

    let mut splices: Vec<(usize, usize, NodeRef)> = vec![];

    match info {
        UpdateSigMode::DocSigInfo(info) => {
            // TODO: separate these if possible
            let broadcast = if info.broadcast { "broadcast ".to_owned() } else { "".to_owned() };
            let fn_mode = format!("{:} ", info.fn_mode);
            let mode_str = broadcast + &fn_mode;
            splices.push((fn_idx, 0, mk_sig_keyword_node(&mode_str)));

            let arg0_idx = get_arg0_idx(&full_text, fn_idx);

            let mut arg_idx = arg0_idx;

            for i in 0..info.param_modes.len() {
                match info.param_modes[i] {
                    ParamMode::Default => {}
                    ParamMode::Tracked => {
                        splices.push((arg_idx, 0, mk_sig_keyword_node("tracked ")));
                    }
                }

                let name = get_arg_name(&full_text, arg_idx);
                if name.starts_with("verus_tmp_") {
                    let is_tracked = full_text[arg_idx + name.len() + 2..].starts_with("Tracked");
                    let is_ghost = full_text[arg_idx + name.len() + 2..].starts_with("Ghost");
                    if is_tracked {
                        splices.push((arg_idx, 0, mk_sig_keyword_node("Tracked")));
                        splices.push((arg_idx, 10, NodeRef::new_text("(")));
                        splices.push((arg_idx + name.len(), 0, NodeRef::new_text(")")));
                    } else if is_ghost {
                        splices.push((arg_idx, 0, mk_sig_keyword_node("Ghost")));
                        splices.push((arg_idx, 10, NodeRef::new_text("(")));
                        splices.push((arg_idx + name.len(), 0, NodeRef::new_text(")")));
                    }
                }

                arg_idx = next_comma_or_rparen(&full_text, arg_idx);

                // skip of the comma and space
                arg_idx += 2;
            }

            let needs_return_annotation =
                matches!(info.ret_mode, ParamMode::Tracked) || !info.ret_name.is_empty();

            // rustdoc commonly omits `-> ()` from signatures; if we need to insert return
            // annotations but can't find an arrow, treat it as `-> ()`.
            if needs_return_annotation && full_text[arg_idx..].find("->").is_none() {
                let rparen_idx = find_end_of_arg_list(full_text, arg0_idx - 1);
                let insert_idx = rparen_idx + 1;

                let qual_name = QualName::new(None, ns!(html), local_name!("span"));
                let container = NodeRef::new_element(qual_name, vec![]);
                container.append(NodeRef::new_text(" -> "));
                if matches!(info.ret_mode, ParamMode::Tracked) {
                    container.append(mk_sig_keyword_node("tracked "));
                }
                if !info.ret_name.is_empty() {
                    let ret_name_str = format!("{:} : ", info.ret_name);
                    container.append(mk_ret_name_node(&ret_name_str));
                }
                container.append(NodeRef::new_text("()"));
                splices.push((insert_idx, 0, container));
            } else {
                match info.ret_mode {
                    ParamMode::Default => {}
                    ParamMode::Tracked => {
                        if let Some(arrow_rel) = full_text[arg_idx..].find("->") {
                            let arrow_idx = arrow_rel + arg_idx;
                            let type_idx = arrow_idx + 3;
                            splices.push((type_idx, 0, mk_sig_keyword_node("tracked ")));
                        }
                    }
                };

                if !info.ret_name.is_empty() {
                    let string_to_insert = format!("{:} : ", info.ret_name);
                    if let Some(arrow_rel) = full_text[arg_idx..].find("->") {
                        let arrow_idx = arrow_rel + arg_idx;
                        let type_idx = arrow_idx + 3;
                        splices.push((type_idx, 0, mk_ret_name_node(&string_to_insert)));
                    }
                }
            }
        }
        UpdateSigMode::BroadcastGroup => {
            splices.push((fn_idx, 0, mk_sig_keyword_node("broadcast group ")));
        }
    }

    // Reverse order since inserting text should invalidate later indices
    for (idx, cut_len, new_node) in splices.into_iter().rev() {
        if cut_len > 0 {
            do_text_cut(&node, idx, cut_len);
        }

        let mut idx = idx;
        do_text_splice(&node, &new_node, &mut idx, true);
    }
}

fn find_end_of_arg_list(s: &str, open_paren_idx: usize) -> usize {
    let s: &[u8] = s.as_bytes();

    let mut depth: usize = 0;
    let mut i = open_paren_idx;
    loop {
        if s[i] == b'(' || s[i] == b'[' || s[i] == b'{' || s[i] == b'<' {
            depth += 1;
        } else if s[i] == b')' || s[i] == b']' || s[i] == b'}' || (s[i] == b'>' && s[i - 1] != b'-')
        {
            depth -= 1;
            if depth == 0 {
                return i;
            }
        }

        i += 1;
    }
}

fn get_node_to_modify(docblock_elem: &NodeRef) -> Option<(NodeRef, String, usize)> {
    let mut summary = docblock_elem.previous_sibling().unwrap();

    if summary.text_contents() == "Expand description" {
        // This is applicable to pages devoted to a single fn not inside an impl
        let details = summary.parent().unwrap();
        summary = details.previous_sibling().unwrap();
    }

    let code_header = summary.select_first(".code-header");
    match code_header {
        Ok(ch) => {
            summary = ch.as_node().clone();
        }
        Err(_) => {
            let code_block = summary.select_first("code");
            match code_block {
                Ok(cb) => {
                    summary = cb.as_node().clone();
                }
                Err(_) => {}
            }
        }
    }

    let full_text = summary.text_contents();
    let fn_idx = if let Some(fn_idx) = full_text.find("fn") {
        fn_idx
    } else {
        return None;
    };

    Some((summary, full_text, fn_idx))
}

fn get_arg_name(t: &str, idx: usize) -> &str {
    let b = t.as_bytes();
    let mut i = idx;
    while i < b.len() && b[i] != b':' {
        i += 1;
    }
    &t[idx..i]
}

/// traverse a dom tree, counting characters, to delete the specified text
fn do_text_cut(elem: &NodeRef, idx: usize, cut_len: usize) -> (usize, usize) {
    match elem.as_text() {
        Some(text_ref) => {
            let t: &mut String = &mut text_ref.borrow_mut();

            if idx < t.len() {
                let a = if idx + cut_len <= t.len() {
                    *t = t[0..idx].to_string() + &t[idx + cut_len..];
                    (0, 0)
                } else {
                    *t = t[0..idx].to_string();
                    (0, idx + cut_len - t.len())
                };

                if t.len() == 0 {
                    elem.detach();
                }

                a
            } else {
                (idx - t.len(), cut_len)
            }
        }
        None => {
            let mut child_opt = elem.first_child();
            let mut idx = idx;
            let mut cut_len = cut_len;
            while let Some(c) = child_opt {
                let next_child_opt = c.next_sibling();

                let (idx0, cut_len0) = do_text_cut(&c, idx, cut_len);
                idx = idx0;
                cut_len = cut_len0;
                if cut_len == 0 {
                    break;
                }

                child_opt = next_child_opt;
            }

            (idx, cut_len)
        }
    }
}

/// traverse a dom tree, counting characters, to insert the given node at the right spot
fn do_text_splice(elem: &NodeRef, inserted: &NodeRef, idx: &mut usize, root: bool) -> bool {
    match elem.as_text() {
        Some(text_ref) => {
            let t: &mut String = &mut text_ref.borrow_mut();

            if *idx == 0 {
                elem.insert_before(inserted.clone());
                true
            } else if *idx == t.len() && root {
                elem.insert_after(inserted.clone());
                true
            } else if *idx < t.len() {
                let second_text_node = NodeRef::new_text(&t[*idx..]);
                *t = t[..*idx].to_string();
                elem.insert_after(second_text_node);
                elem.insert_after(inserted.clone());
                true
            } else {
                *idx -= t.len();
                false
            }
        }
        None => {
            if *idx == 0 {
                elem.prepend(inserted.clone());
                return true;
            }

            let mut child_opt = elem.first_child();
            while let Some(c) = child_opt {
                let done = do_text_splice(&c, inserted, idx, false);
                if done {
                    return true;
                }

                child_opt = c.next_sibling();

                if *idx == 0 && (child_opt.is_some() || root) {
                    c.insert_after(inserted.clone());
                    return true;
                }
            }

            false
        }
    }
}

fn get_arg0_idx(s: &str, i: usize) -> usize {
    let s: &[u8] = s.as_bytes();

    let mut depth: usize = 0;
    let mut i = i;
    loop {
        if depth == 0 && s[i] == b'(' {
            return i + 1;
        } else if s[i] == b'(' || s[i] == b'[' || s[i] == b'{' || s[i] == b'<' {
            depth += 1;
        } else if s[i] == b')' || s[i] == b']' || s[i] == b'}' || (s[i] == b'>' && s[i - 1] != b'-')
        {
            depth -= 1;
        }

        i += 1;
    }
}

fn next_comma_or_rparen(s: &str, i: usize) -> usize {
    let s: &[u8] = s.as_bytes();

    let mut depth: usize = 0;
    let mut i = i;
    loop {
        if s[i] == b'(' || s[i] == b'[' || s[i] == b'{' || s[i] == b'<' {
            depth += 1;
        } else if depth == 0 && (s[i] == b',' || s[i] == b')') {
            return i;
        } else if s[i] == b')' || s[i] == b']' || s[i] == b'}' || (s[i] == b'>' && s[i - 1] != b'-')
        {
            depth -= 1;
        }

        i += 1;
    }
}

fn get_opt_trait_info(path: &Path, document: &NodeRef) -> Option<TraitInfo> {
    let filename = path.file_name().unwrap();
    if filename.to_string_lossy().starts_with("trait.") {
        let nodes: Vec<_> = document.select(".item-decl").expect("code selector").collect();
        if nodes.len() == 1 { Some(TraitInfo { node: nodes[0].as_node().clone() }) } else { None }
    } else {
        None
    }
}

/// Given string and index of 'fn some_name', returns 'some_name'
fn get_fn_name(full_text: &str, fn_idx: usize) -> Option<&str> {
    let idx = fn_idx + 3;
    let suff = &full_text[idx..];
    let m = match (suff.find("<"), suff.find("(")) {
        (None, None) => {
            return None;
        }
        (Some(x), None) => x,
        (None, Some(x)) => x,
        (Some(y), Some(x)) => std::cmp::min(y, x),
    };
    Some(&suff[..m])
}

fn get_fn_idx_in_trait(node: &NodeRef, fn_name: &str) -> Option<usize> {
    let possible_prefixes = [
        "// Required methods\n    ",
        "// Required method\n    ",
        "// Provided methods\n    ",
        "// Provided method\n    ",
        ";\n    ",
    ];
    let text = node.text_contents();
    for pref in possible_prefixes.iter() {
        let s = pref.to_string() + "fn " + fn_name;
        if let Some(idx) = text.find(s.as_str()) {
            let nextc = idx + pref.len() + 3 + fn_name.len();
            if nextc < text.len()
                && (text.as_bytes()[nextc] == b'<' || text.as_bytes()[nextc] == b'(')
            {
                return Some(idx + pref.len());
            }
        }
    }
    None
}

fn write_css(dir_path: &Path) {
    let css_dir = dir_path.join("static.files");

    let mut rustdoc_css_path = None;

    for dir_entry in std::fs::read_dir(css_dir).unwrap() {
        let path = dir_entry.unwrap().path();
        if let Some(filename_os) = path.file_name() {
            if let Some(filename) = filename_os.to_str() {
                if filename.starts_with("rustdoc-") {
                    rustdoc_css_path = Some(path);
                }
            }
        }
    }

    let rustdoc_css_path = rustdoc_css_path.unwrap();

    let mut rustdoc_css = std::fs::OpenOptions::new().append(true).open(rustdoc_css_path).unwrap();
    rustdoc_css
        .write_all(
            r#"
/* The spec block matches rustdoc's `<div class="where">` shape:
 * inside an item-decl `<pre>` it flows inline so that the literal
 * `\n` we emit before/inside it controls the layout (no extra blank
 * line from a block-level box), and on the fallback path (no
 * stand-alone item-decl pre — trait-method pages, etc.) it falls
 * back to a block-level display so it's still visually separated
 * from the surrounding doc text.
 */
.verus-spec {
  display: block;
}
pre .verus-spec {
  display: inline;
}

/* Use the theme's main text color for verus-only keywords so they
 * read the same as `pub`/`fn` in any theme (light/dark/ayu) instead
 * of the previous hardcoded dark-green that disappeared on dark
 * backgrounds. The italic flag keeps them visually distinct without
 * relying on color alone.
 */
.verus-sig-keyword,
.verus-spec-keyword {
  font-family: "Source Code Pro", monospace;
  color: var(--main-color, currentColor);
  font-style: italic;
}

/* Fallback path: trait-method pages and similar that don't expose a
 * stand-alone item-decl `<pre>` keep the legacy block layout, where
 * each clause is its own code block in the docblock. Reset the
 * surrounding pre's padding/background so it still reads as a
 * continuation of the surrounding text rather than a separate
 * snippet.
 */
.verus-spec-code,
.verus-body-code {
  padding: 0 !important;
  margin: 0;
  background: transparent !important;
  border: 0 !important;
}
.verus-spec-code code,
.verus-body-code code {
  background: transparent !important;
}
"#
            .as_bytes(),
        )
        .expect("write css file");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_sig_update(input_sig: &str, info: DocSigInfo) -> String {
        let html = format!(r#"<h4 class="code-header">{}</h4>"#, input_sig);
        let document = kuchiki::parse_html().one(html);
        let node = document.select_first("h4").unwrap().as_node().clone();

        let full_text = node.text_contents();
        let fn_idx = full_text.find("fn").unwrap();
        do_splices_for_info(&node, &full_text, fn_idx, &UpdateSigMode::DocSigInfo(&info));

        node.text_contents()
    }

    #[test]
    fn inserts_unit_return_arrow_when_missing_and_ret_name_present() {
        let out = run_sig_update(
            "pub fn foo()",
            DocSigInfo {
                fn_mode: "exec".to_string(),
                ret_mode: ParamMode::Default,
                ret_name: "r".to_string(),
                param_modes: vec![],
                broadcast: false,
            },
        );
        assert!(out.contains("foo() -> r : ()"), "out: {}", out);
    }

    #[test]
    fn inserts_unit_return_arrow_when_missing_and_ret_mode_tracked() {
        let out = run_sig_update(
            "pub fn foo()",
            DocSigInfo {
                fn_mode: "exec".to_string(),
                ret_mode: ParamMode::Tracked,
                ret_name: "".to_string(),
                param_modes: vec![],
                broadcast: false,
            },
        );
        assert!(out.contains("foo() -> tracked ()"), "out: {}", out);
    }

    #[test]
    fn does_not_insert_arrow_when_missing_and_no_return_annotations() {
        let out = run_sig_update(
            "pub fn foo()",
            DocSigInfo {
                fn_mode: "exec".to_string(),
                ret_mode: ParamMode::Default,
                ret_name: "".to_string(),
                param_modes: vec![],
                broadcast: false,
            },
        );
        assert!(!out.contains("->"), "out: {}", out);
    }
}
