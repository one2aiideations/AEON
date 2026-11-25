//! JavaScript interpreter: a tree-walking evaluator over the syntax tree from [`crate::js`].
//!
//! What it does: closures, `this`, prototypes, classes (with `extends`, `super`, fields,
//! accessors), destructuring, spread, exceptions, labelled loops, getters and setters, optional
//! chaining, per-iteration `let` bindings in `for` loops, timers on a virtual clock.
//!
//! Safety limits: every statement, call and loop iteration costs a step; a script that exceeds
//! the step budget, the call depth, the object count, or the string and array size limits is
//! halted with an error the script cannot catch. There is no `unsafe`, no file or network access
//! and no `eval`; what a script can touch is exactly what the host hands it.
//!
//! Known differences from the standard (kept deliberately small): strings are Rust text and
//! `length` counts characters, not UTF-16 units; arrays have no holes; `let` has no temporal dead
//! zone; objects that form cycles are never freed (the whole interpreter is dropped with the
//! page); no `async`, generators, `Symbol` or `Date` objects yet (`Promise`, `Map`, `Set` and
//! regular expressions exist).

use crate::js::{self, *};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

/// A queued promise callback.
pub(crate) type Microtask = Box<dyn FnOnce(&mut Interp)>;

/// A thrown value (an `Err` in the interpreter's results).
pub type R<T> = Result<T, Value>;
/// A shared object.
pub type Obj = Rc<RefCell<Object>>;
/// A shared scope.
pub type ScopeRef = Rc<RefCell<Scope>>;
/// A host function: the interpreter, `this`, the arguments.
pub type NativeFn = Rc<dyn Fn(&mut Interp, &Value, &[Value]) -> R<Value>>;

/// Longest string a script may build, in bytes.
pub const MAX_STRING: usize = 8_000_000;
/// Largest array length.
pub const MAX_ARRAY: usize = 1_000_000;

/// A JavaScript value.
#[derive(Clone)]
pub enum Value {
    /// `undefined`
    Undefined,
    /// `null`
    Null,
    /// A boolean.
    Bool(bool),
    /// A number.
    Num(f64),
    /// A string.
    Str(Rc<str>),
    /// An object, array or function.
    Obj(Obj),
}

impl Value {
    /// A string value.
    #[must_use]
    pub fn str(s: &str) -> Value {
        Value::Str(Rc::from(s))
    }

    /// Whether the value counts as true in a condition.
    #[must_use]
    pub fn truthy(&self) -> bool {
        match self {
            Value::Undefined | Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0 && !n.is_nan(),
            Value::Str(s) => !s.is_empty(),
            Value::Obj(_) => true,
        }
    }

    fn is_nullish(&self) -> bool {
        matches!(self, Value::Undefined | Value::Null)
    }

    /// The object, if this is one.
    #[must_use]
    pub fn as_obj(&self) -> Option<&Obj> {
        if let Value::Obj(o) = self {
            Some(o)
        } else {
            None
        }
    }
}

/// A property.
#[derive(Clone)]
pub struct Prop {
    /// The value (for data properties).
    pub value: Value,
    /// The getter.
    pub get: Option<Value>,
    /// The setter.
    pub set: Option<Value>,
    /// Whether `for-in`, `Object.keys` and spread see it.
    pub enumerable: bool,
}

impl Prop {
    fn data(v: Value) -> Prop {
        Prop {
            value: v,
            get: None,
            set: None,
            enumerable: true,
        }
    }
    fn hidden(v: Value) -> Prop {
        Prop {
            value: v,
            get: None,
            set: None,
            enumerable: false,
        }
    }
}

/// How a function built from script is constructed.
pub struct CtorInfo {
    derived: bool,
    default: bool,
    fields: Rc<Vec<(Rc<str>, Option<Expr>)>>,
}

/// A function made from script.
pub struct Closure {
    func: Rc<Func>,
    scope: ScopeRef,
    home: Option<Obj>,
    ctor: Option<CtorInfo>,
}

/// What an object is.
pub enum Kind {
    /// An ordinary object.
    Plain,
    /// An array.
    Array(Vec<Value>),
    /// A function from script.
    Func(Closure),
    /// A host function and its name.
    Native(NativeFn, Rc<str>),
    /// `f.bind(this, ...args)`
    Bound(Value, Value, Vec<Value>),
    /// A `Map` or a `Set` (a set keeps `undefined` as every value).
    Map(MapData),
    /// A `Promise`.
    Promise(PromiseData),
    /// A regular expression and its source text.
    Regex(Rc<crate::regex::Regex>, Rc<str>),
}

/// What a value is when used as a `Map` key (same-value-zero equality).
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum MapKey {
    /// `undefined`
    Undef,
    /// `null`
    Null,
    /// A boolean.
    Bool(bool),
    /// A number (bit pattern, with `NaN` and `-0` normalised).
    Num(u64),
    /// A string.
    Str(Rc<str>),
    /// An object, by identity.
    Obj(usize),
}

impl MapKey {
    pub(crate) fn of(v: &Value) -> MapKey {
        match v {
            Value::Undefined => MapKey::Undef,
            Value::Null => MapKey::Null,
            Value::Bool(b) => MapKey::Bool(*b),
            Value::Num(n) if n.is_nan() => MapKey::Num(f64::NAN.to_bits()),
            Value::Num(n) if *n == 0.0 => MapKey::Num(0),
            Value::Num(n) => MapKey::Num(n.to_bits()),
            Value::Str(t) => MapKey::Str(t.clone()),
            Value::Obj(o) => MapKey::Obj(Rc::as_ptr(o) as *const () as usize),
        }
    }
}

/// The contents of a `Map` or `Set`: hashed lookup, insertion order kept.
#[derive(Default)]
pub struct MapData {
    pub(crate) index: HashMap<MapKey, usize>,
    pub(crate) entries: Vec<Option<(Value, Value)>>,
    pub(crate) live: usize,
}

/// A promise.
#[derive(Default)]
pub struct PromiseData {
    /// 0 pending, 1 fulfilled, 2 rejected.
    pub(crate) state: u8,
    pub(crate) value: Option<Value>,
    pub(crate) reactions: Vec<(Value, Value, Obj)>,
}

/// An object.
pub struct Object {
    /// The prototype.
    pub proto: Option<Obj>,
    /// Own properties.
    pub props: HashMap<Rc<str>, Prop>,
    pub(crate) order: Vec<Rc<str>>,
    /// What it is.
    pub kind: Kind,
    /// Set by `Object.freeze`.
    pub frozen: bool,
    /// A label such as `"Error"` used when printing.
    pub class: &'static str,
}

/// A variable binding.
pub struct Binding {
    v: Value,
    mutable: bool,
}

/// A scope.
pub struct Scope {
    vars: HashMap<Rc<str>, Binding>,
    parent: Option<ScopeRef>,
}

fn new_scope(parent: Option<&ScopeRef>) -> ScopeRef {
    Rc::new(RefCell::new(Scope {
        vars: HashMap::new(),
        parent: parent.cloned(),
    }))
}

/// The built-in prototypes.
pub struct Protos {
    pub(crate) object: Obj,
    pub(crate) function: Obj,
    pub(crate) array: Obj,
    pub(crate) string: Obj,
    pub(crate) number: Obj,
    pub(crate) boolean: Obj,
    pub(crate) promise: Obj,
    pub(crate) regexp: Obj,
    pub(crate) errors: HashMap<&'static str, Obj>,
}

struct Timer {
    id: u32,
    due: f64,
    every: Option<f64>,
    f: Value,
    args: Vec<Value>,
}

enum Flow {
    Normal,
    Return(Value),
    Break(Option<Rc<str>>),
    Continue(Option<Rc<str>>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Let,
    Const,
    Assign,
}

/// The interpreter.
pub struct Interp {
    pub(crate) global: ScopeRef,
    pub(crate) global_obj: Obj,
    pub(crate) protos: Protos,
    steps: u64,
    /// Steps a run may take before it is halted.
    pub step_limit: u64,
    depth: usize,
    /// Deepest call nesting.
    pub max_depth: usize,
    objects: usize,
    /// Most objects a script may create.
    pub object_limit: usize,
    fatal: Option<String>,
    short: bool,
    /// Lines written by `console.log` and friends (the newest are kept).
    pub console: Vec<String>,
    timers: Vec<Timer>,
    regex_cache: HashMap<(String, String), Rc<crate::regex::Regex>>,
    pub(crate) microtasks: VecDeque<Microtask>,
    next_timer: u32,
    /// The virtual clock in milliseconds.
    pub clock: f64,
}

fn index_key(k: &str) -> Option<usize> {
    if k.is_empty()
        || k.len() > 9
        || !k.bytes().all(|b| b.is_ascii_digit())
        || (k.len() > 1 && k.starts_with('0'))
    {
        return None;
    }
    k.parse().ok()
}

/// A number as JavaScript prints it.
#[must_use]
pub fn num_to_string(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n == 0.0 {
        "0".into()
    } else if n.is_infinite() {
        if n > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        }
    } else if n.abs() >= 1e21 || n.abs() < 1e-6 {
        let s = format!("{n:e}");
        match s.split_once('e') {
            Some((m, e)) if e.starts_with('-') => format!("{m}e{e}"),
            Some((m, e)) => format!("{m}e+{e}"),
            None => s,
        }
    } else {
        format!("{n}")
    }
}

/// A string read as a number.
#[must_use]
pub fn str_to_num(s: &str) -> f64 {
    let t = s.trim();
    if t.is_empty() {
        return 0.0;
    }
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u64::from_str_radix(h, 16).map_or(f64::NAN, |v| v as f64);
    }
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    if t.bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-'))
    {
        t.parse().unwrap_or(f64::NAN)
    } else {
        f64::NAN
    }
}

fn to_int32(n: f64) -> i32 {
    if !n.is_finite() {
        return 0;
    }
    n.trunc().rem_euclid(4_294_967_296.0) as u64 as u32 as i32
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}

impl Interp {
    /// A fresh interpreter with the standard library installed.
    #[must_use]
    pub fn new() -> Interp {
        let mk = |proto: Option<Obj>| -> Obj {
            Rc::new(RefCell::new(Object {
                proto,
                props: HashMap::new(),
                order: Vec::new(),
                kind: Kind::Plain,
                frozen: false,
                class: "Object",
            }))
        };
        let object = mk(None);
        let function = mk(Some(object.clone()));
        let array = mk(Some(object.clone()));
        let string = mk(Some(object.clone()));
        let number = mk(Some(object.clone()));
        let boolean = mk(Some(object.clone()));
        let promise = mk(Some(object.clone()));
        let regexp = mk(Some(object.clone()));
        let error = mk(Some(object.clone()));
        let mut errors = HashMap::new();
        errors.insert("Error", error);
        for n in [
            "TypeError",
            "RangeError",
            "ReferenceError",
            "SyntaxError",
            "EvalError",
        ] {
            errors.insert(n, mk(errors.get("Error").cloned()));
        }
        let global_obj = mk(Some(object.clone()));
        let mut i = Interp {
            global: new_scope(None),
            global_obj,
            protos: Protos {
                object,
                function,
                array,
                string,
                number,
                boolean,
                promise,
                regexp,
                errors,
            },
            steps: 0,
            step_limit: 5_000_000,
            depth: 0,
            max_depth: 100,
            objects: 0,
            object_limit: 2_000_000,
            fatal: None,
            short: false,
            console: Vec::new(),
            timers: Vec::new(),
            regex_cache: HashMap::new(),
            microtasks: VecDeque::new(),
            next_timer: 1,
            clock: 0.0,
        };
        crate::jslib::install(&mut i);
        i
    }

    // ------------------------------------------------------------ host interface

    /// Runs a script; the value of its last expression statement is returned.
    ///
    /// # Errors
    /// The error text, such as `Uncaught TypeError: x is not a function`.
    pub fn run(&mut self, src: &str) -> Result<Value, String> {
        let prog = js::parse(src).map_err(|e| e.to_string())?;
        self.run_program(&prog)
    }

    /// Runs an already parsed script.
    ///
    /// # Errors
    /// As [`Interp::run`].
    pub fn run_program(&mut self, prog: &[Stmt]) -> Result<Value, String> {
        self.steps = 0;
        self.fatal = None;
        let scope = self.global.clone();
        self.hoist_vars(prog, &scope);
        self.hoist_funcs(prog, &scope);
        let mut last = Value::Undefined;
        for st in prog {
            let r = match st {
                Stmt::Expr(e) => self.eval(e, &scope).map(|v| {
                    last = v;
                    Flow::Normal
                }),
                other => self.exec(other, &scope),
            };
            if let Err(e) = r {
                return Err(self.describe_error(&e));
            }
        }
        self.run_microtasks();
        Ok(last)
    }

    /// Defines a global name (a host object or function).
    pub fn set_global(&mut self, name: &str, v: Value) {
        self.global.borrow_mut().vars.insert(
            Rc::from(name),
            Binding {
                v: v.clone(),
                mutable: true,
            },
        );
        self.define(&self.global_obj.clone(), name, Prop::data(v));
    }

    /// Makes a host function.
    pub fn native(
        &mut self,
        name: &str,
        f: impl Fn(&mut Interp, &Value, &[Value]) -> R<Value> + 'static,
    ) -> Value {
        let proto = self.protos.function.clone();
        Value::Obj(self.alloc(
            Some(proto),
            Kind::Native(Rc::new(f), Rc::from(name)),
            "Function",
        ))
    }

    /// Makes a new empty object.
    pub fn new_obj(&mut self) -> Obj {
        let p = self.protos.object.clone();
        self.alloc(Some(p), Kind::Plain, "Object")
    }

    /// Makes a regular expression object from a pattern and flags.
    ///
    /// # Errors
    /// A thrown `SyntaxError` for a bad pattern or flags.
    pub fn new_regexp(&mut self, source: &str, flags: &str) -> R<Value> {
        let key = (source.to_string(), flags.to_string());
        let re = if let Some(r) = self.regex_cache.get(&key) {
            r.clone()
        } else {
            match crate::regex::Regex::new(source, flags) {
                Ok(r) => {
                    let r = Rc::new(r);
                    if self.regex_cache.len() < 500 {
                        self.regex_cache.insert(key, r.clone());
                    }
                    r
                }
                Err(m) => {
                    return self.throw(
                        "SyntaxError",
                        &format!("Invalid regular expression: /{source}/: {m}"),
                    )
                }
            }
        };
        let p = self.protos.regexp.clone();
        let o = self.alloc(Some(p), Kind::Regex(re, Rc::from(source)), "RegExp");
        self.define(&o, "lastIndex", Prop::hidden(Value::Num(0.0)));
        Ok(Value::Obj(o))
    }

