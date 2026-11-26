//! The JavaScript standard library installed into every [`Interp`]: `Object`, `Function`,
//! `Array`, `String`, `Number`, `Boolean`, the error types, `Math`, `JSON`, `console`, timers,
//! `Map`, `Set`, `Promise`, `RegExp`, `Date` (UTC) and `performance.now`.

use crate::interp::{
    is_callable, make_bound, num_to_string, str_to_num, to_i32, Interp, Kind, MapData, MapKey, Obj,
    PromiseData, Prop, Value, R,
};
use crate::regex::{Flags, Match, Regex};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).cloned().unwrap_or(Value::Undefined)
}

fn hidden(v: Value) -> Prop {
    Prop {
        value: v,
        get: None,
        set: None,
        enumerable: false,
    }
}

fn method(
    i: &mut Interp,
    o: &Obj,
    name: &str,
    f: impl Fn(&mut Interp, &Value, &[Value]) -> R<Value> + 'static,
) {
    let fv = i.native(name, f);
    i.define(o, name, hidden(fv));
}

fn s(t: &str) -> Value {
    Value::str(t)
}

fn array_items(i: &mut Interp, v: &Value) -> R<Vec<Value>> {
    if let Value::Obj(o) = v {
        if let Kind::Array(a) = &o.borrow().kind {
            return Ok(a.clone());
        }
    }
    let _ = i;
    Err(i.make_error("TypeError", "this is not an array"))
}

fn rel(n: f64, len: usize) -> usize {
    if n.is_nan() {
        return 0;
    }
    if n < 0.0 {
        let r = len as f64 + n.trunc();
        if r < 0.0 {
            0
        } else {
            r as usize
        }
    } else {
        (n.trunc() as usize).min(len)
    }
}

fn num_arg(i: &mut Interp, a: &[Value], k: usize, default: f64) -> R<f64> {
    match a.get(k) {
        None | Some(Value::Undefined) => Ok(default),
        Some(v) => i.coerce_num(v),
    }
}

fn strict_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Num(x), Value::Num(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Obj(x), Value::Obj(y)) => Rc::ptr_eq(x, y),
        _ => false,
    }
}

fn same_value_zero(a: &Value, b: &Value) -> bool {
    matches!((a, b), (Value::Num(x), Value::Num(y)) if x.is_nan() && y.is_nan()) || strict_eq(a, b)
}

fn merge_sort(i: &mut Interp, v: Vec<Value>, cmp: &Value) -> R<Vec<Value>> {
    if v.len() <= 1 {
        return Ok(v);
    }
    let mid = v.len() / 2;
    let right = merge_sort(i, v[mid..].to_vec(), cmp)?;
    let left = merge_sort(i, v[..mid].to_vec(), cmp)?;
    let mut out = Vec::with_capacity(left.len() + right.len());
    let (mut a, mut b) = (0, 0);
    while a < left.len() && b < right.len() {
        let take_right = match (&left[a], &right[b]) {
            (Value::Undefined, _) => true,
            (_, Value::Undefined) => false,
            (x, y) => {
                if is_callable(cmp) {
                    let r = i.call_value(cmp, Value::Undefined, vec![x.clone(), y.clone()])?;
                    i.coerce_num(&r)? > 0.0
                } else {
                    i.text(x)? > i.text(y)?
                }
            }
        };
        if take_right {
            out.push(right[b].clone());
            b += 1;
        } else {
            out.push(left[a].clone());
            a += 1;
        }
    }
    out.extend_from_slice(&left[a..]);
    out.extend_from_slice(&right[b..]);
    Ok(out)
}

fn this_str(i: &mut Interp, this: &Value) -> R<Rc<str>> {
    if matches!(this, Value::Undefined | Value::Null) {
        return Err(i.make_error("TypeError", "String method called on null or undefined"));
    }
    i.text(this)
}

fn chars_of(t: &str) -> Vec<char> {
    t.chars().collect()
}

fn find_sub(hay: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from.min(hay.len()));
    }
    if needle.len() > hay.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&k| hay[k..k + needle.len()] == *needle)
}

fn quote(t: &str) -> String {
    let mut o = String::from("\"");
    for c in t.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn json_str(
    i: &mut Interp,
    v: &Value,
    indent: &str,
    cur: &str,
    seen: &mut Vec<Obj>,
) -> R<Option<String>> {
    Ok(Some(match v {
        Value::Undefined => return Ok(None),
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Num(n) => {
            if n.is_finite() {
                num_to_string(*n)
            } else {
                "null".into()
            }
        }
        Value::Str(t) => quote(t),
        Value::Obj(o) => {
            if is_callable(v) {
                return Ok(None);
            }
            if seen.iter().any(|x| Rc::ptr_eq(x, o)) {
                return Err(i.make_error("TypeError", "Converting circular structure to JSON"));
            }
            if seen.len() > 60 {
                return Err(i.make_error("RangeError", "the value is nested too deeply"));
            }
            let to_json = i.get_prop(v, "toJSON")?;
            if is_callable(&to_json) {
                let r = i.call_value(&to_json, v.clone(), Vec::new())?;
                return json_str(i, &r, indent, cur, seen);
            }
            seen.push(o.clone());
            let inner = format!("{cur}{indent}");
            let (open, close, sep) = if indent.is_empty() {
                (String::new(), String::new(), ",".to_string())
            } else {
                (
                    format!("\n{inner}"),
                    format!("\n{cur}"),
                    format!(",\n{inner}"),
                )
            };
            let arr = if let Kind::Array(a) = &o.borrow().kind {
                Some(a.clone())
            } else {
                None
            };
            let out = if let Some(items) = arr {
                let mut parts = Vec::new();
                for x in &items {
                    parts.push(
                        json_str(i, x, indent, &inner, seen)?.unwrap_or_else(|| "null".into()),
                    );
                }
                if parts.is_empty() {
                    "[]".into()
                } else {
                    format!("[{open}{}{close}]", parts.join(&sep))
                }
            } else {
                let mut parts = Vec::new();
                for k in i.own_keys(o) {
                    let x = i.get_prop(v, &k)?;
                    if let Some(t) = json_str(i, &x, indent, &inner, seen)? {
                        parts.push(format!(
                            "{}:{}{t}",
                            quote(&k),
                            if indent.is_empty() { "" } else { " " }
                        ));
                    }
                }
                if parts.is_empty() {
                    "{}".into()
                } else {
                    format!("{{{open}{}{close}}}", parts.join(&sep))
                }
            };
            seen.pop();
            out
        }
    }))
}

struct JsonParser<'a> {
    c: &'a [char],
    p: usize,
}

impl JsonParser<'_> {
    fn ws(&mut self) {
        while self.p < self.c.len() && self.c[self.p].is_whitespace() {
            self.p += 1;
        }
    }

    fn fail<T>(&self, i: &mut Interp) -> R<T> {
        Err(i.make_error(
            "SyntaxError",
            &format!("Unexpected token in JSON at position {}", self.p),
        ))
    }

    fn value(&mut self, i: &mut Interp, depth: usize) -> R<Value> {
        if depth > 100 {
            return Err(i.make_error("RangeError", "the JSON is nested too deeply"));
        }
        self.ws();
        let Some(&ch) = self.c.get(self.p) else {
            return self.fail(i);
        };
        match ch {
            '{' => {
                self.p += 1;
                let o = i.new_obj();
                self.ws();
                if self.c.get(self.p) == Some(&'}') {
                    self.p += 1;
                    return Ok(Value::Obj(o));
                }
                loop {
                    self.ws();
                    let k = self.string(i)?;
                    self.ws();
                    if self.c.get(self.p) != Some(&':') {
                        return self.fail(i);
                    }
                    self.p += 1;
                    let v = self.value(i, depth + 1)?;
                    i.put(&o, &k, v);
                    self.ws();
                    match self.c.get(self.p) {
                        Some(',') => self.p += 1,
                        Some('}') => {
                            self.p += 1;
                            return Ok(Value::Obj(o));
                        }
                        _ => return self.fail(i),
                    }
                }
            }
            '[' => {
                self.p += 1;
                let mut items = Vec::new();
                self.ws();
                if self.c.get(self.p) == Some(&']') {
                    self.p += 1;
                    return Ok(i.new_array(items));
                }
                loop {
                    items.push(self.value(i, depth + 1)?);
                    self.ws();
                    match self.c.get(self.p) {
                        Some(',') => self.p += 1,
                        Some(']') => {
                            self.p += 1;
                            return Ok(i.new_array(items));
                        }
                        _ => return self.fail(i),
                    }
                }
            }
            '"' => Ok(Value::Str(Rc::from(self.string(i)?.as_str()))),
            't' | 'f' | 'n' => {
                for (word, v) in [
                    ("true", Value::Bool(true)),
                    ("false", Value::Bool(false)),
                    ("null", Value::Null),
                ] {
                    let w: Vec<char> = word.chars().collect();
                    if self.c[self.p..].starts_with(&w) {
                        self.p += w.len();
                        return Ok(v);
                    }
                }
                self.fail(i)
            }
            _ => {
                let st = self.p;
                while self.p < self.c.len()
                    && matches!(self.c[self.p], '0'..='9' | '-' | '+' | '.' | 'e' | 'E')
                {
                    self.p += 1;
                }
                let t: String = self.c[st..self.p].iter().collect();
                match t.parse::<f64>() {
                    Ok(n) if !t.is_empty() => Ok(Value::Num(n)),
                    _ => {
                        self.p = st;
                        self.fail(i)
                    }
                }
            }
        }
    }

    fn string(&mut self, i: &mut Interp) -> R<String> {
        if self.c.get(self.p) != Some(&'"') {
            return self.fail(i);
        }
        self.p += 1;
        let mut out = String::new();
        while let Some(&ch) = self.c.get(self.p) {
            self.p += 1;
            match ch {
                '"' => return Ok(out),
                '\\' => {
                    let Some(&e) = self.c.get(self.p) else {
                        return self.fail(i);
                    };
                    self.p += 1;
                    match e {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        '/' | '\\' | '"' => out.push(e),
                        'u' => {
                            let h: String = self.c.iter().skip(self.p).take(4).collect();
                            let Ok(v) = u32::from_str_radix(&h, 16) else {
                                return self.fail(i);
                            };
                            self.p += 4;
                            out.push(char::from_u32(v).unwrap_or('\u{fffd}'));
                        }
                        _ => return self.fail(i),
                    }
                }
                c if (c as u32) < 0x20 => return self.fail(i),
                c => out.push(c),
            }
        }
        self.fail(i)
    }
}

fn to_radix(mut n: f64, radix: u32) -> String {
    if !n.is_finite() {
        return num_to_string(n);
    }
    let neg = n < 0.0;
    n = n.abs();
    let mut ip = n.trunc();
    let mut fp = n - ip;
    let mut digits = Vec::new();
    if ip == 0.0 {
        digits.push('0');
    }
    while ip >= 1.0 && digits.len() < 1100 {
        let d = (ip % f64::from(radix)) as u32;
        digits.push(char::from_digit(d, radix).unwrap_or('0'));
        ip = (ip / f64::from(radix)).trunc();
    }
    digits.reverse();
    let mut out: String = digits.into_iter().collect();
    if fp > 0.0 {
        out.push('.');
        for _ in 0..20 {
            fp *= f64::from(radix);
            let d = fp.trunc() as u32;
            out.push(char::from_digit(d, radix).unwrap_or('0'));
            fp -= f64::from(d);
            if fp <= 0.0 {
                break;
            }
        }
    }
    if neg {
        format!("-{out}")
    } else {
        out
    }
}

fn ctor(
    i: &mut Interp,
    name: &str,
    proto: &Obj,
    f: impl Fn(&mut Interp, &Value, &[Value]) -> R<Value> + 'static,
) -> Obj {
    let fv = i.native(name, f);
    let fo = fv.as_obj().cloned().unwrap_or_else(|| i.new_obj());
    i.define(&fo, "prototype", hidden(Value::Obj(proto.clone())));
    i.define(proto, "constructor", hidden(fv.clone()));
    i.set_global(name, fv);
    fo
}

