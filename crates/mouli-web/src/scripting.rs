//! Scripts on a page: the `document` and its elements as JavaScript objects, events and timers.
//!
//! [`Scripting::new`] parses the page, runs every inline `<script>` in order (an error in one is
//! logged and the next still runs), then fires `DOMContentLoaded` and `load`. Afterwards the host
//! feeds in clicks and keys ([`Scripting::dispatch`]) and time ([`Scripting::advance`]); whenever
//! the page changed, [`Scripting::take_dirty`] says so and [`Scripting::snapshot`] gives the new
//! document to lay out again.
//!
//! The script sees: `document` (`getElementById`, `querySelector(All)`, `createElement`,
//! `createTextNode`, `body`, `head`, `title`, ...), elements (attributes, `textContent`,
//! `innerHTML`, `className`, `classList`, `style`, tree walking and editing, `matches`, `closest`,
//! `addEventListener` with bubbling, `onclick`-style properties, `dispatchEvent`, `click`),
//! `Event`/`CustomEvent`, `window` events, `alert` (written to the console). There is no network,
//! storage, `eval` or `Function` constructor, and no way to reach anything outside the page.

use crate::css::{self, State};
use crate::dom::{Dom, Kind as NodeKind, NodeId};
use crate::interp::{is_callable, Interp, Obj, Prop, Value, R};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

const WINDOW: usize = usize::MAX;

struct Host {
    dom: Dom,
    wrappers: HashMap<NodeId, Obj>,
    styles: HashMap<NodeId, Obj>,
    listeners: HashMap<(usize, String), Vec<Value>>,
    dirty: bool,
    proto: Obj,
}

type H = Rc<RefCell<Host>>;
type Get = fn(&mut Interp, &H, NodeId) -> R<Value>;
type Set = fn(&mut Interp, &H, NodeId, &Value) -> R<()>;
type Method = fn(&mut Interp, &H, NodeId, &[Value]) -> R<Value>;

/// A page with its scripts.
pub struct Scripting {
    interp: Interp,
    host: H,
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).cloned().unwrap_or(Value::Undefined)
}

fn hidden_prop(v: Value) -> Prop {
    Prop {
        value: v,
        get: None,
        set: None,
        enumerable: false,
    }
}

fn id_of(i: &mut Interp, this: &Value) -> R<NodeId> {
    if let Value::Obj(_) = this {
        if let Value::Num(n) = i.get_prop(this, "__id")? {
            return Ok(if n < 0.0 { WINDOW } else { n as usize });
        }
    }
    Err(i.make_error("TypeError", "Illegal invocation"))
}

fn wrap(i: &mut Interp, h: &H, id: NodeId) -> Value {
    if id == WINDOW {
        return Value::Obj(i.global_obj.clone());
    }
    if let Some(o) = h.borrow().wrappers.get(&id) {
        return Value::Obj(o.clone());
    }
    let proto = h.borrow().proto.clone();
    let o = i.new_obj();
    o.borrow_mut().proto = Some(proto);
    i.define(&o, "__id", hidden_prop(Value::Num(id as f64)));
    h.borrow_mut().wrappers.insert(id, o.clone());
    Value::Obj(o)
}

fn wrap_all(i: &mut Interp, h: &H, ids: &[NodeId]) -> Value {
    let items = ids.iter().map(|&n| wrap(i, h, n)).collect();
    i.new_array(items)
}

fn opt(i: &mut Interp, h: &H, id: Option<NodeId>) -> Value {
    id.map_or(Value::Null, |n| wrap(i, h, n))
}

fn dom_mut<T>(h: &H, f: impl FnOnce(&mut Dom) -> T) -> T {
    let mut b = h.borrow_mut();
    b.dirty = true;
    f(&mut b.dom)
}

fn accessor(i: &mut Interp, h: &H, o: &Obj, name: &str, get: Get, set: Option<Set>) {
    let hg = h.clone();
    let g = i.native(name, move |i, t, _| {
        let id = id_of(i, t)?;
        get(i, &hg, id)
    });
    let s = set.map(|set| {
        let hs = h.clone();
        i.native(name, move |i, t, a| {
            let id = id_of(i, t)?;
            set(i, &hs, id, &arg(a, 0))?;
            Ok(Value::Undefined)
        })
    });
    i.define(
        o,
        name,
        Prop {
            value: Value::Undefined,
            get: Some(g),
            set: s,
            enumerable: false,
        },
    );
}

fn method(i: &mut Interp, h: &H, o: &Obj, name: &str, f: Method) {
    let hh = h.clone();
    let fv = i.native(name, move |i, t, a| {
        let id = id_of(i, t)?;
        f(i, &hh, id, a)
    });
    i.define(o, name, hidden_prop(fv));
}

fn node_arg(i: &mut Interp, v: &Value) -> R<NodeId> {
    match id_of(i, v) {
        Ok(n) if n != WINDOW => Ok(n),
        _ => Err(i.make_error("TypeError", "the argument is not a node")),
    }
}

fn select_all(i: &mut Interp, h: &H, root: NodeId, sel: &str) -> R<Vec<NodeId>> {
    let parsed: Option<Vec<css::Selector>> = sel
        .split(',')
        .map(|s| css::parse_selector(s.trim()))
        .collect();
    let Some(sels) = parsed else {
        return Err(i.make_error("SyntaxError", &format!("'{sel}' is not a valid selector")));
    };
    let b = h.borrow();
    let st = State::default();
    Ok(b.dom
        .descendants(root)
        .into_iter()
        .filter(|&n| sels.iter().any(|s| css::matches(&b.dom, n, s, &st)))
        .collect())
}