    /// Makes an array.
    pub fn new_array(&mut self, items: Vec<Value>) -> Value {
        let p = self.protos.array.clone();
        Value::Obj(self.alloc(Some(p), Kind::Array(items), "Array"))
    }

    /// Sets an own, visible property.
    pub fn put(&mut self, o: &Obj, key: &str, v: Value) {
        self.define(o, key, Prop::data(v));
    }

    /// Reads a property.
    ///
    /// # Errors
    /// The thrown value (reading from `undefined`, a throwing getter).
    pub fn get_prop(&mut self, base: &Value, key: &str) -> R<Value> {
        self.get(base, key)
    }

    /// Sets a property (running setters).
    ///
    /// # Errors
    /// The thrown value.
    pub fn set_prop(&mut self, base: &Value, key: &str, v: Value) -> R<()> {
        self.set(base, key, v)
    }

    /// A global variable's value, if defined.
    #[must_use]
    pub fn get_global(&self, name: &str) -> Option<Value> {
        self.lookup(name, &self.global)
    }

    /// Calls a function value.
    ///
    /// # Errors
    /// The thrown value.
    pub fn call_value(&mut self, f: &Value, this: Value, args: Vec<Value>) -> R<Value> {
        self.call(f, this, args)
    }

    /// Why the last run was halted by a limit, if it was.
    #[must_use]
    pub fn halted(&self) -> Option<&str> {
        self.fatal.as_deref()
    }

    /// Reads a value as text.
    ///
    /// # Errors
    /// A throwing `toString`.
    pub fn text(&mut self, v: &Value) -> R<Rc<str>> {
        self.coerce_str(v)
    }

    /// Makes an error value of the given kind.
    pub fn make_error(&mut self, kind: &str, msg: &str) -> Value {
        let proto = self
            .protos
            .errors
            .get(kind)
            .or_else(|| self.protos.errors.get("Error"))
            .cloned();
        let o = self.alloc(proto, Kind::Plain, "Error");
        self.define(&o, "message", Prop::hidden(Value::str(msg)));
        Value::Obj(o)
    }

    fn throw<T>(&mut self, kind: &str, msg: &str) -> R<T> {
        Err(self.make_error(kind, msg))
    }

    /// The text shown for an uncaught error value.
    pub fn error_text(&mut self, e: &Value) -> String {
        self.describe_error(e)
    }

    fn describe_error(&mut self, e: &Value) -> String {
        if let Some(f) = &self.fatal {
            return format!("Halted: {f}");
        }
        if let Value::Obj(o) = e {
            if o.borrow().class == "Error" {
                let name = self
                    .get(e, "name")
                    .ok()
                    .and_then(|n| self.coerce_str(&n).ok())
                    .map_or_else(|| "Error".to_string(), |s| s.to_string());
                let msg = self
                    .get(e, "message")
                    .ok()
                    .and_then(|n| self.coerce_str(&n).ok())
                    .map_or_else(String::new, |s| s.to_string());
                return format!("Uncaught {name}: {msg}");
            }
        }
        format!("Uncaught {}", self.inspect(e))
    }

    // ------------------------------------------------------------ allocation and properties

    pub(crate) fn alloc(&mut self, proto: Option<Obj>, kind: Kind, class: &'static str) -> Obj {
        self.objects += 1;
        if self.objects > self.object_limit && self.fatal.is_none() {
            self.fatal = Some("too many objects".into());
        }
        Rc::new(RefCell::new(Object {
            proto,
            props: HashMap::new(),
            order: Vec::new(),
            kind,
            frozen: false,
            class,
        }))
    }

    pub(crate) fn define(&mut self, o: &Obj, key: &str, p: Prop) {
        let mut b = o.borrow_mut();
        let k: Rc<str> = Rc::from(key);
        if b.props.insert(k.clone(), p).is_none() {
            b.order.push(k);
        }
    }

    /// Own enumerable string keys in definition order (array indices first).
    pub(crate) fn own_keys(&self, o: &Obj) -> Vec<Rc<str>> {
        let b = o.borrow();
        let mut out: Vec<Rc<str>> = Vec::new();
        if let Kind::Array(v) = &b.kind {
            out.extend((0..v.len()).map(|i| Rc::from(i.to_string().as_str())));
        }
        out.extend(
            b.order
                .iter()
                .filter(|k| b.props.get(*k).is_some_and(|p| p.enumerable))
                .cloned(),
        );
        out
    }

    fn step(&mut self) -> R<()> {
        self.steps += 1;
        if self.steps > self.step_limit && self.fatal.is_none() {
            self.fatal = Some("the script ran for too long".into());
        }
        if let Some(f) = &self.fatal {
            let m = f.clone();
            return self.throw("RangeError", &m);
        }
        Ok(())
    }

    pub(crate) fn get(&mut self, base: &Value, key: &str) -> R<Value> {
        match base {
            Value::Obj(o) => self.get_obj(o, key, base),
            Value::Str(s) => {
                if key == "length" {
                    return Ok(Value::Num(s.chars().count() as f64));
                }
                if let Some(i) = index_key(key) {
                    return Ok(s.chars().nth(i).map_or(Value::Undefined, |c| {
                        Value::Str(Rc::from(c.to_string().as_str()))
                    }));
                }
                let p = self.protos.string.clone();
                self.get_obj(&p, key, base)
            }
            Value::Num(_) => {
                let p = self.protos.number.clone();
                self.get_obj(&p, key, base)
            }
            Value::Bool(_) => {
                let p = self.protos.boolean.clone();
                self.get_obj(&p, key, base)
            }
            Value::Undefined | Value::Null => {
                let what = if matches!(base, Value::Null) {
                    "null"
                } else {
                    "undefined"
                };
                self.throw(
                    "TypeError",
                    &format!("Cannot read properties of {what} (reading '{key}')"),
                )
            }
        }
    }

    fn get_obj(&mut self, o: &Obj, key: &str, this: &Value) -> R<Value> {
        if key == "prototype" {
            let make = {
                let b = o.borrow();
                matches!(&b.kind, Kind::Func(c) if !c.func.is_arrow)
                    && !b.props.contains_key("prototype")
            };
            if make {
                let p = self.new_obj();
                self.define(&p, "constructor", Prop::hidden(Value::Obj(o.clone())));
                self.define(o, "prototype", Prop::hidden(Value::Obj(p)));
            }
        }
        let mut cur = o.clone();
        loop {
            let next;
            {
                let b = cur.borrow();
                match &b.kind {
                    Kind::Array(v) => {
                        if key == "length" {
                            return Ok(Value::Num(v.len() as f64));
                        }
                        if let Some(i) = index_key(key) {
                            if let Some(x) = v.get(i) {
                                return Ok(x.clone());
                            }
                        }
                    }
                    Kind::Func(c) if key == "name" && !b.props.contains_key("name") => {
                        return Ok(Value::Str(
                            c.func.name.clone().unwrap_or_else(|| Rc::from("")),
                        ));
                    }
                    Kind::Native(_, n) if key == "name" && !b.props.contains_key("name") => {
                        return Ok(Value::Str(n.clone()))
                    }
                    _ => {}
                }
                if let Some(p) = b.props.get(key) {
                    if p.get.is_some() || p.set.is_some() {
                        let g = p.get.clone();
                        drop(b);
                        return match g {
                            Some(g) => self.call(&g, this.clone(), Vec::new()),
                            None => Ok(Value::Undefined),
                        };
                    }
                    return Ok(p.value.clone());
                }
                next = b.proto.clone();
            }
            match next {
                Some(n) => cur = n,
                None => return Ok(Value::Undefined),
            }
        }
    }

    pub(crate) fn set(&mut self, base: &Value, key: &str, v: Value) -> R<()> {
        let o = match base {
            Value::Obj(o) => o.clone(),
            Value::Undefined | Value::Null => {
                return self.throw(
                    "TypeError",
                    &format!(
                        "Cannot set properties of {} (setting '{key}')",
                        if matches!(base, Value::Null) {
                            "null"
                        } else {
                            "undefined"
                        }
                    ),
                );
            }
            _ => return Ok(()),
        };
        {
            let mut b = o.borrow_mut();
            if b.frozen {
                return Ok(());
            }
            if let Kind::Array(arr) = &mut b.kind {
                if key == "length" {
                    let n = match v {
                        Value::Num(n) if n >= 0.0 && n.fract() == 0.0 && n <= MAX_ARRAY as f64 => {
                            n as usize
                        }
                        _ => {
                            drop(b);
                            return self.throw("RangeError", "Invalid array length");
                        }
                    };
                    arr.resize(n, Value::Undefined);
                    return Ok(());
                }
                if let Some(i) = index_key(key) {
                    if i >= MAX_ARRAY {
                        drop(b);
                        return self.throw("RangeError", "the array is too large");
                    }
                    if i >= arr.len() {
                        arr.resize(i + 1, Value::Undefined);
                    }
                    arr[i] = v;
                    return Ok(());
                }
            }
        }
        // A setter anywhere on the chain wins.
        let mut cur = Some(o.clone());
        while let Some(c) = cur {
            let setter = {
                let b = c.borrow();
                match b.props.get(key) {
                    Some(p) if p.get.is_some() || p.set.is_some() => Some(p.set.clone()),
                    Some(_) => None,
                    None => {
                        cur = b.proto.clone();
                        continue;
                    }
                }
            };
            return match setter {
                Some(Some(f)) => self.call(&f, base.clone(), vec![v]).map(|_| ()),
                Some(None) => Ok(()),
                None => {
                    self.define_data(&o, key, v);
                    Ok(())
                }
            };
        }
        self.define_data(&o, key, v);
        Ok(())
    }

    fn define_data(&mut self, o: &Obj, key: &str, v: Value) {
        let mut b = o.borrow_mut();
        if let Some(p) = b.props.get_mut(key) {
            p.value = v;
        } else {
            let k: Rc<str> = Rc::from(key);
            b.props.insert(k.clone(), Prop::data(v));
            b.order.push(k);
        }
    }

    fn delete(&mut self, base: &Value, key: &str) -> bool {
        if let Value::Obj(o) = base {
            let mut b = o.borrow_mut();
            if b.frozen {
                return false;
            }
            if let Kind::Array(a) = &mut b.kind {
                if let Some(i) = index_key(key) {
                    if i < a.len() {
                        a[i] = Value::Undefined;
                    }
                    return true;
                }
            }
            if b.props.remove(key).is_some() {
                b.order.retain(|k| &**k != key);
            }
        }
        true
    }

    fn has_property(&self, o: &Obj, key: &str) -> bool {
        let mut cur = Some(o.clone());
        while let Some(c) = cur {
            let b = c.borrow();
            if b.props.contains_key(key) {
                return true;
            }
            if let Kind::Array(v) = &b.kind {
                if key == "length" || index_key(key).is_some_and(|i| i < v.len()) {
                    return true;
                }
            }
            cur = b.proto.clone();
        }
        false
    }

    // ------------------------------------------------------------ conversions

    pub(crate) fn coerce_primitive(&mut self, v: &Value) -> R<Value> {
        let Value::Obj(o) = v else {
            return Ok(v.clone());
        };
        for name in ["valueOf", "toString"] {
            let f = self.get(v, name)?;
            if is_callable(&f) {
                let r = self.call(&f, v.clone(), Vec::new())?;
                if !matches!(r, Value::Obj(_)) {
                    return Ok(r);
                }
            }
        }
        let _ = o;
        self.throw("TypeError", "cannot convert an object to a primitive value")
    }

    pub(crate) fn coerce_str(&mut self, v: &Value) -> R<Rc<str>> {
        Ok(match v {
            Value::Undefined => Rc::from("undefined"),
            Value::Null => Rc::from("null"),
            Value::Bool(b) => Rc::from(if *b { "true" } else { "false" }),
            Value::Num(n) => Rc::from(num_to_string(*n).as_str()),
            Value::Str(s) => s.clone(),
            Value::Obj(_) => {
                let p = self.coerce_primitive(v)?;
                self.coerce_str(&p)?
            }
        })
    }

    pub(crate) fn coerce_num(&mut self, v: &Value) -> R<f64> {
        Ok(match v {
            Value::Undefined => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => f64::from(u8::from(*b)),
            Value::Num(n) => *n,
            Value::Str(s) => str_to_num(s),
            Value::Obj(_) => {
                let p = self.coerce_primitive(v)?;
                self.coerce_num(&p)?
            }
        })
    }

    fn key_of(&mut self, v: &Value) -> R<Rc<str>> {
        match v {
            Value::Num(n) if n.fract() == 0.0 && n.abs() < 1e15 => {
                Ok(Rc::from((*n as i64).to_string().as_str()))
            }
            _ => self.coerce_str(v),
        }
    }