pub(crate) fn install(i: &mut Interp) {
    let go = i.global_obj.clone();
    i.set_global("globalThis", Value::Obj(go.clone()));
    i.set_global("window", Value::Obj(go));
    i.set_global("NaN", Value::Num(f64::NAN));
    i.set_global("Infinity", Value::Num(f64::INFINITY));
    install_object(i);
    install_function(i);
    install_array(i);
    install_string(i);
    install_regexp(i);
    install_date(i);
    install_number(i);
    install_errors(i);
    install_collections(i);
    install_promise(i);
    install_math_json(i);
    install_globals(i);
}

fn install_object(i: &mut Interp) {
    let proto = i.protos.object.clone();
    let o = ctor(i, "Object", &proto, |i, _, a| match arg(a, 0) {
        Value::Undefined | Value::Null => Ok(Value::Obj(i.new_obj())),
        v => Ok(v),
    });
    method(i, &proto, "hasOwnProperty", |i, t, a| {
        let k = i.text(&arg(a, 0))?;
        Ok(Value::Bool(match t {
            Value::Obj(o) => {
                o.borrow().props.contains_key(&*k)
                    || matches!(&o.borrow().kind, Kind::Array(v) if k.parse::<usize>().is_ok_and(|n| n < v.len()))
            }
            _ => false,
        }))
    });
    method(i, &proto, "toString", |_, t, _| {
        Ok(s(match t {
            Value::Obj(o) if matches!(o.borrow().kind, Kind::Array(_)) => "[object Array]",
            Value::Null => "[object Null]",
            Value::Undefined => "[object Undefined]",
            _ => "[object Object]",
        }))
    });
    method(i, &proto, "valueOf", |_, t, _| Ok(t.clone()));
    method(i, &o, "keys", |i, _, a| {
        let keys = match arg(a, 0) {
            Value::Obj(o) => i.own_keys(&o),
            Value::Str(t) => (0..t.chars().count())
                .map(|n| Rc::from(n.to_string().as_str()))
                .collect(),
            _ => Vec::new(),
        };
        Ok(i.new_array(keys.into_iter().map(Value::Str).collect()))
    });
    method(i, &o, "values", |i, _, a| {
        let base = arg(a, 0);
        let mut out = Vec::new();
        if let Value::Obj(o) = &base {
            for k in i.own_keys(o) {
                out.push(i.get_prop(&base, &k)?);
            }
        }
        Ok(i.new_array(out))
    });
    method(i, &o, "entries", |i, _, a| {
        let base = arg(a, 0);
        let mut out = Vec::new();
        if let Value::Obj(o) = &base {
            for k in i.own_keys(o) {
                let v = i.get_prop(&base, &k)?;
                out.push(i.new_array(vec![Value::Str(k), v]));
            }
        }
        Ok(i.new_array(out))
    });
    method(i, &o, "fromEntries", |i, _, a| {
        let o = i.new_obj();
        for e in array_items(i, &arg(a, 0))? {
            let pair = array_items(i, &e)?;
            let k = i.text(&pair.first().cloned().unwrap_or(Value::Undefined))?;
            i.put(&o, &k, pair.get(1).cloned().unwrap_or(Value::Undefined));
        }
        Ok(Value::Obj(o))
    });
    method(i, &o, "assign", |i, _, a| {
        let target = arg(a, 0);
        for src in a.iter().skip(1) {
            if let Value::Obj(so) = src {
                for k in i.own_keys(so) {
                    let v = i.get_prop(src, &k)?;
                    i.set_prop(&target, &k, v)?;
                }
            }
        }
        Ok(target)
    });
    method(i, &o, "freeze", |_, _, a| {
        if let Value::Obj(o) = arg(a, 0) {
            o.borrow_mut().frozen = true;
        }
        Ok(arg(a, 0))
    });
    method(i, &o, "isFrozen", |_, _, a| {
        Ok(Value::Bool(
            matches!(arg(a, 0), Value::Obj(o) if o.borrow().frozen),
        ))
    });
    method(i, &o, "create", |i, _, a| {
        let p = arg(a, 0).as_obj().cloned();
        let ob = i.new_obj();
        ob.borrow_mut().proto = p;
        Ok(Value::Obj(ob))
    });
    method(i, &o, "getPrototypeOf", |_, _, a| {
        Ok(match arg(a, 0) {
            Value::Obj(o) => o.borrow().proto.clone().map_or(Value::Null, Value::Obj),
            _ => Value::Null,
        })
    });
    method(i, &o, "setPrototypeOf", |_, _, a| {
        if let Value::Obj(o) = arg(a, 0) {
            o.borrow_mut().proto = arg(a, 1).as_obj().cloned();
        }
        Ok(arg(a, 0))
    });
    method(i, &o, "getOwnPropertyNames", |i, _, a| {
        let names: Vec<Value> = match arg(a, 0) {
            Value::Obj(o) => {
                let b = o.borrow();
                let mut v: Vec<Value> = Vec::new();
                if let Kind::Array(x) = &b.kind {
                    v.extend((0..x.len()).map(|n| s(&n.to_string())));
                }
                v.extend(b.order.iter().map(|k| Value::Str(k.clone())));
                v
            }
            _ => Vec::new(),
        };
        Ok(i.new_array(names))
    });
    method(i, &o, "hasOwn", |i, _, a| {
        let k = i.text(&arg(a, 1))?;
        Ok(Value::Bool(
            matches!(arg(a, 0), Value::Obj(o) if o.borrow().props.contains_key(&*k)),
        ))
    });
    method(i, &o, "defineProperty", |i, _, a| {
        let (Value::Obj(target), desc) = (arg(a, 0), arg(a, 2)) else {
            return Err(i.make_error("TypeError", "Object.defineProperty called on a non-object"));
        };
        let key = i.text(&arg(a, 1))?;
        let get = i.get_prop(&desc, "get")?;
        let set = i.get_prop(&desc, "set")?;
        let value = i.get_prop(&desc, "value")?;
        let en = i.get_prop(&desc, "enumerable")?.truthy();
        i.define(
            &target,
            &key,
            Prop {
                value,
                get: is_callable(&get).then_some(get),
                set: is_callable(&set).then_some(set),
                enumerable: en,
            },
        );
        Ok(Value::Obj(target))
    });
}

fn install_function(i: &mut Interp) {
    let proto = i.protos.function.clone();
    let _ = ctor(i, "Function", &proto, |i, _, _| {
        Err(i.make_error("EvalError", "the Function constructor is not available"))
    });
    method(i, &proto, "call", |i, t, a| {
        i.call_value(t, arg(a, 0), a.iter().skip(1).cloned().collect())
    });
    method(i, &proto, "apply", |i, t, a| {
        let args = match arg(a, 1) {
            Value::Undefined | Value::Null => Vec::new(),
            v => array_items(i, &v)?,
        };
        i.call_value(t, arg(a, 0), args)
    });
    method(i, &proto, "bind", |i, t, a| {
        if !is_callable(t) {
            return Err(i.make_error("TypeError", "Bind must be called on a function"));
        }
        Ok(make_bound(
            i,
            t.clone(),
            arg(a, 0),
            a.iter().skip(1).cloned().collect(),
        ))
    });
    method(i, &proto, "toString", |i, t, _| {
        let n = i.get_prop(t, "name")?;
        Ok(s(&format!("function {}() {{ [code] }}", i.text(&n)?)))
    });
}

