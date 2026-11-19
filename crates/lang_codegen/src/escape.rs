//! Escape analysis (docs/09, phase 2, item 1): which struct constructions can live on the stack.
//!
//! A struct object is created with `alloca` instead of the collector when the analysis proves
//! that no reference to it outlives the function call that created it. The analysis is
//! flow-insensitive and conservative: whenever it is unsure, the object escapes and goes on the
//! heap as before.
//!
//! **Values.** For every expression the analysis computes which *sources* it may evaluate to.
//! A source is a struct construction site in the function, or one of the function's parameters.
//! Locals accumulate the sources of everything assigned to them.
//!
//! **Escaping.** A source escapes when a value that may be it reaches anything that can outlive
//! the call or that the analysis does not follow:
//! - a `return`;
//! - a field, element, dict entry, tuple, optional, result, variant or another struct's field;
//! - a local captured by a closure, or a closure's own captured variable;
//! - a trait object, a bound method, a spawned task, an f-string (which calls `Display`);
//! - an argument to anything but a direct call of a function whose parameter does not escape,
//!   or one of the few builtins that only read their argument;
//! - an operator, `match`, `with`, a loop source.
//!
//! Reading a field of an object (`p.x`), or assigning to one (`p.x = v`, where `v` escapes but
//! `p` does not), is not an escape.
//!
//! **Parameter summaries.** Whether each function's parameters escape is computed for the whole
//! program at once: every parameter starts out non-escaping, and the analysis of every function
//! is repeated until nothing more escapes. Recursion needs nothing special.
//!
//! **Loops.** Each site gets one stack slot per call, reused every time the site runs. A site
//! inside a loop therefore qualifies only if at most one local can hold its object: that local
//! is overwritten whenever the site runs again, so the previous object is unreachable by then.
//! A second local (`q = p`) could still refer to the slot, and would see the next iteration's
//! values, so it makes the site ineligible.

use lang_sema::hir::{
    Builtin, Expr, ExprKind, FPart, FuncId, Function, LocalId, LoopSource, PatternKind, Place, Program, Stmt,
};
use std::collections::{BTreeSet, HashMap, HashSet};

/// The struct constructions that may be allocated on the stack, by address. The HIR is borrowed
/// unchanged by code generation, so the address identifies the expression.
pub(crate) type StackSites = HashSet<*const Expr>;

/// What a value may be: a construction site (by index in the function's site list) or a
/// parameter (by position).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Source {
    Site(usize),
    Param(usize),
}

type Sources = BTreeSet<Source>;