    fn strict_equals(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Num(x), Value::Num(y)) => x == y,
            (Value::Str(x), Value::Str(y)) => x == y,
            (Value::Obj(x), Value::Obj(y)) => Rc::ptr_eq(x, y),
            _ => false,
        }
    }

    fn loose_equals(&mut self, a: &Value, b: &Value) -> R<bool> {
        Ok(match (a, b) {
            (Value::Undefined | Value::Null, Value::Undefined | Value::Null) => true,
            (Value::Undefined | Value::Null, _) | (_, Value::Undefined | Value::Null) => false,
            (Value::Obj(_), Value::Obj(_)) => Self::strict_equals(a, b),
            (Value::Obj(_), _) => {
                let p = self.coerce_primitive(a)?;
                return self.loose_equals(&p, b);
            }
            (_, Value::Obj(_)) => {
                let p = self.coerce_primitive(b)?;
                return self.loose_equals(a, &p);
            }
            (Value::Str(x), Value::Str(y)) => x == y,
            _ => self.coerce_num(a)? == self.coerce_num(b)?,
        })
    }

    fn type_of(v: &Value) -> &'static str {
        match v {
            Value::Undefined => "undefined",
            Value::Null => "object",
            Value::Bool(_) => "boolean",
            Value::Num(_) => "number",
            Value::Str(_) => "string",
            Value::Obj(o) => match o.borrow().kind {
                Kind::Func(_) | Kind::Native(..) | Kind::Bound(..) => "function",
                _ => "object",
            },
        }
    }

    // ------------------------------------------------------------ printing

    /// Shows a value the way `console.log` does.
    pub fn inspect(&mut self, v: &Value) -> String {
        let mut seen = Vec::new();
        self.inspect_in(v, 0, &mut seen, true)
    }

    fn inspect_in(&mut self, v: &Value, depth: usize, seen: &mut Vec<Obj>, top: bool) -> String {
        match v {
            Value::Str(s) if top => s.to_string(),
            Value::Str(s) => format!("'{s}'"),
            Value::Obj(o) => {
                if seen.iter().any(|x| Rc::ptr_eq(x, o)) {
                    return "[Circular]".into();
                }
                let (is_fn, arr, class) = {
                    let b = o.borrow();
                    (
                        matches!(b.kind, Kind::Func(_) | Kind::Native(..) | Kind::Bound(..)),
                        if let Kind::Array(a) = &b.kind {
                            Some(a.clone())
                        } else {
                            None
                        },
                        b.class,
                    )
                };
                if is_fn {
                    let n = self
                        .get(v, "name")
                        .ok()
                        .and_then(|n| self.coerce_str(&n).ok())
                        .unwrap_or_else(|| Rc::from(""));
                    return if n.is_empty() {
                        "[Function (anonymous)]".into()
                    } else {
                        format!("[Function: {n}]")
                    };
                }
                if class == "Error" {
                    let name = self
                        .get(v, "name")
                        .ok()
                        .and_then(|n| self.coerce_str(&n).ok())
                        .unwrap_or_else(|| Rc::from("Error"));
                    let msg = self
                        .get(v, "message")
                        .ok()
                        .and_then(|n| self.coerce_str(&n).ok())
                        .unwrap_or_else(|| Rc::from(""));
                    return if msg.is_empty() {
                        name.to_string()
                    } else {
                        format!("{name}: {msg}")
                    };
                }
                if let Kind::Regex(re, src) = &o.borrow().kind {
                    return format!("/{src}/{}", crate::jslib::flags_string(&re.flags));
                }
                let special = {
                    let b = o.borrow();
                    match &b.kind {
                        Kind::Map(m) => Some((
                            b.class == "Set",
                            m.entries.iter().flatten().cloned().collect::<Vec<_>>(),
                            m.live,
                        )),
                        Kind::Promise(p) => {
                            let t = match (p.state, &p.value) {
                                (1, Some(v)) => Some(v.clone()),
                                (2, Some(v)) => Some(v.clone()),
                                _ => None,
                            };
                            drop(b);
                            let st = o.borrow();
                            let Kind::Promise(p) = &st.kind else {
                                return String::new();
                            };
                            let label = match p.state {
                                0 => "<pending>".to_string(),
                                1 => String::new(),
                                _ => "<rejected> ".to_string(),
                            };
                            drop(st);
                            let inner = t
                                .map(|v| self.inspect_in(&v, depth + 1, seen, false))
                                .unwrap_or_default();
                            return format!("Promise {{ {label}{inner} }}");
                        }
                        _ => None,
                    }
                };
                if let Some((is_set, items, live)) = special {
                    if depth > 2 {
                        return if is_set {
                            "[Set]".into()
                        } else {
                            "[Map]".into()
                        };
                    }
                    seen.push(o.clone());
                    let parts: Vec<String> = items
                        .iter()
                        .take(100)
                        .map(|(k, v)| {
                            let ks = self.inspect_in(k, depth + 1, seen, false);
                            if is_set {
                                ks
                            } else {
                                format!("{ks} => {}", self.inspect_in(v, depth + 1, seen, false))
                            }
                        })
                        .collect();
                    seen.pop();
                    let name = if is_set { "Set" } else { "Map" };
                    return if parts.is_empty() {
                        format!("{name}({live}) {{}}")
                    } else {
                        format!("{name}({live}) {{ {} }}", parts.join(", "))
                    };
                }
                if depth > 2 {
                    return if arr.is_some() {
                        "[Array]".into()
                    } else {
                        "[Object]".into()
                    };
                }
                seen.push(o.clone());
                let out = if let Some(items) = arr {
                    let parts: Vec<String> = items
                        .iter()
                        .take(100)
                        .map(|x| self.inspect_in(x, depth + 1, seen, false))
                        .collect();
                    let more = if items.len() > 100 {
                        format!(", ... {} more items", items.len() - 100)
                    } else {
                        String::new()
                    };
                    if parts.is_empty() {
                        "[]".into()
                    } else {
                        format!("[ {}{more} ]", parts.join(", "))
                    }
                } else {
                    let keys = self.own_keys(o);
                    let mut parts = Vec::new();
                    for k in keys.iter().take(100) {
                        let val = o
                            .borrow()
                            .props
                            .get(k)
                            .map(|p| (p.value.clone(), p.get.is_some() || p.set.is_some()));
                        let text = match val {
                            Some((_, true)) => "[Getter/Setter]".to_string(),
                            Some((x, false)) => self.inspect_in(&x, depth + 1, seen, false),
                            None => continue,
                        };
                        parts.push(format!("{k}: {text}"));
                    }
                    if parts.is_empty() {
                        "{}".into()
                    } else {
                        format!("{{ {} }}", parts.join(", "))
                    }
                };
                seen.pop();
                out
            }
            other => self
                .coerce_str(other)
                .map(|s| s.to_string())
                .unwrap_or_default(),
        }
    }

    // ------------------------------------------------------------ scopes

    fn lookup(&self, name: &str, s: &ScopeRef) -> Option<Value> {
        let mut cur = s.clone();
        loop {
            let next = {
                let b = cur.borrow();
                if let Some(x) = b.vars.get(name) {
                    return Some(x.v.clone());
                }
                b.parent.clone()
            };
            cur = next?;
        }
    }

    fn declare(&self, s: &ScopeRef, name: &Rc<str>, v: Value, mutable: bool) {
        s.borrow_mut()
            .vars
            .insert(name.clone(), Binding { v, mutable });
    }

    fn assign_ident(&mut self, name: &Rc<str>, v: Value, s: &ScopeRef) -> R<()> {
        let mut cur = s.clone();
        loop {
            let next = {
                let mut b = cur.borrow_mut();
                if let Some(x) = b.vars.get_mut(name) {
                    if !x.mutable {
                        drop(b);
                        return self.throw("TypeError", "Assignment to constant variable.");
                    }
                    x.v = v;
                    return Ok(());
                }
                b.parent.clone()
            };
            match next {
                Some(n) => cur = n,
                None => break,
            }
        }
        // An undeclared name becomes a global (sloppy mode).
        let g = self.global.clone();
        self.declare(&g, name, v, true);
        Ok(())
    }

    fn hoist_vars(&self, stmts: &[Stmt], s: &ScopeRef) {
        for st in stmts {
            match st {
                Stmt::Var(VarKind::Var, decls) => {
                    for (p, _) in decls {
                        self.hoist_pattern(p, s);
                    }
                }
                Stmt::If(_, a, b) => {
                    self.hoist_vars(std::slice::from_ref(a), s);
                    if let Some(b) = b {
                        self.hoist_vars(std::slice::from_ref(b), s);
                    }
                }
                Stmt::For(init, _, _, body) => {
                    if let Some(i) = init {
                        self.hoist_vars(std::slice::from_ref(i), s);
                    }
                    self.hoist_vars(std::slice::from_ref(body), s);
                }
                Stmt::ForIn(init, _, body, _) => {
                    self.hoist_vars(std::slice::from_ref(init), s);
                    self.hoist_vars(std::slice::from_ref(body), s);
                }
                Stmt::While(_, b) | Stmt::DoWhile(b, _) | Stmt::Labeled(_, b) => {
                    self.hoist_vars(std::slice::from_ref(b), s)
                }
                Stmt::Block(b) => self.hoist_vars(b, s),
                Stmt::Try(a, c, f) => {
                    self.hoist_vars(a, s);
                    if let Some((_, c)) = c {
                        self.hoist_vars(c, s);
                    }
                    if let Some(f) = f {
                        self.hoist_vars(f, s);
                    }
                }
                Stmt::Switch(_, cases) => {
                    for (_, b) in cases {
                        self.hoist_vars(b, s);
                    }
                }
                _ => {}
            }
        }
    }

    fn hoist_pattern(&self, p: &Pattern, s: &ScopeRef) {
        match p {
            Pattern::Ident(n) => {
                if !s.borrow().vars.contains_key(n) {
                    self.declare(s, n, Value::Undefined, true);
                }
            }
            Pattern::Array(items, rest) => {
                for (p, _) in items.iter().flatten() {
                    self.hoist_pattern(p, s);
                }
                if let Some(r) = rest {
                    self.hoist_pattern(r, s);
                }
            }
            Pattern::Object(props, rest) => {
                for (_, p, _) in props {
                    self.hoist_pattern(p, s);
                }
                if let Some(r) = rest {
                    self.hoist_pattern(r, s);
                }
            }
            Pattern::Member(_) => {}
        }
    }

    fn hoist_funcs(&mut self, stmts: &[Stmt], s: &ScopeRef) {
        for st in stmts {
            if let Stmt::Func(f) = st {
                let fv = self.make_function(f.clone(), s, None);
                if let Some(n) = &f.name {
                    self.declare(s, n, fv, true);
                }
            }
        }
    }

    fn make_function(&mut self, f: Rc<Func>, s: &ScopeRef, home: Option<Obj>) -> Value {
        let proto = self.protos.function.clone();
        Value::Obj(self.alloc(
            Some(proto),
            Kind::Func(Closure {
                func: f,
                scope: s.clone(),
                home,
                ctor: None,
            }),
            "Function",
        ))
    }

    // ------------------------------------------------------------ calls

    pub(crate) fn call(&mut self, f: &Value, this: Value, args: Vec<Value>) -> R<Value> {
        let Value::Obj(fo) = f else {
            return self.throw("TypeError", "value is not a function");
        };
        enum Plan {
            Native(NativeFn),
            Bound(Value, Value, Vec<Value>),
            Closure,
            No,
        }
        let plan = match &fo.borrow().kind {
            Kind::Native(n, _) => Plan::Native(n.clone()),
            Kind::Bound(t, th, a) => Plan::Bound(t.clone(), th.clone(), a.clone()),
            Kind::Func(c) => {
                if c.ctor.is_some() {
                    Plan::No
                } else {
                    Plan::Closure
                }
            }
            _ => Plan::No,
        };
        match plan {
            Plan::Native(n) => {
                self.step()?;
                n(self, &this, &args)
            }
            Plan::Bound(t, th, mut a) => {
                a.extend(args);
                self.call(&t, th, a)
            }
            Plan::Closure => self.call_closure(fo, this, args),
            Plan::No => {
                let is_class = matches!(&fo.borrow().kind, Kind::Func(c) if c.ctor.is_some());
                if is_class {
                    self.throw(
                        "TypeError",
                        "Class constructor cannot be invoked without 'new'",
                    )
                } else {
                    self.throw("TypeError", "value is not a function")
                }
            }
        }
    }

    fn call_closure(&mut self, fo: &Obj, this: Value, args: Vec<Value>) -> R<Value> {
        self.step()?;
        if self.depth >= self.max_depth {
            return self.throw("RangeError", "Maximum call stack size exceeded");
        }
        let (func, scope, home) = match &fo.borrow().kind {
            Kind::Func(c) => (c.func.clone(), c.scope.clone(), c.home.clone()),
            _ => return self.throw("TypeError", "value is not a function"),
        };
        let s = new_scope(Some(&scope));
        if !func.is_arrow {
            if let Some(n) = &func.name {
                self.declare(&s, n, Value::Obj(fo.clone()), true);
            }
            self.declare(&s, &Rc::from("this"), this, true);
            self.declare(&s, &Rc::from("%func"), Value::Obj(fo.clone()), false);
            if let Some(h) = home {
                self.declare(&s, &Rc::from("%home"), Value::Obj(h), false);
            }
            if func.uses_arguments {
                let a = self.new_array(args.clone());
                self.declare(&s, &Rc::from("arguments"), a, true);
            }
        }
        self.depth += 1;
        let r = self.run_body(&func, &s, args);
        self.depth -= 1;
        r
    }

    fn run_body(&mut self, func: &Rc<Func>, s: &ScopeRef, args: Vec<Value>) -> R<Value> {
        for (i, p) in func.params.iter().enumerate() {
            if p.rest {
                let rest = self.new_array(args.get(i..).map(<[Value]>::to_vec).unwrap_or_default());
                self.bind_pattern(&p.pat, rest, s, Mode::Let)?;
                break;
            }
            let mut v = args.get(i).cloned().unwrap_or(Value::Undefined);
            if matches!(v, Value::Undefined) {
                if let Some(d) = &p.default {
                    v = self.eval(d, s)?;
                }
            }
            self.bind_pattern(&p.pat, v, s, Mode::Let)?;
        }
        self.hoist_vars(&func.body, s);
        self.hoist_funcs(&func.body, s);
        for st in &func.body {
            match self.exec(st, s)? {
                Flow::Return(v) => return Ok(v),
                Flow::Normal => {}
                _ => break,
            }
        }
        Ok(Value::Undefined)
    }

    fn construct(&mut self, f: &Value, args: Vec<Value>) -> R<Value> {
        let Value::Obj(fo) = f else {
            return self.throw("TypeError", "value is not a constructor");
        };
        let (is_arrow, is_callable_kind) = match &fo.borrow().kind {
            Kind::Func(c) => (c.func.is_arrow, true),
            Kind::Native(..) | Kind::Bound(..) => (false, true),
            _ => (false, false),
        };
        if !is_callable_kind || is_arrow {
            return self.throw("TypeError", "value is not a constructor");
        }
        if let Kind::Bound(t, _, a) = &fo.borrow().kind {
            let (t, mut a) = (t.clone(), a.clone());
            a.extend(args);
            return self.construct(&t, a);
        }
        let proto = match self.get(f, "prototype")? {
            Value::Obj(p) => p,
            _ => self.protos.object.clone(),
        };
        let this = Value::Obj(self.alloc(Some(proto), Kind::Plain, "Object"));
        let r = self.run_ctor(fo, &this, args)?;
        Ok(if matches!(r, Value::Obj(_)) { r } else { this })
    }

    fn run_ctor(&mut self, fo: &Obj, this: &Value, args: Vec<Value>) -> R<Value> {
        enum P {
            Native(NativeFn),
            Closure {
                derived: bool,
                default: bool,
                has_info: bool,
            },
        }
        let p = match &fo.borrow().kind {
            Kind::Native(n, _) => P::Native(n.clone()),
            Kind::Func(c) => P::Closure {
                derived: c.ctor.as_ref().is_some_and(|i| i.derived),
                default: c.ctor.as_ref().is_some_and(|i| i.default),
                has_info: c.ctor.is_some(),
            },
            _ => return self.throw("TypeError", "value is not a constructor"),
        };
        match p {
            P::Native(n) => {
                self.step()?;
                n(self, this, &args)
            }
            P::Closure {
                derived,
                default,
                has_info,
            } => {
                if derived && default {
                    let parent = fo.borrow().proto.clone();
                    if let Some(parent) = parent {
                        self.run_ctor(&parent, this, args)?;
                    }
                    self.init_fields(fo, this)?;
                    return Ok(Value::Undefined);
                }
                if has_info && !derived {
                    self.init_fields(fo, this)?;
                    if default {
                        return Ok(Value::Undefined);
                    }
                }
                self.call_closure(fo, this.clone(), args)
            }
        }
    }

    fn init_fields(&mut self, fo: &Obj, this: &Value) -> R<()> {
        let (fields, scope) = match &fo.borrow().kind {
            Kind::Func(c) => match &c.ctor {
                Some(i) if !i.fields.is_empty() => (i.fields.clone(), c.scope.clone()),
                _ => return Ok(()),
            },
            _ => return Ok(()),
        };
        let s = new_scope(Some(&scope));
        self.declare(&s, &Rc::from("this"), this.clone(), true);
        for (k, init) in fields.iter() {
            let v = match init {
                Some(e) => self.eval(e, &s)?,
                None => Value::Undefined,
            };
            if let Value::Obj(o) = this {
                self.define(o, k, Prop::data(v));
            }
        }
        Ok(())
    }

    fn super_call(&mut self, args: &[Elem], s: &ScopeRef) -> R<Value> {
        let args = self.eval_args(args, s)?;
        let (Some(Value::Obj(func)), Some(this)) =
            (self.lookup("%func", s), self.lookup("this", s))
        else {
            return self.throw(
                "SyntaxError",
                "`super` is only valid inside a class constructor",
            );
        };
        let parent = func.borrow().proto.clone();
        let Some(parent) = parent else {
            return self.throw("TypeError", "the class has no parent to call");
        };
        self.run_ctor(&parent, &this, args)?;
        self.init_fields(&func, &this)?;
        Ok(Value::Undefined)
    }

    fn instance_of(&mut self, v: &Value, c: &Value) -> R<bool> {
        if !is_callable(c) {
            return self.throw(
                "TypeError",
                "the right side of `instanceof` is not callable",
            );
        }
        let Value::Obj(o) = v else { return Ok(false) };
        let Value::Obj(target) = self.get(c, "prototype")? else {
            return Ok(false);
        };
        let mut cur = o.borrow().proto.clone();
        while let Some(p) = cur {
            if Rc::ptr_eq(&p, &target) {
                return Ok(true);
            }
            cur = p.borrow().proto.clone();
        }
        Ok(false)
    }

    pub(crate) fn iter_values(&mut self, v: &Value) -> R<Vec<Value>> {
        match v {
            Value::Obj(o) => {
                {
                    let b = o.borrow();
                    match &b.kind {
                        Kind::Array(a) => return Ok(a.clone()),
                        Kind::Map(m) => {
                            let is_set = b.class == "Set";
                            let items: Vec<(Value, Value)> =
                                m.entries.iter().flatten().cloned().collect();
                            drop(b);
                            return Ok(items
                                .into_iter()
                                .map(|(k, v)| {
                                    if is_set {
                                        k
                                    } else {
                                        self.new_array(vec![k, v])
                                    }
                                })
                                .collect());
                        }
                        _ => {}
                    }
                }
                self.throw("TypeError", "the value is not iterable")
            }
            Value::Str(s) => Ok(s
                .chars()
                .map(|c| Value::Str(Rc::from(c.to_string().as_str())))
                .collect()),
            _ => self.throw("TypeError", "the value is not iterable"),
        }
    }

    // ------------------------------------------------------------ patterns

    fn bind_pattern(&mut self, p: &Pattern, v: Value, s: &ScopeRef, mode: Mode) -> R<()> {
        match p {
            Pattern::Ident(n) => match mode {
                Mode::Let => {
                    self.declare(s, n, v, true);
                    Ok(())
                }
                Mode::Const => {
                    self.declare(s, n, v, false);
                    Ok(())
                }
                Mode::Assign => self.assign_ident(n, v, s),
            },
            Pattern::Array(items, rest) => {
                let vals = self.iter_values(&v)?;
                for (i, it) in items.iter().enumerate() {
                    let Some((pat, default)) = it else { continue };
                    let mut x = vals.get(i).cloned().unwrap_or(Value::Undefined);
                    if matches!(x, Value::Undefined) {
                        if let Some(d) = default {
                            x = self.eval(d, s)?;
                        }
                    }
                    self.bind_pattern(pat, x, s, mode)?;
                }
                if let Some(r) = rest {
                    let tail = self.new_array(
                        vals.get(items.len()..)
                            .map(<[Value]>::to_vec)
                            .unwrap_or_default(),
                    );
                    self.bind_pattern(r, tail, s, mode)?;
                }
                Ok(())
            }
            Pattern::Object(props, rest) => {
                if v.is_nullish() {
                    return self.throw("TypeError", "Cannot destructure a null or undefined value");
                }
                let mut used: Vec<Rc<str>> = Vec::new();
                for (k, pat, default) in props {
                    let key = self.eval_key(k, s)?;
                    let mut x = self.get(&v, &key)?;
                    used.push(key);
                    if matches!(x, Value::Undefined) {
                        if let Some(d) = default {
                            x = self.eval(d, s)?;
                        }
                    }
                    self.bind_pattern(pat, x, s, mode)?;
                }
                if let Some(r) = rest {
                    let o = self.new_obj();
                    if let Value::Obj(src) = &v {
                        for k in self.own_keys(src) {
                            if !used.contains(&k) {
                                let x = self.get(&v, &k)?;
                                self.put(&o, &k, x);
                            }
                        }
                    }
                    self.bind_pattern(r, Value::Obj(o), s, mode)?;
                }
                Ok(())
            }
            Pattern::Member(e) => {
                let Expr::Member { obj, prop, .. } = &**e else {
                    return self.throw("SyntaxError", "invalid assignment target");
                };
                let (base, _) = self.member_base(obj, s)?;
                let key = self.eval_key(prop, s)?;
                self.set(&base, &key, v)
            }
        }
    }

    fn eval_key(&mut self, k: &PropKey, s: &ScopeRef) -> R<Rc<str>> {
        match k {
            PropKey::Name(n) => Ok(n.clone()),
            PropKey::Computed(e) => {
                let v = self.eval(e, s)?;
                self.key_of(&v)
            }
        }
    }

    fn member_base(&mut self, obj: &Expr, s: &ScopeRef) -> R<(Value, Value)> {
        if let Expr::Super = obj {
            let home = self.lookup("%home", s);
            let this = self.lookup("this", s).unwrap_or(Value::Undefined);
            let base = match home {
                Some(Value::Obj(h)) => h
                    .borrow()
                    .proto
                    .clone()
                    .map_or(Value::Undefined, Value::Obj),
                _ => Value::Undefined,
            };
            // Writes through `super.x = v` go to `this`.
            return Ok((
                if matches!(base, Value::Undefined) {
                    this.clone()
                } else {
                    base
                },
                this,
            ));
        }
        let v = self.eval(obj, s)?;
        Ok((v.clone(), v))
    }

    // ------------------------------------------------------------ statements

    fn exec(&mut self, st: &Stmt, s: &ScopeRef) -> R<Flow> {
        self.exec_with(st, s, &[])
    }

    fn exec_block(&mut self, stmts: &[Stmt], s: &ScopeRef) -> R<Flow> {
        self.hoist_funcs(stmts, s);
        for st in stmts {
            match self.exec(st, s)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    fn loop_control(flow: Flow, labels: &[Rc<str>]) -> Result<bool, Flow> {
        // Ok(true) = leave the loop normally; Ok(false) = go on; Err(flow) = propagate.
        match flow {
            Flow::Normal => Ok(false),
            Flow::Break(None) => Ok(true),
            Flow::Break(Some(l)) if labels.contains(&l) => Ok(true),
            Flow::Continue(None) => Ok(false),
            Flow::Continue(Some(l)) if labels.contains(&l) => Ok(false),
            other => Err(other),
        }
    }

    fn exec_with(&mut self, st: &Stmt, s: &ScopeRef, labels: &[Rc<str>]) -> R<Flow> {
        self.step()?;
        match st {
            Stmt::Expr(e) => {
                self.eval(e, s)?;
                Ok(Flow::Normal)
            }
            Stmt::Var(kind, decls) => {
                for (pat, init) in decls {
                    let v = match init {
                        Some(e) => self.eval(e, s)?,
                        None if *kind == VarKind::Var => continue,
                        None => Value::Undefined,
                    };
                    let mode = match kind {
                        VarKind::Var => Mode::Assign,
                        VarKind::Let => Mode::Let,
                        VarKind::Const => Mode::Const,
                    };
                    self.bind_pattern(pat, v, s, mode)?;
                }
                Ok(Flow::Normal)
            }
            Stmt::Func(_) | Stmt::Empty => Ok(Flow::Normal),
            Stmt::Class(c) => {
                let v = self.eval_class(c, s)?;
                if let Some(n) = &c.name {
                    self.declare(s, n, v, true);
                }
                Ok(Flow::Normal)
            }
            Stmt::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e, s)?,
                    None => Value::Undefined,
                };
                Ok(Flow::Return(v))
            }
            Stmt::If(c, a, b) => {
                if self.eval(c, s)?.truthy() {
                    self.exec(a, s)
                } else if let Some(b) = b {
                    self.exec(b, s)
                } else {
                    Ok(Flow::Normal)
                }
            }
            Stmt::Block(b) => {
                let inner = new_scope(Some(s));
                self.exec_block(b, &inner)
            }
            Stmt::While(c, body) => {
                while self.eval(c, s)?.truthy() {
                    self.step()?;
                    match Self::loop_control(self.exec(body, s)?, labels) {
                        Ok(true) => break,
                        Ok(false) => {}
                        Err(f) => return Ok(f),
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::DoWhile(body, c) => {
                loop {
                    self.step()?;
                    match Self::loop_control(self.exec(body, s)?, labels) {
                        Ok(true) => break,
                        Ok(false) => {}
                        Err(f) => return Ok(f),
                    }
                    if !self.eval(c, s)?.truthy() {
                        break;
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::For(init, test, update, body) => self.exec_for(
                init.as_deref(),
                test.as_ref(),
                update.as_ref(),
                body,
                s,
                labels,
            ),
            Stmt::ForIn(init, iter, body, of) => self.exec_for_in(init, iter, body, *of, s, labels),
            Stmt::Break(l) => Ok(Flow::Break(l.clone())),
            Stmt::Continue(l) => Ok(Flow::Continue(l.clone())),
            Stmt::Throw(e) => {
                let v = self.eval(e, s)?;
                Err(v)
            }
            Stmt::Try(block, catch, finally) => {
                let inner = new_scope(Some(s));
                let mut r = self.exec_block(block, &inner);
                if let (Err(e), Some((param, cbody))) = (&r, catch) {
                    if self.fatal.is_none() {
                        let cs = new_scope(Some(s));
                        let e = e.clone();
                        r = match param {
                            Some(p) => self
                                .bind_pattern(p, e, &cs, Mode::Let)
                                .and_then(|()| self.exec_block(cbody, &cs)),
                            None => self.exec_block(cbody, &cs),
                        };
                    }
                }
                if let Some(f) = finally {
                    let fs = new_scope(Some(s));
                    match self.exec_block(f, &fs)? {
                        Flow::Normal => {}
                        other => return Ok(other),
                    }
                }
                r
            }
            Stmt::Switch(disc, cases) => {
                let d = self.eval(disc, s)?;
                let inner = new_scope(Some(s));
                let mut start = None;
                for (i, (test, _)) in cases.iter().enumerate() {
                    if let Some(t) = test {
                        let tv = self.eval(t, &inner)?;
                        if Self::strict_equals(&d, &tv) {
                            start = Some(i);
                            break;
                        }
                    }
                }
                let start = start.or_else(|| cases.iter().position(|(t, _)| t.is_none()));
                if let Some(start) = start {
                    for (_, body) in &cases[start..] {
                        match self.exec_block(body, &inner)? {
                            Flow::Normal => {}
                            Flow::Break(None) => return Ok(Flow::Normal),
                            other => return Ok(other),
                        }
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::Labeled(l, body) => {
                let mut ls = labels.to_vec();
                ls.push(l.clone());
                match self.exec_with(body, s, &ls)? {
                    Flow::Break(Some(b)) if b == *l => Ok(Flow::Normal),
                    other => Ok(other),
                }
            }
        }
    }

    fn exec_for(
        &mut self,
        init: Option<&Stmt>,
        test: Option<&Expr>,
        update: Option<&Expr>,
        body: &Stmt,
        s: &ScopeRef,
        labels: &[Rc<str>],
    ) -> R<Flow> {
        let mut lexical: Vec<Rc<str>> = Vec::new();
        let mut cur = new_scope(Some(s));
        if let Some(i) = init {
            if let Stmt::Var(k, decls) = i {
                if *k != VarKind::Var {
                    for (p, _) in decls {
                        collect_names(p, &mut lexical);
                    }
                }
            }
            self.exec(i, &cur)?;
        }
        loop {
            self.step()?;
            if let Some(t) = test {
                if !self.eval(t, &cur)?.truthy() {
                    break;
                }
            }
            match Self::loop_control(self.exec(body, &cur)?, labels) {
                Ok(true) => break,
                Ok(false) => {}
                Err(f) => return Ok(f),
            }
            // A fresh scope per iteration, so closures capture that iteration's variables.
            if !lexical.is_empty() {
                let next = new_scope(Some(s));
                for n in &lexical {
                    let b = cur.borrow();
                    if let Some(x) = b.vars.get(n) {
                        next.borrow_mut().vars.insert(
                            n.clone(),
                            Binding {
                                v: x.v.clone(),
                                mutable: x.mutable,
                            },
                        );
                    }
                }
                cur = next;
            }
            if let Some(u) = update {
                self.eval(u, &cur)?;
            }
        }
        Ok(Flow::Normal)
    }

    fn exec_for_in(
        &mut self,
        init: &Stmt,
        iter: &Expr,
        body: &Stmt,
        of: bool,
        s: &ScopeRef,
        labels: &[Rc<str>],
    ) -> R<Flow> {
        let coll = self.eval(iter, s)?;
        let items: Vec<Value> = if of {
            self.iter_values(&coll)?
        } else {
            let mut keys: Vec<Rc<str>> = Vec::new();
            match &coll {
                Value::Obj(o) => {
                    let mut cur = Some(o.clone());
                    while let Some(c) = cur {
                        for k in self.own_keys(&c) {
                            if !keys.contains(&k) {
                                keys.push(k);
                            }
                        }
                        cur = c.borrow().proto.clone();
                    }
                }
                Value::Str(t) => {
                    keys.extend((0..t.chars().count()).map(|i| Rc::from(i.to_string().as_str())))
                }
                _ => {}
            }
            keys.into_iter().map(Value::Str).collect()
        };
        for item in items {
            self.step()?;
            let inner = new_scope(Some(s));
            match init {
                Stmt::Var(k, decls) if decls.len() == 1 => {
                    let mode = match k {
                        VarKind::Var => Mode::Assign,
                        VarKind::Let => Mode::Let,
                        VarKind::Const => Mode::Const,
                    };
                    self.bind_pattern(&decls[0].0, item, &inner, mode)?;
                }
                Stmt::Expr(Expr::Ident(n)) => self.assign_ident(n, item, &inner)?,
                Stmt::Expr(e @ Expr::Member { .. }) => self.bind_pattern(
                    &Pattern::Member(Box::new(e.clone())),
                    item,
                    &inner,
                    Mode::Assign,
                )?,
                _ => return self.throw("SyntaxError", "unsupported loop variable"),
            }
            match Self::loop_control(self.exec(body, &inner)?, labels) {
                Ok(true) => break,
                Ok(false) => {}
                Err(f) => return Ok(f),
            }
        }
        Ok(Flow::Normal)
    }

    // ------------------------------------------------------------ classes

    fn eval_class(&mut self, c: &Rc<Class>, s: &ScopeRef) -> R<Value> {
        let cs = new_scope(Some(s));
        let (parent_ctor, proto_parent) = match &c.superclass {
            Some(e) => {
                let p = self.eval(e, &cs)?;
                match &p {
                    Value::Null => (None, None),
                    v if is_callable(v) => {
                        let pp = match self.get(v, "prototype")? {
                            Value::Obj(o) => Some(o),
                            _ => None,
                        };
                        (p.as_obj().cloned(), pp)
                    }
                    _ => {
                        return self.throw(
                            "TypeError",
                            "the class extends a value that is not a constructor",
                        )
                    }
                }
            }
            None => (None, Some(self.protos.object.clone())),
        };
        let derived = c.superclass.is_some();
        let proto = self.alloc(proto_parent, Kind::Plain, "Object");
        let (func, default) = match &c.ctor {
            Some(f) => (f.clone(), false),
            None => (
                Rc::new(Func {
                    name: c.name.clone(),
                    params: Vec::new(),
                    body: Vec::new(),
                    is_arrow: false,
                    is_ctor: true,
                    uses_arguments: false,
                }),
                true,
            ),
        };
        let mut fields: Vec<(Rc<str>, Option<Expr>)> = Vec::new();
        let mut statics: Vec<(Rc<str>, Option<Expr>)> = Vec::new();
        for m in &c.members {
            if let MemberKind::Field(init) = &m.kind {
                let key = self.eval_key(&m.key, &cs)?;
                if m.is_static {
                    statics.push((key, init.clone()))
                } else {
                    fields.push((key, init.clone()))
                }
            }
        }
        let fproto = parent_ctor
            .clone()
            .unwrap_or_else(|| self.protos.function.clone());
        let ctor = self.alloc(
            Some(fproto),
            Kind::Func(Closure {
                func,
                scope: cs.clone(),
                home: Some(proto.clone()),
                ctor: Some(CtorInfo {
                    derived,
                    default,
                    fields: Rc::new(fields),
                }),
            }),
            "Function",
        );
        self.define(&ctor, "prototype", Prop::hidden(Value::Obj(proto.clone())));
        self.define(
            &proto,
            "constructor",
            Prop::hidden(Value::Obj(ctor.clone())),
        );
        if let Some(n) = &c.name {
            self.declare(&cs, n, Value::Obj(ctor.clone()), false);
        }
        for m in &c.members {
            let target = if m.is_static {
                ctor.clone()
            } else {
                proto.clone()
            };
            let key = self.eval_key(&m.key, &cs)?;
            match &m.kind {
                MemberKind::Method(f) => {
                    let fv = self.make_function(f.clone(), &cs, Some(target.clone()));
                    self.define(&target, &key, Prop::hidden(fv));
                }
                MemberKind::Getter(f) => {
                    let fv = self.make_function(f.clone(), &cs, Some(target.clone()));
                    self.merge_accessor(&target, &key, Some(fv), None);
                }
                MemberKind::Setter(f) => {
                    let fv = self.make_function(f.clone(), &cs, Some(target.clone()));
                    self.merge_accessor(&target, &key, None, Some(fv));
                }
                MemberKind::Field(_) => {}
            }
        }
        let cv = Value::Obj(ctor);
        if !statics.is_empty() {
            let ss = new_scope(Some(&cs));
            self.declare(&ss, &Rc::from("this"), cv.clone(), true);
            for (k, init) in statics {
                let v = match init {
                    Some(e) => self.eval(&e, &ss)?,
                    None => Value::Undefined,
                };
                if let Value::Obj(o) = &cv {
                    self.define(o, &k, Prop::data(v));
                }
            }
        }
        Ok(cv)
    }

    fn merge_accessor(&mut self, o: &Obj, key: &str, get: Option<Value>, set: Option<Value>) {
        let mut p = o.borrow().props.get(key).cloned().unwrap_or(Prop {
            value: Value::Undefined,
            get: None,
            set: None,
            enumerable: false,
        });
        if get.is_some() {
            p.get = get;
        }
        if set.is_some() {
            p.set = set;
        }
        self.define(o, key, p);
    }

    // ------------------------------------------------------------ expressions

    fn eval_args(&mut self, args: &[Elem], s: &ScopeRef) -> R<Vec<Value>> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            match a {
                Elem::Item(e) => out.push(self.eval(e, s)?),
                Elem::Spread(e) => {
                    let v = self.eval(e, s)?;
                    out.extend(self.iter_values(&v)?);
                }
                Elem::Hole => out.push(Value::Undefined),
            }
        }
        Ok(out)
    }

    fn describe(e: &Expr) -> String {
        match e {
            Expr::Ident(n) => n.to_string(),
            Expr::Member { obj, prop, .. } => match &**prop {
                PropKey::Name(n) => format!("{}.{n}", Self::describe(obj)),
                PropKey::Computed(_) => format!("{}[...]", Self::describe(obj)),
            },
            Expr::This => "this".into(),
            _ => "expression".into(),
        }
    }

    fn get_computed(&mut self, base: &Value, k: &Value) -> R<Value> {
        if let (Value::Obj(o), Value::Num(n)) = (base, k) {
            if let Kind::Array(a) = &o.borrow().kind {
                if *n >= 0.0 && n.fract() == 0.0 {
                    return Ok(a.get(*n as usize).cloned().unwrap_or(Value::Undefined));
                }
            }
        }
        let key = self.key_of(k)?;
        self.get(base, &key)
    }

    fn eval(&mut self, e: &Expr, s: &ScopeRef) -> R<Value> {
        match e {
            Expr::Num(n) => Ok(Value::Num(*n)),
            Expr::Str(t) => Ok(Value::Str(t.clone())),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Null => Ok(Value::Null),
            Expr::Regex(p, f) => self.new_regexp(p, f),
            Expr::Ident(n) => match self.lookup(n, s) {
                Some(v) => Ok(v),
                None if &**n == "undefined" => Ok(Value::Undefined),
                None => self.throw("ReferenceError", &format!("{n} is not defined")),
            },
            Expr::This => Ok(self.lookup("this", s).unwrap_or(Value::Undefined)),
            Expr::Super => self.throw("SyntaxError", "`super` is only valid before `.` or `(`"),
            Expr::Template(strs, exprs) => {
                let mut out = String::from(&*strs[0]);
                for (i, x) in exprs.iter().enumerate() {
                    let v = self.eval(x, s)?;
                    out.push_str(&self.coerce_str(&v)?);
                    out.push_str(&strs[i + 1]);
                    if out.len() > MAX_STRING {
                        return self.throw("RangeError", "the string is too long");
                    }
                }
                Ok(Value::Str(Rc::from(out.as_str())))
            }
            Expr::Array(items) => {
                let v = self.eval_args(items, s)?;
                if v.len() > MAX_ARRAY {
                    return self.throw("RangeError", "the array is too large");
                }
                Ok(self.new_array(v))
            }
            Expr::Object(props) => self.eval_object(props, s),
            Expr::Function(f) => Ok(self.make_function(f.clone(), s, None)),
            Expr::Class(c) => self.eval_class(c, s),
            Expr::Unary(op, x) => self.eval_unary(*op, x, s),
            Expr::Update {
                delta,
                prefix,
                target,
            } => {
                let (base, key, old) = self.read_target(target, s)?;
                let n = self.coerce_num(&old)?;
                let new = Value::Num(n + delta);
                self.write_target(target, base, key, new.clone(), s)?;
                Ok(if *prefix { new } else { Value::Num(n) })
            }
            Expr::Binary(op, a, b) => {
                let x = self.eval(a, s)?;
                let y = self.eval(b, s)?;
                self.binary(*op, &x, &y)
            }
            Expr::Logical(op, a, b) => {
                let x = self.eval(a, s)?;
                let take_right = match op {
                    Logic::And => x.truthy(),
                    Logic::Or => !x.truthy(),
                    Logic::Nullish => x.is_nullish(),
                };
                if take_right {
                    self.eval(b, s)
                } else {
                    Ok(x)
                }
            }
            Expr::Assign {
                op,
                logic,
                target,
                value,
            } => self.eval_assign(*op, *logic, target, value, s),
            Expr::Cond(c, a, b) => {
                if self.eval(c, s)?.truthy() {
                    self.eval(a, s)
                } else {
                    self.eval(b, s)
                }
            }
            Expr::Seq(v) => {
                let mut last = Value::Undefined;
                for x in v {
                    last = self.eval(x, s)?;
                }
                Ok(last)
            }
            Expr::Chain(inner) => {
                let r = self.eval(inner, s);
                self.short = false;
                r
            }
            Expr::Member {
                obj,
                prop,
                optional,
            } => {
                let (base, _) = self.member_base(obj, s)?;
                if self.short {
                    return Ok(Value::Undefined);
                }
                if *optional && base.is_nullish() {
                    self.short = true;
                    return Ok(Value::Undefined);
                }
                match &**prop {
                    PropKey::Name(n) => self.get(&base, n),
                    PropKey::Computed(k) => {
                        let kv = self.eval(k, s)?;
                        self.get_computed(&base, &kv)
                    }
                }
            }
            Expr::Call {
                callee,
                args,
                optional,
            } => {
                if matches!(&**callee, Expr::Super) {
                    return self.super_call(args, s);
                }
                let (f, this) = match &**callee {
                    Expr::Member {
                        obj,
                        prop,
                        optional: mo,
                    } => {
                        let (base, this) = self.member_base(obj, s)?;
                        if self.short {
                            return Ok(Value::Undefined);
                        }
                        if *mo && base.is_nullish() {
                            self.short = true;
                            return Ok(Value::Undefined);
                        }
                        let f = match &**prop {
                            PropKey::Name(n) => self.get(&base, n)?,
                            PropKey::Computed(k) => {
                                let kv = self.eval(k, s)?;
                                self.get_computed(&base, &kv)?
                            }
                        };
                        (f, this)
                    }
                    other => {
                        let f = self.eval(other, s)?;
                        if self.short {
                            return Ok(Value::Undefined);
                        }
                        (f, Value::Undefined)
                    }
                };
                if *optional && f.is_nullish() {
                    self.short = true;
                    return Ok(Value::Undefined);
                }
                if !is_callable(&f) {
                    return self.throw(
                        "TypeError",
                        &format!("{} is not a function", Self::describe(callee)),
                    );
                }
                let argv = self.eval_args(args, s)?;
                self.call(&f, this, argv)
            }
            Expr::New(callee, args) => {
                let f = self.eval(callee, s)?;
                if !is_callable(&f) {
                    return self.throw(
                        "TypeError",
                        &format!("{} is not a constructor", Self::describe(callee)),
                    );
                }
                let argv = self.eval_args(args, s)?;
                self.construct(&f, argv)
            }
        }
    }

    fn eval_object(&mut self, props: &[ObjProp], s: &ScopeRef) -> R<Value> {
        let o = self.new_obj();
        for p in props {
            match p {
                ObjProp::Init(k, e) => {
                    let key = self.eval_key(k, s)?;
                    let v = self.eval(e, s)?;
                    self.define(&o, &key, Prop::data(v));
                }
                ObjProp::Getter(k, f) => {
                    let key = self.eval_key(k, s)?;
                    let fv = self.make_function(f.clone(), s, Some(o.clone()));
                    self.merge_accessor(&o, &key, Some(fv), None);
                    if let Some(p) = o.borrow_mut().props.get_mut(&*key) {
                        p.enumerable = true;
                    }
                }
                ObjProp::Setter(k, f) => {
                    let key = self.eval_key(k, s)?;
                    let fv = self.make_function(f.clone(), s, Some(o.clone()));
                    self.merge_accessor(&o, &key, None, Some(fv));
                    if let Some(p) = o.borrow_mut().props.get_mut(&*key) {
                        p.enumerable = true;
                    }
                }
                ObjProp::Spread(e) => {
                    let v = self.eval(e, s)?;
                    match &v {
                        Value::Obj(src) => {
                            for k in self.own_keys(src) {
                                let x = self.get(&v, &k)?;
                                self.define(&o, &k, Prop::data(x));
                            }
                        }
                        Value::Str(t) => {
                            for (i, c) in t.chars().enumerate() {
                                self.define(
                                    &o,
                                    &i.to_string(),
                                    Prop::data(Value::Str(Rc::from(c.to_string().as_str()))),
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(Value::Obj(o))
    }

    fn eval_unary(&mut self, op: Unary, x: &Expr, s: &ScopeRef) -> R<Value> {
        match op {
            Unary::Typeof => {
                if let Expr::Ident(n) = x {
                    if self.lookup(n, s).is_none() {
                        return Ok(Value::str("undefined"));
                    }
                }
                let v = self.eval(x, s)?;
                Ok(Value::str(Self::type_of(&v)))
            }
            Unary::Delete => {
                if let Expr::Member { obj, prop, .. } = x {
                    let base = self.eval(obj, s)?;
                    let key = match &**prop {
                        PropKey::Name(n) => n.clone(),
                        PropKey::Computed(k) => {
                            let kv = self.eval(k, s)?;
                            self.key_of(&kv)?
                        }
                    };
                    return Ok(Value::Bool(self.delete(&base, &key)));
                }
                self.eval(x, s)?;
                Ok(Value::Bool(true))
            }
            _ => {
                let v = self.eval(x, s)?;
                Ok(match op {
                    Unary::Neg => Value::Num(-self.coerce_num(&v)?),
                    Unary::Plus => Value::Num(self.coerce_num(&v)?),
                    Unary::Not => Value::Bool(!v.truthy()),
                    Unary::BitNot => Value::Num(f64::from(!to_int32(self.coerce_num(&v)?))),
                    _ => Value::Undefined,
                })
            }
        }
    }

    /// Reads an assignment target once: the object, the key and the current value.
    fn read_target(&mut self, t: &Expr, s: &ScopeRef) -> R<(Value, Rc<str>, Value)> {
        match t {
            Expr::Ident(n) => {
                let v = self.eval(t, s)?;
                Ok((Value::Undefined, n.clone(), v))
            }
            Expr::Member { obj, prop, .. } => {
                let (base, _) = self.member_base(obj, s)?;
                let key = self.eval_key(prop, s)?;
                let v = self.get(&base, &key)?;
                Ok((base, key, v))
            }
            _ => self.throw("SyntaxError", "invalid assignment target"),
        }
    }

    fn write_target(
        &mut self,
        t: &Expr,
        base: Value,
        key: Rc<str>,
        v: Value,
        s: &ScopeRef,
    ) -> R<()> {
        match t {
            Expr::Ident(n) => self.assign_ident(n, v, s),
            _ => self.set(&base, &key, v),
        }
    }

    fn eval_assign(
        &mut self,
        op: Option<Bin>,
        logic: Option<Logic>,
        target: &Pattern,
        value: &Expr,
        s: &ScopeRef,
    ) -> R<Value> {
        if op.is_none() && logic.is_none() {
            if let Pattern::Member(m) = target {
                if let Expr::Member { obj, prop, .. } = &**m {
                    // Evaluate the object and key before the value, as the language does.
                    let (base, _) = self.member_base(obj, s)?;
                    let kv = match &**prop {
                        PropKey::Computed(k) => Some(self.eval(k, s)?),
                        PropKey::Name(_) => None,
                    };
                    let v = self.eval(value, s)?;
                    match (&base, &kv) {
                        (Value::Obj(o), Some(Value::Num(n)))
                            if *n >= 0.0 && n.fract() == 0.0 && (*n as usize) < MAX_ARRAY =>
                        {
                            let mut b = o.borrow_mut();
                            let frozen = b.frozen;
                            if let (Kind::Array(a), false) = (&mut b.kind, frozen) {
                                let i = *n as usize;
                                if i >= a.len() {
                                    a.resize(i + 1, Value::Undefined);
                                }
                                a[i] = v.clone();
                                return Ok(v);
                            }
                        }
                        _ => {}
                    }
                    let key = match (&**prop, kv) {
                        (PropKey::Name(n), _) => n.clone(),
                        (_, Some(k)) => self.key_of(&k)?,
                        _ => Rc::from(""),
                    };
                    self.set(&base, &key, v.clone())?;
                    return Ok(v);
                }
            }
            let v = self.eval(value, s)?;
            self.bind_pattern(target, v.clone(), s, Mode::Assign)?;
            return Ok(v);
        }
        let (texpr, base, key, old) = match target {
            Pattern::Ident(n) => {
                let e = Expr::Ident(n.clone());
                let (b, k, v) = self.read_target(&e, s)?;
                (e, b, k, v)
            }
            Pattern::Member(m) => {
                let (b, k, v) = self.read_target(m, s)?;
                ((**m).clone(), b, k, v)
            }
            _ => return self.throw("SyntaxError", "invalid assignment target"),
        };
        let new = if let Some(l) = logic {
            let keep = match l {
                Logic::And => !old.truthy(),
                Logic::Or => old.truthy(),
                Logic::Nullish => !old.is_nullish(),
            };
            if keep {
                return Ok(old);
            }
            self.eval(value, s)?
        } else {
            let rhs = self.eval(value, s)?;
            self.binary(op.unwrap_or(Bin::Add), &old, &rhs)?
        };
        self.write_target(&texpr, base, key, new.clone(), s)?;
        Ok(new)
    }

    fn binary(&mut self, op: Bin, a: &Value, b: &Value) -> R<Value> {
        Ok(match op {
            Bin::Add => {
                if let (Value::Num(x), Value::Num(y)) = (a, b) {
                    return Ok(Value::Num(x + y));
                }
                let x = self.coerce_primitive(a)?;
                let y = self.coerce_primitive(b)?;
                if matches!(x, Value::Str(_)) || matches!(y, Value::Str(_)) {
                    let (p, q) = (self.coerce_str(&x)?, self.coerce_str(&y)?);
                    if p.len() + q.len() > MAX_STRING {
                        return self.throw("RangeError", "the string is too long");
                    }
                    let mut t = String::with_capacity(p.len() + q.len());
                    t.push_str(&p);
                    t.push_str(&q);
                    Value::Str(Rc::from(t.as_str()))
                } else {
                    Value::Num(self.coerce_num(&x)? + self.coerce_num(&y)?)
                }
            }
            Bin::Sub | Bin::Mul | Bin::Div | Bin::Rem | Bin::Exp => {
                let (x, y) = (self.coerce_num(a)?, self.coerce_num(b)?);
                Value::Num(match op {
                    Bin::Sub => x - y,
                    Bin::Mul => x * y,
                    Bin::Div => x / y,
                    Bin::Rem => x % y,
                    _ => {
                        if y == 0.0 {
                            1.0
                        } else if (x == 1.0 || x == -1.0) && y.is_infinite() {
                            f64::NAN
                        } else {
                            x.powf(y)
                        }
                    }
                })
            }
            Bin::Eq => Value::Bool(self.loose_equals(a, b)?),
            Bin::Ne => Value::Bool(!self.loose_equals(a, b)?),
            Bin::StrictEq => Value::Bool(Self::strict_equals(a, b)),
            Bin::StrictNe => Value::Bool(!Self::strict_equals(a, b)),
            Bin::Lt | Bin::Le | Bin::Gt | Bin::Ge => {
                let x = self.coerce_primitive(a)?;
                let y = self.coerce_primitive(b)?;
                if let (Value::Str(p), Value::Str(q)) = (&x, &y) {
                    return Ok(Value::Bool(match op {
                        Bin::Lt => p < q,
                        Bin::Le => p <= q,
                        Bin::Gt => p > q,
                        _ => p >= q,
                    }));
                }
                let (p, q) = (self.coerce_num(&x)?, self.coerce_num(&y)?);
                Value::Bool(match op {
                    Bin::Lt => p < q,
                    Bin::Le => p <= q,
                    Bin::Gt => p > q,
                    _ => p >= q,
                })
            }
            Bin::BitAnd | Bin::BitOr | Bin::BitXor | Bin::Shl | Bin::Shr | Bin::UShr => {
                let (x, y) = (to_int32(self.coerce_num(a)?), to_int32(self.coerce_num(b)?));
                Value::Num(match op {
                    Bin::BitAnd => f64::from(x & y),
                    Bin::BitOr => f64::from(x | y),
                    Bin::BitXor => f64::from(x ^ y),
                    Bin::Shl => f64::from(x.wrapping_shl(y as u32 & 31)),
                    Bin::Shr => f64::from(x >> (y as u32 & 31)),
                    _ => f64::from((x as u32) >> (y as u32 & 31)),
                })
            }
            Bin::In => {
                let Value::Obj(o) = b else {
                    return self.throw("TypeError", "the right side of `in` is not an object");
                };
                let k = self.key_of(a)?;
                Value::Bool(self.has_property(o, &k))
            }
            Bin::InstanceOf => Value::Bool(self.instance_of(a, b)?),
        })
    }

    // ------------------------------------------------------------ timers

    pub(crate) fn add_timer(&mut self, f: Value, delay: f64, every: bool, args: Vec<Value>) -> u32 {
        let id = self.next_timer;
        self.next_timer += 1;
        let d = if delay.is_finite() && delay > 1.0 {
            delay
        } else {
            1.0
        };
        if self.timers.len() < 10_000 {
            self.timers.push(Timer {
                id,
                due: self.clock + d,
                every: if every { Some(d) } else { None },
                f,
                args,
            });
        }
        id
    }

    pub(crate) fn clear_timer(&mut self, id: u32) {
        self.timers.retain(|t| t.id != id);
    }

    /// Runs the queued promise callbacks (and anything they queue), up to a bound.
    pub fn run_microtasks(&mut self) {
        let mut n = 0;
        while let Some(t) = self.microtasks.pop_front() {
            t(self);
            n += 1;
            if n > 100_000 {
                self.microtasks.clear();
                break;
            }
        }
    }

    /// How many timers are waiting.
    #[must_use]
    pub fn pending_timers(&self) -> usize {
        self.timers.len()
    }

    /// Runs the timers that fall due within the next `ms` milliseconds of virtual time, in order.
    /// Returns how many ran; errors thrown inside timer callbacks are appended to `console`.
    pub fn advance(&mut self, ms: f64) -> usize {
        let until = self.clock + ms;
        let mut ran = 0;
        while ran < 10_000 {
            let pick = self
                .timers
                .iter()
                .enumerate()
                .filter(|(_, t)| t.due <= until)
                .min_by(|a, b| a.1.due.total_cmp(&b.1.due).then(a.1.id.cmp(&b.1.id)))
                .map(|(i, _)| i);
            let Some(i) = pick else { break };
            let t = self.timers.remove(i);
            self.clock = t.due.max(self.clock);
            if let Some(every) = t.every {
                self.timers.push(Timer {
                    id: t.id,
                    due: self.clock + every,
                    every: t.every,
                    f: t.f.clone(),
                    args: t.args.clone(),
                });
            }
            self.steps = 0;
            if let Err(e) = self.call(&t.f, Value::Undefined, t.args) {
                let m = self.describe_error(&e);
                self.log_line(m);
                self.fatal = None;
            }
            self.run_microtasks();
            ran += 1;
        }
        self.clock = until;
        ran
    }

    pub(crate) fn log_line(&mut self, line: String) {
        if self.console.len() >= 1000 {
            self.console.remove(0);
        }
        let mut line = line;
        if line.len() > 4096 {
            line.truncate(4096);
        }
        self.console.push(line);
    }
}

fn collect_names(p: &Pattern, out: &mut Vec<Rc<str>>) {
    match p {
        Pattern::Ident(n) => out.push(n.clone()),
        Pattern::Array(items, rest) => {
            for (p, _) in items.iter().flatten() {
                collect_names(p, out);
            }
            if let Some(r) = rest {
                collect_names(r, out);
            }
        }
        Pattern::Object(props, rest) => {
            for (_, p, _) in props {
                collect_names(p, out);
            }
            if let Some(r) = rest {
                collect_names(r, out);
            }
        }
        Pattern::Member(_) => {}
    }
}

/// Whether a value can be called.
#[must_use]
pub fn is_callable(v: &Value) -> bool {
    matches!(v, Value::Obj(o) if matches!(o.borrow().kind, Kind::Func(_) | Kind::Native(..) | Kind::Bound(..)))
}

pub(crate) fn make_bound(i: &mut Interp, target: Value, this: Value, args: Vec<Value>) -> Value {
    let p = i.protos.function.clone();
    Value::Obj(i.alloc(Some(p), Kind::Bound(target, this, args), "Function"))
}

pub(crate) fn to_i32(n: f64) -> i32 {
    to_int32(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a script on a thread with a large stack and returns the value as text, or the error.
    fn run(src: &str) -> String {
        let src = src.to_string();
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn(move || {
                let mut i = Interp::new();
                match i.run(&src) {
                    Ok(v) => i.inspect(&v),
                    Err(e) => e,
                }
            })
            .unwrap()
            .join()
            .unwrap()
    }

    fn console(src: &str) -> Vec<String> {
        let src = src.to_string();
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn(move || {
                let mut i = Interp::new();
                let r = i.run(&src);
                let mut out = i.console.clone();
                if let Err(e) = r {
                    out.push(e);
                }
                out
            })
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn arithmetic_strings_and_coercion() {
        assert_eq!(run("1 + 2 * 3 - 4 / 2"), "5");
        assert_eq!(run("2 ** 10 + 7 % 4"), "1027");
        assert_eq!(run("'a' + 1 + 2"), "a12");
        assert_eq!(run("1 + 2 + 'a'"), "3a");
        assert_eq!(run("'5' * '4'"), "20");
        assert_eq!(run("0.1 + 0.2"), "0.30000000000000004");
        assert_eq!(run("1 / 3 * 3"), "1");
        assert_eq!(run("(-7) % 3"), "-1");
        assert_eq!(run("5 & 3 | 8 ^ 1"), "9");
        assert_eq!(run("-1 >>> 28"), "15");
        assert_eq!(run("1 << 31"), "-2147483648");
        assert_eq!(run("1e21 + ''"), "1e+21");
        assert_eq!(run("0.000001 + ''"), "0.000001");
        assert_eq!(run("1e-7 + ''"), "1e-7");
        assert_eq!(run("[1,2] + [3]"), "1,23");
        assert_eq!(run("({}) + ''"), "[object Object]");
        assert_eq!(run("typeof 1 + typeof 'a' + typeof undefined + typeof null + typeof {} + typeof (() => 1) + typeof nope"), "numberstringundefinedobjectobjectfunctionundefined");
    }

    #[test]
    fn equality_and_truthiness() {
        assert_eq!(run("[null == undefined, null === undefined, 1 == '1', 1 === '1', NaN == NaN, 0 == false, '' == 0, [] == false, ({}) == '[object Object]']"), "[ true, false, true, false, false, true, true, true, true ]");
        assert_eq!(
            run("[!!0, !!'', !!NaN, !!null, !![], !!{}, !!'0']"),
            "[ false, false, false, false, true, true, true ]"
        );
        assert_eq!(run("0 || 'x'"), "x");
        assert_eq!(run("0 ?? 'x'"), "0");
        assert_eq!(run("null ?? 'x'"), "x");
        assert_eq!(run("1 && 2"), "2");
    }

    #[test]
    fn variables_scopes_and_closures() {
        assert_eq!(run("var a = 1; { let a = 2; } a"), "1");
        assert_eq!(run("let x = 1; function f() { return x; } x = 5; f()"), "5");
        assert_eq!(run("function counter() { let n = 0; return () => ++n; } const c = counter(); c(); c(); c()"), "3");
        assert_eq!(
            run("hoisted(); function hoisted() { return 7 } hoisted()"),
            "7"
        );
        assert_eq!(
            run("function f() { return v; var v = 3; } f()"),
            "undefined"
        );
        assert_eq!(
            run("const fs = []; for (let i = 0; i < 3; i++) fs.push(() => i); fs.map(f => f())"),
            "[ 0, 1, 2 ]"
        );
        assert_eq!(
            run("var fs = []; for (var i = 0; i < 3; i++) fs.push(() => i); fs.map(f => f())"),
            "[ 3, 3, 3 ]"
        );
        assert!(run("const c = 1; c = 2").contains("Assignment to constant"));
        assert!(run("nope").contains("ReferenceError: nope is not defined"));
        assert_eq!(run("undeclared = 4; undeclared"), "4");
        assert_eq!(
            run("const f = function fact(n) { return n <= 1 ? 1 : n * fact(n - 1); }; f(5)"),
            "120"
        );
    }

    #[test]
    fn functions_this_and_arguments() {
        assert_eq!(
            run("function f(a, b = 10, ...r) { return [a, b, r.length]; } f(1)"),
            "[ 1, 10, 0 ]"
        );
        assert_eq!(
            run("function f(a, b = 10, ...r) { return [a, b, r]; } f(1, 2, 3, 4)"),
            "[ 1, 2, [ 3, 4 ] ]"
        );
        assert_eq!(
            run("function f() { return arguments.length; } f(1, 2, 3)"),
            "3"
        );
        assert_eq!(
            run("const o = { v: 4, m() { return this.v; }, a: () => typeof this }; [o.m(), o.a()]"),
            "[ 4, 'undefined' ]"
        );
        assert_eq!(run("function f() { return this.x; } f.call({ x: 9 })"), "9");
        assert_eq!(
            run("function f(a, b) { return this.x + a + b; } f.apply({ x: 1 }, [2, 3])"),
            "6"
        );
        assert_eq!(
            run("function f(a, b) { return this.x + a + b; } const g = f.bind({ x: 1 }, 2); g(3)"),
            "6"
        );
        assert_eq!(run("const add = (a) => (b) => a + b; add(1)(2)"), "3");
        assert_eq!(run("Math.max(...[1, 5, 3], 4)"), "5");
        assert!(run("const o = {}; o.f()").contains("TypeError: o.f is not a function"));
        assert!(run("let u; u.x").contains("Cannot read properties of undefined (reading 'x')"));
    }

    #[test]
    fn objects_prototypes_and_accessors() {
        assert_eq!(
            run("const o = { a: 1, 'b c': 2, [1 + 1]: 3 }; [o.a, o['b c'], o[2]]"),
            "[ 1, 2, 3 ]"
        );
        assert_eq!(run("const a = 1, b = 2; ({ a, b })"), "{ a: 1, b: 2 }");
        assert_eq!(
            run("({ ...{ a: 1 }, b: 2, ...{ a: 3 } })"),
            "{ a: 3, b: 2 }"
        );
        assert_eq!(run("const o = { get v() { return 5; }, set v(x) { this._v = x; } }; o.v = 8; [o.v, o._v]"), "[ 5, 8 ]");
        assert_eq!(run("function P(n) { this.n = n; } P.prototype.hi = function () { return 'hi ' + this.n; }; new P('x').hi()"), "hi x");
        assert_eq!(run("function P() {} const p = new P(); [p instanceof P, p instanceof Object, ({}) instanceof P]"), "[ true, true, false ]");
        assert_eq!(run("const o = Object.create({ inherited: 1 }); o.own = 2; [o.inherited, Object.keys(o).join(), 'inherited' in o, o.hasOwnProperty('inherited')]"), "[ 1, 'own', true, false ]");
        assert_eq!(
            run("const o = { a: 1, b: 2 }; delete o.a; Object.keys(o)"),
            "[ 'b' ]"
        );
        assert_eq!(
            run("const o = Object.freeze({ a: 1 }); o.a = 2; o.b = 3; [o.a, o.b]"),
            "[ 1, undefined ]"
        );
        assert_eq!(
            run("Object.entries({ a: 1, b: 2 }).map(([k, v]) => k + v).join('')"),
            "a1b2"
        );
        assert_eq!(
            run("Object.fromEntries([['a', 1], ['b', 2]])"),
            "{ a: 1, b: 2 }"
        );
        assert_eq!(
            run("Object.assign({ a: 1 }, { b: 2 }, { a: 3 })"),
            "{ a: 3, b: 2 }"
        );
        assert_eq!(run("a?.b?.c"), "Uncaught ReferenceError: a is not defined");
        assert_eq!(
            run("const a = { b: null }; [a.b?.c, a.x?.y.z, a.x?.[0], a.f?.()]"),
            "[ undefined, undefined, undefined, undefined ]"
        );
        assert_eq!(run("const a = { b: { c: 4 } }; a?.b?.c"), "4");
    }

    #[test]
    fn classes_inheritance_super_and_fields() {
        let src = "
            class Animal {
                legs = 4;
                static count = 0;
                constructor(name) { this.name = name; Animal.count++; }
                speak() { return this.name + ' makes a sound'; }
                get label() { return 'animal:' + this.name; }
                static make(n) { return new this(n); }
            }
            class Dog extends Animal {
                tricks = [];
                constructor(name) { super(name); this.tricks.push('sit'); }
                speak() { return super.speak() + ' (woof)'; }
            }
            const d = Dog.make('Rex');
            [d.speak(), d.label, d.legs, d.tricks.length, Animal.count, d instanceof Animal, d instanceof Dog]
        ";
        assert_eq!(
            run(src),
            "[ 'Rex makes a sound (woof)', 'animal:Rex', 4, 1, 1, true, true ]"
        );
        assert_eq!(
            run("class A { constructor() { this.x = 1; } } class B extends A {} new B().x"),
            "1"
        );
        assert_eq!(
            run("class A { f = 2; } class B extends A { g = this.f + 1; } new B().g"),
            "3"
        );
        assert!(run("class A {} A()").contains("Class constructor"));
        assert_eq!(run("class E extends Error { constructor(m) { super(m); this.name = 'E'; } } let r; try { throw new E('bad'); } catch (e) { r = [e.message, e.name, e instanceof E, e instanceof Error] } r"), "[ 'bad', 'E', true, true ]");
        assert_eq!(
            run("const C = class { m() { return 1 } }; new C().m()"),
            "1"
        );
    }

    #[test]
    fn destructuring_spread_and_templates() {
        assert_eq!(
            run("const [a, , b = 5, ...r] = [1, 2, undefined, 4, 5]; [a, b, r]"),
            "[ 1, 5, [ 4, 5 ] ]"
        );
        assert_eq!(
            run("const { a, b: { c = 9 }, ...rest } = { a: 1, b: {}, x: 2, y: 3 }; [a, c, rest]"),
            "[ 1, 9, { x: 2, y: 3 } ]"
        );
        assert_eq!(run("let a = 1, b = 2; [a, b] = [b, a]; [a, b]"), "[ 2, 1 ]");
        assert_eq!(
            run("const o = {}; ({ x: o.p, y: o.q = 7 } = { x: 1 }); o"),
            "{ p: 1, q: 7 }"
        );
        assert_eq!(
            run("function f({ a, b } = { a: 1, b: 2 }, [c] = [3]) { return a + b + c; } f()"),
            "6"
        );
        assert_eq!(run("[...'abc', ...[1, 2]]"), "[ 'a', 'b', 'c', 1, 2 ]");
        assert_eq!(
            run("const n = 3; `n=${n}, n2=${n * n}, nested=${`<${n}>`}`"),
            "n=3, n2=9, nested=<3>"
        );
        assert!(run("const { a } = null").contains("TypeError"));
    }

    #[test]
    fn control_flow() {
        assert_eq!(run("let s = 0; for (let i = 1; i <= 10; i++) { if (i % 2) continue; if (i > 8) break; s += i; } s"), "20");
        assert_eq!(
            run("let s = ''; for (const k in { a: 1, b: 2 }) s += k; s"),
            "ab"
        );
        assert_eq!(run("let s = 0; for (const x of [1, 2, 3]) s += x; s"), "6");
        assert_eq!(
            run("let s = ''; for (const c of 'hey') s = c + s; s"),
            "yeh"
        );
        assert_eq!(run("let i = 0; do { i++ } while (i < 5); i"), "5");
        assert_eq!(
            run("let i = 0; while (true) { if (++i > 3) break; } i"),
            "4"
        );
        assert_eq!(run("let n = 0; outer: for (let i = 0; i < 3; i++) { for (let j = 0; j < 3; j++) { if (j == 1) continue outer; if (i == 2) break outer; n++; } } n"), "2");
        assert_eq!(run("function f(x) { switch (x) { case 1: return 'one'; case 2: case 3: return 'few'; default: return 'many'; } } [f(1), f(3), f(9)]"), "[ 'one', 'few', 'many' ]");
        assert_eq!(run("let r = []; switch (2) { case 1: r.push(1); case 2: r.push(2); case 3: r.push(3); break; case 4: r.push(4); } r"), "[ 2, 3 ]");
        assert_eq!(
            run("function f() { try { return 'try'; } finally { log = 'finally'; } } [f(), log]"),
            "[ 'try', 'finally' ]"
        );
        assert_eq!(
            run("let r; try { null.x } catch (e) { r = e instanceof TypeError } r"),
            "true"
        );
        assert_eq!(
            run("let r; try { throw { code: 4 } } catch ({ code }) { r = code } r"),
            "4"
        );
        assert_eq!(run("let r; try { try { throw new Error('in') } finally { r = 'cleanup' } } catch (e) { r += ':' + e.message } r"), "cleanup:in");
        assert!(run("throw new RangeError('boom')").contains("Uncaught RangeError: boom"));
        assert_eq!(run("x = 5; var x; typeof x"), "number");
    }

    #[test]
    fn arrays() {
        assert_eq!(
            run("const a = [3, 1, 2]; a.push(4); a.unshift(0); [a.pop(), a.shift(), a.length]"),
            "[ 4, 0, 3 ]"
        );
        assert_eq!(
            run("[1, 2, 3, 4].map(x => x * 2).filter(x => x > 2).reduce((a, b) => a + b, 0)"),
            "18"
        );
        assert_eq!(run("[3, 1, 10, 2].sort()"), "[ 1, 10, 2, 3 ]");
        assert_eq!(
            run("[3, 1, 10, 2].sort((a, b) => a - b)"),
            "[ 1, 2, 3, 10 ]"
        );
        assert_eq!(run("[1, 2, 3, 4, 5].slice(1, -1)"), "[ 2, 3, 4 ]");
        assert_eq!(
            run("const a = [1, 2, 3, 4, 5]; const r = a.splice(1, 2, 'x'); [a, r]"),
            "[ [ 1, 'x', 4, 5 ], [ 2, 3 ] ]"
        );
        assert_eq!(run("[[1, [2]], 3].flat(2)"), "[ 1, 2, 3 ]");
        assert_eq!(
            run("[1, 2, 3].indexOf(2) + [1, 2].includes(3) + [NaN].includes(NaN)"),
            "2"
        );
        assert_eq!(
            run("[1, 2, 3].find(x => x > 1) + [1, 2, 3].findIndex(x => x > 5)"),
            "1"
        );
        assert_eq!(
            run("[[1, 2].some(x => x > 1), [1, 2].every(x => x > 1)]"),
            "[ true, false ]"
        );
        assert_eq!(run("[1, 2, 3].join('-')"), "1-2-3");
        assert_eq!(
            run("Array.from({ length: 3 }, (_, i) => i * i)"),
            "[ 0, 1, 4 ]"
        );
        assert_eq!(run("const a = []; a[3] = 1; a.length"), "4");
        assert_eq!(run("const a = [1, 2, 3]; a.length = 1; a"), "[ 1 ]");
        assert_eq!(run("Array.isArray([]) && !Array.isArray({})"), "true");
        assert_eq!(run("[1, 2, 3].at(-1)"), "3");
        assert_eq!(run("new Array(3).fill(0)"), "[ 0, 0, 0 ]");
        assert_eq!(run("[1, 2, 3].reverse()"), "[ 3, 2, 1 ]");
        assert_eq!(run("[1].concat([2, 3], 4)"), "[ 1, 2, 3, 4 ]");
        assert!(run("[].reduce((a, b) => a)").contains("Reduce of empty array"));
    }

    #[test]
    fn strings_numbers_math_and_json() {
        assert_eq!(
            run("'Hello'.toUpperCase() + 'WORLD'.toLowerCase() + ' x '.trim()"),
            "HELLOworldx"
        );
        assert_eq!(
            run("['abc'.length, 'abc'[1], 'abc'.charAt(2), 'abc'.charCodeAt(0), 'héllo'.length]"),
            "[ 3, 'b', 'c', 97, 5 ]"
        );
        assert_eq!(
            run("['a,b,c'.split(','), 'abc'.split('')]"),
            "[ [ 'a', 'b', 'c' ], [ 'a', 'b', 'c' ] ]"
        );
        assert_eq!(run("'abcdef'.slice(1, -1) + 'abcdef'.substring(4, 2) + 'abc'.indexOf('c') + 'abc'.includes('bc')"), "bcdecd2true");
        assert_eq!(
            run("'a-b-c'.replace('-', '+') + 'a-b-c'.replaceAll('-', '+')"),
            "a+b-ca+b+c"
        );
        assert_eq!(
            run("'5'.padStart(3, '0') + 'x'.padEnd(3, '.') + 'ab'.repeat(2)"),
            "005x..abab"
        );
        assert_eq!(run("[parseInt('42px'), parseInt('ff', 16), parseFloat('3.5abc'), parseInt('x'), Number('12'), Number(''), Number('1e3')]"), "[ 42, 255, 3.5, NaN, 12, 0, 1000 ]");
        assert_eq!(run("[(1.005).toFixed(2), (255).toString(16), (255).toString(2), (0.5).toString(2), Number.isInteger(5.0)]"), "[ '1.00', 'ff', '11111111', '0.1', true ]");
        assert_eq!(run("[Math.floor(-1.5), Math.ceil(1.2), Math.round(2.5), Math.round(-2.5), Math.abs(-3), Math.sqrt(16), Math.pow(2, 8), Math.min(), Math.max(1, 3)]"), "[ -2, 2, 3, -2, 3, 4, 256, Infinity, 3 ]");
        assert_eq!(run("const r = Math.random(); r >= 0 && r < 1"), "true");
        assert_eq!(run("JSON.stringify({ a: [1, 'x', null, undefined], b: { c: true }, d: undefined, e: () => 1 })"), "{\"a\":[1,\"x\",null,null],\"b\":{\"c\":true}}");
        assert_eq!(
            run("JSON.stringify({ a: 1, b: [2] }, null, 2)"),
            "{\n  \"a\": 1,\n  \"b\": [\n    2\n  ]\n}"
        );
        assert_eq!(
            run("JSON.parse('{\"a\":[1,2,{\"b\":\"x\\\\n\"}],\"c\":null,\"d\":-1.5e2}')"),
            "{ a: [ 1, 2, { b: 'x\n' } ], c: null, d: -150 }"
        );
        assert!(run("JSON.parse('{bad}')").contains("SyntaxError"));
        assert!(run("const o = {}; o.o = o; JSON.stringify(o)").contains("circular"));
        assert_eq!(
            run("JSON.stringify('he said \"hi\"')"),
            "\"he said \\\"hi\\\"\""
        );
    }

    #[test]
    fn console_and_timers_run_on_a_virtual_clock() {
        let out = console("console.log('a', 1, [1, 2], { x: 'y' }); console.warn('w'); console.error(new Error('e'));");
        assert_eq!(
            out,
            vec!["a 1 [ 1, 2 ] { x: 'y' }", "warning: w", "error: Error: e"]
        );
        let mut i = Interp::new();
        i.run("setTimeout(() => console.log('b'), 20); setTimeout(() => console.log('a'), 10); const id = setInterval(() => console.log('tick'), 15); setTimeout(() => clearInterval(id), 40);").unwrap();
        assert_eq!(i.pending_timers(), 4);
        i.advance(12.0);
        assert_eq!(i.console, vec!["a"]);
        i.advance(100.0);
        assert_eq!(i.console, vec!["a", "tick", "b", "tick"]);
        assert_eq!(i.pending_timers(), 0);
        let mut i = Interp::new();
        i.run("setTimeout(() => { throw new Error('late') }, 1); setTimeout(() => console.log('still runs'), 2);").unwrap();
        i.advance(10.0);
        assert_eq!(i.console, vec!["Uncaught Error: late", "still runs"]);
    }

    #[test]
    fn maps_and_sets_keep_order_and_use_same_value_zero() {
        assert_eq!(run("const m = new Map([['a', 1], ['b', 2]]); m.set('a', 9).set(NaN, 'n').set(0, 'z'); [m.size, m.get('a'), m.get(NaN), m.get(-0), m.has('b'), m.has('q'), m.delete('b'), m.size]"), "[ 4, 9, 'n', 'z', true, false, true, 3 ]");
        assert_eq!(run("const o = {}; const m = new Map(); m.set(o, 1); m.set({}, 2); [m.get(o), m.size, [...m.keys()].length]"), "[ 1, 2, 2 ]");
        assert_eq!(run("[...new Set([3, 1, 3, 2, 1])]"), "[ 3, 1, 2 ]");
        assert_eq!(
            run("const s = new Set('hello'); [s.size, s.has('l'), [...s].join('')]"),
            "[ 4, true, 'helo' ]"
        );
        assert_eq!(run("const r = []; new Map([[1, 'a'], [2, 'b']]).forEach((v, k) => r.push(k + v)); for (const [k, v] of new Map([[3, 'c']])) r.push(k + v); r"), "[ '1a', '2b', '3c' ]");
        assert_eq!(
            run("new Map([['k', [1, 2]]])"),
            "Map(1) { 'k' => [ 1, 2 ] }"
        );
        assert_eq!(run("new Set([1, 'x'])"), "Set(2) { 1, 'x' }");
        assert_eq!(run("const m = new Map(); for (let i = 0; i < 50000; i++) m.set('k' + i, i); for (let i = 0; i < 49990; i++) m.delete('k' + i); [m.size, m.get('k49995'), [...m.values()].length]"), "[ 10, 49995, 10 ]");
        assert!(run("Map()").contains("requires 'new'"));
        assert!(run("Map.prototype.get.call({}, 1)").contains("not a Map or Set"));
        assert_eq!(run("class Counter extends Map { inc(k) { this.set(k, (this.get(k) || 0) + 1); return this } } const c = new Counter(); c.inc('a').inc('a'); c.get('a')"), "2");
    }

    #[test]
    fn promises_run_their_callbacks_in_order_after_the_script() {
        let out = console("
            const log = (...a) => console.log(...a);
            log('start');
            setTimeout(() => log('timeout'), 0);
            Promise.resolve(1).then(v => { log('then', v); return v + 1; }).then(v => log('then2', v));
            queueMicrotask(() => log('micro'));
            new Promise((res, rej) => rej(new Error('no'))).catch(e => log('caught', e.message)).finally(() => log('finally'));
            log('end');
        ");
        assert_eq!(
            out,
            vec![
                "start",
                "end",
                "then 1",
                "micro",
                "caught no",
                "then2 2",
                "finally"
            ]
        );
        let out = console("
            Promise.all([1, Promise.resolve(2), new Promise(r => setTimeout(() => r(3), 10))]).then(v => console.log('all', v));
            Promise.race([new Promise(r => setTimeout(() => r('slow'), 50)), new Promise(r => setTimeout(() => r('fast'), 5))]).then(v => console.log('race', v));
            Promise.allSettled([Promise.reject('x'), 5]).then(v => console.log(JSON.stringify(v)));
            Promise.resolve({ then(res) { res('thenable'); } }).then(v => console.log(v));
        ");
        assert_eq!(out, vec!["[{\"status\":\"rejected\",\"reason\":\"x\"},{\"status\":\"fulfilled\",\"value\":5}]", "thenable"]);
        let mut i = Interp::new();
        i.run("Promise.all([1, new Promise(r => setTimeout(() => r(3), 10))]).then(v => console.log('all', v)); Promise.race([new Promise(r => setTimeout(() => r('slow'), 50)), new Promise(r => setTimeout(() => r('fast'), 5))]).then(v => console.log('race', v));").unwrap();
        i.advance(100.0);
        assert_eq!(i.console, vec!["race fast", "all [ 1, 3 ]"]);
        assert_eq!(run("Promise.resolve(5)"), "Promise { 5 }");
        assert_eq!(run("new Promise(() => {})"), "Promise { <pending> }");
        assert!(run("Promise(() => {})").contains("requires 'new'"));
        // A promise that resolves with itself is rejected, and a chain cannot starve the interpreter.
        assert_eq!(console("const p = new Promise(r => setTimeout(() => r(p))); p.catch(e => console.log(e instanceof TypeError)); setTimeout(() => {}, 5)").len(), 0);
        let mut i = Interp::new();
        i.run("function loop() { Promise.resolve().then(loop) } loop();")
            .unwrap();
        assert_eq!(i.pending_timers(), 0);
    }

    #[test]
    fn regular_expressions_work_in_scripts() {
        assert_eq!(run("/b+/.test('abbbc')"), "true");
        assert_eq!(run("/^\\d+$/.test('12a')"), "false");
        assert_eq!(run("'a1b22c333'.match(/\\d+/g)"), "[ '1', '22', '333' ]");
        assert_eq!(run("'abc'.match(/x/)"), "null");
        assert_eq!(
            run("const m = 'key=value'.match(/(\\w+)=(\\w+)/); [m[0], m[1], m[2], m.index]"),
            "[ 'key=value', 'key', 'value', 0 ]"
        );
        assert_eq!(
            run("const m = '2024-05-06'.match(/(?<y>\\d+)-(?<m>\\d+)/); [m.groups.y, m.groups.m]"),
            "[ '2024', '05' ]"
        );
        assert_eq!(run("'a-b-c'.replace(/-/g, '+')"), "a+b+c");
        assert_eq!(run("'a-b-c'.replace(/-/, '+')"), "a+b-c");
        assert_eq!(
            run("'John Smith'.replace(/(\\w+) (\\w+)/, '$2, $1')"),
            "Smith, John"
        );
        assert_eq!(run("'abc'.replace(/b/, '[$&|$`|$\\'|$$]')"), "a[b|a|c|$]c");
        assert_eq!(run("'a1b2'.replace(/\\d/g, d => d * 2)"), "a2b4");
        assert_eq!(run("'x'.replace(/(?<n>x)/, '<$<n>>')"), "<x>");
        assert_eq!(run("'a, b ,c'.split(/\\s*,\\s*/)"), "[ 'a', 'b', 'c' ]");
        assert_eq!(run("'abc'.split(/(?:)/)"), "[ 'a', 'b', 'c' ]");
        assert_eq!(run("'a1b2c'.split(/(\\d)/)"), "[ 'a', '1', 'b', '2', 'c' ]");
        assert_eq!(run("'one two'.search(/two/)"), "4");
        assert_eq!(
            run("[...'a1b2'.matchAll(/\\d/g)].map(m => m[0] + m.index)"),
            "[ '11', '23' ]"
        );
        assert_eq!(run("const re = /o/g; const r = []; let m; while ((m = re.exec('foo boo')) !== null) r.push(m.index + ':' + re.lastIndex); r"), "[ '1:2', '2:3', '5:6', '6:7' ]");
        assert_eq!(run("const re = /a/g; re.test('aa'); re.lastIndex"), "1");
        assert_eq!(
            run("new RegExp('a+', 'gi').flags + new RegExp(/x/gi).source"),
            "gix"
        );
        assert_eq!(run("String(/a\\/b/g)"), "/a\\/b/g");
        assert_eq!(run("const r = /x/gim; [r.global, r.ignoreCase, r.multiline, r.sticky, r.source, r.flags]"), "[ true, true, true, false, 'x', 'gim' ]");
        assert_eq!(
            run("/A/i.test('a') && !/A/.test('a') && /a$/m.test('a\\nb')"),
            "true"
        );
        assert_eq!(run("'  hi  '.replace(/^\\s+|\\s+$/g, '')"), "hi");
        assert_eq!(
            run("'camelCaseString'.replace(/([A-Z])/g, ' $1').toLowerCase()"),
            "camel case string"
        );
        assert_eq!(run("/(\\d+)\\.(\\d+)/.exec('v10.25')[2]"), "25");
        assert_eq!(run("'x'.replaceAll(/x/g, 'y')"), "y");
        assert!(run("'x'.replaceAll(/x/, 'y')").contains("global"));
        assert!(run("new RegExp('(')").contains("SyntaxError: Invalid regular expression"));
        assert!(run("/a/.exec.call({}, 'a')").contains("TypeError"));
        assert!(run("/(a+)+b/.test('a'.repeat(40))").contains("took too long"));
        assert_eq!(run("/a/"), "/a/");
        assert_eq!(run("typeof /a/ + (/a/ instanceof RegExp)"), "objecttrue");
    }

    #[test]
    fn limits_halt_runaway_scripts_and_cannot_be_caught() {
        assert!(run("while (true) {}").contains("ran for too long"));
        assert!(
            run("try { while (true) {} } catch (e) { 'caught' }").contains("Halted"),
            "{}",
            run("try { while (true) {} } catch (e) { 'caught' }")
        );
        assert!(run("function f() { return f(); } f()").contains("Maximum call stack"));
        assert!(run("let s = 'x'; while (true) s += s;").contains("too long"));
        assert!(run("const a = []; while (true) a.push(1);").contains("too large"));
        assert!(run("new Array(-1)").contains("Invalid array length"));
        assert!(run("'x'.repeat(1e9)").contains("Invalid string length"));
        let mut i = Interp::new();
        i.object_limit = 1000;
        assert!(i
            .run("const a = []; while (true) a.push({});")
            .err()
            .unwrap()
            .contains("too many objects"));
        // A later script runs normally after a halt.
        assert_eq!(i.run("1 + 1").map(|v| i.inspect(&v)), Ok("2".to_string()));
    }

    #[test]
    fn syntax_errors_come_back_as_text_and_nothing_panics() {
        assert!(run("let = ;").contains("SyntaxError"));
        assert!(run("async function f() {}").contains("not supported"));
        for src in [
            "null.x",
            "undefined()",
            "new 5",
            "({}).x.y",
            "[].x.y",
            "1()",
            "x => x()()",
            "class A extends 5 {}",
            "'a' in 'b'",
            "5 instanceof 5",
            "/re/",
            "super.x",
            "new (class { constructor() { return 5 } })()",
            "Object.defineProperty(1, 'x', {})",
            "[...5]",
            "for (const x of 5) {}",
            "({}).toString.call()",
        ] {
            let _ = run(src);
        }
    }
}

#[cfg(test)]
mod stack_probe {
    use super::*;
    use std::cell::Cell;

    #[test]
    #[ignore = "measurement"]
    fn probe() {
        let r = std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn(|| {
                let lo = Rc::new(Cell::new(usize::MAX));
                let hi = Rc::new(Cell::new(0usize));
                let (l, h) = (lo.clone(), hi.clone());
                let mut i = Interp::new();
                let f = i.native("probe", move |_, _, _| {
                    let x = 0u8;
                    let a = std::ptr::addr_of!(x) as usize;
                    l.set(l.get().min(a));
                    h.set(h.get().max(a));
                    Ok(Value::Undefined)
                });
                i.set_global("probe", f);
                i.run("function f(n) { if (!n) { probe(); return 0 } probe(); return f(n - 1) + 1 } f(60)").unwrap();
                (hi.get() - lo.get()) / 60
            })
            .unwrap()
            .join();
        println!("stack per JS call level: {} bytes", r.unwrap());
    }
}