fn install_array(i: &mut Interp) {
    let proto = i.protos.array.clone();
    let a = ctor(i, "Array", &proto, |i, _, a| {
        if let [Value::Num(n)] = a {
            if *n < 0.0 || n.fract() != 0.0 || *n > 1_000_000.0 {
                return Err(i.make_error("RangeError", "Invalid array length"));
            }
            return Ok(i.new_array(vec![Value::Undefined; *n as usize]));
        }
        Ok(i.new_array(a.to_vec()))
    });
    method(i, &a, "isArray", |_, _, a| {
        Ok(Value::Bool(
            matches!(arg(a, 0), Value::Obj(o) if matches!(o.borrow().kind, Kind::Array(_))),
        ))
    });
    method(i, &a, "of", |i, _, a| Ok(i.new_array(a.to_vec())));
    method(i, &a, "from", |i, _, a| {
        let src = arg(a, 0);
        let mut items = match &src {
            Value::Str(t) => t.chars().map(|c| s(&c.to_string())).collect(),
            Value::Obj(o) if matches!(o.borrow().kind, Kind::Array(_)) => array_items(i, &src)?,
            Value::Obj(_) => {
                let n = i.get_prop(&src, "length")?;
                let n = i.coerce_num(&n)?;
                let n = if n.is_finite() && n > 0.0 {
                    (n as usize).min(1_000_000)
                } else {
                    0
                };
                let mut v = Vec::with_capacity(n);
                for k in 0..n {
                    v.push(i.get_prop(&src, &k.to_string())?);
                }
                v
            }
            _ => Vec::new(),
        };
        let f = arg(a, 1);
        if is_callable(&f) {
            for (k, x) in items.iter_mut().enumerate() {
                *x = i.call_value(&f, Value::Undefined, vec![x.clone(), Value::Num(k as f64)])?;
            }
        }
        Ok(i.new_array(items))
    });
    method(i, &proto, "push", |i, t, a| {
        let Value::Obj(o) = t else {
            return Err(i.make_error("TypeError", "push called on a non-array"));
        };
        let mut b = o.borrow_mut();
        let Kind::Array(v) = &mut b.kind else {
            drop(b);
            return Err(i.make_error("TypeError", "push called on a non-array"));
        };
        if v.len() + a.len() > 1_000_000 {
            drop(b);
            return Err(i.make_error("RangeError", "the array is too large"));
        }
        v.extend_from_slice(a);
        Ok(Value::Num(v.len() as f64))
    });
    method(i, &proto, "pop", |i, t, _| {
        let Value::Obj(o) = t else {
            return Err(i.make_error("TypeError", "pop called on a non-array"));
        };
        if let Kind::Array(v) = &mut o.borrow_mut().kind {
            return Ok(v.pop().unwrap_or(Value::Undefined));
        }
        Ok(Value::Undefined)
    });
    method(i, &proto, "shift", |i, t, _| {
        let Value::Obj(o) = t else {
            return Err(i.make_error("TypeError", "shift called on a non-array"));
        };
        if let Kind::Array(v) = &mut o.borrow_mut().kind {
            return Ok(if v.is_empty() {
                Value::Undefined
            } else {
                v.remove(0)
            });
        }
        Ok(Value::Undefined)
    });
    method(i, &proto, "unshift", |i, t, a| {
        let Value::Obj(o) = t else {
            return Err(i.make_error("TypeError", "unshift called on a non-array"));
        };
        if let Kind::Array(v) = &mut o.borrow_mut().kind {
            v.splice(0..0, a.iter().cloned());
            return Ok(Value::Num(v.len() as f64));
        }
        Ok(Value::Undefined)
    });
    method(i, &proto, "splice", |i, t, a| {
        let Value::Obj(o) = t else {
            return Err(i.make_error("TypeError", "splice called on a non-array"));
        };
        let len = array_items(i, t)?.len();
        let st = rel(num_arg(i, a, 0, 0.0)?, len);
        let del = if a.len() < 2 {
            len - st
        } else {
            (num_arg(i, a, 1, 0.0)?.max(0.0) as usize).min(len - st)
        };
        let removed: Vec<Value> = if let Kind::Array(v) = &mut o.borrow_mut().kind {
            v.splice(st..st + del, a.iter().skip(2).cloned()).collect()
        } else {
            Vec::new()
        };
        Ok(i.new_array(removed))
    });
    method(i, &proto, "reverse", |i, t, _| {
        let Value::Obj(o) = t else {
            return Err(i.make_error("TypeError", "reverse called on a non-array"));
        };
        if let Kind::Array(v) = &mut o.borrow_mut().kind {
            v.reverse();
        }
        Ok(t.clone())
    });
    method(i, &proto, "sort", |i, t, a| {
        let items = array_items(i, t)?;
        let sorted = merge_sort(i, items, &arg(a, 0))?;
        if let Value::Obj(o) = t {
            if let Kind::Array(v) = &mut o.borrow_mut().kind {
                *v = sorted;
            }
        }
        Ok(t.clone())
    });
    method(i, &proto, "fill", |i, t, a| {
        let Value::Obj(o) = t else {
            return Err(i.make_error("TypeError", "fill called on a non-array"));
        };
        let len = array_items(i, t)?.len();
        let st = rel(num_arg(i, a, 1, 0.0)?, len);
        let en = rel(num_arg(i, a, 2, len as f64)?, len);
        if let Kind::Array(v) = &mut o.borrow_mut().kind {
            for x in v.iter_mut().take(en).skip(st) {
                *x = arg(a, 0);
            }
        }
        Ok(t.clone())
    });
    method(i, &proto, "slice", |i, t, a| {
        let v = array_items(i, t)?;
        let st = rel(num_arg(i, a, 0, 0.0)?, v.len());
        let en = rel(num_arg(i, a, 1, v.len() as f64)?, v.len());
        Ok(i.new_array(if st < en {
            v[st..en].to_vec()
        } else {
            Vec::new()
        }))
    });
    method(i, &proto, "concat", |i, t, a| {
        let mut v = array_items(i, t)?;
        for x in a {
            match x {
                Value::Obj(o) if matches!(o.borrow().kind, Kind::Array(_)) => {
                    v.extend(array_items(i, x)?)
                }
                other => v.push(other.clone()),
            }
        }
        if v.len() > 1_000_000 {
            return Err(i.make_error("RangeError", "the array is too large"));
        }
        Ok(i.new_array(v))
    });
    method(i, &proto, "join", |i, t, a| {
        let v = array_items(i, t)?;
        let sep = match arg(a, 0) {
            Value::Undefined => Rc::from(","),
            x => i.text(&x)?,
        };
        let mut parts = Vec::with_capacity(v.len());
        for x in &v {
            parts.push(if matches!(x, Value::Undefined | Value::Null) {
                Rc::from("")
            } else {
                i.text(x)?
            });
        }
        Ok(s(&parts.join(&sep)))
    });
    method(i, &proto, "toString", |i, t, _| {
        let j = i.get_prop(t, "join")?;
        i.call_value(&j, t.clone(), Vec::new())
    });
    method(i, &proto, "indexOf", |i, t, a| {
        let v = array_items(i, t)?;
        let from = rel(num_arg(i, a, 1, 0.0)?, v.len());
        Ok(Value::Num(
            v.iter()
                .skip(from)
                .position(|x| strict_eq(x, &arg(a, 0)))
                .map_or(-1.0, |p| (p + from) as f64),
        ))
    });
    method(i, &proto, "lastIndexOf", |i, t, a| {
        let v = array_items(i, t)?;
        Ok(Value::Num(
            v.iter()
                .rposition(|x| strict_eq(x, &arg(a, 0)))
                .map_or(-1.0, |p| p as f64),
        ))
    });
    method(i, &proto, "includes", |i, t, a| {
        Ok(Value::Bool(
            array_items(i, t)?
                .iter()
                .any(|x| same_value_zero(x, &arg(a, 0))),
        ))
    });
    method(i, &proto, "at", |i, t, a| {
        let v = array_items(i, t)?;
        let n = num_arg(i, a, 0, 0.0)?.trunc();
        let k = if n < 0.0 { v.len() as f64 + n } else { n };
        Ok(if k >= 0.0 {
            v.get(k as usize).cloned().unwrap_or(Value::Undefined)
        } else {
            Value::Undefined
        })
    });
    method(i, &proto, "flat", |i, t, a| {
        let depth = num_arg(i, a, 0, 1.0)?;
        fn flat(i: &mut Interp, v: Vec<Value>, d: f64, out: &mut Vec<Value>) -> R<()> {
            for x in v {
                if d >= 1.0
                    && matches!(&x, Value::Obj(o) if matches!(o.borrow().kind, Kind::Array(_)))
                {
                    let inner = array_items(i, &x)?;
                    flat(i, inner, d - 1.0, out)?;
                } else {
                    out.push(x);
                }
            }
            Ok(())
        }
        let mut out = Vec::new();
        let items = array_items(i, t)?;
        flat(i, items, depth.min(50.0), &mut out)?;
        Ok(i.new_array(out))
    });
    method(i, &proto, "keys", |i, t, _| {
        let n = array_items(i, t)?.len();
        Ok(i.new_array((0..n).map(|k| Value::Num(k as f64)).collect()))
    });
    method(i, &proto, "entries", |i, t, _| {
        let v = array_items(i, t)?;
        let pairs = v
            .into_iter()
            .enumerate()
            .map(|(k, x)| i.new_array(vec![Value::Num(k as f64), x]))
            .collect();
        Ok(i.new_array(pairs))
    });
    // The callback family.
    type Step = fn(&mut Interp, &Value, &Value, usize, &Value, &mut Vec<Value>) -> R<bool>;
    fn each(
        i: &mut Interp,
        t: &Value,
        a: &[Value],
        mut step: impl FnMut(&mut Interp, Value, &Value, usize) -> R<bool>,
    ) -> R<()> {
        let f = arg(a, 0);
        if !is_callable(&f) {
            return Err(i.make_error("TypeError", "the callback is not a function"));
        }
        let items = array_items(i, t)?;
        let this_arg = arg(a, 1);
        for (k, x) in items.into_iter().enumerate() {
            let r = i.call_value(
                &f,
                this_arg.clone(),
                vec![x.clone(), Value::Num(k as f64), t.clone()],
            )?;
            if step(i, x, &r, k)? {
                break;
            }
        }
        Ok(())
    }
    let _: Option<Step> = None;
    method(i, &proto, "forEach", |i, t, a| {
        each(i, t, a, |_, _, _, _| Ok(false))?;
        Ok(Value::Undefined)
    });
    method(i, &proto, "map", |i, t, a| {
        let mut out = Vec::new();
        each(i, t, a, |_, _, r, _| {
            out.push(r.clone());
            Ok(false)
        })?;
        Ok(i.new_array(out))
    });
    method(i, &proto, "filter", |i, t, a| {
        let mut out = Vec::new();
        each(i, t, a, |_, x, r, _| {
            if r.truthy() {
                out.push(x);
            }
            Ok(false)
        })?;
        Ok(i.new_array(out))
    });
    method(i, &proto, "find", |i, t, a| {
        let mut found = Value::Undefined;
        each(i, t, a, |_, x, r, _| {
            if r.truthy() {
                found = x;
                return Ok(true);
            }
            Ok(false)
        })?;
        Ok(found)
    });
    method(i, &proto, "findIndex", |i, t, a| {
        let mut found = -1.0;
        each(i, t, a, |_, _, r, k| {
            if r.truthy() {
                found = k as f64;
                return Ok(true);
            }
            Ok(false)
        })?;
        Ok(Value::Num(found))
    });
    method(i, &proto, "some", |i, t, a| {
        let mut any = false;
        each(i, t, a, |_, _, r, _| {
            any = r.truthy();
            Ok(any)
        })?;
        Ok(Value::Bool(any))
    });
    method(i, &proto, "every", |i, t, a| {
        let mut all = true;
        each(i, t, a, |_, _, r, _| {
            all = r.truthy();
            Ok(!all)
        })?;
        Ok(Value::Bool(all))
    });
    method(i, &proto, "reduce", |i, t, a| {
        let f = arg(a, 0);
        if !is_callable(&f) {
            return Err(i.make_error("TypeError", "the callback is not a function"));
        }
        let items = array_items(i, t)?;
        let mut it = items.into_iter().enumerate();
        let mut acc = if a.len() > 1 {
            a[1].clone()
        } else {
            match it.next() {
                Some((_, x)) => x,
                None => {
                    return Err(
                        i.make_error("TypeError", "Reduce of empty array with no initial value")
                    )
                }
            }
        };
        for (k, x) in it {
            acc = i.call_value(
                &f,
                Value::Undefined,
                vec![acc, x, Value::Num(k as f64), t.clone()],
            )?;
        }
        Ok(acc)
    });
}