/// Find the construction sites in `program` that may be allocated on the stack.
pub(crate) fn stack_sites(program: &Program) -> StackSites {
    // Parameters that escape, per function. Grows monotonically until a fixed point.
    let mut escaping: Vec<Vec<bool>> = program.functions.iter().map(|f| vec![false; f.params.len()]).collect();
    loop {
        let mut changed = false;
        for (index, function) in program.functions.iter().enumerate() {
            if !analysable(function) {
                if escaping[index].iter().any(|e| !e) {
                    escaping[index].iter_mut().for_each(|e| *e = true);
                    changed = true;
                }
                continue;
            }
            let escaped = Analysis::run(program, function, &escaping).escaped;
            for (param, escapes) in escaping[index].iter_mut().enumerate() {
                if !*escapes && escaped.contains(&Source::Param(param)) {
                    *escapes = true;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut sites = StackSites::new();
    for function in program.functions.iter().filter(|f| analysable(f)) {
        let result = Analysis::run(program, function, &escaping);
        for (index, site) in result.sites.iter().enumerate() {
            let source = Source::Site(index);
            if result.escaped.contains(&source) {
                continue;
            }
            if site.in_loop && result.holders.get(&source).is_some_and(|locals| locals.len() > 1) {
                continue;
            }
            sites.insert(site.expr);
        }
    }
    sites
}

/// Functions whose body is compiled and whose parameters are exactly the call's arguments.
fn analysable(function: &Function) -> bool {
    !function.generic && !function.closure && function.extern_symbol.is_none()
}

struct Site {
    expr: *const Expr,
    in_loop: bool,
}

struct Analysis<'p> {
    program: &'p Program,
    function: &'p Function,
    escaping: &'p [Vec<bool>],
    sites: Vec<Site>,
    /// Site indexes by address, so that repeated passes over the body agree.
    site_index: HashMap<*const Expr, usize>,
    locals: HashMap<LocalId, Sources>,
    /// For each source, the locals it may be stored in.
    holders: HashMap<Source, HashSet<LocalId>>,
    escaped: Sources,
    loop_depth: usize,
    changed: bool,
}

impl<'p> Analysis<'p> {
    fn run(program: &'p Program, function: &'p Function, escaping: &'p [Vec<bool>]) -> Analysis<'p> {
        let mut analysis = Analysis {
            program,
            function,
            escaping,
            sites: Vec::new(),
            site_index: HashMap::new(),
            locals: HashMap::new(),
            holders: HashMap::new(),
            escaped: Sources::new(),
            loop_depth: 0,
            changed: false,
        };
        for (position, &param) in function.params.iter().enumerate() {
            analysis.assign(param, [Source::Param(position)].into());
        }
        // Locals only gain sources, so passes repeat until nothing changes; the first pass also
        // numbers the sites.
        loop {
            analysis.changed = false;
            analysis.loop_depth = 0;
            analysis.block(&function.body);
            if !analysis.changed {
                break;
            }
        }
        analysis
    }

    fn escape(&mut self, sources: Sources) {
        for source in sources {
            self.changed |= self.escaped.insert(source);
        }
    }

    fn assign(&mut self, local: LocalId, sources: Sources) {
        if sources.is_empty() {
            return;
        }
        let info = &self.function.locals[local.0];
        if info.boxed || info.capture.is_some() {
            // A closure can see the variable, so it can see the object.
            self.escape(sources);
            return;
        }
        for &source in &sources {
            self.changed |= self.holders.entry(source).or_default().insert(local);
        }
        let held = self.locals.entry(local).or_default();
        for source in sources {
            self.changed |= held.insert(source);
        }
    }

    fn block(&mut self, stmts: &'p [Stmt]) {
        for stmt in stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: &'p Stmt) {
        match stmt {
            Stmt::Assign { place, value } => {
                let sources = self.expr(value);
                match place {
                    Place::Local(local) => self.assign(*local, sources),
                    Place::Field { object, .. } => {
                        // Writing into an object does not leak the object, only the value.
                        self.expr(object);
                        self.escape(sources);
                    }
                    Place::Index { container, index, .. } => {
                        self.expr(container);
                        let index = self.expr(index);
                        self.escape(index);
                        self.escape(sources);
                    }
                }
            }
            Stmt::Expr(expr) => {
                self.expr(expr);
            }
            Stmt::Return(value) => {
                if let Some(value) = value {
                    let sources = self.expr(value);
                    self.escape(sources);
                }
            }
            Stmt::If { cond, then, otherwise } => {
                self.expr(cond);
                self.block(then);
                self.block(otherwise);
            }
            Stmt::While { cond, body } => {
                self.loop_depth += 1;
                self.expr(cond);
                self.block(body);
                self.loop_depth -= 1;
            }
            Stmt::For { source, body, .. } => {
                let sources = match source {
                    LoopSource::Range { start, stop, step } => {
                        let mut all = self.expr(start);
                        all.extend(self.expr(stop));
                        all.extend(self.expr(step));
                        all
                    }
                    LoopSource::List(e)
                    | LoopSource::Str(e)
                    | LoopSource::DictKeys(e)
                    | LoopSource::DictValues(e)
                    | LoopSource::DictItems(e)
                    | LoopSource::Iterator { iterator: e, .. }
                    | LoopSource::Channel(e) => self.expr(e),
                };
                self.escape(sources);
                // The loop variables hold elements of the source or values from `next`, never
                // an object built in this function that has not escaped: those would have had
                // to be stored into the source first.
                self.loop_depth += 1;
                self.block(body);
                self.loop_depth -= 1;
            }
            Stmt::Match { subject, cases } => {
                let sources = self.expr(subject);
                self.escape(sources);
                for case in cases {
                    self.pattern(&case.pattern.kind);
                    if let Some(guard) = &case.guard {
                        self.expr(guard);
                    }
                    self.block(&case.body);
                }
            }
            Stmt::Break | Stmt::Continue => {}
            Stmt::With { object, body, .. } => {
                // The object's `exit` method receives it; treat that as an escape.
                if let Some(sources) = self.locals.get(object).cloned() {
                    self.escape(sources);
                }
                self.block(body);
            }
        }
    }

    fn pattern(&mut self, pattern: &'p PatternKind) {
        match pattern {
            PatternKind::Literal(expr) => {
                self.expr(expr);
            }
            PatternKind::Variant { fields, .. } | PatternKind::Tuple(fields) | PatternKind::Or(fields) => {
                for field in fields {
                    self.pattern(&field.kind);
                }
            }
            PatternKind::Some(inner) | PatternKind::Ok(inner) | PatternKind::Err(inner) => self.pattern(&inner.kind),
            PatternKind::Wild | PatternKind::Bind(_) | PatternKind::None => {}
        }
    }

    /// Analyse `expr` and return what it may evaluate to.
    fn expr(&mut self, expr: &'p Expr) -> Sources {
        match &expr.kind {
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Str(_) | ExprKind::None => {
                Sources::new()
            }
            ExprKind::FuncRef(_) => Sources::new(),
            ExprKind::Local(local) => self.locals.get(local).cloned().unwrap_or_default(),
            ExprKind::StructNew { fields, .. } => {
                for field in fields {
                    let sources = self.expr(field);
                    self.escape(sources);
                }
                let address = expr as *const Expr;
                let index = match self.site_index.get(&address) {
                    Some(&index) => index,
                    None => {
                        let index = self.sites.len();
                        let in_loop = self.loop_depth > 0;
                        self.sites.push(Site { expr: address, in_loop });
                        self.site_index.insert(address, index);
                        index
                    }
                };
                [Source::Site(index)].into()
            }
            ExprKind::Field { object, .. } => {
                self.expr(object);
                Sources::new()
            }
            ExprKind::Call { func, args } => {
                let summary = self.callee_summary(*func, args.len());
                for (position, arg) in args.iter().enumerate() {
                    let sources = self.expr(arg);
                    if summary.get(position).is_none_or(|&escapes| escapes) {
                        self.escape(sources);
                    }
                }
                // A non-escaping parameter is never returned, so the result is none of these.
                Sources::new()
            }
            ExprKind::Builtin { op, args } => {
                // `copy` reads the fields into a new object. `hash` and comparisons may call a
                // user's own implementation, which could keep the object, so they escape.
                let reads_only = matches!(op, Builtin::Copy);
                for arg in args {
                    let sources = self.expr(arg);
                    if !reads_only {
                        self.escape(sources);
                    }
                }
                Sources::new()
            }
            ExprKind::IfElse { cond, then, otherwise } => {
                self.expr(cond);
                let mut sources = self.expr(then);
                sources.extend(self.expr(otherwise));
                sources
            }
            ExprKind::Block { stmts, value } => {
                self.block(stmts);
                self.expr(value)
            }
            ExprKind::And(lhs, rhs) | ExprKind::Or(lhs, rhs) => {
                self.expr(lhs);
                self.expr(rhs);
                Sources::new()
            }
            ExprKind::Unary { operand, .. } => {
                let sources = self.expr(operand);
                self.escape(sources);
                Sources::new()
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                let sources = self.expr(lhs);
                self.escape(sources);
                let sources = self.expr(rhs);
                self.escape(sources);
                Sources::new()
            }
            ExprKind::TupleGet { tuple, .. } => {
                self.expr(tuple);
                Sources::new()
            }
            ExprKind::Index { container, index } => {
                self.expr(container);
                let sources = self.expr(index);
                self.escape(sources);
                Sources::new()
            }
            ExprKind::Slice { container, start, stop } => {
                self.expr(container);
                for bound in [start, stop].into_iter().flatten() {
                    let sources = self.expr(bound);
                    self.escape(sources);
                }
                Sources::new()
            }
            ExprKind::FString(parts) => {
                for part in parts {
                    if let FPart::Value { expr, .. } = part {
                        let sources = self.expr(expr);
                        self.escape(sources);
                    }
                }
                Sources::new()
            }
            ExprKind::Some(inner)
            | ExprKind::Ok(inner)
            | ExprKind::Err(inner)
            | ExprKind::ToDyn { value: inner, .. }
            | ExprKind::Upcast { value: inner, .. }
            | ExprKind::Try { value: inner, .. } => {
                let sources = self.expr(inner);
                self.escape(sources);
                Sources::new()
            }
            ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Variant { fields: items, .. } => {
                for item in items {
                    let sources = self.expr(item);
                    self.escape(sources);
                }
                Sources::new()
            }
            ExprKind::Dict(entries) => {
                for (key, value) in entries {
                    let sources = self.expr(key);
                    self.escape(sources);
                    let sources = self.expr(value);
                    self.escape(sources);
                }
                Sources::new()
            }
            ExprKind::DynCall { receiver, args, .. } => {
                let sources = self.expr(receiver);
                self.escape(sources);
                for arg in args {
                    let sources = self.expr(arg);
                    self.escape(sources);
                }
                Sources::new()
            }
            ExprKind::Closure { captures, .. } => {
                for local in captures {
                    if let Some(sources) = self.locals.get(local).cloned() {
                        self.escape(sources);
                    }
                }
                Sources::new()
            }
            ExprKind::BoundMethod { receiver, .. } => {
                let sources = self.expr(receiver);
                self.escape(sources);
                Sources::new()
            }
            ExprKind::CallValue { callee, args } | ExprKind::Spawn { callee: Some(callee), args, .. } => {
                let sources = self.expr(callee);
                self.escape(sources);
                for arg in args {
                    let sources = self.expr(arg);
                    self.escape(sources);
                }
                Sources::new()
            }
            ExprKind::Spawn { callee: None, args, .. } => {
                for arg in args {
                    let sources = self.expr(arg);
                    self.escape(sources);
                }
                Sources::new()
            }
        }
    }

    /// Whether each parameter of `func` escapes; a callee the analysis cannot see into has
    /// every parameter escape.
    fn callee_summary(&self, func: FuncId, args: usize) -> Vec<bool> {
        let callee = &self.program.functions[func.0];
        if !analysable(callee) || callee.params.len() != args {
            return vec![true; args];
        }
        self.escaping[func.0].clone()
    }
}