fn kebab(name: &str) -> String {
    let mut o = String::new();
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            o.push('-');
            o.push(c.to_ascii_lowercase());
        } else {
            o.push(c);
        }
    }
    o
}

fn parse_style(text: &str) -> Vec<(String, String)> {
    text.split(';')
        .filter_map(|d| {
            let (k, v) = d.split_once(':')?;
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_string());
            (!k.is_empty()).then_some((k, v))
        })
        .collect()
}

fn write_style(decls: &[(String, String)]) -> String {
    decls
        .iter()
        .map(|(k, v)| format!("{k}: {v};"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn style_get(h: &H, id: NodeId, prop: &str) -> String {
    let b = h.borrow();
    let decls = parse_style(b.dom.attr(id, "style").unwrap_or(""));
    decls
        .into_iter()
        .rev()
        .find(|(k, _)| k == prop)
        .map(|(_, v)| v)
        .unwrap_or_default()
}

fn style_set(h: &H, id: NodeId, prop: &str, value: &str) {
    let text = h.borrow().dom.attr(id, "style").unwrap_or("").to_string();
    let mut decls = parse_style(&text);
    decls.retain(|(k, _)| k != prop);
    if !value.is_empty() {
        decls.push((prop.to_string(), value.to_string()));
    }
    let out = write_style(&decls);
    dom_mut(h, |d| d.set_attr(id, "style", &out));
}

const STYLE_PROPS: [&str; 52] = [
    "color",
    "backgroundColor",
    "background",
    "display",
    "width",
    "height",
    "minWidth",
    "maxWidth",
    "minHeight",
    "maxHeight",
    "margin",
    "marginTop",
    "marginRight",
    "marginBottom",
    "marginLeft",
    "padding",
    "paddingTop",
    "paddingRight",
    "paddingBottom",
    "paddingLeft",
    "border",
    "borderTop",
    "borderBottom",
    "borderRadius",
    "borderColor",
    "borderWidth",
    "fontSize",
    "fontWeight",
    "fontStyle",
    "fontFamily",
    "textAlign",
    "textDecoration",
    "lineHeight",
    "position",
    "top",
    "left",
    "right",
    "bottom",
    "opacity",
    "overflow",
    "visibility",
    "zIndex",
    "flex",
    "flexDirection",
    "justifyContent",
    "alignItems",
    "gap",
    "cursor",
    "transform",
    "boxShadow",
    "whiteSpace",
    "verticalAlign",
];

fn style_object(i: &mut Interp, h: &H, id: NodeId) -> Value {
    if let Some(o) = h.borrow().styles.get(&id) {
        return Value::Obj(o.clone());
    }
    let o = i.new_obj();
    for name in STYLE_PROPS {
        let css_name = kebab(name);
        let (hg, cg) = (h.clone(), css_name.clone());
        let g = i.native(name, move |_, _, _| {
            Ok(Value::str(&style_get(&hg, id, &cg)))
        });
        let (hs, cs) = (h.clone(), css_name);
        let s = i.native(name, move |i, _, a| {
            let v = i.text(&arg(a, 0))?;
            style_set(&hs, id, &cs, &v);
            Ok(Value::Undefined)
        });
        i.define(
            &o,
            name,
            Prop {
                value: Value::Undefined,
                get: Some(g),
                set: Some(s),
                enumerable: true,
            },
        );
    }
    let hh = h.clone();
    let f = i.native("setProperty", move |i, _, a| {
        let (k, v) = (i.text(&arg(a, 0))?, i.text(&arg(a, 1))?);
        style_set(&hh, id, &k.to_ascii_lowercase(), &v);
        Ok(Value::Undefined)
    });
    i.define(&o, "setProperty", hidden_prop(f));
    let hh = h.clone();
    let f = i.native("getPropertyValue", move |i, _, a| {
        let k = i.text(&arg(a, 0))?;
        Ok(Value::str(&style_get(&hh, id, &k.to_ascii_lowercase())))
    });
    i.define(&o, "getPropertyValue", hidden_prop(f));
    let hh = h.clone();
    let f = i.native("removeProperty", move |i, _, a| {
        let k = i.text(&arg(a, 0))?;
        let old = style_get(&hh, id, &k.to_ascii_lowercase());
        style_set(&hh, id, &k.to_ascii_lowercase(), "");
        Ok(Value::str(&old))
    });
    i.define(&o, "removeProperty", hidden_prop(f));
    let (hg, hs) = (h.clone(), h.clone());
    let g = i.native("cssText", move |_, _, _| {
        Ok(Value::str(hg.borrow().dom.attr(id, "style").unwrap_or("")))
    });
    let s = i.native("cssText", move |i, _, a| {
        let v = i.text(&arg(a, 0))?;
        dom_mut(&hs, |d| d.set_attr(id, "style", &v));
        Ok(Value::Undefined)
    });
    i.define(
        &o,
        "cssText",
        Prop {
            value: Value::Undefined,
            get: Some(g),
            set: Some(s),
            enumerable: false,
        },
    );
    h.borrow_mut().styles.insert(id, o.clone());
    Value::Obj(o)
}

fn class_list(i: &mut Interp, h: &H, id: NodeId) -> Value {
    let o = i.new_obj();
    type CF = fn(&mut Vec<String>, &str, Option<bool>) -> Option<bool>;
    fn set_classes(h: &H, id: NodeId, list: &[String]) {
        let v = list.join(" ");
        dom_mut(h, |d| d.set_attr(id, "class", &v));
    }
    let ops: [(&str, CF); 3] = [
        ("add", |l, c, _| {
            if !l.iter().any(|x| x == c) {
                l.push(c.to_string());
            }
            None
        }),
        ("remove", |l, c, _| {
            l.retain(|x| x != c);
            None
        }),
        ("toggle", |l, c, force| {
            let has = l.iter().any(|x| x == c);
            let want = force.unwrap_or(!has);
            if want && !has {
                l.push(c.to_string());
            } else if !want {
                l.retain(|x| x != c);
            }
            Some(want)
        }),
    ];
    for (name, op) in ops {
        let hh = h.clone();
        let f = i.native(name, move |i, _, a| {
            let force = match a.get(1) {
                Some(Value::Bool(b)) if name == "toggle" => Some(*b),
                _ => None,
            };
            let mut result = None;
            let names: Vec<Rc<str>> = if name == "toggle" {
                vec![i.text(&arg(a, 0))?]
            } else {
                let mut v = Vec::new();
                for x in a {
                    v.push(i.text(x)?);
                }
                v
            };
            for c in names {
                let mut list: Vec<String> = hh
                    .borrow()
                    .dom
                    .classes(id)
                    .into_iter()
                    .map(str::to_string)
                    .collect();
                result = op(&mut list, &c, force);
                set_classes(&hh, id, &list);
            }
            Ok(result.map_or(Value::Undefined, Value::Bool))
        });
        i.define(&o, name, hidden_prop(f));
    }
    let hh = h.clone();
    let f = i.native("contains", move |i, _, a| {
        let c = i.text(&arg(a, 0))?;
        Ok(Value::Bool(hh.borrow().dom.classes(id).contains(&&*c)))
    });
    i.define(&o, "contains", hidden_prop(f));
    Value::Obj(o)
}

fn attr_accessor(i: &mut Interp, h: &H, proto: &Obj, js: &'static str, attr: &'static str) {
    let hg = h.clone();
    let g = i.native(js, move |i, t, _| {
        let id = id_of(i, t)?;
        Ok(Value::str(hg.borrow().dom.attr(id, attr).unwrap_or("")))
    });
    let hs = h.clone();
    let s = i.native(js, move |i, t, a| {
        let id = id_of(i, t)?;
        let v = i.text(&arg(a, 0))?;
        dom_mut(&hs, |d| d.set_attr(id, attr, &v));
        Ok(Value::Undefined)
    });
    i.define(
        proto,
        js,
        Prop {
            value: Value::Undefined,
            get: Some(g),
            set: Some(s),
            enumerable: false,
        },
    );
}

fn hierarchy_error(i: &mut Interp) -> Value {
    i.make_error(
        "Error",
        "HierarchyRequestError: the node cannot be placed there",
    )
}

fn make_event(i: &mut Interp, kind: &str) -> Obj {
    let ev = i.new_obj();
    i.put(&ev, "type", Value::str(kind));
    i.put(&ev, "bubbles", Value::Bool(true));
    i.put(&ev, "defaultPrevented", Value::Bool(false));
    for (name, field) in [
        ("preventDefault", "defaultPrevented"),
        ("stopPropagation", "__stop"),
    ] {
        let f = i.native(name, move |i, t, _| {
            i.set_prop(t, field, Value::Bool(true))?;
            Ok(Value::Undefined)
        });
        i.define(&ev, name, hidden_prop(f));
    }
    ev
}

/// Delivers an event to `target` and up through its ancestors, then the window. Returns `false`
/// when a listener called `preventDefault`.
fn fire(i: &mut Interp, h: &H, target: usize, kind: &str, ev: &Value) -> R<bool> {
    let path: Vec<usize> = if target == WINDOW {
        vec![WINDOW]
    } else {
        let mut p: Vec<usize> = h.borrow().dom.ancestry(target);
        p.reverse();
        p.push(WINDOW);
        p
    };
    let t = wrap(i, h, target);
    i.set_prop(ev, "target", t)?;
    for node in path {
        let this = wrap(i, h, node);
        i.set_prop(ev, "currentTarget", this.clone())?;
        let mut fs: Vec<Value> = h
            .borrow()
            .listeners
            .get(&(node, kind.to_string()))
            .cloned()
            .unwrap_or_default();
        let handler = i.get_prop(&this, &format!("on{kind}"))?;
        if is_callable(&handler) {
            fs.insert(0, handler);
        }
        for f in fs {
            if let Err(e) = i.call_value(&f, this.clone(), vec![ev.clone()]) {
                let m = i.error_text(&e);
                i.console.push(m);
            }
        }
        if i.get_prop(ev, "__stop")?.truthy() {
            break;
        }
    }
    Ok(!i.get_prop(ev, "defaultPrevented")?.truthy())
}

fn install_node(i: &mut Interp, h: &H, p: &Obj) {
    accessor(
        i,
        h,
        p,
        "nodeType",
        |_, h, id| {
            Ok(Value::Num(match h.borrow().dom.node(id).map(|n| &n.kind) {
                Some(NodeKind::Element { .. }) => 1.0,
                Some(NodeKind::Text(_)) => 3.0,
                _ => 9.0,
            }))
        },
        None,
    );
    accessor(
        i,
        h,
        p,
        "tagName",
        |_, h, id| {
            Ok(h.borrow()
                .dom
                .tag(id)
                .map_or(Value::Undefined, |t| Value::str(&t.to_ascii_uppercase())))
        },
        None,
    );
    accessor(
        i,
        h,
        p,
        "nodeName",
        |_, h, id| {
            Ok(Value::str(
                &match h.borrow().dom.node(id).map(|n| &n.kind) {
                    Some(NodeKind::Element { tag, .. }) => tag.to_ascii_uppercase(),
                    Some(NodeKind::Text(_)) => "#text".into(),
                    _ => "#document".into(),
                },
            ))
        },
        None,
    );
    accessor(
        i,
        h,
        p,
        "textContent",
        |_, h, id| Ok(Value::str(&h.borrow().dom.text_content(id))),
        Some(|i, h, id, v| {
            let t = i.text(v)?;
            dom_mut(h, |d| d.set_text_content(id, &t));
            Ok(())
        }),
    );
    accessor(
        i,
        h,
        p,
        "nodeValue",
        |_, h, id| Ok(h.borrow().dom.text(id).map_or(Value::Null, Value::str)),
        Some(|i, h, id, v| {
            let t = i.text(v)?;
            dom_mut(h, |d| d.set_text_content(id, &t));
            Ok(())
        }),
    );
    accessor(
        i,
        h,
        p,
        "innerHTML",
        |_, h, id| Ok(Value::str(&h.borrow().dom.inner_html(id))),
        Some(|i, h, id, v| {
            let t = i.text(v)?;
            dom_mut(h, |d| d.set_inner_html(id, &t));
            h.borrow_mut().wrappers.retain(|&k, _| k != usize::MAX);
            Ok(())
        }),
    );
    accessor(
        i,
        h,
        p,
        "id",
        |_, h, id| Ok(Value::str(h.borrow().dom.attr(id, "id").unwrap_or(""))),
        Some(|i, h, id, v| {
            let t = i.text(v)?;
            dom_mut(h, |d| d.set_attr(id, "id", &t));
            Ok(())
        }),
    );
    accessor(
        i,
        h,
        p,
        "className",
        |_, h, id| Ok(Value::str(h.borrow().dom.attr(id, "class").unwrap_or(""))),
        Some(|i, h, id, v| {
            let t = i.text(v)?;
            dom_mut(h, |d| d.set_attr(id, "class", &t));
            Ok(())
        }),
    );
    for (js, attr) in [
        ("value", "value"),
        ("href", "href"),
        ("src", "src"),
        ("title", "title"),
        ("name", "name"),
        ("type", "type"),
        ("placeholder", "placeholder"),
    ] {
        attr_accessor(i, h, p, js, attr);
    }
    accessor(
        i,
        h,
        p,
        "style",
        |i, h, id| Ok(style_object(i, h, id)),
        None,
    );
    accessor(
        i,
        h,
        p,
        "classList",
        |i, h, id| Ok(class_list(i, h, id)),
        None,
    );
    accessor(
        i,
        h,
        p,
        "children",
        |i, h, id| {
            let ids = h.borrow().dom.element_children(id);
            Ok(wrap_all(i, h, &ids))
        },
        None,
    );
    accessor(
        i,
        h,
        p,
        "childNodes",
        |i, h, id| {
            let ids = h.borrow().dom.children(id).to_vec();
            Ok(wrap_all(i, h, &ids))
        },
        None,
    );
    accessor(
        i,
        h,
        p,
        "firstChild",
        |i, h, id| {
            let n = h.borrow().dom.children(id).first().copied();
            Ok(opt(i, h, n))
        },
        None,
    );
    accessor(
        i,
        h,
        p,
        "lastChild",
        |i, h, id| {
            let n = h.borrow().dom.children(id).last().copied();
            Ok(opt(i, h, n))
        },
        None,
    );
    accessor(
        i,
        h,
        p,
        "firstElementChild",
        |i, h, id| {
            let n = h.borrow().dom.element_children(id).first().copied();
            Ok(opt(i, h, n))
        },
        None,
    );
    accessor(
        i,
        h,
        p,
        "lastElementChild",
        |i, h, id| {
            let n = h.borrow().dom.element_children(id).last().copied();
            Ok(opt(i, h, n))
        },
        None,
    );
    accessor(
        i,
        h,
        p,
        "parentNode",
        |i, h, id| {
            let n = h.borrow().dom.parent(id);
            Ok(opt(i, h, n))
        },
        None,
    );
    accessor(
        i,
        h,
        p,
        "parentElement",
        |i, h, id| {
            let n = h
                .borrow()
                .dom
                .parent(id)
                .filter(|&q| h.borrow().dom.tag(q).is_some());
            Ok(opt(i, h, n))
        },
        None,
    );
    fn sibling(h: &H, id: NodeId, step: isize, elements: bool) -> Option<NodeId> {
        let b = h.borrow();
        let parent = b.dom.parent(id)?;
        let kids: Vec<NodeId> = if elements {
            b.dom.element_children(parent)
        } else {
            b.dom.children(parent).to_vec()
        };
        let at = kids.iter().position(|&k| k == id)? as isize + step;
        usize::try_from(at).ok().and_then(|a| kids.get(a).copied())
    }
    accessor(
        i,
        h,
        p,
        "nextSibling",
        |i, h, id| {
            let n = sibling(h, id, 1, false);
            Ok(opt(i, h, n))
        },
        None,
    );
    accessor(
        i,
        h,
        p,
        "previousSibling",
        |i, h, id| {
            let n = sibling(h, id, -1, false);
            Ok(opt(i, h, n))
        },
        None,
    );
    accessor(
        i,
        h,
        p,
        "nextElementSibling",
        |i, h, id| {
            let n = sibling(h, id, 1, true);
            Ok(opt(i, h, n))
        },
        None,
    );
    accessor(
        i,
        h,
        p,
        "previousElementSibling",
        |i, h, id| {
            let n = sibling(h, id, -1, true);
            Ok(opt(i, h, n))
        },
        None,
    );
}

fn install_methods(i: &mut Interp, h: &H, p: &Obj) {
    method(i, h, p, "getAttribute", |i, h, id, a| {
        let n = i.text(&arg(a, 0))?;
        Ok(h.borrow()
            .dom
            .attr(id, &n.to_ascii_lowercase())
            .map_or(Value::Null, Value::str))
    });
    method(i, h, p, "hasAttribute", |i, h, id, a| {
        let n = i.text(&arg(a, 0))?;
        Ok(Value::Bool(
            h.borrow().dom.attr(id, &n.to_ascii_lowercase()).is_some(),
        ))
    });
    method(i, h, p, "setAttribute", |i, h, id, a| {
        let (n, v) = (i.text(&arg(a, 0))?, i.text(&arg(a, 1))?);
        dom_mut(h, |d| d.set_attr(id, &n, &v));
        Ok(Value::Undefined)
    });
    method(i, h, p, "removeAttribute", |i, h, id, a| {
        let n = i.text(&arg(a, 0))?;
        dom_mut(h, |d| d.remove_attr(id, &n));
        Ok(Value::Undefined)
    });
    method(i, h, p, "appendChild", |i, h, id, a| {
        let c = node_arg(i, &arg(a, 0))?;
        if !dom_mut(h, |d| d.append_child(id, c)) {
            return Err(hierarchy_error(i));
        }
        Ok(arg(a, 0))
    });
    method(i, h, p, "insertBefore", |i, h, id, a| {
        let c = node_arg(i, &arg(a, 0))?;
        let r = if matches!(arg(a, 1), Value::Null | Value::Undefined) {
            None
        } else {
            Some(node_arg(i, &arg(a, 1))?)
        };
        if !dom_mut(h, |d| d.insert_before(id, c, r)) {
            return Err(hierarchy_error(i));
        }
        Ok(arg(a, 0))
    });
    method(i, h, p, "removeChild", |i, h, id, a| {
        let c = node_arg(i, &arg(a, 0))?;
        if h.borrow().dom.parent(c) != Some(id) {
            return Err(i.make_error(
                "Error",
                "NotFoundError: the node is not a child of this node",
            ));
        }
        dom_mut(h, |d| d.detach(c));
        Ok(arg(a, 0))
    });
    method(i, h, p, "remove", |_, h, id, _| {
        dom_mut(h, |d| d.detach(id));
        Ok(Value::Undefined)
    });
    method(i, h, p, "append", |i, h, id, a| {
        for x in a {
            let c = match x {
                Value::Obj(_) => node_arg(i, x)?,
                other => {
                    let t = i.text(other)?;
                    match dom_mut(h, |d| d.create_text(&t)) {
                        Some(n) => n,
                        None => continue,
                    }
                }
            };
            if !dom_mut(h, |d| d.append_child(id, c)) {
                return Err(hierarchy_error(i));
            }
        }
        Ok(Value::Undefined)
    });
    method(i, h, p, "cloneNode", |i, h, id, a| {
        let deep = arg(a, 0).truthy();
        let n = dom_mut(h, |d| d.clone_node(id, deep));
        Ok(opt(i, h, n))
    });
    method(i, h, p, "contains", |i, h, id, a| {
        let o = node_arg(i, &arg(a, 0))?;
        Ok(Value::Bool(h.borrow().dom.is_inside(o, id)))
    });
    method(i, h, p, "querySelector", |i, h, id, a| {
        let s = i.text(&arg(a, 0))?;
        let r = select_all(i, h, id, &s)?;
        Ok(opt(i, h, r.first().copied()))
    });
    method(i, h, p, "querySelectorAll", |i, h, id, a| {
        let s = i.text(&arg(a, 0))?;
        let r = select_all(i, h, id, &s)?;
        Ok(wrap_all(i, h, &r))
    });
    method(i, h, p, "getElementsByTagName", |i, h, id, a| {
        let t = i.text(&arg(a, 0))?.to_ascii_lowercase();
        let r: Vec<NodeId> = {
            let b = h.borrow();
            b.dom
                .descendants(id)
                .into_iter()
                .filter(|&n| t == "*" || b.dom.tag(n) == Some(&t))
                .collect()
        };
        Ok(wrap_all(i, h, &r))
    });
    method(i, h, p, "getElementsByClassName", |i, h, id, a| {
        let c = i.text(&arg(a, 0))?;
        let r: Vec<NodeId> = {
            let b = h.borrow();
            b.dom
                .descendants(id)
                .into_iter()
                .filter(|&n| b.dom.classes(n).contains(&&*c))
                .collect()
        };
        Ok(wrap_all(i, h, &r))
    });
    method(i, h, p, "getElementById", |i, h, _, a| {
        let n = i.text(&arg(a, 0))?;
        let r = h.borrow().dom.by_id(&n);
        Ok(opt(i, h, r))
    });
    method(i, h, p, "matches", |i, h, id, a| {
        let s = i.text(&arg(a, 0))?;
        let all = select_all(i, h, 0, &s)?;
        Ok(Value::Bool(all.contains(&id)))
    });
    method(i, h, p, "closest", |i, h, id, a| {
        let s = i.text(&arg(a, 0))?;
        let all = select_all(i, h, 0, &s)?;
        let mut cur = Some(id);
        while let Some(n) = cur {
            if all.contains(&n) {
                return Ok(wrap(i, h, n));
            }
            cur = h.borrow().dom.parent(n);
        }
        Ok(Value::Null)
    });
    method(i, h, p, "addEventListener", |i, h, id, a| {
        let kind = i.text(&arg(a, 0))?.to_string();
        let f = arg(a, 1);
        if is_callable(&f) {
            let mut b = h.borrow_mut();
            let list = b.listeners.entry((id, kind)).or_default();
            let same =
                |x: &Value| matches!((x, &f), (Value::Obj(p), Value::Obj(q)) if Rc::ptr_eq(p, q));
            if !list.iter().any(same) && list.len() < 100 {
                list.push(f);
            }
        }
        Ok(Value::Undefined)
    });
    method(i, h, p, "removeEventListener", |i, h, id, a| {
        let kind = i.text(&arg(a, 0))?.to_string();
        let f = arg(a, 1);
        if let Some(list) = h.borrow_mut().listeners.get_mut(&(id, kind)) {
            list.retain(|x| !matches!((x, &f), (Value::Obj(p), Value::Obj(q)) if Rc::ptr_eq(p, q)));
        }
        Ok(Value::Undefined)
    });
    method(i, h, p, "dispatchEvent", |i, h, id, a| {
        let ev = arg(a, 0);
        let kind = i.get_prop(&ev, "type")?;
        let kind = i.text(&kind)?.to_string();
        Ok(Value::Bool(fire(i, h, id, &kind, &ev)?))
    });
    method(i, h, p, "click", |i, h, id, _| {
        let ev = Value::Obj(make_event(i, "click"));
        fire(i, h, id, "click", &ev)?;
        Ok(Value::Undefined)
    });
    for name in ["focus", "blur"] {
        method(i, h, p, name, |_, _, _, _| Ok(Value::Undefined));
    }
}

fn install_document(i: &mut Interp, h: &H) {
    let doc = wrap(i, h, 0);
    let Value::Obj(d) = &doc else { return };
    accessor(
        i,
        h,
        d,
        "body",
        |i, h, _| {
            let n = h.borrow().dom.body();
            Ok(opt(i, h, n))
        },
        None,
    );
    accessor(
        i,
        h,
        d,
        "head",
        |i, h, _| {
            let n = h.borrow().dom.find_all("head").first().copied();
            Ok(opt(i, h, n))
        },
        None,
    );
    accessor(
        i,
        h,
        d,
        "documentElement",
        |i, h, _| {
            let n = h.borrow().dom.element_children(0).first().copied();
            Ok(opt(i, h, n))
        },
        None,
    );
    accessor(
        i,
        h,
        d,
        "title",
        |_, h, _| Ok(Value::str(&h.borrow().dom.title().unwrap_or_default())),
        Some(|i, h, _, v| {
            let t = i.text(v)?;
            let existing = h.borrow().dom.find_all("title").first().copied();
            let head = h.borrow().dom.find_all("head").first().copied();
            dom_mut(h, |d| {
                let node = existing.or_else(|| {
                    let n = d.create_element("title")?;
                    d.append_child(head?, n).then_some(n)
                });
                if let Some(n) = node {
                    d.set_text_content(n, &t);
                }
            });
            Ok(())
        }),
    );
    method(i, h, d, "createElement", |i, h, _, a| {
        let t = i.text(&arg(a, 0))?;
        match dom_mut(h, |d| d.create_element(&t)) {
            Some(n) => Ok(wrap(i, h, n)),
            None => Err(i.make_error("Error", "InvalidCharacterError or the document is full")),
        }
    });
    method(i, h, d, "createTextNode", |i, h, _, a| {
        let t = i.text(&arg(a, 0))?;
        match dom_mut(h, |d| d.create_text(&t)) {
            Some(n) => Ok(wrap(i, h, n)),
            None => Err(i.make_error("Error", "the document is full")),
        }
    });
    i.set_global("document", doc);
}

fn install_window(i: &mut Interp, h: &H) {
    let w = i.global_obj.clone();
    i.define(&w, "__id", hidden_prop(Value::Num(-1.0)));
    for name in ["addEventListener", "removeEventListener", "dispatchEvent"] {
        let proto = h.borrow().proto.clone();
        let f = i
            .get_prop(&Value::Obj(proto), name)
            .unwrap_or(Value::Undefined);
        i.define(&w, name, hidden_prop(f));
    }
    for name in ["Event", "CustomEvent"] {
        let f = i.native(name, |i, _, a| {
            let k = i.text(&arg(a, 0))?;
            let ev = make_event(i, &k);
            if let Value::Obj(opts) = arg(a, 1) {
                if let Some(d) = i
                    .get_prop(&Value::Obj(opts.clone()), "detail")
                    .ok()
                    .filter(|d| !matches!(d, Value::Undefined))
                {
                    i.put(&ev, "detail", d);
                }
                let b = i.get_prop(&Value::Obj(opts), "bubbles")?;
                if matches!(b, Value::Bool(false)) {
                    i.put(&ev, "bubbles", Value::Bool(false));
                }
            }
            Ok(Value::Obj(ev))
        });
        i.set_global(name, f);
    }
    let f = i.native("alert", |i, _, a| {
        let t = i.text(&arg(a, 0))?;
        i.console.push(format!("alert: {t}"));
        Ok(Value::Undefined)
    });
    i.set_global("alert", f);
}

impl Scripting {
    /// Parses `html`, runs its scripts and fires the load events.
    #[must_use]
    pub fn new(html: &str) -> Scripting {
        let dom = Dom::parse(html);
        let scripts = dom.inline_scripts();
        let mut interp = Interp::new();
        // Measured: about 3 KB of stack per call level in release builds (21 KB in debug).
        interp.max_depth = 48;
        let proto = interp.new_obj();
        let host: H = Rc::new(RefCell::new(Host {
            dom,
            wrappers: HashMap::new(),
            styles: HashMap::new(),
            listeners: HashMap::new(),
            dirty: false,
            proto: proto.clone(),
        }));
        install_node(&mut interp, &host, &proto);
        install_methods(&mut interp, &host, &proto);
        install_document(&mut interp, &host);
        install_window(&mut interp, &host);
        let mut s = Scripting { interp, host };
        for code in scripts {
            if let Err(e) = s.interp.run(&code) {
                s.interp.console.push(e);
            }
        }
        s.dispatch(0, "DOMContentLoaded");
        s.dispatch(WINDOW, "load");
        s
    }

    /// Delivers an event of `kind` to node `target` (0 is the document) and returns `false` if a
    /// listener prevented the default action.
    pub fn dispatch(&mut self, target: usize, kind: &str) -> bool {
        let ev = Value::Obj(make_event(&mut self.interp, kind));
        self.dispatch_with(target, kind, ev)
    }

    /// Like [`Scripting::dispatch`], with extra fields on the event (for example `key`).
    pub fn dispatch_fields(&mut self, target: usize, kind: &str, fields: &[(&str, Value)]) -> bool {
        let ev = make_event(&mut self.interp, kind);
        for (k, v) in fields {
            self.interp.put(&ev, k, v.clone());
        }
        self.dispatch_with(target, kind, Value::Obj(ev))
    }

    fn dispatch_with(&mut self, target: usize, kind: &str, ev: Value) -> bool {
        match fire(&mut self.interp, &self.host, target, kind, &ev) {
            Ok(ok) => ok,
            Err(e) => {
                let m = self.interp.error_text(&e);
                self.interp.console.push(m);
                true
            }
        }
    }

    /// Moves the virtual clock forward, running timers that fall due.
    pub fn advance(&mut self, ms: f64) -> usize {
        self.interp.advance(ms)
    }

    /// Whether the page changed since the last call.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.host.borrow_mut().dirty)
    }

    /// A copy of the current document, to lay out.
    #[must_use]
    pub fn snapshot(&self) -> Dom {
        self.host.borrow().dom.clone()
    }

    /// What the scripts wrote to the console, and their errors.
    #[must_use]
    pub fn console(&self) -> &[String] {
        &self.interp.console
    }

    /// How many timers are waiting.
    #[must_use]
    pub fn pending_timers(&self) -> usize {
        self.interp.pending_timers()
    }

    /// Runs more script text on the page (for tests and a future console).
    ///
    /// # Errors
    /// The error text.
    pub fn eval(&mut self, code: &str) -> Result<String, String> {
        let v = self.interp.run(code)?;
        Ok(self.interp.inspect(&v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn(f)
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn scripts_read_and_change_the_page() {
        with_stack(|| {
            let mut s = Scripting::new(
                "<h1 id=t>Hi</h1><ul id=l><li class=a>1</li><li>2</li></ul>
                 <script>
                   const t = document.getElementById('t');
                   t.textContent = 'Hello ' + document.querySelectorAll('li').length;
                   const li = document.createElement('li');
                   li.className = 'new x'; li.textContent = '3';
                   document.getElementById('l').appendChild(li);
                   li.classList.add('y'); li.classList.remove('x');
                   li.style.color = 'red'; li.style.backgroundColor = '#eee';
                   document.title = 'T!';
                   console.log(li.className, li.getAttribute('style'));
                 </script>",
            );
            let d = s.snapshot();
            assert_eq!(d.text_content(d.by_id("t").unwrap()), "Hello 2");
            assert_eq!(d.inner_html(d.by_id("l").unwrap()), "<li class=\"a\">1</li><li>2</li><li class=\"new y\" style=\"color: red; background-color: #eee;\">3</li>");
            assert_eq!(d.title().as_deref(), Some("T!"));
            assert_eq!(s.console(), ["new y color: red; background-color: #eee;"]);
            assert!(s.take_dirty());
            assert!(!s.take_dirty());
        });
    }

    #[test]
    fn selectors_and_tree_walking() {
        with_stack(|| {
            let mut s =
                Scripting::new("<div id=a class=box><p>x</p><p class=k>y</p><span>z</span></div>");
            let r = |s: &mut Scripting, c: &str| s.eval(c).unwrap();
            assert_eq!(
                r(&mut s, "document.querySelector('.box > p.k').textContent"),
                "y"
            );
            assert_eq!(
                r(&mut s, "document.querySelectorAll('p, span').length"),
                "3"
            );
            assert_eq!(r(&mut s, "const a = document.getElementById('a'); [a.children.length, a.firstElementChild.tagName, a.lastChild.tagName, a.children[1].previousElementSibling.textContent, a.children[2].nextSibling]"), "[ 3, 'P', 'SPAN', 'x', null ]");
            assert_eq!(
                r(&mut s, "a.querySelector('span').closest('.box') === a"),
                "true"
            );
            assert_eq!(r(&mut s, "[a.matches('div'), a.matches('p'), a.contains(a.children[0]), a.children[0].parentNode === a]"), "[ true, false, true, true ]");
            assert_eq!(r(&mut s, "document.querySelector('.nope')"), "null");
            assert!(s
                .eval("document.querySelector('[[')")
                .unwrap_err()
                .contains("SyntaxError"));
            assert_eq!(
                r(&mut s, "a.children[0].remove(); a.innerHTML"),
                "<p class=\"k\">y</p><span>z</span>"
            );
            assert_eq!(r(&mut s, "a.innerHTML = '<b>1</b>'; [a.children.length, a.children[0].tagName, document.body.lastElementChild === a]"), "[ 1, 'B', true ]");
            assert_eq!(r(&mut s, "document.getElementById('a') === a"), "true");
            assert!(s
                .eval("a.appendChild(a)")
                .unwrap_err()
                .contains("HierarchyRequestError"));
        });
    }

    #[test]
    fn events_bubble_and_can_be_prevented() {
        with_stack(|| {
            let mut s = Scripting::new(
                "<div id=o><button id=b>go</button></div>
                 <script>
                   const log = [];
                   const o = document.getElementById('o'), b = document.getElementById('b');
                   o.addEventListener('click', e => log.push('outer:' + e.target.id + '>' + e.currentTarget.id));
                   b.addEventListener('click', e => { log.push('button'); e.preventDefault(); });
                   b.onclick = () => log.push('prop');
                   document.addEventListener('click', () => log.push('doc'));
                   window.addEventListener('click', () => log.push('win'));
                   document.addEventListener('DOMContentLoaded', () => log.push('ready'));
                   window.addEventListener('load', () => log.push('load'));
                 </script>",
            );
            let b = s.snapshot().by_id("b").unwrap();
            assert!(!s.dispatch(b, "click"), "preventDefault is reported");
            assert_eq!(
                s.eval("log.join()").unwrap(),
                "'ready,load,prop,button,outer:b>o,doc,win'".trim_matches('\'')
            );
            // stopPropagation, removeEventListener, custom events with detail, keys.
            s.eval("const stop = e => { e.stopPropagation(); }; b.addEventListener('click', stop); log.length = 0;").unwrap();
            s.dispatch(b, "click");
            assert_eq!(s.eval("log.join()").unwrap(), "prop,button");
            s.eval("b.removeEventListener('click', stop); log.length = 0; o.addEventListener('ping', e => log.push(e.detail.n)); b.dispatchEvent(new CustomEvent('ping', { detail: { n: 7 } })); document.addEventListener('keydown', e => log.push(e.key));").unwrap();
            s.dispatch_fields(0, "keydown", &[("key", Value::str("Enter"))]);
            assert_eq!(s.eval("log.join()").unwrap(), "7,Enter");
            // An error in a listener is logged and the others still run.
            s.eval(
                "b.addEventListener('click', () => { throw new Error('bad') }); log.length = 0;",
            )
            .unwrap();
            s.dispatch(b, "click");
            assert!(s
                .console()
                .iter()
                .any(|l| l.contains("Uncaught Error: bad")));
            assert!(s.eval("log.join()").unwrap().contains("outer"));
        });
    }

    #[test]
    fn timers_change_the_page_over_time() {
        with_stack(|| {
            let mut s = Scripting::new("<p id=c>0</p><script>let n = 0; const p = document.getElementById('c'); setInterval(() => { p.textContent = ++n; }, 100); setTimeout(() => { p.style.display = 'none'; }, 250);</script>");
            s.take_dirty();
            s.advance(120.0);
            assert!(s.take_dirty());
            let d = s.snapshot();
            assert_eq!(d.text_content(d.by_id("c").unwrap()), "1");
            s.advance(200.0);
            let d = s.snapshot();
            assert_eq!(d.text_content(d.by_id("c").unwrap()), "3");
            assert_eq!(
                d.attr(d.by_id("c").unwrap(), "style"),
                Some("display: none;")
            );
        });
    }

    #[test]
    fn a_broken_script_does_not_stop_the_next_and_loops_are_halted() {
        with_stack(|| {
            let s = Scripting::new("<p id=a></p><script>null.x</script><script>document.getElementById('a').textContent = 'ok'</script><script>while (true) {}</script><script>document.getElementById('a').id = 'b'</script>");
            let d = s.snapshot();
            assert_eq!(d.text_content(d.by_id("b").unwrap()), "ok");
            assert_eq!(s.console().len(), 2);
            assert!(s.console()[0].contains("Cannot read properties of null"));
            assert!(s.console()[1].contains("ran for too long"));
        });
    }

    #[test]
    fn the_page_cannot_reach_outside() {
        with_stack(|| {
            let mut s = Scripting::new("<p>x</p>");
            for code in [
                "typeof fetch",
                "typeof XMLHttpRequest",
                "typeof localStorage",
                "typeof require",
                "typeof process",
                "typeof eval",
                "typeof Function('return 1')",
            ] {
                let r = s.eval(code);
                assert!(r == Ok("undefined".into()) || r.is_err(), "{code}: {r:?}");
            }
            assert!(s.eval("document.createElement('')").is_err());
            assert!(s
                .eval("Object.getPrototypeOf(document).appendChild.call({}, 1)")
                .is_err());
        });
    }
}