fn install_string(i: &mut Interp) {
    let proto = i.protos.string.clone();
    let so = ctor(i, "String", &proto, |i, this, a| {
        let t = match a.first() {
            None => Rc::from(""),
            Some(v) => i.text(v)?,
        };
        // `new String(x)` on a fresh object just returns the primitive text.
        let _ = this;
        Ok(Value::Str(t))
    });
    method(i, &so, "fromCharCode", |_, _, a| {
        let mut out = String::new();
        for v in a {
            if let Value::Num(n) = v {
                out.push(char::from_u32(*n as u32).unwrap_or('\u{fffd}'));
            }
        }
        Ok(s(&out))
    });
    method(i, &proto, "toString", |i, t, _| {
        Ok(Value::Str(this_str(i, t)?))
    });
    method(i, &proto, "valueOf", |i, t, _| {
        Ok(Value::Str(this_str(i, t)?))
    });
    method(i, &proto, "charAt", |i, t, a| {
        let c = chars_of(&this_str(i, t)?);
        let k = num_arg(i, a, 0, 0.0)?;
        Ok(s(&if k >= 0.0 {
            c.get(k as usize).map(char::to_string).unwrap_or_default()
        } else {
            String::new()
        }))
    });
    method(i, &proto, "at", |i, t, a| {
        let c = chars_of(&this_str(i, t)?);
        let n = num_arg(i, a, 0, 0.0)?.trunc();
        let k = if n < 0.0 { c.len() as f64 + n } else { n };
        Ok(if k >= 0.0 {
            c.get(k as usize)
                .map_or(Value::Undefined, |ch| s(&ch.to_string()))
        } else {
            Value::Undefined
        })
    });
    method(i, &proto, "charCodeAt", |i, t, a| {
        let c = chars_of(&this_str(i, t)?);
        let k = num_arg(i, a, 0, 0.0)?;
        Ok(Value::Num(if k >= 0.0 {
            c.get(k as usize)
                .map_or(f64::NAN, |ch| f64::from(*ch as u32))
        } else {
            f64::NAN
        }))
    });
    method(i, &proto, "codePointAt", |i, t, a| {
        let c = chars_of(&this_str(i, t)?);
        let k = num_arg(i, a, 0, 0.0)?;
        Ok(if k >= 0.0 {
            c.get(k as usize)
                .map_or(Value::Undefined, |ch| Value::Num(f64::from(*ch as u32)))
        } else {
            Value::Undefined
        })
    });
    method(i, &proto, "indexOf", |i, t, a| {
        let h = chars_of(&this_str(i, t)?);
        let n = chars_of(&i.text(&arg(a, 0))?);
        let from = rel(num_arg(i, a, 1, 0.0)?, h.len());
        Ok(Value::Num(
            find_sub(&h, &n, from).map_or(-1.0, |p| p as f64),
        ))
    });
    method(i, &proto, "lastIndexOf", |i, t, a| {
        let h = chars_of(&this_str(i, t)?);
        let n = chars_of(&i.text(&arg(a, 0))?);
        let mut last = -1.0;
        let mut from = 0;
        while let Some(p) = find_sub(&h, &n, from) {
            last = p as f64;
            from = p + 1;
            if from > h.len() {
                break;
            }
        }
        Ok(Value::Num(last))
    });
    method(i, &proto, "includes", |i, t, a| {
        let h = chars_of(&this_str(i, t)?);
        let n = chars_of(&i.text(&arg(a, 0))?);
        Ok(Value::Bool(find_sub(&h, &n, 0).is_some()))
    });
    method(i, &proto, "startsWith", |i, t, a| {
        let h = this_str(i, t)?;
        let n = i.text(&arg(a, 0))?;
        let from = num_arg(i, a, 1, 0.0)?.max(0.0) as usize;
        let rest: String = h.chars().skip(from).collect();
        Ok(Value::Bool(rest.starts_with(&*n)))
    });
    method(i, &proto, "endsWith", |i, t, a| {
        let h = this_str(i, t)?;
        let n = i.text(&arg(a, 0))?;
        Ok(Value::Bool(h.ends_with(&*n)))
    });
    method(i, &proto, "slice", |i, t, a| {
        let c = chars_of(&this_str(i, t)?);
        let st = rel(num_arg(i, a, 0, 0.0)?, c.len());
        let en = rel(num_arg(i, a, 1, c.len() as f64)?, c.len());
        Ok(s(&if st < en {
            c[st..en].iter().collect::<String>()
        } else {
            String::new()
        }))
    });
    method(i, &proto, "substring", |i, t, a| {
        let c = chars_of(&this_str(i, t)?);
        let clamp = |n: f64| {
            if n.is_nan() || n < 0.0 {
                0
            } else {
                (n as usize).min(c.len())
            }
        };
        let (x, y) = (
            clamp(num_arg(i, a, 0, 0.0)?),
            clamp(num_arg(i, a, 1, c.len() as f64)?),
        );
        let (lo, hi) = if x <= y { (x, y) } else { (y, x) };
        Ok(s(&c[lo..hi].iter().collect::<String>()))
    });
    method(i, &proto, "substr", |i, t, a| {
        let c = chars_of(&this_str(i, t)?);
        let st = rel(num_arg(i, a, 0, 0.0)?, c.len());
        let n = num_arg(i, a, 1, (c.len() - st) as f64)?.max(0.0) as usize;
        Ok(s(&c[st..(st + n).min(c.len())].iter().collect::<String>()))
    });
    method(i, &proto, "toUpperCase", |i, t, _| {
        Ok(s(&this_str(i, t)?.to_uppercase()))
    });
    method(i, &proto, "toLowerCase", |i, t, _| {
        Ok(s(&this_str(i, t)?.to_lowercase()))
    });
    method(i, &proto, "trim", |i, t, _| Ok(s(this_str(i, t)?.trim())));
    method(i, &proto, "trimStart", |i, t, _| {
        Ok(s(this_str(i, t)?.trim_start()))
    });
    method(i, &proto, "trimEnd", |i, t, _| {
        Ok(s(this_str(i, t)?.trim_end()))
    });
    method(i, &proto, "repeat", |i, t, a| {
        let h = this_str(i, t)?;
        let n = num_arg(i, a, 0, 0.0)?;
        if n < 0.0 || !n.is_finite() || h.len() as f64 * n > crate::interp::MAX_STRING as f64 {
            return Err(i.make_error("RangeError", "Invalid string length"));
        }
        Ok(s(&h.repeat(n as usize)))
    });
    for (name, start) in [("padStart", true), ("padEnd", false)] {
        method(i, &proto, name, move |i, t, a| {
            let h = this_str(i, t)?;
            let target = num_arg(i, a, 0, 0.0)?
                .max(0.0)
                .min(crate::interp::MAX_STRING as f64) as usize;
            let fill = match arg(a, 1) {
                Value::Undefined => Rc::from(" "),
                x => i.text(&x)?,
            };
            let len = h.chars().count();
            if fill.is_empty() || target <= len {
                return Ok(Value::Str(h));
            }
            let pad: String = fill.chars().cycle().take(target - len).collect();
            Ok(s(&if start {
                format!("{pad}{h}")
            } else {
                format!("{h}{pad}")
            }))
        });
    }
    method(i, &proto, "concat", |i, t, a| {
        let mut out = this_str(i, t)?.to_string();
        for x in a {
            out.push_str(&i.text(x)?);
        }
        Ok(s(&out))
    });
    method(i, &proto, "split", |i, t, a| {
        let h = this_str(i, t)?;
        let limit = num_arg(i, a, 1, 1_000_000.0)?.max(0.0) as usize;
        let parts: Vec<Value> = match arg(a, 0) {
            Value::Undefined => vec![Value::Str(h)],
            sep => {
                let sep = i.text(&sep)?;
                if sep.is_empty() {
                    h.chars().map(|c| s(&c.to_string())).collect()
                } else {
                    h.split(&*sep).map(s).collect()
                }
            }
        };
        Ok(i.new_array(parts.into_iter().take(limit).collect()))
    });
    for (name, all) in [("replace", false), ("replaceAll", true)] {
        method(i, &proto, name, move |i, t, a| {
            let h = this_str(i, t)?;
            if matches!(arg(a, 0), Value::Obj(_)) {
                return Err(i.make_error("TypeError", "regular expressions are not supported yet"));
            }
            let pat = i.text(&arg(a, 0))?;
            let rep = arg(a, 1);
            let mut out = String::new();
            let mut rest: &str = &h;
            let mut first = true;
            while let Some(p) = if pat.is_empty() && !first {
                None
            } else {
                rest.find(&*pat)
            } {
                out.push_str(&rest[..p]);
                let r = if is_callable(&rep) {
                    let v = i.call_value(&rep, Value::Undefined, vec![Value::Str(pat.clone())])?;
                    i.text(&v)?.to_string()
                } else {
                    i.text(&rep)?.to_string()
                };
                out.push_str(&r);
                if pat.is_empty() {
                    break;
                }
                rest = &rest[p + pat.len()..];
                first = false;
                if !all {
                    break;
                }
            }
            out.push_str(rest);
            if out.len() > crate::interp::MAX_STRING {
                return Err(i.make_error("RangeError", "the string is too long"));
            }
            Ok(s(&out))
        });
    }
    method(i, &proto, "localeCompare", |i, t, a| {
        let (x, y) = (this_str(i, t)?, i.text(&arg(a, 0))?);
        Ok(Value::Num(match x.cmp(&y) {
            std::cmp::Ordering::Less => -1.0,
            std::cmp::Ordering::Equal => 0.0,
            std::cmp::Ordering::Greater => 1.0,
        }))
    });
}

fn install_number(i: &mut Interp) {
    let np = i.protos.number.clone();
    let n = ctor(i, "Number", &np, |i, _, a| {
        Ok(Value::Num(match a.first() {
            None => 0.0,
            Some(v) => i.coerce_num(v)?,
        }))
    });
    for (k, v) in [
        ("MAX_SAFE_INTEGER", 9_007_199_254_740_991.0),
        ("MIN_SAFE_INTEGER", -9_007_199_254_740_991.0),
        ("EPSILON", f64::EPSILON),
        ("MAX_VALUE", f64::MAX),
        ("MIN_VALUE", 5e-324),
        ("POSITIVE_INFINITY", f64::INFINITY),
        ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
        ("NaN", f64::NAN),
    ] {
        i.put(&n, k, Value::Num(v));
    }
    method(i, &n, "isInteger", |_, _, a| {
        Ok(Value::Bool(
            matches!(arg(a, 0), Value::Num(x) if x.is_finite() && x.fract() == 0.0),
        ))
    });
    method(i, &n, "isSafeInteger", |_, _, a| {
        Ok(Value::Bool(
            matches!(arg(a, 0), Value::Num(x) if x.is_finite() && x.fract() == 0.0 && x.abs() <= 9_007_199_254_740_991.0),
        ))
    });
    method(i, &n, "isFinite", |_, _, a| {
        Ok(Value::Bool(
            matches!(arg(a, 0), Value::Num(x) if x.is_finite()),
        ))
    });
    method(i, &n, "isNaN", |_, _, a| {
        Ok(Value::Bool(
            matches!(arg(a, 0), Value::Num(x) if x.is_nan()),
        ))
    });
    method(i, &np, "toFixed", |i, t, a| {
        let x = i.coerce_num(t)?;
        let d = num_arg(i, a, 0, 0.0)?;
        if !(0.0..=100.0).contains(&d) {
            return Err(i.make_error(
                "RangeError",
                "toFixed() digits argument must be between 0 and 100",
            ));
        }
        Ok(s(&if x.is_finite() && x.abs() < 1e21 {
            format!("{:.*}", d as usize, x)
        } else {
            num_to_string(x)
        }))
    });
    method(i, &np, "toString", |i, t, a| {
        let x = i.coerce_num(t)?;
        let r = num_arg(i, a, 0, 10.0)?;
        if !(2.0..=36.0).contains(&r) {
            return Err(i.make_error("RangeError", "toString() radix must be between 2 and 36"));
        }
        Ok(s(&if r == 10.0 {
            num_to_string(x)
        } else {
            to_radix(x, r as u32)
        }))
    });
    method(i, &np, "valueOf", |i, t, _| {
        Ok(Value::Num(i.coerce_num(t)?))
    });
    let bp = i.protos.boolean.clone();
    let _ = ctor(i, "Boolean", &bp, |_, _, a| {
        Ok(Value::Bool(arg(a, 0).truthy()))
    });
    method(i, &bp, "toString", |_, t, _| {
        Ok(s(if t.truthy() { "true" } else { "false" }))
    });
    method(i, &bp, "valueOf", |_, t, _| Ok(t.clone()));
}

fn install_errors(i: &mut Interp) {
    let kinds: Vec<(&'static str, Obj)> = i
        .protos
        .errors
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect();
    let base = i.protos.errors.get("Error").cloned();
    for (kind, proto) in kinds {
        i.define(&proto, "name", hidden(s(kind)));
        i.define(&proto, "message", hidden(s("")));
        let ctor_obj = ctor(i, kind, &proto, move |i, this, a| {
            let msg = match a.first() {
                None | Some(Value::Undefined) => None,
                Some(v) => Some(i.text(v)?),
            };
            let target = match this {
                Value::Obj(o)
                    if o.borrow().class == "Object"
                        && o.borrow().props.is_empty()
                        && !Rc::ptr_eq(o, &i.global_obj) =>
                {
                    o.borrow_mut().class = "Error";
                    Value::Obj(o.clone())
                }
                _ => i.make_error(kind, ""),
            };
            if let (Some(m), Value::Obj(o)) = (msg, &target) {
                i.define(o, "message", hidden(Value::Str(m)));
            }
            Ok(target)
        });
        if kind != "Error" {
            if let Some(b) = i.get_global("Error").and_then(|e| e.as_obj().cloned()) {
                ctor_obj.borrow_mut().proto = Some(b);
            }
        }
    }
    if let Some(b) = base {
        method(i, &b, "toString", |i, t, _| {
            let n = i.get_prop(t, "name")?;
            let m = i.get_prop(t, "message")?;
            let (n, m) = (i.text(&n)?, i.text(&m)?);
            Ok(s(&if m.is_empty() {
                n.to_string()
            } else {
                format!("{n}: {m}")
            }))
        });
    }
}

fn install_math_json(i: &mut Interp) {
    let m = i.new_obj();
    for (k, v) in [
        ("PI", std::f64::consts::PI),
        ("E", std::f64::consts::E),
        ("LN2", std::f64::consts::LN_2),
        ("LN10", std::f64::consts::LN_10),
        ("SQRT2", std::f64::consts::SQRT_2),
        ("LOG2E", std::f64::consts::LOG2_E),
        ("LOG10E", std::f64::consts::LOG10_E),
    ] {
        i.put(&m, k, Value::Num(v));
    }
    type Unary = (&'static str, fn(f64) -> f64);
    let one: [Unary; 20] = [
        ("abs", f64::abs),
        ("floor", f64::floor),
        ("ceil", f64::ceil),
        ("trunc", f64::trunc),
        ("sqrt", f64::sqrt),
        ("cbrt", f64::cbrt),
        ("sin", f64::sin),
        ("cos", f64::cos),
        ("tan", f64::tan),
        ("asin", f64::asin),
        ("acos", f64::acos),
        ("atan", f64::atan),
        ("exp", f64::exp),
        ("log", f64::ln),
        ("log2", f64::log2),
        ("log10", f64::log10),
        ("round", |x| (x + 0.5).floor()),
        ("sign", |x| {
            if x.is_nan() || x == 0.0 {
                x
            } else {
                x.signum()
            }
        }),
        ("sinh", f64::sinh),
        ("cosh", f64::cosh),
    ];
    for (name, f) in one {
        method(i, &m, name, move |i, _, a| {
            Ok(Value::Num(f(num_arg(i, a, 0, f64::NAN)?)))
        });
    }
    method(i, &m, "pow", |i, _, a| {
        let (x, y) = (num_arg(i, a, 0, f64::NAN)?, num_arg(i, a, 1, f64::NAN)?);
        Ok(Value::Num(if y == 0.0 { 1.0 } else { x.powf(y) }))
    });
    method(i, &m, "atan2", |i, _, a| {
        Ok(Value::Num(num_arg(i, a, 0, f64::NAN)?.atan2(num_arg(
            i,
            a,
            1,
            f64::NAN,
        )?)))
    });
    method(i, &m, "hypot", |i, _, a| {
        let mut t = 0.0;
        for v in a {
            let x = i.coerce_num(v)?;
            t += x * x;
        }
        Ok(Value::Num(t.sqrt()))
    });
    method(i, &m, "max", |i, _, a| {
        let mut r = f64::NEG_INFINITY;
        for v in a {
            let x = i.coerce_num(v)?;
            if x.is_nan() {
                return Ok(Value::Num(f64::NAN));
            }
            r = r.max(x);
        }
        Ok(Value::Num(r))
    });
    method(i, &m, "min", |i, _, a| {
        let mut r = f64::INFINITY;
        for v in a {
            let x = i.coerce_num(v)?;
            if x.is_nan() {
                return Ok(Value::Num(f64::NAN));
            }
            r = r.min(x);
        }
        Ok(Value::Num(r))
    });
    let seed = std::cell::Cell::new(0x2545_F491_4F6C_DD1D_u64);
    method(i, &m, "random", move |_, _, _| {
        let mut x = seed.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        seed.set(x);
        Ok(Value::Num((x >> 11) as f64 / (1u64 << 53) as f64))
    });
    method(i, &m, "imul", |i, _, a| {
        Ok(Value::Num(f64::from(
            to_i32(num_arg(i, a, 0, 0.0)?).wrapping_mul(to_i32(num_arg(i, a, 1, 0.0)?)),
        )))
    });
    i.set_global("Math", Value::Obj(m));

    let j = i.new_obj();
    method(i, &j, "stringify", |i, _, a| {
        let indent = match arg(a, 2) {
            Value::Num(n) => " ".repeat(n.clamp(0.0, 10.0) as usize),
            Value::Str(t) => t.chars().take(10).collect(),
            _ => String::new(),
        };
        Ok(
            match json_str(i, &arg(a, 0), &indent, "", &mut Vec::new())? {
                Some(t) => s(&t),
                None => Value::Undefined,
            },
        )
    });
    method(i, &j, "parse", |i, _, a| {
        let t = i.text(&arg(a, 0))?;
        let c: Vec<char> = t.chars().collect();
        let mut p = JsonParser { c: &c, p: 0 };
        let v = p.value(i, 0)?;
        p.ws();
        if p.p != c.len() {
            return p.fail(i);
        }
        Ok(v)
    });
    i.set_global("JSON", Value::Obj(j));
}

fn install_globals(i: &mut Interp) {
    let c = i.new_obj();
    for name in ["log", "info", "warn", "error", "debug"] {
        method(i, &c, name, move |i, _, a| {
            let mut parts = Vec::new();
            for v in a {
                parts.push(i.inspect(v));
            }
            let prefix = match name {
                "warn" => "warning: ",
                "error" => "error: ",
                _ => "",
            };
            i.log_line(format!("{prefix}{}", parts.join(" ")));
            Ok(Value::Undefined)
        });
    }
    i.set_global("console", Value::Obj(c));
    let g = i.global_obj.clone();
    for (name, every) in [("setTimeout", false), ("setInterval", true)] {
        method(i, &g, name, move |i, _, a| {
            let f = arg(a, 0);
            if !is_callable(&f) {
                return Err(i.make_error("TypeError", "the timer callback is not a function"));
            }
            let d = num_arg(i, a, 1, 0.0)?;
            let id = i.add_timer(f, d, every, a.iter().skip(2).cloned().collect());
            Ok(Value::Num(f64::from(id)))
        });
        let f = i
            .get_prop(&Value::Obj(g.clone()), name)
            .unwrap_or(Value::Undefined);
        i.set_global(name, f);
    }
    for name in ["clearTimeout", "clearInterval"] {
        method(i, &g, name, |i, _, a| {
            if let Value::Num(n) = arg(a, 0) {
                i.clear_timer(n as u32);
            }
            Ok(Value::Undefined)
        });
        let f = i
            .get_prop(&Value::Obj(g.clone()), name)
            .unwrap_or(Value::Undefined);
        i.set_global(name, f);
    }
    type G = fn(&mut Interp, &Value, &[Value]) -> R<Value>;
    let fns: [(&str, G); 4] = [
        ("parseInt", |i, _, a| {
            let t = i.text(&arg(a, 0))?;
            let t = t.trim();
            let (neg, t) = match t.strip_prefix('-') {
                Some(r) => (true, r),
                None => (false, t.strip_prefix('+').unwrap_or(t)),
            };
            let mut radix = num_arg(i, a, 1, 10.0)? as u32;
            let mut t = t;
            if (radix == 0 || radix == 16) && (t.starts_with("0x") || t.starts_with("0X")) {
                t = &t[2..];
                radix = 16;
            }
            if radix == 0 {
                radix = 10;
            }
            if !(2..=36).contains(&radix) {
                return Ok(Value::Num(f64::NAN));
            }
            let digits: Vec<u32> = t.chars().map_while(|c| c.to_digit(radix)).collect();
            if digits.is_empty() {
                return Ok(Value::Num(f64::NAN));
            }
            let v = digits
                .iter()
                .fold(0.0, |acc, d| acc * f64::from(radix) + f64::from(*d));
            Ok(Value::Num(if neg { -v } else { v }))
        }),
        ("parseFloat", |i, _, a| {
            let t = i.text(&arg(a, 0))?;
            let t = t.trim();
            let mut end = 0;
            let b = t.as_bytes();
            let mut seen_dot = false;
            let mut seen_e = false;
            while end < b.len() {
                let ch = b[end];
                let ok = ch.is_ascii_digit()
                    || ((ch == b'-' || ch == b'+')
                        && (end == 0 || matches!(b[end - 1], b'e' | b'E')))
                    || (ch == b'.' && !seen_dot && !seen_e)
                    || ((ch == b'e' || ch == b'E') && !seen_e && end > 0);
                if !ok {
                    break;
                }
                seen_dot |= ch == b'.';
                seen_e |= ch == b'e' || ch == b'E';
                end += 1;
            }
            let mut head = &t[..end];
            while !head.is_empty() && head.parse::<f64>().is_err() {
                head = &head[..head.len() - 1];
            }
            if head.is_empty() && t.starts_with("Infinity") {
                return Ok(Value::Num(f64::INFINITY));
            }
            Ok(Value::Num(head.parse().unwrap_or(f64::NAN)))
        }),
        ("isNaN", |i, _, a| {
            Ok(Value::Bool(i.coerce_num(&arg(a, 0))?.is_nan()))
        }),
        ("isFinite", |i, _, a| {
            Ok(Value::Bool(i.coerce_num(&arg(a, 0))?.is_finite()))
        }),
    ];
    for (name, f) in fns {
        let fv = i.native(name, f);
        i.set_global(name, fv);
    }
    let _ = str_to_num;
    // performance.now reads the virtual clock.
    let p = i.new_obj();
    method(i, &p, "now", |i, _, _| Ok(Value::Num(i.clock)));
    i.set_global("performance", Value::Obj(p));
}

fn with_map<T>(i: &mut Interp, this: &Value, f: impl FnOnce(&mut MapData) -> T) -> R<T> {
    if let Value::Obj(o) = this {
        if let Kind::Map(m) = &mut o.borrow_mut().kind {
            return Ok(f(m));
        }
    }
    Err(i.make_error(
        "TypeError",
        "the method was called on a value that is not a Map or Set",
    ))
}

fn map_put(i: &mut Interp, this: &Value, k: Value, v: Value) -> R<()> {
    let k = if matches!(k, Value::Num(n) if n == 0.0) {
        Value::Num(0.0)
    } else {
        k
    };
    let full = with_map(i, this, |m| {
        let key = MapKey::of(&k);
        if let Some(&at) = m.index.get(&key) {
            if let Some(e) = m.entries[at].as_mut() {
                e.1 = v;
            }
            false
        } else if m.live >= 1_000_000 {
            true
        } else {
            m.index.insert(key, m.entries.len());
            m.entries.push(Some((k, v)));
            m.live += 1;
            false
        }
    })?;
    if full {
        return Err(i.make_error("RangeError", "the collection is too large"));
    }
    Ok(())
}

fn map_remove(m: &mut MapData, k: &Value) -> bool {
    let Some(at) = m.index.remove(&MapKey::of(k)) else {
        return false;
    };
    m.entries[at] = None;
    m.live -= 1;
    if m.entries.len() > 2 * m.live + 16 {
        let kept: Vec<(Value, Value)> = m.entries.drain(..).flatten().collect();
        m.index.clear();
        for (n, (k, v)) in kept.into_iter().enumerate() {
            m.index.insert(MapKey::of(&k), n);
            m.entries.push(Some((k, v)));
        }
    }
    true
}

fn install_collections(i: &mut Interp) {
    for (name, is_set) in [("Map", false), ("Set", true)] {
        let proto = i.new_obj();
        let label: &'static str = name;
        let _ = ctor(i, name, &proto, move |i, this, a| {
            let Value::Obj(o) = this else {
                return Err(
                    i.make_error("TypeError", &format!("Constructor {label} requires 'new'"))
                );
            };
            {
                let mut b = o.borrow_mut();
                b.kind = Kind::Map(MapData::default());
                b.class = label;
            }
            if !matches!(arg(a, 0), Value::Undefined | Value::Null) {
                for item in i.iter_values(&arg(a, 0))? {
                    if is_set {
                        map_put(i, this, item, Value::Undefined)?;
                    } else {
                        let pair = array_items(i, &item)?;
                        map_put(i, this, arg(&pair, 0), arg(&pair, 1))?;
                    }
                }
            }
            Ok(this.clone())
        });
        let g = {
            let f = i.native("size", |i, t, _| {
                Ok(Value::Num(with_map(i, t, |m| m.live)? as f64))
            });
            Prop {
                value: Value::Undefined,
                get: Some(f),
                set: None,
                enumerable: false,
            }
        };
        i.define(&proto, "size", g);
        method(i, &proto, "has", |i, t, a| {
            Ok(Value::Bool(with_map(i, t, |m| {
                m.index.contains_key(&MapKey::of(&arg(a, 0)))
            })?))
        });
        method(i, &proto, "delete", |i, t, a| {
            Ok(Value::Bool(with_map(i, t, |m| map_remove(m, &arg(a, 0)))?))
        });
        method(i, &proto, "clear", |i, t, _| {
            with_map(i, t, |m| *m = MapData::default())?;
            Ok(Value::Undefined)
        });
        if is_set {
            method(i, &proto, "add", |i, t, a| {
                map_put(i, t, arg(a, 0), Value::Undefined)?;
                Ok(t.clone())
            });
        } else {
            method(i, &proto, "set", |i, t, a| {
                map_put(i, t, arg(a, 0), arg(a, 1))?;
                Ok(t.clone())
            });
            method(i, &proto, "get", |i, t, a| {
                Ok(with_map(i, t, |m| {
                    m.index
                        .get(&MapKey::of(&arg(a, 0)))
                        .and_then(|&at| m.entries[at].as_ref().map(|e| e.1.clone()))
                })?
                .unwrap_or(Value::Undefined))
            });
        }
        for (mname, which) in [("keys", 0), ("values", 1), ("entries", 2)] {
            method(i, &proto, mname, move |i, t, _| {
                let items: Vec<(Value, Value)> =
                    with_map(i, t, |m| m.entries.iter().flatten().cloned().collect())?;
                let out = items
                    .into_iter()
                    .map(|(k, v)| match (which, is_set) {
                        (0, _) | (1, true) => k,
                        (1, false) => v,
                        (_, true) => i.new_array(vec![k.clone(), k]),
                        _ => i.new_array(vec![k, v]),
                    })
                    .collect();
                Ok(i.new_array(out))
            });
        }
        method(i, &proto, "forEach", move |i, t, a| {
            let f = arg(a, 0);
            if !is_callable(&f) {
                return Err(i.make_error("TypeError", "the callback is not a function"));
            }
            let items: Vec<(Value, Value)> =
                with_map(i, t, |m| m.entries.iter().flatten().cloned().collect())?;
            for (k, v) in items {
                let first = if is_set { k.clone() } else { v };
                i.call_value(&f, arg(a, 1), vec![first, k, t.clone()])?;
            }
            Ok(Value::Undefined)
        });
    }
}

// ------------------------------------------------------------------ promises

pub(crate) fn new_promise(i: &mut Interp) -> Obj {
    let p = i.protos.promise.clone();
    i.alloc(Some(p), Kind::Promise(PromiseData::default()), "Promise")
}

pub(crate) fn settle(i: &mut Interp, p: &Obj, state: u8, value: Value) {
    let reactions = {
        let mut b = p.borrow_mut();
        let Kind::Promise(d) = &mut b.kind else {
            return;
        };
        if d.state != 0 {
            return;
        }
        d.state = state;
        d.value = Some(value.clone());
        std::mem::take(&mut d.reactions)
    };
    for (on_f, on_r, q) in reactions {
        queue_reaction(i, state, value.clone(), on_f, on_r, q);
    }
}

fn queue_reaction(i: &mut Interp, state: u8, value: Value, on_f: Value, on_r: Value, q: Obj) {
    i.microtasks.push_back(Box::new(move |i: &mut Interp| {
        let handler = if state == 1 { on_f } else { on_r };
        if is_callable(&handler) {
            match i.call_value(&handler, Value::Undefined, vec![value]) {
                Ok(v) => resolve(i, &q, v),
                Err(e) => settle(i, &q, 2, e),
            }
        } else if state == 1 {
            resolve(i, &q, value);
        } else {
            settle(i, &q, 2, value);
        }
    }));
}

pub(crate) fn resolvers(i: &mut Interp, p: &Obj) -> (Value, Value) {
    let done = Rc::new(Cell::new(false));
    let (p1, d1) = (p.clone(), done.clone());
    let res = i.native("resolve", move |i, _, a| {
        if !d1.replace(true) {
            resolve(i, &p1, arg(a, 0));
        }
        Ok(Value::Undefined)
    });
    let (p2, d2) = (p.clone(), done);
    let rej = i.native("reject", move |i, _, a| {
        if !d2.replace(true) {
            settle(i, &p2, 2, arg(a, 0));
        }
        Ok(Value::Undefined)
    });
    (res, rej)
}

pub(crate) fn resolve(i: &mut Interp, p: &Obj, x: Value) {
    if let Value::Obj(xo) = &x {
        if Rc::ptr_eq(xo, p) {
            let e = i.make_error("TypeError", "a promise cannot be resolved with itself");
            settle(i, p, 2, e);
            return;
        }
        match i.get_prop(&x, "then") {
            Err(e) => settle(i, p, 2, e),
            Ok(then) if is_callable(&then) => {
                let p = p.clone();
                i.microtasks.push_back(Box::new(move |i: &mut Interp| {
                    let (res, rej) = resolvers(i, &p);
                    if let Err(e) = i.call_value(&then, x, vec![res, rej.clone()]) {
                        let _ = i.call_value(&rej, Value::Undefined, vec![e]);
                    }
                }));
            }
            Ok(_) => settle(i, p, 1, x),
        }
    } else {
        settle(i, p, 1, x);
    }
}

fn promise_then(i: &mut Interp, this: &Value, on_f: Value, on_r: Value) -> R<Value> {
    let Value::Obj(p) = this else {
        return Err(i.make_error("TypeError", "then called on a value that is not a promise"));
    };
    let q = new_promise(i);
    let now = {
        let mut b = p.borrow_mut();
        let Kind::Promise(d) = &mut b.kind else {
            drop(b);
            return Err(i.make_error("TypeError", "then called on a value that is not a promise"));
        };
        if d.state == 0 {
            d.reactions.push((on_f.clone(), on_r.clone(), q.clone()));
            None
        } else {
            Some((d.state, d.value.clone().unwrap_or(Value::Undefined)))
        }
    };
    if let Some((state, v)) = now {
        queue_reaction(i, state, v, on_f, on_r, q.clone());
    }
    Ok(Value::Obj(q))
}

pub(crate) fn promise_from(i: &mut Interp, x: Value) -> Obj {
    if let Value::Obj(o) = &x {
        if matches!(o.borrow().kind, Kind::Promise(_)) {
            return o.clone();
        }
    }
    let p = new_promise(i);
    resolve(i, &p, x);
    p
}

fn install_promise(i: &mut Interp) {
    let proto = i.protos.promise.clone();
    let po = ctor(i, "Promise", &proto, |i, this, a| {
        let exec = arg(a, 0);
        let Value::Obj(o) = this else {
            return Err(i.make_error("TypeError", "Promise constructor requires 'new'"));
        };
        if !is_callable(&exec) {
            return Err(i.make_error("TypeError", "the Promise executor is not a function"));
        }
        {
            let mut b = o.borrow_mut();
            b.kind = Kind::Promise(PromiseData::default());
            b.class = "Promise";
        }
        let (res, rej) = resolvers(i, o);
        if let Err(e) = i.call_value(&exec, Value::Undefined, vec![res, rej.clone()]) {
            i.call_value(&rej, Value::Undefined, vec![e])?;
        }
        Ok(this.clone())
    });
    method(i, &proto, "then", |i, t, a| {
        promise_then(i, t, arg(a, 0), arg(a, 1))
    });
    method(i, &proto, "catch", |i, t, a| {
        promise_then(i, t, Value::Undefined, arg(a, 0))
    });
    method(i, &proto, "finally", |i, t, a| {
        let f = arg(a, 0);
        let f2 = f.clone();
        let on_f = i.native("finally", move |i, _, a| {
            if is_callable(&f) {
                i.call_value(&f, Value::Undefined, Vec::new())?;
            }
            Ok(arg(a, 0))
        });
        let on_r = i.native("finally", move |i, _, a| {
            if is_callable(&f2) {
                i.call_value(&f2, Value::Undefined, Vec::new())?;
            }
            Err(arg(a, 0))
        });
        promise_then(i, t, on_f, on_r)
    });
    method(i, &po, "resolve", |i, _, a| {
        Ok(Value::Obj(promise_from(i, arg(a, 0))))
    });
    method(i, &po, "reject", |i, _, a| {
        let p = new_promise(i);
        settle(i, &p, 2, arg(a, 0));
        Ok(Value::Obj(p))
    });
    for (name, mode) in [("all", 0u8), ("allSettled", 1), ("race", 2)] {
        method(i, &po, name, move |i, _, a| {
            let items = i.iter_values(&arg(a, 0))?;
            let out = new_promise(i);
            let results = Rc::new(RefCell::new(vec![Value::Undefined; items.len()]));
            let left = Rc::new(Cell::new(items.len()));
            if items.is_empty() && mode != 2 {
                let arr = i.new_array(Vec::new());
                resolve(i, &out, arr);
            }
            for (k, item) in items.into_iter().enumerate() {
                let p = Value::Obj(promise_from(i, item));
                let (r1, l1, o1) = (results.clone(), left.clone(), out.clone());
                let on_f = i.native("onFulfilled", move |i, _, a| {
                    let v = if mode == 1 {
                        let o = i.new_obj();
                        i.put(&o, "status", Value::str("fulfilled"));
                        i.put(&o, "value", arg(a, 0));
                        Value::Obj(o)
                    } else {
                        arg(a, 0)
                    };
                    if mode == 2 {
                        resolve(i, &o1, v);
                    } else {
                        r1.borrow_mut()[k] = v;
                        l1.set(l1.get() - 1);
                        if l1.get() == 0 {
                            let all = r1.borrow().clone();
                            let arr = i.new_array(all);
                            resolve(i, &o1, arr);
                        }
                    }
                    Ok(Value::Undefined)
                });
                let (r2, l2, o2) = (results.clone(), left.clone(), out.clone());
                let on_r = i.native("onRejected", move |i, _, a| {
                    if mode == 1 {
                        let o = i.new_obj();
                        i.put(&o, "status", Value::str("rejected"));
                        i.put(&o, "reason", arg(a, 0));
                        r2.borrow_mut()[k] = Value::Obj(o);
                        l2.set(l2.get() - 1);
                        if l2.get() == 0 {
                            let all = r2.borrow().clone();
                            let arr = i.new_array(all);
                            resolve(i, &o2, arr);
                        }
                    } else {
                        settle(i, &o2, 2, arg(a, 0));
                    }
                    Ok(Value::Undefined)
                });
                promise_then(i, &p, on_f, on_r)?;
            }
            Ok(Value::Obj(out))
        });
    }
    let qm = i.native("queueMicrotask", |i, _, a| {
        let f = arg(a, 0);
        if !is_callable(&f) {
            return Err(i.make_error("TypeError", "the callback is not a function"));
        }
        i.microtasks.push_back(Box::new(move |i: &mut Interp| {
            if let Err(e) = i.call_value(&f, Value::Undefined, Vec::new()) {
                let m = i.error_text(&e);
                i.console.push(m);
            }
        }));
        Ok(Value::Undefined)
    });
    i.set_global("queueMicrotask", qm);
}

// ------------------------------------------------------------------ regular expressions

/// A flags string in the order JavaScript prints it.
pub(crate) fn flags_string(f: &Flags) -> String {
    let mut s = String::new();
    for (on, c) in [
        (f.global, 'g'),
        (f.ignore_case, 'i'),
        (f.multiline, 'm'),
        (f.dot_all, 's'),
        (f.unicode, 'u'),
        (f.sticky, 'y'),
    ] {
        if on {
            s.push(c);
        }
    }
    s
}

fn regex_of(v: &Value) -> Option<(Rc<Regex>, Rc<str>)> {
    if let Value::Obj(o) = v {
        if let Kind::Regex(r, src) = &o.borrow().kind {
            return Some((r.clone(), src.clone()));
        }
    }
    None
}

fn exec_at(i: &mut Interp, rx: &Regex, text: &[char], from: usize) -> R<Option<Match>> {
    rx.exec(text, from)
        .map_err(|m| i.make_error("RangeError", m))
}

fn slice(text: &[char], a: usize, b: usize) -> Value {
    Value::str(&text[a..b].iter().collect::<String>())
}

fn match_array(i: &mut Interp, text: &[char], m: &Match, rx: &Regex) -> Value {
    let items: Vec<Value> = m
        .spans
        .iter()
        .map(|sp| sp.map_or(Value::Undefined, |(a, b)| slice(text, a, b)))
        .collect();
    let arr = i.new_array(items);
    if let Value::Obj(o) = &arr {
        i.put(o, "index", Value::Num(m.spans[0].map_or(0, |s| s.0) as f64));
        i.put(o, "input", Value::str(&text.iter().collect::<String>()));
        let groups = if rx.names.is_empty() {
            Value::Undefined
        } else {
            let g = i.new_obj();
            for (name, idx) in &rx.names {
                let v = m
                    .spans
                    .get(*idx)
                    .copied()
                    .flatten()
                    .map_or(Value::Undefined, |(a, b)| slice(text, a, b));
                i.put(&g, name, v);
            }
            Value::Obj(g)
        };
        i.put(o, "groups", groups);
    }
    arr
}

/// Expands `$1`, `$&`, `` $` ``, `$'`, `$<name>` and `$$` in a replacement string.
fn expand(tpl: &str, text: &[char], m: &Match, rx: &Regex) -> String {
    let t: Vec<char> = tpl.chars().collect();
    let (ms, me) = m.spans[0].unwrap_or((0, 0));
    let mut out = String::new();
    let mut k = 0;
    while k < t.len() {
        if t[k] != '$' || k + 1 >= t.len() {
            out.push(t[k]);
            k += 1;
            continue;
        }
        match t[k + 1] {
            '$' => {
                out.push('$');
                k += 2;
            }
            '&' => {
                out.extend(&text[ms..me]);
                k += 2;
            }
            '`' => {
                out.extend(&text[..ms]);
                k += 2;
            }
            '\'' => {
                out.extend(&text[me..]);
                k += 2;
            }
            '<' if !rx.names.is_empty() => match t[k + 2..].iter().position(|c| *c == '>') {
                Some(e) => {
                    let name: String = t[k + 2..k + 2 + e].iter().collect();
                    if let Some((_, idx)) = rx.names.iter().find(|(n, _)| *n == name) {
                        if let Some(Some((a, b))) = m.spans.get(*idx) {
                            out.extend(&text[*a..*b]);
                        }
                    }
                    k += e + 3;
                }
                None => {
                    out.push('$');
                    k += 1;
                }
            },
            d if d.is_ascii_digit() => {
                // One or two digits, as long as the group exists.
                let two = t
                    .get(k + 2)
                    .filter(|c| c.is_ascii_digit())
                    .map(|c| (d as usize - 48) * 10 + (*c as usize - 48));
                let (n, used) = match two {
                    Some(n) if n >= 1 && n <= rx.groups => (n, 3),
                    _ => (d as usize - 48, 2),
                };
                if n >= 1 && n <= rx.groups {
                    if let Some(Some((a, b))) = m.spans.get(n) {
                        out.extend(&text[*a..*b]);
                    }
                    k += used;
                } else {
                    out.push('$');
                    k += 1;
                }
            }
            _ => {
                out.push('$');
                k += 1;
            }
        }
    }
    out
}

fn all_matches(i: &mut Interp, rx: &Regex, text: &[char]) -> R<Vec<Match>> {
    let mut out = Vec::new();
    let mut from = 0;
    while from <= text.len() {
        let Some(m) = exec_at(i, rx, text, from)? else {
            break;
        };
        let (s, e) = m.spans[0].unwrap_or((from, from));
        from = if e == s { e + 1 } else { e };
        out.push(m);
        if out.len() > 100_000 {
            return Err(i.make_error("RangeError", "too many matches"));
        }
    }
    Ok(out)
}

fn install_regexp(i: &mut Interp) {
    let proto = i.protos.regexp.clone();
    let _ = ctor(i, "RegExp", &proto, |i, _, a| {
        let (src, flags): (String, String) = match regex_of(&arg(a, 0)) {
            Some((r, s)) => {
                let f = match arg(a, 1) {
                    Value::Undefined => flags_string(&r.flags),
                    v => i.text(&v)?.to_string(),
                };
                (s.to_string(), f)
            }
            None => {
                let s = match arg(a, 0) {
                    Value::Undefined => "(?:)".to_string(),
                    v => i.text(&v)?.to_string(),
                };
                let f = match arg(a, 1) {
                    Value::Undefined => String::new(),
                    v => i.text(&v)?.to_string(),
                };
                (s, f)
            }
        };
        i.new_regexp(&src, &flags)
    });
    for (name, pick) in [
        ("global", 0u8),
        ("ignoreCase", 1),
        ("multiline", 2),
        ("dotAll", 3),
        ("unicode", 4),
        ("sticky", 5),
    ] {
        let g = i.native(name, move |i, t, _| {
            let Some((r, _)) = regex_of(t) else {
                return Err(i.make_error("TypeError", "not a regular expression"));
            };
            let f = r.flags;
            Ok(Value::Bool(
                [
                    f.global,
                    f.ignore_case,
                    f.multiline,
                    f.dot_all,
                    f.unicode,
                    f.sticky,
                ][usize::from(pick)],
            ))
        });
        i.define(
            &proto,
            name,
            Prop {
                value: Value::Undefined,
                get: Some(g),
                set: None,
                enumerable: false,
            },
        );
    }
    let g = i.native("source", |i, t, _| match regex_of(t) {
        Some((_, s)) => Ok(Value::Str(s)),
        None => Err(i.make_error("TypeError", "not a regular expression")),
    });
    i.define(
        &proto,
        "source",
        Prop {
            value: Value::Undefined,
            get: Some(g),
            set: None,
            enumerable: false,
        },
    );
    let g = i.native("flags", |i, t, _| match regex_of(t) {
        Some((r, _)) => Ok(Value::str(&flags_string(&r.flags))),
        None => Err(i.make_error("TypeError", "not a regular expression")),
    });
    i.define(
        &proto,
        "flags",
        Prop {
            value: Value::Undefined,
            get: Some(g),
            set: None,
            enumerable: false,
        },
    );
    method(i, &proto, "toString", |i, t, _| match regex_of(t) {
        Some((r, s)) => Ok(Value::str(&format!("/{s}/{}", flags_string(&r.flags)))),
        None => Err(i.make_error("TypeError", "not a regular expression")),
    });
    fn run(i: &mut Interp, this: &Value, arg0: &Value) -> R<Value> {
        let Some((rx, _)) = regex_of(this) else {
            return Err(i.make_error(
                "TypeError",
                "exec called on a value that is not a regular expression",
            ));
        };
        let text: Vec<char> = i.text(arg0)?.chars().collect();
        let uses_last = rx.flags.global || rx.flags.sticky;
        let from = if uses_last {
            let li = i.get_prop(this, "lastIndex")?;
            let n = i.coerce_num(&li)?;
            if n.is_finite() && n > 0.0 {
                n as usize
            } else {
                0
            }
        } else {
            0
        };
        if from > text.len() {
            i.set_prop(this, "lastIndex", Value::Num(0.0))?;
            return Ok(Value::Null);
        }
        match exec_at(i, &rx, &text, from)? {
            None => {
                if uses_last {
                    i.set_prop(this, "lastIndex", Value::Num(0.0))?;
                }
                Ok(Value::Null)
            }
            Some(m) => {
                if uses_last {
                    let end = m.spans[0].map_or(0, |s| s.1);
                    i.set_prop(this, "lastIndex", Value::Num(end as f64))?;
                }
                Ok(match_array(i, &text, &m, &rx))
            }
        }
    }
    method(i, &proto, "exec", |i, t, a| run(i, t, &arg(a, 0)));
    method(i, &proto, "test", |i, t, a| {
        Ok(Value::Bool(!matches!(run(i, t, &arg(a, 0))?, Value::Null)))
    });

    // String methods that take expressions.
    let sp = i.protos.string.clone();
    method(i, &sp, "match", |i, t, a| {
        let text: Vec<char> = this_str(i, t)?.chars().collect();
        let rv = match regex_of(&arg(a, 0)) {
            Some(_) => arg(a, 0),
            None => {
                let p = match arg(a, 0) {
                    Value::Undefined => "(?:)".into(),
                    v => i.text(&v)?.to_string(),
                };
                i.new_regexp(&p, "")?
            }
        };
        let Some((rx, _)) = regex_of(&rv) else {
            return Ok(Value::Null);
        };
        if rx.flags.global {
            let ms = all_matches(i, &rx, &text)?;
            if ms.is_empty() {
                return Ok(Value::Null);
            }
            let items = ms
                .iter()
                .map(|m| m.spans[0].map_or(Value::Undefined, |(a, b)| slice(&text, a, b)))
                .collect();
            Ok(i.new_array(items))
        } else {
            Ok(match exec_at(i, &rx, &text, 0)? {
                Some(m) => match_array(i, &text, &m, &rx),
                None => Value::Null,
            })
        }
    });
    method(i, &sp, "matchAll", |i, t, a| {
        let text: Vec<char> = this_str(i, t)?.chars().collect();
        let Some((rx, _)) = regex_of(&arg(a, 0)) else {
            return Err(i.make_error("TypeError", "matchAll needs a regular expression"));
        };
        if !rx.flags.global {
            return Err(i.make_error(
                "TypeError",
                "matchAll must be given a global regular expression",
            ));
        }
        let ms = all_matches(i, &rx, &text)?;
        let items = ms.iter().map(|m| match_array(i, &text, m, &rx)).collect();
        Ok(i.new_array(items))
    });
    method(i, &sp, "search", |i, t, a| {
        let text: Vec<char> = this_str(i, t)?.chars().collect();
        let rv = match regex_of(&arg(a, 0)) {
            Some(_) => arg(a, 0),
            None => {
                let p = i.text(&arg(a, 0))?.to_string();
                i.new_regexp(&p, "")?
            }
        };
        let Some((rx, _)) = regex_of(&rv) else {
            return Ok(Value::Num(-1.0));
        };
        Ok(Value::Num(
            exec_at(i, &rx, &text, 0)?
                .and_then(|m| m.spans[0])
                .map_or(-1.0, |s| s.0 as f64),
        ))
    });
    for name in ["replace", "replaceAll", "split"] {
        let old = i
            .get_prop(&Value::Obj(sp.clone()), name)
            .unwrap_or(Value::Undefined);
        method(i, &sp, name, move |i, t, a| {
            let Some((rx, _)) = regex_of(&arg(a, 0)) else {
                return i.call_value(&old, t.clone(), a.to_vec());
            };
            let text: Vec<char> = this_str(i, t)?.chars().collect();
            if name == "split" {
                let limit = match arg(a, 1) {
                    Value::Undefined => usize::MAX,
                    v => i.coerce_num(&v)?.max(0.0) as usize,
                };
                let mut out: Vec<Value> = Vec::new();
                let (mut last_end, mut from) = (0usize, 0usize);
                if text.is_empty() {
                    let m = exec_at(i, &rx, &text, 0)?;
                    return Ok(i.new_array(if m.is_some() {
                        Vec::new()
                    } else {
                        vec![Value::str("")]
                    }));
                }
                while from <= text.len() {
                    let Some(m) = exec_at(i, &rx, &text, from)? else {
                        break;
                    };
                    let (s, e) = m.spans[0].unwrap_or((from, from));
                    if s >= text.len() && e == s {
                        break;
                    }
                    if e == s && s == last_end {
                        from = s + 1;
                        continue;
                    }
                    out.push(slice(&text, last_end, s));
                    for sp2 in m.spans.iter().skip(1) {
                        out.push(sp2.map_or(Value::Undefined, |(x, y)| slice(&text, x, y)));
                    }
                    last_end = e;
                    from = if e == s { e + 1 } else { e };
                    if out.len() >= limit.min(1_000_000) {
                        break;
                    }
                }
                out.push(slice(&text, last_end, text.len()));
                out.truncate(limit.min(1_000_000));
                return Ok(i.new_array(out));
            }
            // replace / replaceAll
            if name == "replaceAll" && !rx.flags.global {
                return Err(i.make_error(
                    "TypeError",
                    "replaceAll must be given a global regular expression",
                ));
            }
            let ms = if rx.flags.global {
                all_matches(i, &rx, &text)?
            } else {
                exec_at(i, &rx, &text, 0)?.into_iter().collect()
            };
            let rep = arg(a, 1);
            let mut out = String::new();
            let mut at = 0usize;
            for m in &ms {
                let (s, e) = m.spans[0].unwrap_or((0, 0));
                out.extend(&text[at..s]);
                if is_callable(&rep) {
                    let mut args: Vec<Value> = m
                        .spans
                        .iter()
                        .map(|sp| sp.map_or(Value::Undefined, |(x, y)| slice(&text, x, y)))
                        .collect();
                    args.push(Value::Num(s as f64));
                    args.push(Value::str(&text.iter().collect::<String>()));
                    if !rx.names.is_empty() {
                        let g = i.new_obj();
                        for (n, idx) in &rx.names {
                            let v = m
                                .spans
                                .get(*idx)
                                .copied()
                                .flatten()
                                .map_or(Value::Undefined, |(x, y)| slice(&text, x, y));
                            i.put(&g, n, v);
                        }
                        args.push(Value::Obj(g));
                    }
                    let r = i.call_value(&rep, Value::Undefined, args)?;
                    out.push_str(&i.text(&r)?);
                } else {
                    let tpl = i.text(&rep)?;
                    out.push_str(&expand(&tpl, &text, m, &rx));
                }
                at = e;
                if out.len() > crate::interp::MAX_STRING {
                    return Err(i.make_error("RangeError", "the string is too long"));
                }
            }
            out.extend(&text[at..]);
            Ok(Value::str(&out))
        });
    }
}

// ------------------------------------------------------------------ Date

const MS_DAY: f64 = 86_400_000.0;
const MAX_TIME: f64 = 8.64e15;

/// Days since 1970-01-01 of a civil date (proleptic Gregorian; Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day number.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Year, month (0-11), day, hours, minutes, seconds, milliseconds, weekday (0 = Sunday).
fn parts(t: f64) -> [i64; 8] {
    let ms = t as i64;
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    [
        y,
        m - 1,
        d,
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1000 % 60,
        rem % 1000,
        (days + 4).rem_euclid(7),
    ]
}

/// A time value from components that may be out of range (month 14, day 0, hour 25 ...).
fn make_time(y: f64, mo: f64, d: f64, h: f64, mi: f64, s: f64, ms: f64) -> f64 {
    if ![y, mo, d, h, mi, s, ms].iter().all(|v| v.is_finite()) {
        return f64::NAN;
    }
    let (y, mo) = (y.trunc(), mo.trunc());
    let ym = y + (mo / 12.0).floor();
    let mn = mo.rem_euclid(12.0);
    if ym.abs() > 400_000.0 {
        return f64::NAN;
    }
    let days = days_from_civil(ym as i64, mn as i64 + 1, 1) as f64 + d.trunc() - 1.0;
    let t = days * MS_DAY
        + h.trunc() * 3_600_000.0
        + mi.trunc() * 60_000.0
        + s.trunc() * 1000.0
        + ms.trunc();
    if t.abs() > MAX_TIME {
        f64::NAN
    } else {
        t
    }
}

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `2024-05-06T07:08:09.010Z`, or `None` for an invalid time.
pub(crate) fn iso_string(t: f64) -> Option<String> {
    if !t.is_finite() {
        return None;
    }
    let p = parts(t);
    let year = if (0..=9999).contains(&p[0]) {
        format!("{:04}", p[0])
    } else {
        format!("{}{:06}", if p[0] < 0 { '-' } else { '+' }, p[0].abs())
    };
    Some(format!(
        "{year}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        p[1] + 1,
        p[2],
        p[3],
        p[4],
        p[5],
        p[6]
    ))
}

fn date_string(t: f64) -> String {
    if !t.is_finite() {
        return "Invalid Date".into();
    }
    let p = parts(t);
    format!(
        "{} {} {:02} {:04} {:02}:{:02}:{:02} GMT+0000 (UTC)",
        DAYS[p[7] as usize], MONTHS[p[1] as usize], p[2], p[0], p[3], p[4], p[5]
    )
}

/// Reads ISO-8601 dates (`2024-05-06`, `2024-05-06T07:08`, `...:09.010Z`, `+02:00`); `NaN` otherwise.
fn parse_date(s: &str) -> f64 {
    let b = s.trim().as_bytes();
    let num = |from: usize, n: usize| -> Option<f64> {
        let part = b.get(from..from + n)?;
        part.iter()
            .all(u8::is_ascii_digit)
            .then(|| std::str::from_utf8(part).ok()?.parse::<f64>().ok())?
    };
    let Some(y) = num(0, 4) else { return f64::NAN };
    let (mut mo, mut d, mut h, mut mi, mut sec, mut ms) = (1.0, 1.0, 0.0, 0.0, 0.0, 0.0);
    let mut at = 4;
    if b.get(at) == Some(&b'-') {
        let Some(v) = num(at + 1, 2) else {
            return f64::NAN;
        };
        mo = v;
        at += 3;
        if b.get(at) == Some(&b'-') {
            let Some(v) = num(at + 1, 2) else {
                return f64::NAN;
            };
            d = v;
            at += 3;
        }
    }
    let mut offset = 0.0;
    if matches!(b.get(at), Some(b'T' | b't' | b' ')) {
        let (Some(hh), Some(mm)) = (num(at + 1, 2), num(at + 4, 2)) else {
            return f64::NAN;
        };
        h = hh;
        mi = mm;
        at += 6;
        if b.get(at) == Some(&b':') {
            let Some(v) = num(at + 1, 2) else {
                return f64::NAN;
            };
            sec = v;
            at += 3;
            if b.get(at) == Some(&b'.') {
                let st = at + 1;
                let mut e = st;
                while b.get(e).is_some_and(u8::is_ascii_digit) {
                    e += 1;
                }
                if e == st {
                    return f64::NAN;
                }
                let frac = std::str::from_utf8(&b[st..e.min(st + 3)]).unwrap_or("0");
                ms = frac.parse::<f64>().unwrap_or(0.0) * 10f64.powi(3 - frac.len() as i32);
                at = e;
            }
        }
        match b.get(at) {
            Some(b'Z' | b'z') => at += 1,
            Some(&sign @ (b'+' | b'-')) => {
                let (Some(oh), Some(om)) = (num(at + 1, 2), num(at + 4, 2)) else {
                    return f64::NAN;
                };
                offset = (oh * 60.0 + om) * 60_000.0 * if sign == b'-' { -1.0 } else { 1.0 };
                at += 6;
            }
            _ => {}
        }
    }
    if at != b.len()
        || !(1.0..=12.0).contains(&mo)
        || !(1.0..=31.0).contains(&d)
        || h > 24.0
        || mi > 59.0
        || sec > 59.0
    {
        return f64::NAN;
    }
    make_time(y, mo - 1.0, d, h, mi, sec, ms) - offset
}

fn time_of(i: &mut Interp, this: &Value) -> R<f64> {
    if let Value::Obj(o) = this {
        if let Kind::Date(t) = &o.borrow().kind {
            return Ok(*t);
        }
    }
    Err(i.make_error("TypeError", "this is not a Date object"))
}

fn set_time(i: &mut Interp, this: &Value, t: f64) -> R<Value> {
    let t = if t.is_finite() && t.abs() <= MAX_TIME {
        t.trunc()
    } else {
        f64::NAN
    };
    if let Value::Obj(o) = this {
        if let Kind::Date(slot) = &mut o.borrow_mut().kind {
            *slot = t;
            return Ok(Value::Num(t));
        }
    }
    Err(i.make_error("TypeError", "this is not a Date object"))
}

fn now_ms(i: &Interp) -> f64 {
    (i.epoch_ms + i.clock).floor()
}

fn install_date(i: &mut Interp) {
    let proto = i.new_obj();
    let d = ctor(i, "Date", &proto, |i, this, a| {
        let Value::Obj(o) = this else {
            // Called without `new`: the current time as text.
            return Ok(Value::str(&date_string(now_ms(i))));
        };
        let t = match a.len() {
            0 => now_ms(i),
            1 => match &a[0] {
                Value::Str(sv) => parse_date(sv),
                Value::Obj(src) if matches!(src.borrow().kind, Kind::Date(_)) => time_of(i, &a[0])?,
                v => i.coerce_num(v)?,
            },
            _ => {
                let mut n = [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
                for (k, slot) in n.iter_mut().enumerate() {
                    if let Some(v) = a.get(k) {
                        *slot = i.coerce_num(v)?;
                    }
                }
                if (0.0..=99.0).contains(&n[0]) {
                    n[0] += 1900.0;
                }
                make_time(n[0], n[1], n[2], n[3], n[4], n[5], n[6])
            }
        };
        let t = if t.is_finite() && t.abs() <= MAX_TIME {
            t.trunc()
        } else {
            f64::NAN
        };
        {
            let mut b = o.borrow_mut();
            b.kind = Kind::Date(t);
            b.class = "Date";
        }
        Ok(this.clone())
    });
    method(i, &d, "now", |i, _, _| Ok(Value::Num(now_ms(i))));
    method(i, &d, "parse", |i, _, a| {
        let t = i.text(&arg(a, 0))?;
        Ok(Value::Num(parse_date(&t)))
    });
    method(i, &d, "UTC", |i, _, a| {
        let mut n = [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        for (k, slot) in n.iter_mut().enumerate() {
            if let Some(v) = a.get(k) {
                *slot = i.coerce_num(v)?;
            }
        }
        if (0.0..=99.0).contains(&n[0]) {
            n[0] += 1900.0;
        }
        Ok(Value::Num(make_time(
            n[0], n[1], n[2], n[3], n[4], n[5], n[6],
        )))
    });
    for name in ["getTime", "valueOf"] {
        method(i, &proto, name, |i, t, _| Ok(Value::Num(time_of(i, t)?)));
    }
    method(i, &proto, "getTimezoneOffset", |i, t, _| {
        time_of(i, t)?;
        Ok(Value::Num(0.0))
    });
    // Getters (the time zone is UTC, so local and UTC are the same).
    let getters: [(&str, usize); 8] = [
        ("FullYear", 0),
        ("Month", 1),
        ("Date", 2),
        ("Hours", 3),
        ("Minutes", 4),
        ("Seconds", 5),
        ("Milliseconds", 6),
        ("Day", 7),
    ];
    for (name, idx) in getters {
        for prefix in ["get", "getUTC"] {
            method(i, &proto, &format!("{prefix}{name}"), move |i, t, _| {
                let tm = time_of(i, t)?;
                Ok(Value::Num(if tm.is_finite() {
                    parts(tm)[idx] as f64
                } else {
                    f64::NAN
                }))
            });
        }
    }
    // Setters: each replaces some components and keeps the rest.
    let setters: [(&str, usize, usize); 7] = [
        ("FullYear", 0, 3),
        ("Month", 1, 2),
        ("Date", 2, 1),
        ("Hours", 3, 4),
        ("Minutes", 4, 3),
        ("Seconds", 5, 2),
        ("Milliseconds", 6, 1),
    ];
    for (name, first, max_args) in setters {
        for prefix in ["set", "setUTC"] {
            method(i, &proto, &format!("{prefix}{name}"), move |i, t, a| {
                let tm = time_of(i, t)?;
                let base = if tm.is_finite() {
                    tm
                } else if first == 0 {
                    0.0
                } else {
                    return Ok(Value::Num(f64::NAN));
                };
                let p = parts(base);
                let mut c: [f64; 7] = [
                    p[0] as f64,
                    p[1] as f64,
                    p[2] as f64,
                    p[3] as f64,
                    p[4] as f64,
                    p[5] as f64,
                    p[6] as f64,
                ];
                for k in 0..max_args.min(a.len()).max(1) {
                    c[first + k] = match a.get(k) {
                        Some(v) => i.coerce_num(v)?,
                        None => f64::NAN,
                    };
                }
                set_time(i, t, make_time(c[0], c[1], c[2], c[3], c[4], c[5], c[6]))
            });
        }
    }
    method(i, &proto, "setTime", |i, t, a| {
        let v = i.coerce_num(&arg(a, 0))?;
        set_time(i, t, v)
    });
    method(i, &proto, "toISOString", |i, t, _| {
        let tm = time_of(i, t)?;
        match iso_string(tm) {
            Some(s) => Ok(Value::str(&s)),
            None => Err(i.make_error("RangeError", "Invalid time value")),
        }
    });
    method(i, &proto, "toJSON", |i, t, _| {
        let tm = time_of(i, t)?;
        Ok(iso_string(tm).map_or(Value::Null, |s| Value::str(&s)))
    });
    method(i, &proto, "toString", |i, t, _| {
        Ok(Value::str(&date_string(time_of(i, t)?)))
    });
    method(i, &proto, "toUTCString", |i, t, _| {
        let tm = time_of(i, t)?;
        if !tm.is_finite() {
            return Ok(Value::str("Invalid Date"));
        }
        let p = parts(tm);
        Ok(Value::str(&format!(
            "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
            DAYS[p[7] as usize], p[2], MONTHS[p[1] as usize], p[0], p[3], p[4], p[5]
        )))
    });
    method(i, &proto, "toDateString", |i, t, _| {
        let tm = time_of(i, t)?;
        if !tm.is_finite() {
            return Ok(Value::str("Invalid Date"));
        }
        let p = parts(tm);
        Ok(Value::str(&format!(
            "{} {} {:02} {:04}",
            DAYS[p[7] as usize], MONTHS[p[1] as usize], p[2], p[0]
        )))
    });
    method(i, &proto, "toTimeString", |i, t, _| {
        let tm = time_of(i, t)?;
        if !tm.is_finite() {
            return Ok(Value::str("Invalid Date"));
        }
        let p = parts(tm);
        Ok(Value::str(&format!(
            "{:02}:{:02}:{:02} GMT+0000 (UTC)",
            p[3], p[4], p[5]
        )))
    });
    method(i, &proto, "toLocaleDateString", |i, t, _| {
        let tm = time_of(i, t)?;
        if !tm.is_finite() {
            return Ok(Value::str("Invalid Date"));
        }
        let p = parts(tm);
        Ok(Value::str(&format!("{}/{}/{}", p[1] + 1, p[2], p[0])))
    });
    method(i, &proto, "toLocaleTimeString", |i, t, _| {
        let tm = time_of(i, t)?;
        if !tm.is_finite() {
            return Ok(Value::str("Invalid Date"));
        }
        let p = parts(tm);
        let h12 = if p[3] % 12 == 0 { 12 } else { p[3] % 12 };
        Ok(Value::str(&format!(
            "{h12}:{:02}:{:02} {}",
            p[4],
            p[5],
            if p[3] < 12 { "AM" } else { "PM" }
        )))
    });
    method(i, &proto, "toLocaleString", |i, t, _| {
        let tm = time_of(i, t)?;
        if !tm.is_finite() {
            return Ok(Value::str("Invalid Date"));
        }
        let p = parts(tm);
        let h12 = if p[3] % 12 == 0 { 12 } else { p[3] % 12 };
        Ok(Value::str(&format!(
            "{}/{}/{}, {h12}:{:02}:{:02} {}",
            p[1] + 1,
            p[2],
            p[0],
            p[4],
            p[5],
            if p[3] < 12 { "AM" } else { "PM" }
        )))
    });
}
