//! Bytecode generation, task 1.6 (D-033 to D-036).
//!
//! Compiles a function and the functions nested in it to register
//! bytecode (D-019): parameters are `r0..rP`, every binding of the function
//! that no nested function uses gets a fixed register after them, and
//! temporaries are allocated stack-wise above the locals. Bindings that
//! nested functions use ("captured") live in environment slots instead
//! (D-036): every scope with captured bindings pushes an environment when
//! entered, and a use addresses its slot as (depth, slot) through the
//! chain of environments.
//!
//! Values are numbers, strings, booleans, `null`, `undefined` and
//! functions; the code has the usual operators, `typeof`, template
//! literals, structured control flow, `switch`, `try`/`catch`/`finally`,
//! nested functions, closures and calls (proper tail calls in strict
//! code). `let`/`const` bindings that may be used in
//! their temporal dead zone hold the uninitialized marker until declared
//! and are checked at every use. Everything else is reported as
//! [`CompileError::Unsupported`], never miscompiled: arrays, tagged
//! templates, `this` in sloppy functions, block-level function
//! declarations, destructuring. String literals and template cooked
//! strings become string constants (D-044); strings with lone surrogates
//! are unsupported until the constant pool can hold UTF-16. Task 1.7e
//! (D-052) adds object literals with data properties, property reads,
//! writes, compound and update assignments, `delete`, `in` and method
//! calls; getters, setters, spread, `__proto__` and optional chaining are
//! unsupported.
//!
//! Register-writing contract: `expr_to(e, dst)` may write `dst` before it
//! has finished reading `e`'s operands, so `dst` must be a register `e`
//! doesn't read: a fresh temporary, or a local `e` provably doesn't
//! mention (see [`Gen::assign_local`]).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use ferrojs_bytecode::{
    label_offset, AsmLabel, Assembler, Const, Count, EnvSlot, Func, Handler, Ic, Imm, Limits, Reg,
};
use ferrojs_value::Value;
use oxc_ast::ast::*;
use oxc_ast_visit::{walk, Visit};
use oxc_semantic::{ScopeFlags, ScopeId, Scoping, SymbolFlags, SymbolId};
use oxc_span::GetSpan;

use crate::{parse, Allocator, Goal, Parsed, SyntaxError};

/// One compiled function: its bytecode and what the frame and the
/// verifier need to know about it (the future `FunctionTemplate`).
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledFunction {
    pub name: String,
    /// Declared parameter count `P`: parameters are `r0..rP`.
    pub params: u32,
    /// The function's `length`: the parameters before the first one with
    /// a default value (ExpectedArgumentCount).
    pub length: u32,
    /// Register count `R`, parameters included.
    pub registers: u32,
    /// The constant pool: numbers and strings (global names so far).
    pub constants: Vec<Constant>,
    /// Feedback slots.
    pub ics: u32,
    pub strict: bool,
    pub code: Vec<u8>,
    /// The exception table, innermost ranges first.
    pub handlers: Vec<Handler>,
    /// Nested function templates, indexed by `Func` operands.
    pub functions: Vec<CompiledFunction>,
}

impl CompiledFunction {
    /// What the verifier checks this function's code against.
    pub fn limits(&self) -> Limits {
        Limits {
            registers: self.registers,
            constants: self.constants.len() as u32,
            ics: self.ics,
            functions: self.functions.len() as u32,
        }
    }

    /// The constant pool as values; `string` makes the value of a string
    /// constant (the runtime allocates or interns it).
    pub fn constant_values(&self, mut string: impl FnMut(&str) -> Value) -> Vec<Value> {
        self.constants
            .iter()
            .map(|c| match c {
                Constant::Number(d) => Value::number(*d),
                Constant::String(s) => string(s),
            })
            .collect()
    }
}

/// A constant-pool entry.
#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    Number(f64),
    String(String),
}

/// Why a function couldn't be compiled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompileError {
    /// The source has syntax or early errors.
    Syntax(Vec<SyntaxError>),
    /// Valid JavaScript this compiler can't handle yet.
    Unsupported { what: String, offset: u32 },
    /// More registers, constants or feedback slots than operands can
    /// address, or a jump that can't be encoded.
    TooLarge(String),
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::Syntax(errors) => write!(f, "SyntaxError: {}", errors[0]),
            CompileError::Unsupported { what, offset } => {
                write!(f, "not supported yet: {what} (at byte {offset})")
            }
            CompileError::TooLarge(what) => write!(f, "function too large: {what}"),
        }
    }
}

impl std::error::Error for CompileError {}

type CResult<T> = Result<T, CompileError>;

fn unsupported<T>(what: impl Into<String>, span: oxc_span::Span) -> CResult<T> {
    Err(CompileError::Unsupported {
        what: what.into(),
        offset: span.start,
    })
}

/// Parses `source` as a script and compiles its first top-level function
/// declaration. Convenience for tests and tools until scripts compile as a
/// whole (task 1.6e).
pub fn compile_function_source(source: &str) -> CResult<CompiledFunction> {
    let allocator = Allocator::default();
    let parsed = parse(&allocator, source, Goal::Script).map_err(CompileError::Syntax)?;
    let func = parsed
        .program
        .body
        .iter()
        .find_map(|s| match s {
            Statement::FunctionDeclaration(f) => Some(&**f),
            _ => None,
        })
        .ok_or_else(|| CompileError::Unsupported {
            what: "no top-level function declaration".into(),
            offset: 0,
        })?;
    compile_function(&parsed, func)
}

/// Compiles `func`, which must belong to `parsed`, with the functions
/// nested in it. A use of a binding outside `func` is unsupported.
pub fn compile_function<'a>(parsed: &Parsed<'a>, func: &Function<'a>) -> CResult<CompiledFunction> {
    let compile = || {
        let scoping = parsed.semantic.scoping();
        let f = FnLike::function(func)?;
        let mut an = Analysis::new(scoping, f.scope, global_scope(parsed));
        an.call_targets = parsed.call_targets.clone();
        compile_fn(&an, &f, None)
    };
    compile().map_err(|e| original_offsets(parsed, e))
}

/// Maps the offset of an error found in `parsed`'s AST back to its source
/// (they differ when the front end rewrote call targets, D-045).
fn original_offsets(parsed: &Parsed<'_>, e: CompileError) -> CompileError {
    match e {
        CompileError::Unsupported { what, offset } => CompileError::Unsupported {
            what,
            offset: parsed.original_offset(offset),
        },
        other => other,
    }
}

/// A script's top-level scope, whose bindings are globals (None for a
/// module).
fn global_scope(parsed: &Parsed<'_>) -> Option<ScopeId> {
    if parsed.program.source_type.is_module() {
        return None;
    }
    parsed.program.scope_id.get()
}

/// Compiles a whole script (D-042): its top-level code as a function of no
/// parameters whose bindings are globals, declared up front
/// (GlobalDeclarationInstantiation), with every function nested in it.
/// Returns `undefined`.
pub fn compile_script(parsed: &Parsed<'_>) -> CResult<CompiledFunction> {
    compile_whole_script(parsed).map_err(|e| original_offsets(parsed, e))
}

fn compile_whole_script(parsed: &Parsed<'_>) -> CResult<CompiledFunction> {
    let Some(scope) = global_scope(parsed) else {
        return unsupported("modules", parsed.program.span);
    };
    let scoping = parsed.semantic.scoping();
    let mut an = Analysis::new(scoping, scope, Some(scope));
    an.call_targets = parsed.call_targets.clone();
    let mut g = Gen::new(&an, scope);
    g.claim_locals(0);
    let body = &parsed.program.body;
    g.find_early_references_in(body);

    // GlobalDeclarationInstantiation: lexical names, then var names
    // (functions included), then the functions themselves.
    let mut lexical = Vec::new();
    let mut vars = Vec::new();
    for sym in scoping.iter_bindings_in(scope) {
        let flags = scoping.symbol_flags(sym);
        let name = scoping.symbol_ident(sym).as_str().to_owned();
        if flags.intersects(SymbolFlags::Class) {
            return unsupported("classes", parsed.program.span);
        } else if flags.contains(SymbolFlags::ConstVariable) {
            lexical.push((name, ferrojs_bytecode::GlobalKind::Const));
        } else if flags.contains(SymbolFlags::BlockScopedVariable) {
            lexical.push((name, ferrojs_bytecode::GlobalKind::Let));
        } else {
            vars.push(name);
        }
    }
    for (name, kind) in lexical {
        let c = g.string_const(&name)?;
        g.asm.declare_global(c, Imm(kind.imm()));
    }
    for name in vars {
        let c = g.string_const(&name)?;
        g.asm
            .declare_global(c, Imm(ferrojs_bytecode::GlobalKind::Var.imm()));
    }
    g.enter_lexical(body.iter());
    g.hoist_functions(body)?;
    for stmt in body {
        g.statement(stmt)?;
    }
    let r = g.temp()?;
    g.asm.load_undefined(r);
    g.asm.ret(r);
    g.free(r);
    g.finish(
        String::new(),
        0,
        0,
        scoping.scope_flags(scope).is_strict_mode(),
    )
}

/// Parses `source` as a script and compiles it.
pub fn compile_script_source(source: &str) -> CResult<CompiledFunction> {
    let allocator = Allocator::default();
    let parsed = parse(&allocator, source, Goal::Script).map_err(CompileError::Syntax)?;
    compile_script(&parsed)
}

/// Which bindings live in environments, and where (D-036).
struct Analysis<'s> {
    scoping: &'s Scoping,
    /// Bindings used from a function other than the one that declares
    /// them.
    captured: HashSet<SymbolId>,
    /// For each scope with captured bindings: those bindings, in slot
    /// order.
    env_slots: HashMap<ScopeId, Vec<SymbolId>>,
    slot_of: HashMap<SymbolId, u32>,
    /// The scope of the function being compiled: bindings outside it are
    /// out of reach.
    root: ScopeId,
    /// A script's top-level scope: its bindings are globals, reached by
    /// name (D-042).
    global: Option<ScopeId>,
    /// [`Parsed::call_targets`]: calls assigned to in sloppy code (D-045).
    call_targets: Vec<u32>,
    /// Parameters that a parameter initializer may use before they are
    /// initialized (D-046): every use checks the TDZ marker. Filled in by
    /// each function before its nested functions are compiled.
    param_tdz: RefCell<HashSet<SymbolId>>,
}

impl<'s> Analysis<'s> {
    fn new(scoping: &'s Scoping, root: ScopeId, global: Option<ScopeId>) -> Analysis<'s> {
        let mut captured = HashSet::new();
        for sym in scoping.symbol_ids() {
            if Some(scoping.symbol_scope_id(sym)) == global {
                continue;
            }
            let home = owner(scoping, scoping.symbol_scope_id(sym));
            let used_elsewhere = scoping
                .get_resolved_reference_ids(sym)
                .iter()
                .any(|&r| owner(scoping, scoping.get_reference(r).scope_id()) != home);
            if used_elsewhere {
                captured.insert(sym);
            }
        }
        let mut env_slots: HashMap<ScopeId, Vec<SymbolId>> = HashMap::new();
        let mut slot_of = HashMap::new();
        let mut ordered: Vec<SymbolId> = captured.iter().copied().collect();
        ordered.sort();
        for sym in ordered {
            let slots = env_slots.entry(scoping.symbol_scope_id(sym)).or_default();
            slot_of.insert(sym, slots.len() as u32);
            slots.push(sym);
        }
        Analysis {
            scoping,
            captured,
            env_slots,
            slot_of,
            root,
            global,
            call_targets: Vec::new(),
            param_tdz: RefCell::new(HashSet::new()),
        }
    }

    /// Is `scope` the root function's scope or inside it?
    fn in_root(&self, mut scope: ScopeId) -> bool {
        loop {
            if scope == self.root {
                return true;
            }
            match self.scoping.scope_parent_id(scope) {
                Some(p) => scope = p,
                None => return false,
            }
        }
    }

    fn has_env(&self, scope: ScopeId) -> bool {
        self.env_slots.contains_key(&scope)
    }
}

/// The function scope that owns `scope`: the nearest enclosing scope that
/// is a function (arrows included) or the top level.
fn owner(scoping: &Scoping, mut scope: ScopeId) -> ScopeId {
    loop {
        if scoping
            .scope_flags(scope)
            .intersects(ScopeFlags::Function | ScopeFlags::Top)
        {
            return scope;
        }
        match scoping.scope_parent_id(scope) {
            Some(p) => scope = p,
            None => return scope,
        }
    }
}

/// A function body: statements, or an arrow's expression.
#[derive(Clone, Copy)]
enum Body<'b, 'a> {
    Block(&'b FunctionBody<'a>),
    Expr(&'b Expression<'a>),
}

/// What the compiler needs from a function, arrow or not.
struct FnLike<'b, 'a> {
    name: String,
    span: oxc_span::Span,
    params: &'b FormalParameters<'a>,
    body: Body<'b, 'a>,
    scope: ScopeId,
    /// A named function expression's own name binding.
    own_name: Option<SymbolId>,
    arrow: bool,
}

impl<'b, 'a> FnLike<'b, 'a> {
    fn function(f: &'b Function<'a>) -> CResult<FnLike<'b, 'a>> {
        if f.generator || f.r#async {
            return unsupported("generator and async functions", f.span);
        }
        let Some(body) = &f.body else {
            return unsupported("function without a body", f.span);
        };
        let own_name = match (&f.id, f.r#type) {
            (Some(id), FunctionType::FunctionExpression) => Some(symbol_of(id)),
            _ => None,
        };
        Ok(FnLike {
            name: f
                .id
                .as_ref()
                .map(|id| id.name.as_str().to_owned())
                .unwrap_or_default(),
            span: f.span,
            params: &f.params,
            body: Body::Block(body),
            scope: f.scope_id.get().expect("semantic analysis ran"),
            own_name,
            arrow: false,
        })
    }

    fn arrow(f: &'b ArrowFunctionExpression<'a>) -> CResult<FnLike<'b, 'a>> {
        if f.r#async {
            return unsupported("async functions", f.span);
        }
        let body = match &f.body {
            ArrowFunctionBody::FunctionBody(b) => Body::Block(b),
            e => Body::Expr(e.to_expression()),
        };
        Ok(FnLike {
            name: String::new(),
            span: f.span,
            params: &f.params,
            body,
            scope: f.scope_id.get().expect("semantic analysis ran"),
            own_name: None,
            arrow: true,
        })
    }
}

/// Compiles one function. `parent_stack` is the enclosing function's
/// stack of entered environment scopes, for the closure-creation check.
fn compile_fn(
    an: &Analysis<'_>,
    f: &FnLike<'_, '_>,
    parent_stack: Option<&[ScopeId]>,
) -> CResult<CompiledFunction> {
    let scoping = an.scoping;
    // Every environment scope between the function and its parent
    // function must be entered where the closure is created, or depths
    // computed through it would be wrong.
    if let Some(stack) = parent_stack {
        let mut s = scoping.scope_parent_id(f.scope);
        while let Some(scope) = s {
            if an.has_env(scope) && !stack.contains(&scope) {
                return unsupported(
                    "a closure inside a scope this compiler doesn't enter",
                    f.span,
                );
            }
            if scoping
                .scope_flags(scope)
                .intersects(ScopeFlags::Function | ScopeFlags::Top)
            {
                break;
            }
            s = scoping.scope_parent_id(scope);
        }
    }
    if let Some(own) = f.own_name {
        if scoping.symbol_scope_id(own) != f.scope {
            return unsupported("a function name bound outside its function", f.span);
        }
    }
    let mut g = Gen::new(an, f.scope);
    g.own_name = f.own_name;
    // `this` is the frame's `this` in strict non-arrow functions. Sloppy
    // functions box it (needs the global object) and arrows take their
    // enclosing function's: later.
    g.this_ok = !f.arrow && scoping.scope_flags(f.scope).is_strict_mode();

    // Parameters first, in order: r0..rP. A repeated name (sloppy mode)
    // binds the last parameter with that name.
    let params = f.params;
    if let Some(rest) = &params.rest {
        return unsupported("rest parameters", rest.span);
    }
    let count = params.items.len() as u32;
    let length = params
        .items
        .iter()
        .position(|p| p.initializer.is_some())
        .unwrap_or(params.items.len()) as u32;
    let mut param_syms = Vec::new();
    for p in &params.items {
        let BindingPattern::BindingIdentifier(id) = &p.pattern else {
            return unsupported("destructuring parameters", p.span);
        };
        param_syms.push(symbol_of(id));
    }
    // With default values (D-046), parameters are initialized one by one,
    // left to right: a parameter an initializer may reach before its turn
    // (its own initializer or an earlier one, closures included) gets its
    // own binding with the TDZ marker; the others stay in their argument
    // registers.
    let has_defaults = length < count;
    let mut tdz = HashSet::new();
    if has_defaults {
        let mut refs = ParamRefs {
            scoping,
            params: &param_syms,
            current: 0,
            closures: 0,
            found: Vec::new(),
        };
        for (i, p) in params.items.iter().enumerate() {
            if let Some(init) = &p.initializer {
                refs.current = i;
                refs.visit_expression(init);
            }
        }
        for &(i, j, in_closure) in &refs.found {
            if i <= j {
                tdz.insert(param_syms[j]);
            }
            // A closure in the parameters sees the parameter, but the body
            // sees a separate `var` copy (FunctionDeclarationInstantiation
            // step 28): two bindings for one symbol.
            if in_closure && !scoping.symbol_redeclarations(param_syms[j]).is_empty() {
                return unsupported(
                    "a parameter redeclared in the body and used by a closure in the parameters",
                    params.items[j].span,
                );
            }
        }
        an.param_tdz.borrow_mut().extend(tdz.iter().copied());
    }
    for (i, &sym) in param_syms.iter().enumerate() {
        if !tdz.contains(&sym) {
            g.locals.insert(sym, Reg(i as u32));
        }
    }
    g.next_reg = count;
    g.claim_locals(count);
    for &sym in &tdz {
        if !an.captured.contains(&sym) {
            g.needs_tdz.insert(sym);
        }
    }
    g.find_early_references(f.body);

    // Entry: the function's environment, captured parameters copied in
    // (the last one wins for repeated names), the TDZ marker in lexical
    // bindings, the function's own name, then hoisted declarations.
    let fn_scope_pushed = g.enter_scope(f.scope)?;
    if !has_defaults {
        for (i, &sym) in param_syms.iter().enumerate() {
            if an.captured.contains(&sym) {
                let slot = EnvSlot {
                    depth: 0,
                    slot: an.slot_of[&sym],
                };
                g.asm.store_env(slot, Reg(i as u32));
            }
        }
    }
    if let Some(own) = f.own_name {
        let t = g.temp()?;
        g.asm.load_closure(t);
        g.init_binding(own, f.scope, t);
        g.free(t);
    }
    if has_defaults {
        g.init_params(&params.items, &param_syms, &tdz)?;
    }
    match f.body {
        Body::Block(body) => {
            g.enter_lexical(body.statements.iter());
            g.hoist_functions(&body.statements)?;
            for stmt in &body.statements {
                g.statement(stmt)?;
            }
            let r = g.temp()?;
            g.asm.load_undefined(r);
            g.asm.ret(r);
            g.free(r);
        }
        Body::Expr(e) => g.return_expression(e)?,
    }
    let _ = fn_scope_pushed;

    g.finish(
        f.name.clone(),
        count,
        length,
        scoping.scope_flags(f.scope).is_strict_mode(),
    )
}

fn symbol_of(id: &BindingIdentifier<'_>) -> SymbolId {
    id.symbol_id.get().expect("semantic analysis ran")
}

/// A statement `break` (and maybe `continue`) can target.
struct Breakable {
    labels: Vec<String>,
    break_to: AsmLabel,
    /// `None` for labeled non-loop statements and `switch`.
    continue_to: Option<AsmLabel>,
    /// Loops and `switch`: an unlabeled `break` can target it.
    unlabeled_break: bool,
    /// How many `finally` blocks were open when it was entered: a jump to
    /// it from deeper runs the ones in between first.
    finally_depth: usize,
    /// How many environments were entered at its targets.
    env_depth: usize,
}

/// An open `try … finally`. Jumps leaving it set `kind` and `value`, jump
/// to `fin`, and the code after the finally block finishes them.
struct Finally {
    fin: AsmLabel,
    /// 0 normal completion, 1 throw, 2 return, 3 + i the i-th entry of
    /// `jumps`.
    kind: Reg,
    value: Reg,
    returns: bool,
    /// (breakable index, is continue) pairs routed through this block.
    jumps: Vec<(usize, bool)>,
    /// How many environments were entered at the `try`.
    env_depth: usize,
}

const K_NORMAL: i32 = 0;
const K_THROW: i32 = 1;
const K_RETURN: i32 = 2;
const K_JUMPS: i32 = 3;

/// Where a binding's value lives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Loc {
    Reg(Reg),
    Env(EnvSlot),
    /// A global, by name (D-042): a script's top-level binding, or an
    /// undeclared name. `lexical` for a top-level `let`/`const`.
    Global {
        name: Const,
        lexical: bool,
    },
}

/// What a store to a binding does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WriteMode {
    Normal,
    /// `const`, or a named function expression's name in strict code:
    /// throws a `TypeError`.
    Const,
    /// A named function expression's name in sloppy code: ignored.
    Ignore,
}

/// Where an assignment stores, and what it must check first.
#[derive(Clone, Copy)]
struct Target {
    loc: Loc,
    /// `None` for an undeclared global.
    sym: Option<SymbolId>,
    /// Lexical binding that may be in its TDZ: check before storing.
    tdz: bool,
    mode: WriteMode,
}

impl Target {
    /// A plain register store with no checks.
    fn simple_reg(&self) -> Option<Reg> {
        match self.loc {
            Loc::Reg(r) if !self.tdz && self.mode == WriteMode::Normal => Some(r),
            _ => None,
        }
    }
}

struct Gen<'s> {
    an: &'s Analysis<'s>,
    scoping: &'s Scoping,
    scope: ScopeId,
    asm: Assembler,
    locals: HashMap<SymbolId, Reg>,
    /// Lexical bindings (`let`, `const`) whose declaration has been
    /// compiled.
    initialized: HashSet<SymbolId>,
    /// Lexical bindings in registers that may be used in their TDZ: they
    /// start as the marker and every use checks it (D-034).
    needs_tdz: HashSet<SymbolId>,
    next_reg: u32,
    max_reg: u32,
    constants: Vec<Constant>,
    constant_index: HashMap<u64, u32>,
    string_index: HashMap<String, u32>,
    ics: u32,
    breakables: Vec<Breakable>,
    /// Labels seen on the way to the statement being compiled.
    pending_labels: Vec<String>,
    /// Exception table entries (start, end, target, register), in the
    /// order their ranges close: inner before outer.
    handlers: Vec<(AsmLabel, AsmLabel, AsmLabel, Reg)>,
    finallies: Vec<Finally>,
    /// Names of the catch parameters around the code being compiled.
    catch_names: Vec<String>,
    /// Scopes whose environments are entered here, innermost last.
    scope_stack: Vec<ScopeId>,
    functions: Vec<CompiledFunction>,
    /// Function declarations already created at their scope's entry.
    hoisted: HashSet<u32>,
    own_name: Option<SymbolId>,
    /// `this` compiles to `LoadThis`.
    this_ok: bool,
    /// Inside a `try` block (or the `catch` of a `try … finally`): no
    /// tail calls.
    no_tail: u32,
    /// Whether any scope of this function has an environment (then `try`
    /// saves and restores the current one).
    uses_envs: bool,
}

const MAX_OPERAND: u32 = i32::MAX as u32;

impl<'s> Gen<'s> {
    fn new(an: &'s Analysis<'s>, scope: ScopeId) -> Gen<'s> {
        let uses_envs = an.env_slots.keys().any(|&s| owner(an.scoping, s) == scope);
        Gen {
            an,
            scoping: an.scoping,
            scope,
            asm: Assembler::new(),
            locals: HashMap::new(),
            initialized: HashSet::new(),
            needs_tdz: HashSet::new(),
            next_reg: 0,
            max_reg: 0,
            constants: Vec::new(),
            constant_index: HashMap::new(),
            string_index: HashMap::new(),
            ics: 0,
            breakables: Vec::new(),
            pending_labels: Vec::new(),
            handlers: Vec::new(),
            finallies: Vec::new(),
            catch_names: Vec::new(),
            scope_stack: Vec::new(),
            functions: Vec::new(),
            hoisted: HashSet::new(),
            own_name: None,
            this_ok: false,
            no_tail: 0,
            uses_envs,
        }
    }

    fn env_depth(&self) -> usize {
        self.scope_stack.len()
    }

    fn strict(&self) -> bool {
        self.scoping.scope_flags(self.scope).is_strict_mode()
    }

    /// Gives every binding declared in this function (outside its nested
    /// functions) that no nested function uses a register after the
    /// parameters, in symbol order.
    fn claim_locals(&mut self, params: u32) {
        let mut next = params;
        for sym in self.scoping.symbol_ids() {
            if self.locals.contains_key(&sym) || self.an.captured.contains(&sym) {
                continue;
            }
            if owner(self.scoping, self.scoping.symbol_scope_id(sym)) == self.scope {
                self.locals.insert(sym, Reg(next));
                next += 1;
            }
        }
        self.next_reg = next;
        self.max_reg = next;
    }

    /// Finds the register-held lexical bindings with a use that comes
    /// textually before the end of their declaration (so it may run in the
    /// TDZ). Within one function, textual order is execution order for
    /// bindings no closure uses, except in `switch` ([`Gen::switch`]).
    fn find_early_references(&mut self, body: Body<'_, '_>) {
        let mut v = UseOrder {
            scoping: self.scoping,
            first_use: HashMap::new(),
            decl_end: HashMap::new(),
        };
        match body {
            Body::Block(b) => v.visit_function_body(b),
            Body::Expr(e) => v.visit_expression(e),
        }
        for (sym, end) in v.decl_end {
            if v.first_use.get(&sym).is_some_and(|&start| start < end)
                && is_lexical(self.scoping.symbol_flags(sym))
            {
                self.needs_tdz.insert(sym);
            }
        }
    }

    /// [`Gen::find_early_references`] for a script's statements.
    fn find_early_references_in(&mut self, body: &[Statement<'_>]) {
        let mut v = UseOrder {
            scoping: self.scoping,
            first_use: HashMap::new(),
            decl_end: HashMap::new(),
        };
        for s in body {
            v.visit_statement(s);
        }
        for (sym, end) in v.decl_end {
            if v.first_use.get(&sym).is_some_and(|&start| start < end)
                && is_lexical(self.scoping.symbol_flags(sym))
            {
                self.needs_tdz.insert(sym);
            }
        }
    }

    /// Enters `scope`'s environment if it has one: pushes it and puts the
    /// TDZ marker in its lexical bindings. Returns whether it pushed.
    fn enter_scope(&mut self, scope: ScopeId) -> CResult<bool> {
        let Some(slots) = self.an.env_slots.get(&scope) else {
            return Ok(false);
        };
        let n = u32::try_from(slots.len())
            .map_err(|_| CompileError::TooLarge("environment slots".into()))?;
        self.asm.push_env(Count(n));
        self.scope_stack.push(scope);
        let lexical: Vec<u32> = slots
            .iter()
            .filter(|&&s| is_lexical(self.scoping.symbol_flags(s)))
            .map(|s| self.an.slot_of[s])
            .collect();
        if !lexical.is_empty() {
            let t = self.temp()?;
            self.asm.load_uninitialized(t);
            for slot in lexical {
                self.asm.store_env(EnvSlot { depth: 0, slot }, t);
            }
            self.free(t);
        }
        Ok(true)
    }

    /// Leaves a scope entered by [`Gen::enter_scope`] on the normal path.
    fn leave_scope(&mut self, pushed: bool) {
        if pushed {
            self.asm.pop_env();
            self.scope_stack.pop();
        }
    }

    /// Emits `n` `PopEnv`s (for a jump out of `n` entered scopes).
    fn pop_envs(&mut self, n: usize) {
        for _ in 0..n {
            self.asm.pop_env();
        }
    }

    /// At the start of a scope: puts the TDZ marker into the register-held
    /// lexical bindings declared directly in `stmts` that need checks.
    fn enter_lexical<'b, 'c: 'b>(&mut self, stmts: impl Iterator<Item = &'b Statement<'c>>) {
        for s in stmts {
            if let Statement::VariableDeclaration(d) = s {
                self.mark_uninitialized(d);
            }
        }
    }

    fn mark_uninitialized(&mut self, d: &VariableDeclaration<'_>) {
        if d.kind == VariableDeclarationKind::Var {
            return;
        }
        for decl in &d.declarations {
            if let BindingPattern::BindingIdentifier(id) = &decl.id {
                let sym = symbol_of(id);
                if self.needs_tdz.contains(&sym) {
                    if let Some(&r) = self.locals.get(&sym) {
                        self.asm.load_uninitialized(r);
                    }
                }
            }
        }
    }

    /// Creates the closures of the function declarations in `stmts` (a
    /// function body) and stores them in their bindings.
    fn hoist_functions(&mut self, stmts: &[Statement<'_>]) -> CResult<()> {
        for s in stmts {
            if let Statement::FunctionDeclaration(fd) = s {
                let f = FnLike::function(fd)?;
                let id = fd.id.as_ref().expect("declarations have names");
                let func = self.nested(&f)?;
                let t = self.temp()?;
                self.asm.create_closure(t, func);
                self.init_binding(symbol_of(id), self.scope, t);
                self.free(t);
                self.hoisted.insert(fd.span.start);
            }
        }
        Ok(())
    }

    /// Initializes the parameters of a function with default values, left
    /// to right (IteratorBindingInitialization, D-046): an `undefined`
    /// argument is replaced by its initializer's value. Parameters in
    /// `tdz` start as the TDZ marker in their own binding.
    fn init_params(
        &mut self,
        items: &[FormalParameter<'_>],
        syms: &[SymbolId],
        tdz: &HashSet<SymbolId>,
    ) -> CResult<()> {
        if !tdz.is_empty() {
            let t = self.temp()?;
            self.asm.load_uninitialized(t);
            for (p, sym) in items.iter().zip(syms) {
                if tdz.contains(sym) {
                    match self.locate(*sym, self.scope, p.span)? {
                        Loc::Reg(r) => self.asm.mov(r, t),
                        Loc::Env(slot) => self.asm.store_env(slot, t),
                        Loc::Global { .. } => unreachable!("parameters are not globals"),
                    }
                }
            }
            self.free(t);
        }
        for (i, (p, &sym)) in items.iter().zip(syms).enumerate() {
            // The argument register. Nothing reads it while the initializer
            // runs: a parameter its own initializer uses is in `tdz`, with
            // its binding elsewhere.
            let arg = Reg(i as u32);
            if let Some(init) = &p.initializer {
                let skip = self.asm.new_label();
                let t = self.temp()?;
                self.asm.load_undefined(t);
                self.asm.strict_equal(t, arg, t);
                self.asm.jump_if_false(t, skip);
                self.free(t);
                self.expr_to(init, arg)?;
                self.asm.bind(skip);
            }
            if tdz.contains(&sym) || self.an.captured.contains(&sym) {
                self.init_binding(sym, self.scope, arg);
            }
        }
        Ok(())
    }

    /// Compiles a nested function and returns its index.
    fn nested(&mut self, f: &FnLike<'_, '_>) -> CResult<Func> {
        let compiled = compile_fn(self.an, f, Some(&self.scope_stack))?;
        let i = u32::try_from(self.functions.len())
            .map_err(|_| CompileError::TooLarge("nested functions".into()))?;
        self.functions.push(compiled);
        Ok(Func(i))
    }

    /// Lays out the code and builds the compiled function.
    fn finish(
        self,
        name: String,
        params: u32,
        length: u32,
        strict: bool,
    ) -> CResult<CompiledFunction> {
        let (code, labels) = self
            .asm
            .finish_with_labels()
            .map_err(|e| CompileError::TooLarge(format!("{e:?}")))?;
        let at = |l| label_offset(&labels, l).expect("handler labels are bound");
        let handlers = self
            .handlers
            .iter()
            .map(|&(start, end, target, exception)| Handler {
                start: at(start),
                end: at(end),
                target: at(target),
                exception,
            })
            // A try block that compiled to nothing protects nothing.
            .filter(|h| h.start < h.end)
            .collect();
        Ok(CompiledFunction {
            name,
            params,
            length,
            registers: self.max_reg.max(params),
            constants: self.constants,
            ics: self.ics,
            strict,
            code,
            handlers,
            functions: self.functions,
        })
    }

    fn temp(&mut self) -> CResult<Reg> {
        if self.next_reg >= MAX_OPERAND {
            return Err(CompileError::TooLarge("registers".into()));
        }
        let r = Reg(self.next_reg);
        self.next_reg += 1;
        self.max_reg = self.max_reg.max(self.next_reg);
        Ok(r)
    }

    /// Frees the most recently allocated temporary.
    fn free(&mut self, r: Reg) {
        debug_assert_eq!(
            r.0 + 1,
            self.next_reg,
            "temporaries are freed in stack order"
        );
        self.next_reg = r.0;
    }

    fn ic(&mut self) -> CResult<Ic> {
        if self.ics >= MAX_OPERAND {
            return Err(CompileError::TooLarge("feedback slots".into()));
        }
        self.ics += 1;
        Ok(Ic(self.ics - 1))
    }

    fn load_number(&mut self, dst: Reg, d: f64) -> CResult<()> {
        let i = d as i32;
        if f64::from(i) == d && !(d == 0.0 && d.is_sign_negative()) {
            self.asm.load_int(dst, Imm(i));
            return Ok(());
        }
        let bits = Value::number(d).to_bits();
        let index = match self.constant_index.get(&bits) {
            Some(&i) => i,
            None => {
                let i = self.push_const(Constant::Number(d))?;
                self.constant_index.insert(bits, i);
                i
            }
        };
        self.asm.load_const(dst, Const(index));
        Ok(())
    }

    fn push_const(&mut self, c: Constant) -> CResult<u32> {
        let i = self.constants.len() as u32;
        if i >= MAX_OPERAND {
            return Err(CompileError::TooLarge("constants".into()));
        }
        self.constants.push(c);
        Ok(i)
    }

    /// The constant index of string `text` (shared).
    fn string_const(&mut self, text: &str) -> CResult<Const> {
        if let Some(&i) = self.string_index.get(text) {
            return Ok(Const(i));
        }
        let i = self.push_const(Constant::String(text.to_owned()))?;
        self.string_index.insert(text.to_owned(), i);
        Ok(Const(i))
    }

    // ---- bindings ----

    /// The binding an identifier reference resolves to, and the scope the
    /// reference is in.
    fn resolve(&self, id: &IdentifierReference<'_>) -> Option<SymbolId> {
        let rid = id.reference_id.get()?;
        self.scoping.get_reference(rid).symbol_id()
    }

    /// [`Gen::resolve`] for a use that compiles: refuses an unresolved
    /// `arguments` inside a non-arrow function (the arguments object is
    /// later work, not a global).
    fn resolve_use(&self, id: &IdentifierReference<'_>) -> CResult<Option<SymbolId>> {
        let sym = self.resolve(id);
        if sym.is_none() && id.name == "arguments" {
            let mut s = Some(self.reference_scope(id));
            while let Some(scope) = s {
                let flags = self.scoping.scope_flags(scope);
                if flags.contains(ScopeFlags::Function) && !flags.contains(ScopeFlags::Arrow) {
                    return unsupported("the arguments object", id.span);
                }
                s = self.scoping.scope_parent_id(scope);
            }
        }
        Ok(sym)
    }

    fn reference_scope(&self, id: &IdentifierReference<'_>) -> ScopeId {
        let rid = id.reference_id.get().expect("semantic analysis ran");
        self.scoping.get_reference(rid).scope_id()
    }

    /// Where `sym` lives, seen from code in scope `from`.
    fn locate(&mut self, sym: SymbolId, from: ScopeId, span: oxc_span::Span) -> CResult<Loc> {
        if Some(self.scoping.symbol_scope_id(sym)) == self.an.global {
            let name = self.scoping.symbol_ident(sym).as_str().to_owned();
            let lexical = is_lexical(self.scoping.symbol_flags(sym));
            return Ok(Loc::Global {
                name: self.string_const(&name)?,
                lexical,
            });
        }
        if !self.an.captured.contains(&sym) {
            return match self.locals.get(&sym) {
                Some(&r) => Ok(Loc::Reg(r)),
                None => unsupported("a binding outside the compiled function", span),
            };
        }
        let home = self.scoping.symbol_scope_id(sym);
        if !self.an.in_root(home) {
            return unsupported("a binding outside the compiled function", span);
        }
        let mut depth = 0u32;
        let mut s = from;
        let mut in_this_function = true;
        loop {
            if s == home {
                break;
            }
            if self.an.has_env(s) {
                if in_this_function && !self.scope_stack.contains(&s) {
                    return unsupported("a scope this compiler doesn't enter", span);
                }
                depth += 1;
            }
            if s == self.scope {
                in_this_function = false;
            }
            match self.scoping.scope_parent_id(s) {
                Some(p) => s = p,
                None => return unsupported("a binding outside the compiled function", span),
            }
        }
        // A binding of an enclosing function is on the closure's chain
        // (checked when the closure was created); one of this function must
        // be in an entered scope.
        if in_this_function && !self.scope_stack.contains(&home) {
            return unsupported("a scope this compiler doesn't enter", span);
        }
        Ok(Loc::Env(EnvSlot {
            depth,
            slot: self.an.slot_of[&sym],
        }))
    }

    /// Rejects bindings this part can't handle; returns whether a use
    /// needs a run-time TDZ check.
    fn binding_kind(&self, sym: SymbolId, loc: Loc, span: oxc_span::Span) -> CResult<bool> {
        if let Loc::Global { .. } = loc {
            // The runtime checks globals' TDZ and constness.
            return Ok(false);
        }
        let flags = self.scoping.symbol_flags(sym);
        if flags.intersects(SymbolFlags::Class) {
            return unsupported("class bindings", span);
        }
        if !is_lexical(flags) && !self.an.param_tdz.borrow().contains(&sym) {
            return Ok(false);
        }
        if let Loc::Env(_) = loc {
            // Closures can run at any time: always check.
            return Ok(true);
        }
        if self.needs_tdz.contains(&sym) {
            return Ok(true);
        }
        if !self.initialized.contains(&sym) {
            // `find_early_references` should have caught this use.
            return unsupported("an access the TDZ analysis didn't expect", span);
        }
        Ok(false)
    }

    /// Reads `id` into a register (its own, for a register local), after
    /// its TDZ check. `None` for a global.
    fn read_binding(&mut self, id: &IdentifierReference<'_>) -> CResult<Option<Operand>> {
        let Some(sym) = self.resolve_use(id)? else {
            return Ok(None);
        };
        let loc = self.locate(sym, self.reference_scope(id), id.span)?;
        let check = self.binding_kind(sym, loc, id.span)?;
        Ok(Some(match loc {
            Loc::Reg(r) => {
                if check {
                    self.asm.check_initialized(r);
                }
                Operand {
                    reg: r,
                    temp: false,
                }
            }
            Loc::Env(slot) => {
                let t = self.temp()?;
                self.asm.load_env(t, slot);
                if check {
                    self.asm.check_initialized(t);
                }
                Operand { reg: t, temp: true }
            }
            Loc::Global { name, .. } => {
                let t = self.temp()?;
                self.asm.load_name(t, name);
                Operand { reg: t, temp: true }
            }
        }))
    }

    /// Initializes `sym` (seen from `from`) with `v`: no TDZ or const
    /// checks.
    fn init_binding(&mut self, sym: SymbolId, from: ScopeId, v: Reg) {
        let loc = self
            .locate(sym, from, oxc_span::Span::default())
            .expect("a binding of this function");
        match loc {
            Loc::Reg(r) if r != v => self.asm.mov(r, v),
            Loc::Reg(_) => {}
            Loc::Env(slot) => self.asm.store_env(slot, v),
            Loc::Global {
                name,
                lexical: true,
            } => self.asm.init_global(name, v),
            Loc::Global {
                name,
                lexical: false,
            } => self.asm.store_name(name, v),
        }
    }

    /// Where an assignment to `id` stores.
    fn write_target(&mut self, id: &IdentifierReference<'_>) -> CResult<Target> {
        let Some(sym) = self.resolve_use(id)? else {
            // An undeclared name: a global, created on assignment in sloppy
            // code (the runtime decides).
            let name = self.string_const(id.name.as_str())?;
            return Ok(Target {
                loc: Loc::Global {
                    name,
                    lexical: false,
                },
                sym: None,
                tdz: false,
                mode: WriteMode::Normal,
            });
        };
        let loc = self.locate(sym, self.reference_scope(id), id.span)?;
        let tdz = self.binding_kind(sym, loc, id.span)?;
        let flags = self.scoping.symbol_flags(sym);
        let own_name = self.own_name == Some(sym)
            || (flags.contains(SymbolFlags::FunctionExpression)
                && !flags.intersects(SymbolFlags::Variable));
        let mode = if let Loc::Global { .. } = loc {
            // The runtime throws for global consts.
            WriteMode::Normal
        } else if flags.contains(SymbolFlags::ConstVariable) {
            WriteMode::Const
        } else if own_name {
            // The name of a named function expression is immutable.
            if self.strict() {
                WriteMode::Const
            } else {
                WriteMode::Ignore
            }
        } else {
            WriteMode::Normal
        };
        Ok(Target {
            loc,
            sym: Some(sym),
            tdz,
            mode,
        })
    }

    /// Reads an assignment target's current value (with its TDZ check),
    /// for compound and logical assignments and updates.
    fn read_target(&mut self, t: Target) -> CResult<Operand> {
        Ok(match t.loc {
            Loc::Reg(r) => {
                if t.tdz {
                    self.asm.check_initialized(r);
                }
                Operand {
                    reg: r,
                    temp: false,
                }
            }
            Loc::Env(slot) => {
                let v = self.temp()?;
                self.asm.load_env(v, slot);
                if t.tdz {
                    self.asm.check_initialized(v);
                }
                Operand { reg: v, temp: true }
            }
            Loc::Global { name, .. } => {
                let v = self.temp()?;
                self.asm.load_name(v, name);
                Operand { reg: v, temp: true }
            }
        })
    }

    /// Stores register `v` in `t`: SetMutableBinding (TDZ check, then the
    /// const check).
    fn store_reg(&mut self, t: Target, v: Reg) -> CResult<()> {
        if t.tdz {
            match t.loc {
                Loc::Reg(r) => self.asm.check_initialized(r),
                Loc::Env(slot) => {
                    let c = self.temp()?;
                    self.asm.load_env(c, slot);
                    self.asm.check_initialized(c);
                    self.free(c);
                }
                Loc::Global { .. } => unreachable!("globals have no static TDZ"),
            }
        }
        match t.mode {
            WriteMode::Const => self.asm.throw_const_assign(),
            WriteMode::Ignore => {}
            WriteMode::Normal => match t.loc {
                Loc::Reg(r) if r != v => self.asm.mov(r, v),
                Loc::Reg(_) => {}
                Loc::Env(slot) => self.asm.store_env(slot, v),
                Loc::Global { name, .. } if self.strict() => self.asm.store_name_strict(name, v),
                Loc::Global { name, .. } => self.asm.store_name(name, v),
            },
        }
        Ok(())
    }

    // ---- statements ----

    fn statement(&mut self, s: &Statement<'_>) -> CResult<()> {
        let labels = std::mem::take(&mut self.pending_labels);
        match s {
            Statement::LabeledStatement(l) => {
                let mut labels = labels;
                labels.push(l.label.name.as_str().to_owned());
                if is_loop(&l.body) {
                    self.pending_labels = labels;
                    return self.statement(&l.body);
                }
                let end = self.asm.new_label();
                self.breakables.push(Breakable {
                    labels,
                    break_to: end,
                    continue_to: None,
                    unlabeled_break: false,
                    finally_depth: self.finallies.len(),
                    env_depth: self.env_depth(),
                });
                let r = self.statement(&l.body);
                self.breakables.pop();
                self.asm.bind(end);
                r
            }
            Statement::BlockStatement(b) => self.block(b),
            Statement::SwitchStatement(sw) => self.switch(sw, labels),
            Statement::EmptyStatement(_) | Statement::DebuggerStatement(_) => Ok(()),
            Statement::ExpressionStatement(e) => self.effect(&e.expression),
            Statement::VariableDeclaration(d) => self.declaration(d),
            Statement::FunctionDeclaration(f) if self.hoisted.contains(&f.span.start) => Ok(()),
            Statement::FunctionDeclaration(f) => {
                unsupported("function declarations in blocks (Annex B)", f.span)
            }
            Statement::IfStatement(i) => {
                let cond = self.expr_any(&i.test)?;
                let else_ = self.asm.new_label();
                self.asm.jump_if_false(cond.reg, else_);
                self.release(cond);
                self.statement(&i.consequent)?;
                match &i.alternate {
                    Some(alt) => {
                        let end = self.asm.new_label();
                        self.asm.jump(end);
                        self.asm.bind(else_);
                        self.statement(alt)?;
                        self.asm.bind(end);
                    }
                    None => self.asm.bind(else_),
                }
                Ok(())
            }
            Statement::WhileStatement(w) => {
                let top = self.asm.new_label();
                let (cont, done) = (self.asm.new_label(), self.asm.new_label());
                self.asm.bind(top);
                self.branch_if_false(&w.test, done)?;
                self.loop_body(labels, &w.body, done, cont)?;
                self.asm.bind(cont);
                self.asm.jump_loop(top);
                self.asm.bind(done);
                Ok(())
            }
            Statement::DoWhileStatement(d) => {
                let top = self.asm.new_label();
                let (cont, done) = (self.asm.new_label(), self.asm.new_label());
                self.asm.bind(top);
                self.loop_body(labels, &d.body, done, cont)?;
                self.asm.bind(cont);
                self.branch_if_false(&d.test, done)?;
                self.asm.jump_loop(top);
                self.asm.bind(done);
                Ok(())
            }
            Statement::ForStatement(f) => self.for_statement(f, labels),
            Statement::BreakStatement(b) => {
                let target = self.find_target(b.label.as_ref(), false, b.span)?;
                self.jump_to(target, false)
            }
            Statement::ContinueStatement(c) => {
                let target = self.find_target(c.label.as_ref(), true, c.span)?;
                self.jump_to(target, true)
            }
            Statement::ReturnStatement(r) => match &r.argument {
                Some(e) => self.return_expression(e),
                None => {
                    if self.finallies.is_empty() {
                        let t = self.temp()?;
                        self.asm.load_undefined(t);
                        self.asm.ret(t);
                        self.free(t);
                    } else {
                        let value = self.finallies.last().expect("checked").value;
                        self.asm.load_undefined(value);
                        self.return_value(value);
                    }
                    Ok(())
                }
            },
            Statement::TryStatement(t) => self.try_statement(t),
            Statement::ThrowStatement(t) => {
                let v = self.expr_any(&t.argument)?;
                self.asm.throw(v.reg);
                self.release(v);
                Ok(())
            }
            other => unsupported(statement_name(other), other.span()),
        }
    }

    fn statements(&mut self, list: &[Statement<'_>]) -> CResult<()> {
        for s in list {
            self.statement(s)?;
        }
        Ok(())
    }

    /// `return e`: a proper tail call when `e` is a call in tail position
    /// of strict code (D-021), else evaluate and return (through any open
    /// `finally`).
    fn return_expression(&mut self, e: &Expression<'_>) -> CResult<()> {
        if self.strict() && self.finallies.is_empty() && self.no_tail == 0 {
            return self.tail_return(e);
        }
        if self.finallies.is_empty() {
            let v = self.expr_any(e)?;
            self.asm.ret(v.reg);
            self.release(v);
        } else {
            let value = self.finallies.last().expect("checked").value;
            self.expr_to(e, value)?;
            self.return_value(value);
        }
        Ok(())
    }

    /// `return e` where `e` is in tail position: calls in tail position
    /// within it (through `?:`, `&&`, `||`, `??` and `,`) become tail calls.
    fn tail_return(&mut self, e: &Expression<'_>) -> CResult<()> {
        match e {
            Expression::ParenthesizedExpression(p) => self.tail_return(&p.expression),
            Expression::CallExpression(c) => {
                let call = self.call_operands(c)?;
                self.asm
                    .tail_call(call.callee, call.this, call.args, call.argc);
                self.free_call(call);
                Ok(())
            }
            Expression::ConditionalExpression(c) => {
                let else_ = self.asm.new_label();
                self.branch_if_false(&c.test, else_)?;
                self.tail_return(&c.consequent)?;
                self.asm.bind(else_);
                self.tail_return(&c.alternate)
            }
            Expression::LogicalExpression(l) => {
                let v = self.temp()?;
                self.expr_to(&l.left, v)?;
                let right = self.asm.new_label();
                match l.operator {
                    LogicalOperator::And => self.asm.jump_if_true(v, right),
                    LogicalOperator::Or => self.asm.jump_if_false(v, right),
                    LogicalOperator::Coalesce => {
                        let done = self.asm.new_label();
                        self.asm.jump_if_not_nullish(v, done);
                        self.asm.jump(right);
                        self.asm.bind(done);
                    }
                }
                self.asm.ret(v);
                self.free(v);
                self.asm.bind(right);
                self.tail_return(&l.right)
            }
            Expression::SequenceExpression(s) => {
                let (last, rest) = s.expressions.split_last().expect("at least two");
                for e in rest {
                    self.effect(e)?;
                }
                self.tail_return(last)
            }
            _ => {
                let v = self.expr_any(e)?;
                self.asm.ret(v.reg);
                self.release(v);
                Ok(())
            }
        }
    }

    fn for_statement(&mut self, f: &ForStatement<'_>, labels: Vec<String>) -> CResult<()> {
        let scope = f.scope_id.get().expect("semantic analysis ran");
        let pushed = self.enter_scope(scope)?;
        match &f.init {
            None => {}
            Some(ForStatementInit::VariableDeclaration(d)) => {
                self.mark_uninitialized(d);
                self.declaration(d)?
            }
            Some(init) => self.effect(init.to_expression())?,
        }
        // Closures in the body capture this iteration's copy of the loop
        // variables (CreatePerIterationEnvironment).
        if pushed {
            self.asm.copy_env();
        }
        let top = self.asm.new_label();
        let (cont, exit, done) = (
            self.asm.new_label(),
            self.asm.new_label(),
            self.asm.new_label(),
        );
        self.asm.bind(top);
        if let Some(test) = &f.test {
            self.branch_if_false(test, exit)?;
        }
        self.loop_body(labels, &f.body, exit, cont)?;
        self.asm.bind(cont);
        if pushed {
            self.asm.copy_env();
        }
        if let Some(update) = &f.update {
            self.effect(update)?;
        }
        self.asm.jump_loop(top);
        self.asm.bind(exit);
        self.leave_scope(pushed);
        self.asm.bind(done);
        Ok(())
    }

    fn switch(&mut self, sw: &SwitchStatement<'_>, labels: Vec<String>) -> CResult<()> {
        // The discriminant is evaluated before the case block's scope
        // exists, and kept in a temporary the cases can't assign.
        let d = self.temp()?;
        self.expr_to(&sw.discriminant, d)?;
        // A case can jump past a declaration, so textual order says nothing
        // about the TDZ here: every lexical binding of the block is checked.
        let scope = sw.scope_id.get().expect("semantic analysis ran");
        for sym in self.scoping.iter_bindings_in(scope) {
            if is_lexical(self.scoping.symbol_flags(sym)) {
                self.needs_tdz.insert(sym);
            }
        }
        let pushed = self.enter_scope(scope)?;
        self.enter_lexical(sw.cases.iter().flat_map(|c| c.consequent.iter()));

        let bodies: Vec<AsmLabel> = sw.cases.iter().map(|_| self.asm.new_label()).collect();
        let end = self.asm.new_label();
        // Case tests run in source order, skipping `default`; the first
        // strictly equal one picks the entry point.
        let t = self.temp()?;
        for (case, &body) in sw.cases.iter().zip(&bodies) {
            if let Some(test) = &case.test {
                self.expr_to(test, t)?;
                self.asm.strict_equal(t, d, t);
                self.asm.jump_if_true(t, body);
            }
        }
        self.free(t);
        let default = sw.cases.iter().position(|c| c.test.is_none());
        self.asm.jump(default.map_or(end, |i| bodies[i]));

        self.breakables.push(Breakable {
            labels,
            break_to: end,
            continue_to: None,
            unlabeled_break: true,
            finally_depth: self.finallies.len(),
            env_depth: self.env_depth(),
        });
        let mut result = Ok(());
        for (case, &body) in sw.cases.iter().zip(&bodies) {
            self.asm.bind(body);
            result = self.statements(&case.consequent);
            if result.is_err() {
                break;
            }
        }
        self.breakables.pop();
        result?;
        self.asm.bind(end);
        self.leave_scope(pushed);
        self.free(d);
        Ok(())
    }

    fn loop_body(
        &mut self,
        labels: Vec<String>,
        body: &Statement<'_>,
        break_to: AsmLabel,
        continue_to: AsmLabel,
    ) -> CResult<()> {
        self.breakables.push(Breakable {
            labels,
            break_to,
            continue_to: Some(continue_to),
            unlabeled_break: true,
            finally_depth: self.finallies.len(),
            env_depth: self.env_depth(),
        });
        let r = self.statement(body);
        self.breakables.pop();
        r
    }

    fn find_target(
        &self,
        label: Option<&LabelIdentifier<'_>>,
        is_continue: bool,
        span: oxc_span::Span,
    ) -> CResult<usize> {
        for (i, b) in self.breakables.iter().enumerate().rev() {
            let matches = match label {
                Some(l) => b.labels.iter().any(|n| n == l.name.as_str()),
                // Unlabeled `continue` targets the innermost loop, unlabeled
                // `break` the innermost loop or `switch`.
                None if is_continue => b.continue_to.is_some(),
                None => b.unlabeled_break,
            };
            if matches {
                if is_continue && b.continue_to.is_none() {
                    // Early errors rule this out.
                    return unsupported("continue to a non-loop label", span);
                }
                return Ok(i);
            }
        }
        unsupported("break or continue outside the compiled function", span)
    }

    /// Jumps to breakable `index` (its continue or break label), leaving
    /// the environments entered since and running the `finally` blocks in
    /// between first.
    fn jump_to(&mut self, index: usize, is_continue: bool) -> CResult<()> {
        let b = &self.breakables[index];
        let label = if is_continue {
            b.continue_to.expect("find_target checked")
        } else {
            b.break_to
        };
        if b.finally_depth == self.finallies.len() {
            let pops = self.env_depth() - b.env_depth;
            self.pop_envs(pops);
            self.asm.jump(label);
            return Ok(());
        }
        let depth = self.env_depth();
        let f = self.finallies.last_mut().expect("deeper than the target");
        let i = match f.jumps.iter().position(|&j| j == (index, is_continue)) {
            Some(i) => i,
            None => {
                f.jumps.push((index, is_continue));
                f.jumps.len() - 1
            }
        };
        let (kind, fin, f_depth) = (f.kind, f.fin, f.env_depth);
        let k = K_JUMPS
            .checked_add(i32::try_from(i).unwrap_or(i32::MAX))
            .ok_or_else(|| CompileError::TooLarge("jumps out of finally".into()))?;
        self.pop_envs(depth - f_depth);
        self.asm.load_int(kind, Imm(k));
        self.asm.jump(fin);
        Ok(())
    }

    /// Returns the value in register `value`, which must be the innermost
    /// open finally's value register if there is one.
    fn return_value(&mut self, value: Reg) {
        let depth = self.env_depth();
        match self.finallies.last_mut() {
            None => self.asm.ret(value),
            Some(f) => {
                debug_assert_eq!(f.value, value);
                f.returns = true;
                let (kind, fin, f_depth) = (f.kind, f.fin, f.env_depth);
                self.pop_envs(depth - f_depth);
                self.asm.load_int(kind, Imm(K_RETURN));
                self.asm.jump(fin);
            }
        }
    }

    /// When this function has environments: a register holding the
    /// current one, for handlers to restore.
    fn save_env(&mut self) -> CResult<Option<Reg>> {
        if !self.uses_envs {
            return Ok(None);
        }
        let r = self.temp()?;
        self.asm.get_env(r);
        Ok(Some(r))
    }

    fn try_statement(&mut self, t: &TryStatement<'_>) -> CResult<()> {
        let Some(finalizer) = &t.finalizer else {
            let handler = t.handler.as_ref().expect("try has catch or finally");
            return self.try_catch(&t.block, handler);
        };
        let saved = self.save_env()?;
        let kind = self.temp()?;
        let value = self.temp()?;
        let (start, end, handler, fin) = (
            self.asm.new_label(),
            self.asm.new_label(),
            self.asm.new_label(),
            self.asm.new_label(),
        );
        self.finallies.push(Finally {
            fin,
            kind,
            value,
            returns: false,
            jumps: Vec::new(),
            env_depth: self.env_depth(),
        });
        self.asm.bind(start);
        self.no_tail += 1;
        let body = match &t.handler {
            Some(c) => self.try_catch(&t.block, c),
            None => self.block(&t.block),
        };
        self.no_tail -= 1;
        let f = self.finallies.pop().expect("pushed above");
        body?;
        self.asm.bind(end);
        self.asm.load_int(kind, Imm(K_NORMAL));
        self.asm.jump(fin);
        // A throw anywhere in the try block or the catch block lands here.
        self.handlers.push((start, end, handler, value));
        self.asm.bind(handler);
        if let Some(saved) = saved {
            self.asm.set_env(saved);
        }
        self.asm.load_int(kind, Imm(K_THROW));
        self.asm.bind(fin);
        self.block(finalizer)?;

        // Finish whatever completion brought us here. The routes run with
        // this finally closed, so they go through the enclosing ones.
        let t = self.temp()?;
        let dispatch = |g: &mut Self, k: i32, body: &mut dyn FnMut(&mut Self) -> CResult<()>| {
            let skip = g.asm.new_label();
            g.asm.load_int(t, Imm(k));
            g.asm.strict_equal(t, kind, t);
            g.asm.jump_if_false(t, skip);
            body(g)?;
            g.asm.bind(skip);
            Ok::<(), CompileError>(())
        };
        dispatch(self, K_THROW, &mut |g| {
            g.asm.throw(value);
            Ok(())
        })?;
        if f.returns {
            dispatch(self, K_RETURN, &mut |g| {
                match g.finallies.last().map(|o| o.value) {
                    Some(outer) => {
                        g.asm.mov(outer, value);
                        g.return_value(outer);
                    }
                    None => g.asm.ret(value),
                }
                Ok(())
            })?;
        }
        for (i, &(index, is_continue)) in f.jumps.iter().enumerate() {
            dispatch(self, K_JUMPS + i as i32, &mut |g| {
                g.jump_to(index, is_continue)
            })?;
        }
        self.free(t);
        self.free(value);
        self.free(kind);
        if let Some(saved) = saved {
            self.free(saved);
        }
        Ok(())
    }

    fn try_catch(&mut self, block: &BlockStatement<'_>, c: &CatchClause<'_>) -> CResult<()> {
        let catch_scope = c.scope_id.get().expect("semantic analysis ran");
        // The exception lands in a temporary; the parameter is bound once
        // the catch scope (and maybe its environment) exists.
        let exception = self.temp()?;
        let param = match &c.param {
            None => None,
            Some(p) => {
                let BindingPattern::BindingIdentifier(id) = &p.pattern else {
                    return unsupported("destructuring catch parameters", p.span);
                };
                Some((symbol_of(id), id.name.as_str().to_owned()))
            }
        };
        let saved = self.save_env()?;
        let (start, end, handler, done) = (
            self.asm.new_label(),
            self.asm.new_label(),
            self.asm.new_label(),
            self.asm.new_label(),
        );
        self.asm.bind(start);
        self.no_tail += 1;
        let r = self.block(block);
        self.no_tail -= 1;
        r?;
        self.asm.bind(end);
        self.asm.jump(done);
        self.handlers.push((start, end, handler, exception));
        self.asm.bind(handler);
        if let Some(saved) = saved {
            self.asm.set_env(saved);
        }
        // oxc binds the parameter in the catch body's block scope.
        let body_scope = c.body.scope_id.get().expect("semantic analysis ran");
        let pushed_clause = self.enter_scope(catch_scope)?;
        let pushed_body = self.enter_scope(body_scope)?;
        self.enter_lexical(c.body.body.iter());
        let mut name = None;
        if let Some((sym, n)) = param {
            let home = self.scoping.symbol_scope_id(sym);
            if home != body_scope && home != catch_scope {
                return unsupported("a catch parameter in an unexpected scope", c.span);
            }
            self.init_binding(sym, home, exception);
            name = Some(n);
        }
        let named = name.is_some();
        if let Some(n) = name {
            self.catch_names.push(n);
        }
        let r = self.statements(&c.body.body);
        if named {
            self.catch_names.pop();
        }
        r?;
        self.leave_scope(pushed_body);
        self.leave_scope(pushed_clause);
        self.asm.bind(done);
        if let Some(saved) = saved {
            self.free(saved);
        }
        self.free(exception);
        Ok(())
    }

    fn block(&mut self, b: &BlockStatement<'_>) -> CResult<()> {
        let scope = b.scope_id.get().expect("semantic analysis ran");
        let pushed = self.enter_scope(scope)?;
        self.enter_lexical(b.body.iter());
        self.statements(&b.body)?;
        self.leave_scope(pushed);
        Ok(())
    }

    fn declaration(&mut self, d: &VariableDeclaration<'_>) -> CResult<()> {
        if !matches!(
            d.kind,
            VariableDeclarationKind::Var
                | VariableDeclarationKind::Let
                | VariableDeclarationKind::Const
        ) {
            return unsupported("using declarations", d.span);
        }
        for decl in &d.declarations {
            let BindingPattern::BindingIdentifier(id) = &decl.id else {
                return unsupported("destructuring", decl.span);
            };
            if d.kind == VariableDeclarationKind::Var
                && self.catch_names.iter().any(|n| n == id.name.as_str())
            {
                // Annex B.3.4: the initializer assigns the catch parameter.
                return unsupported("`var` redeclaring a catch parameter", id.span);
            }
            let sym = symbol_of(id);
            let home = self.scoping.symbol_scope_id(sym);
            let loc = self.locate(sym, home, id.span)?;
            match (&decl.init, loc) {
                (Some(init), Loc::Reg(r)) => self.assign_local(r, sym, init)?,
                (Some(init), Loc::Env(slot)) => {
                    let t = self.temp()?;
                    self.expr_to(init, t)?;
                    self.asm.store_env(slot, t);
                    self.free(t);
                }
                // `let x;` sets undefined each time it runs (loops!); `var x;`
                // does nothing.
                (None, _) if d.kind == VariableDeclarationKind::Var => {}
                (None, Loc::Reg(r)) => self.asm.load_undefined(r),
                (None, Loc::Env(slot)) => {
                    let t = self.temp()?;
                    self.asm.load_undefined(t);
                    self.asm.store_env(slot, t);
                    self.free(t);
                }
                (Some(init), Loc::Global { name, lexical }) => {
                    let t = self.temp()?;
                    self.expr_to(init, t)?;
                    if lexical {
                        self.asm.init_global(name, t);
                    } else if self.strict() {
                        self.asm.store_name_strict(name, t);
                    } else {
                        self.asm.store_name(name, t);
                    }
                    self.free(t);
                }
                (None, Loc::Global { name, .. }) => {
                    let t = self.temp()?;
                    self.asm.load_undefined(t);
                    self.asm.init_global(name, t);
                    self.free(t);
                }
            }
            if d.kind != VariableDeclarationKind::Var {
                self.initialized.insert(sym);
            }
        }
        Ok(())
    }

    /// Evaluates `e` into the local `r` of `sym`. Writes directly when `e`
    /// can't read `r` before its last write; otherwise through a temporary.
    fn assign_local(&mut self, r: Reg, sym: SymbolId, e: &Expression<'_>) -> CResult<()> {
        if self.mentions(e, sym) || !writes_last(e) {
            let t = self.temp()?;
            self.expr_to(e, t)?;
            self.asm.mov(r, t);
            self.free(t);
            Ok(())
        } else {
            self.expr_to(e, r)
        }
    }

    /// Conservatively: does `e` mention the local `sym` anywhere?
    fn mentions(&self, e: &Expression<'_>, sym: SymbolId) -> bool {
        let mut found = false;
        visit_identifiers(e, &mut |id| {
            if self.resolve(id) == Some(sym) {
                found = true;
            }
        });
        found
    }

    // ---- expressions ----

    /// Evaluates `e` for its effects only.
    fn effect(&mut self, e: &Expression<'_>) -> CResult<()> {
        match e {
            Expression::ParenthesizedExpression(p) => self.effect(&p.expression),
            Expression::UpdateExpression(u) => self.update(u, None),
            Expression::SequenceExpression(s) => {
                for e in &s.expressions {
                    self.effect(e)?;
                }
                Ok(())
            }
            _ => {
                let v = self.expr_any(e)?;
                self.release(v);
                Ok(())
            }
        }
    }

    /// Evaluates `e` into whatever register is cheapest: a local's own
    /// register for a plain read, else a fresh temporary. Release it with
    /// [`Gen::release`] in stack order.
    fn expr_any(&mut self, e: &Expression<'_>) -> CResult<Operand> {
        if let Expression::ParenthesizedExpression(p) = e {
            return self.expr_any(&p.expression);
        }
        if let Expression::Identifier(id) = e {
            if let Some(o) = self.read_binding(id)? {
                return Ok(o);
            }
        }
        let t = self.temp()?;
        self.expr_to(e, t)?;
        Ok(Operand { reg: t, temp: true })
    }

    fn release(&mut self, o: Operand) {
        if o.temp {
            self.free(o.reg);
        }
    }

    /// Evaluates `e` into `dst` (see the module docs for the contract).
    fn expr_to(&mut self, e: &Expression<'_>, dst: Reg) -> CResult<()> {
        match e {
            Expression::ParenthesizedExpression(p) => self.expr_to(&p.expression, dst),
            Expression::NumericLiteral(n) => self.load_number(dst, n.value),
            Expression::BooleanLiteral(b) => {
                if b.value {
                    self.asm.load_true(dst);
                } else {
                    self.asm.load_false(dst);
                }
                Ok(())
            }
            Expression::NullLiteral(_) => {
                self.asm.load_null(dst);
                Ok(())
            }
            Expression::StringLiteral(lit) => {
                let Some(text) = lit.value.as_str() else {
                    return unsupported("strings with lone surrogates", lit.span);
                };
                let c = self.string_const(text)?;
                self.asm.load_const(dst, c);
                Ok(())
            }
            Expression::TemplateLiteral(t) => self.template(t, dst),
            Expression::Identifier(id) => {
                let Some(sym) = self.resolve_use(id)? else {
                    // Unresolved: a global. These three are non-writable,
                    // non-configurable properties of the global object, and
                    // nothing in this subset can shadow them dynamically
                    // (`with` and direct `eval` are unsupported).
                    return match id.name.as_str() {
                        "undefined" => {
                            self.asm.load_undefined(dst);
                            Ok(())
                        }
                        "NaN" => self.load_number(dst, f64::NAN),
                        "Infinity" => self.load_number(dst, f64::INFINITY),
                        name => {
                            let c = self.string_const(name)?;
                            self.asm.load_name(dst, c);
                            Ok(())
                        }
                    };
                };
                let loc = self.locate(sym, self.reference_scope(id), id.span)?;
                let check = self.binding_kind(sym, loc, id.span)?;
                match loc {
                    Loc::Reg(r) => {
                        if check {
                            self.asm.check_initialized(r);
                        }
                        if r != dst {
                            self.asm.mov(dst, r);
                        }
                    }
                    Loc::Env(slot) => {
                        self.asm.load_env(dst, slot);
                        if check {
                            self.asm.check_initialized(dst);
                        }
                    }
                    Loc::Global { name, .. } => self.asm.load_name(dst, name),
                }
                Ok(())
            }
            Expression::UnaryExpression(u) => self.unary(u, dst),
            Expression::BinaryExpression(b) => self.binary(b, dst),
            Expression::LogicalExpression(l) => {
                self.expr_to(&l.left, dst)?;
                let end = self.asm.new_label();
                match l.operator {
                    LogicalOperator::And => self.asm.jump_if_false(dst, end),
                    LogicalOperator::Or => self.asm.jump_if_true(dst, end),
                    LogicalOperator::Coalesce => self.asm.jump_if_not_nullish(dst, end),
                }
                self.expr_to(&l.right, dst)?;
                self.asm.bind(end);
                Ok(())
            }
            Expression::ConditionalExpression(c) => {
                let (else_, end) = (self.asm.new_label(), self.asm.new_label());
                self.branch_if_false(&c.test, else_)?;
                self.expr_to(&c.consequent, dst)?;
                self.asm.jump(end);
                self.asm.bind(else_);
                self.expr_to(&c.alternate, dst)?;
                self.asm.bind(end);
                Ok(())
            }
            Expression::SequenceExpression(s) => {
                let (last, rest) = s.expressions.split_last().expect("at least two");
                for e in rest {
                    self.effect(e)?;
                }
                self.expr_to(last, dst)
            }
            Expression::AssignmentExpression(a) => self.assignment(a, dst),
            Expression::UpdateExpression(u) => self.update(u, Some(dst)),
            Expression::FunctionExpression(f) => {
                let f = FnLike::function(f)?;
                let func = self.nested(&f)?;
                self.asm.create_closure(dst, func);
                Ok(())
            }
            Expression::ArrowFunctionExpression(f) => {
                let f = FnLike::arrow(f)?;
                let func = self.nested(&f)?;
                self.asm.create_closure(dst, func);
                Ok(())
            }
            Expression::ThisExpression(t) => {
                if !self.this_ok {
                    return unsupported("`this` outside strict functions", t.span);
                }
                self.asm.load_this(dst);
                Ok(())
            }
            Expression::CallExpression(c) => {
                let call = self.call_operands(c)?;
                self.asm
                    .call(dst, call.callee, call.this, call.args, call.argc);
                self.free_call(call);
                Ok(())
            }
            Expression::ObjectExpression(o) => self.object_literal(o, dst),
            Expression::StaticMemberExpression(_)
            | Expression::ComputedMemberExpression(_)
            | Expression::PrivateFieldExpression(_) => {
                let m = e.as_member_expression().expect("a member expression");
                let r = self.prop_ref(m, false)?;
                self.get_prop(dst, &r)?;
                self.release_ref(r);
                Ok(())
            }
            other => unsupported(expression_name(other), other.span()),
        }
    }

    /// Evaluates a call's callee, `this` (undefined) and arguments into
    /// consecutive temporaries.
    fn call_operands(&mut self, c: &CallExpression<'_>) -> CResult<CallOperands> {
        if c.optional {
            return unsupported("optional calls", c.span);
        }
        if matches!(c.callee, Expression::Super(_)) {
            return unsupported("super calls", c.span);
        }
        let callee = self.temp()?;
        let this = self.temp()?;
        if let Some(m) = c.callee.without_parentheses().as_member_expression() {
            // A method call: `this` is the object the method was read from.
            self.check_member(m)?;
            self.expr_to(m.object(), this)?;
            let key = self.member_key(m, false)?;
            let r = PropRef {
                obj: Operand {
                    reg: this,
                    temp: false,
                },
                key,
            };
            self.get_prop(callee, &r)?;
            self.release_ref(r);
        } else {
            self.expr_to(&c.callee, callee)?;
            self.asm.load_undefined(this);
        }
        let mut args = Vec::with_capacity(c.arguments.len());
        for a in &c.arguments {
            let Some(e) = a.as_expression() else {
                return unsupported("spread arguments", a.span());
            };
            let r = self.temp()?;
            self.expr_to(e, r)?;
            args.push(r);
        }
        let argc =
            u32::try_from(args.len()).map_err(|_| CompileError::TooLarge("arguments".into()))?;
        Ok(CallOperands {
            callee,
            this,
            // With no arguments any in-range register will do.
            args: args.first().copied().unwrap_or(this),
            argc: Count(argc),
            temps: args,
        })
    }

    fn free_call(&mut self, call: CallOperands) {
        for &r in call.temps.iter().rev() {
            self.free(r);
        }
        self.free(call.this);
        self.free(call.callee);
    }

    /// Jumps to `target` if ToBoolean(`test`) is false.
    fn branch_if_false(&mut self, test: &Expression<'_>, target: AsmLabel) -> CResult<()> {
        let c = self.expr_any(test)?;
        self.asm.jump_if_false(c.reg, target);
        self.release(c);
        Ok(())
    }

    fn unary(&mut self, u: &UnaryExpression<'_>, dst: Reg) -> CResult<()> {
        let op = match u.operator {
            UnaryOperator::Void => {
                self.effect(&u.argument)?;
                self.asm.load_undefined(dst);
                return Ok(());
            }
            UnaryOperator::Typeof => return self.type_of(&u.argument, dst),
            UnaryOperator::Delete => return self.delete(&u.argument, dst),
            op => op,
        };
        let a = self.expr_any(&u.argument)?;
        match op {
            UnaryOperator::LogicalNot => self.asm.not(dst, a.reg),
            UnaryOperator::UnaryPlus => {
                let ic = self.ic()?;
                self.asm.to_number(dst, a.reg, ic);
            }
            UnaryOperator::UnaryNegation => {
                let ic = self.ic()?;
                self.asm.negate(dst, a.reg, ic);
            }
            UnaryOperator::BitwiseNot => {
                let ic = self.ic()?;
                self.asm.bit_not(dst, a.reg, ic);
            }
            UnaryOperator::Void | UnaryOperator::Typeof | UnaryOperator::Delete => {
                unreachable!("handled above")
            }
        }
        self.release(a);
        Ok(())
    }

    /// `typeof e`: a global name gets `TypeofName`, which gives
    /// `"undefined"` for an undeclared name instead of throwing.
    fn type_of(&mut self, e: &Expression<'_>, dst: Reg) -> CResult<()> {
        if let Expression::Identifier(id) = e.without_parentheses() {
            let global = match self.resolve_use(id)? {
                None => match id.name.as_str() {
                    // Always defined (see `expr_to`).
                    "undefined" | "NaN" | "Infinity" => None,
                    name => Some(self.string_const(name)?),
                },
                Some(sym) => match self.locate(sym, self.reference_scope(id), id.span)? {
                    Loc::Global { name, .. } => Some(name),
                    _ => None,
                },
            };
            if let Some(name) = global {
                self.asm.type_of_name(dst, name);
                return Ok(());
            }
        }
        let a = self.expr_any(e)?;
        self.asm.type_of(dst, a.reg);
        self.release(a);
        Ok(())
    }

    /// A template literal: the cooked strings and the ToString of each
    /// substitution, concatenated left to right. Each substitution is
    /// converted before the next one is evaluated.
    fn template(&mut self, t: &TemplateLiteral<'_>, dst: Reg) -> CResult<()> {
        let mut cooked = Vec::with_capacity(t.quasis.len());
        for q in &t.quasis {
            // Untagged templates with bad escapes are early errors, so
            // `cooked` is always there.
            match q.value.cooked.and_then(|c| c.as_str()) {
                Some(text) => cooked.push(text),
                None => return unsupported("strings with lone surrogates", q.span),
            }
        }
        let (head, spans) = cooked.split_first().expect("a template has a head");
        let mut started = !head.is_empty();
        if started || t.expressions.is_empty() {
            let c = self.string_const(head)?;
            self.asm.load_const(dst, c);
            started = true;
        }
        for (e, text) in t.expressions.iter().zip(spans) {
            if started {
                let v = self.temp()?;
                self.expr_to(e, v)?;
                self.asm.to_string(v, v);
                let ic = self.ic()?;
                self.asm.add(dst, dst, v, ic);
                self.free(v);
            } else {
                self.expr_to(e, dst)?;
                self.asm.to_string(dst, dst);
                started = true;
            }
            if !text.is_empty() {
                let v = self.temp()?;
                let c = self.string_const(text)?;
                self.asm.load_const(v, c);
                let ic = self.ic()?;
                self.asm.add(dst, dst, v, ic);
                self.free(v);
            }
        }
        Ok(())
    }

    fn binary(&mut self, b: &BinaryExpression<'_>, dst: Reg) -> CResult<()> {
        if b.operator == BinaryOperator::Instanceof {
            return unsupported("`instanceof` (needs the built-ins)", b.span);
        }
        // The left operand is read after the right one is evaluated, so it
        // must not sit in a local the right side can assign.
        let lhs = if has_side_effects(&b.right) {
            let t = self.temp()?;
            self.expr_to(&b.left, t)?;
            Operand { reg: t, temp: true }
        } else {
            self.expr_any(&b.left)?
        };
        let rhs = self.expr_any(&b.right)?;
        self.binary_op(b.operator, dst, lhs.reg, rhs.reg)?;
        self.release(rhs);
        self.release(lhs);
        Ok(())
    }

    fn binary_op(&mut self, op: BinaryOperator, dst: Reg, l: Reg, r: Reg) -> CResult<()> {
        use BinaryOperator as B;
        match op {
            B::StrictEquality => self.asm.strict_equal(dst, l, r),
            B::StrictInequality => {
                self.asm.strict_equal(dst, l, r);
                self.asm.not(dst, dst);
            }
            B::Equality => self.asm.equal(dst, l, r),
            B::Inequality => {
                self.asm.equal(dst, l, r);
                self.asm.not(dst, dst);
            }
            _ => {
                let ic = self.ic()?;
                match op {
                    B::Addition => self.asm.add(dst, l, r, ic),
                    B::Subtraction => self.asm.sub(dst, l, r, ic),
                    B::Multiplication => self.asm.mul(dst, l, r, ic),
                    B::Division => self.asm.div(dst, l, r, ic),
                    B::Remainder => self.asm.rem(dst, l, r, ic),
                    B::Exponential => self.asm.exp(dst, l, r, ic),
                    B::ShiftLeft => self.asm.shl(dst, l, r, ic),
                    B::ShiftRight => self.asm.shr(dst, l, r, ic),
                    B::ShiftRightZeroFill => self.asm.ushr(dst, l, r, ic),
                    B::BitwiseOR => self.asm.bit_or(dst, l, r, ic),
                    B::BitwiseXOR => self.asm.bit_xor(dst, l, r, ic),
                    B::BitwiseAnd => self.asm.bit_and(dst, l, r, ic),
                    B::LessThan => self.asm.less_than(dst, l, r, ic),
                    B::GreaterThan => self.asm.greater_than(dst, l, r, ic),
                    B::LessEqualThan => self.asm.less_equal(dst, l, r, ic),
                    B::GreaterEqualThan => self.asm.greater_equal(dst, l, r, ic),
                    B::In => self.asm.has_property(dst, l, r, ic),
                    B::StrictEquality
                    | B::StrictInequality
                    | B::Equality
                    | B::Inequality
                    | B::Instanceof => unreachable!("handled above"),
                }
            }
        }
        Ok(())
    }

    fn assignment(&mut self, a: &AssignmentExpression<'_>, dst: Reg) -> CResult<()> {
        if let Some(call) = a
            .left
            .as_simple_assignment_target()
            .and_then(|t| self.call_target(t))
        {
            return self.assign_to_call(call);
        }
        if let Some(m) = a.left.as_member_expression() {
            return self.assign_property(a, m, dst);
        }
        let AssignmentTarget::AssignmentTargetIdentifier(id) = &a.left else {
            return unsupported("assignment to a pattern", a.left.span());
        };
        let t = self.write_target(id)?;
        match a.operator {
            AssignmentOperator::Assign => {
                if let Some(r) = t.simple_reg() {
                    self.assign_local(r, t.sym.expect("register targets are bindings"), &a.right)?;
                    if r != dst {
                        self.asm.mov(dst, r);
                    }
                } else {
                    // SetMutableBinding runs after the right side.
                    self.expr_to(&a.right, dst)?;
                    self.store_reg(t, dst)?;
                }
            }
            op if op.is_logical() => {
                let cur = self.read_target(t)?;
                self.asm.mov(dst, cur.reg);
                self.release(cur);
                let end = self.asm.new_label();
                match op {
                    AssignmentOperator::LogicalAnd => self.asm.jump_if_false(dst, end),
                    AssignmentOperator::LogicalOr => self.asm.jump_if_true(dst, end),
                    _ => self.asm.jump_if_not_nullish(dst, end),
                }
                self.expr_to(&a.right, dst)?;
                self.store_reg(t, dst)?;
                self.asm.bind(end);
            }
            op => {
                // `x op= e`: the old x is read before e runs.
                let bop = op.to_binary_operator().expect("arithmetic assignment");
                let cur = self.read_target(t)?;
                let old = if !cur.temp && has_side_effects(&a.right) {
                    let r = self.temp()?;
                    self.asm.mov(r, cur.reg);
                    Operand { reg: r, temp: true }
                } else {
                    cur
                };
                let rhs = self.expr_any(&a.right)?;
                match t.simple_reg() {
                    Some(r) => {
                        self.binary_op(bop, r, old.reg, rhs.reg)?;
                        self.release(rhs);
                        self.release(old);
                        if r != dst {
                            self.asm.mov(dst, r);
                        }
                    }
                    None => {
                        let v = self.temp()?;
                        self.binary_op(bop, v, old.reg, rhs.reg)?;
                        self.store_reg(t, v)?;
                        self.asm.mov(dst, v);
                        self.free(v);
                        self.release(rhs);
                        self.release(old);
                    }
                }
            }
        }
        Ok(())
    }

    /// The call, if `t` is a call that sloppy code assigns to (D-045).
    fn call_target<'b, 'a>(&self, t: &'b SimpleAssignmentTarget<'a>) -> Option<&'b Expression<'a>> {
        crate::call_target::site_call(t, &self.an.call_targets)
    }

    /// `f() = e`, `f() += e`, `f()++`, … in sloppy code: the call runs,
    /// then a `ReferenceError` is thrown before anything else is evaluated
    /// (13.15.2 step 1.c, 13.4.2.1). The result register is never written:
    /// the throw ends the block.
    fn assign_to_call(&mut self, call: &Expression<'_>) -> CResult<()> {
        let r = self.expr_any(call)?;
        self.release(r);
        self.asm.throw_invalid_assignment();
        Ok(())
    }

    /// `x++`, `--x`, …: the result (the old value converted with ToNumeric,
    /// or the new value) goes to `dst` if there is one.
    fn update(&mut self, u: &UpdateExpression<'_>, dst: Option<Reg>) -> CResult<()> {
        if let Some(call) = self.call_target(&u.argument) {
            return self.assign_to_call(call);
        }
        if let Some(m) = u.argument.as_member_expression() {
            return self.update_property(u, m, dst);
        }
        let t = match &u.argument {
            SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => self.write_target(id)?,
            other => return unsupported("this update target", other.span()),
        };
        let cur = self.read_target(t)?;
        let ic = self.ic()?;
        let step = |g: &mut Self, d: Reg, s: Reg| match u.operator {
            UpdateOperator::Increment => g.asm.inc(d, s, ic),
            UpdateOperator::Decrement => g.asm.dec(d, s, ic),
        };
        match t.simple_reg() {
            Some(r) => {
                match dst {
                    Some(dst) if !u.prefix => {
                        let ic = self.ic()?;
                        self.asm.to_numeric(dst, r, ic);
                        step(self, r, dst);
                    }
                    _ => {
                        step(self, r, r);
                        if let Some(dst) = dst {
                            self.asm.mov(dst, r);
                        }
                    }
                }
                self.release(cur);
            }
            None => {
                let v = self.temp()?;
                match dst {
                    Some(dst) if !u.prefix => {
                        let ic = self.ic()?;
                        self.asm.to_numeric(dst, cur.reg, ic);
                        step(self, v, dst);
                        self.store_reg(t, v)?;
                    }
                    _ => {
                        step(self, v, cur.reg);
                        self.store_reg(t, v)?;
                        if let Some(dst) = dst {
                            self.asm.mov(dst, v);
                        }
                    }
                }
                self.free(v);
                self.release(cur);
            }
        }
        Ok(())
    }
}

/// A property reference: the object and the key (ARCHITECTURE.md §7).
#[derive(Clone, Copy)]
struct PropRef {
    obj: Operand,
    key: PropKey,
}

#[derive(Clone, Copy)]
enum PropKey {
    /// A static name: a string constant.
    Named(Const),
    /// A computed key in a register.
    Keyed(Operand),
}

impl Gen<'_> {
    // ---- properties (task 1.7e, D-052) ----

    /// Refuses the member expressions this compiler doesn't do.
    fn check_member(&self, m: &MemberExpression<'_>) -> CResult<()> {
        if m.optional() {
            return unsupported("optional chaining", m.span());
        }
        if matches!(m, MemberExpression::PrivateFieldExpression(_)) {
            return unsupported("private fields", m.span());
        }
        if matches!(m.object(), Expression::Super(_)) {
            return unsupported("super properties", m.span());
        }
        Ok(())
    }

    /// The key of `m`, evaluated after its object. `keep` if code that
    /// may assign locals runs before the key is last used: then a key read
    /// from a local is copied first.
    fn member_key(&mut self, m: &MemberExpression<'_>, keep: bool) -> CResult<PropKey> {
        Ok(match m {
            MemberExpression::StaticMemberExpression(s) => {
                PropKey::Named(self.string_const(s.property.name.as_str())?)
            }
            MemberExpression::ComputedMemberExpression(c) => {
                PropKey::Keyed(self.operand(&c.expression, keep)?)
            }
            MemberExpression::PrivateFieldExpression(p) => {
                return unsupported("private fields", p.span)
            }
        })
    }

    /// Evaluates `e` into a register; into a fresh temporary if `keep`
    /// (code that may assign locals runs before the value is used).
    fn operand(&mut self, e: &Expression<'_>, keep: bool) -> CResult<Operand> {
        if keep {
            let t = self.temp()?;
            self.expr_to(e, t)?;
            Ok(Operand { reg: t, temp: true })
        } else {
            self.expr_any(e)
        }
    }

    /// Evaluates the object and key of `m` (the base is read before the
    /// key is evaluated, so it is copied if the key may assign it). `keep`
    /// as for [`Gen::member_key`].
    fn prop_ref(&mut self, m: &MemberExpression<'_>, keep: bool) -> CResult<PropRef> {
        self.check_member(m)?;
        let key_effects = match m {
            MemberExpression::ComputedMemberExpression(c) => has_side_effects(&c.expression),
            _ => false,
        };
        let obj = self.operand(m.object(), keep || key_effects)?;
        let key = self.member_key(m, keep)?;
        Ok(PropRef { obj, key })
    }

    fn release_ref(&mut self, r: PropRef) {
        if let PropKey::Keyed(k) = r.key {
            self.release(k);
        }
        self.release(r.obj);
    }

    /// `dst = obj[key]`.
    fn get_prop(&mut self, dst: Reg, r: &PropRef) -> CResult<()> {
        let ic = self.ic()?;
        match r.key {
            PropKey::Named(c) => self.asm.get_named(dst, r.obj.reg, c, ic),
            PropKey::Keyed(k) => self.asm.get_keyed(dst, r.obj.reg, k.reg, ic),
        }
        Ok(())
    }

    /// `obj[key] = v` (PutValue: strict code throws on a refusal).
    fn set_prop(&mut self, r: &PropRef, v: Reg) -> CResult<()> {
        let ic = self.ic()?;
        let obj = r.obj.reg;
        match (r.key, self.strict()) {
            (PropKey::Named(c), false) => self.asm.set_named(obj, c, v, ic),
            (PropKey::Named(c), true) => self.asm.set_named_strict(obj, c, v, ic),
            (PropKey::Keyed(k), false) => self.asm.set_keyed(obj, k.reg, v, ic),
            (PropKey::Keyed(k), true) => self.asm.set_keyed_strict(obj, k.reg, v, ic),
        }
        Ok(())
    }

    /// `o.x = e`, `o[k] op= e`, `o.x ||= e`, …: the reference (object,
    /// then key) is evaluated first, then (for compound and logical
    /// forms) the old value is read, then `e` runs, then the store.
    fn assign_property(
        &mut self,
        a: &AssignmentExpression<'_>,
        m: &MemberExpression<'_>,
        dst: Reg,
    ) -> CResult<()> {
        let r = self.prop_ref(m, has_side_effects(&a.right))?;
        match a.operator {
            AssignmentOperator::Assign => {
                self.expr_to(&a.right, dst)?;
                self.set_prop(&r, dst)?;
            }
            op if op.is_logical() => {
                self.get_prop(dst, &r)?;
                let end = self.asm.new_label();
                match op {
                    AssignmentOperator::LogicalAnd => self.asm.jump_if_false(dst, end),
                    AssignmentOperator::LogicalOr => self.asm.jump_if_true(dst, end),
                    _ => self.asm.jump_if_not_nullish(dst, end),
                }
                self.expr_to(&a.right, dst)?;
                self.set_prop(&r, dst)?;
                self.asm.bind(end);
            }
            op => {
                let bop = op.to_binary_operator().expect("arithmetic assignment");
                // `dst` is never a local `e` can assign (see the module
                // docs), so the old value can wait there.
                self.get_prop(dst, &r)?;
                let rhs = self.expr_any(&a.right)?;
                self.binary_op(bop, dst, dst, rhs.reg)?;
                self.release(rhs);
                self.set_prop(&r, dst)?;
            }
        }
        self.release_ref(r);
        Ok(())
    }

    /// `o.x++`, `--o[k]`, …
    fn update_property(
        &mut self,
        u: &UpdateExpression<'_>,
        m: &MemberExpression<'_>,
        dst: Option<Reg>,
    ) -> CResult<()> {
        let r = self.prop_ref(m, false)?;
        let v = self.temp()?;
        self.get_prop(v, &r)?;
        let ic = self.ic()?;
        let step = |g: &mut Self, d: Reg, s: Reg| match u.operator {
            UpdateOperator::Increment => g.asm.inc(d, s, ic),
            UpdateOperator::Decrement => g.asm.dec(d, s, ic),
        };
        match dst {
            Some(dst) if !u.prefix => {
                let ic = self.ic()?;
                self.asm.to_numeric(dst, v, ic);
                step(self, v, dst);
                self.set_prop(&r, v)?;
            }
            _ => {
                step(self, v, v);
                self.set_prop(&r, v)?;
                if let Some(dst) = dst {
                    self.asm.mov(dst, v);
                }
            }
        }
        self.free(v);
        self.release_ref(r);
        Ok(())
    }

    /// `delete e`: a property reference is deleted; anything else is
    /// evaluated and gives `true`.
    fn delete(&mut self, e: &Expression<'_>, dst: Reg) -> CResult<()> {
        let inner = e.without_parentheses();
        if let Some(m) = inner.as_member_expression() {
            let r = self.prop_ref(m, false)?;
            let (key, temp) = match r.key {
                PropKey::Keyed(k) => (k.reg, false),
                PropKey::Named(c) => {
                    let t = self.temp()?;
                    self.asm.load_const(t, c);
                    (t, true)
                }
            };
            if self.strict() {
                self.asm.delete_strict(dst, r.obj.reg, key);
            } else {
                self.asm.delete(dst, r.obj.reg, key);
            }
            if temp {
                self.free(key);
            }
            self.release_ref(r);
            return Ok(());
        }
        match inner {
            Expression::Identifier(id) => unsupported("delete of a binding", id.span),
            Expression::ChainExpression(c) => unsupported("optional chaining", c.span),
            _ => {
                self.effect(e)?;
                self.asm.load_true(dst);
                Ok(())
            }
        }
    }

    /// An object literal: a new object, then each member defined in
    /// order (CreateDataPropertyOrThrow).
    fn object_literal(&mut self, o: &ObjectExpression<'_>, dst: Reg) -> CResult<()> {
        self.asm.new_object(dst);
        for p in &o.properties {
            let p = match p {
                ObjectPropertyKind::ObjectProperty(p) => p,
                ObjectPropertyKind::SpreadProperty(s) => {
                    return unsupported("spread in object literals", s.span)
                }
            };
            if p.kind != PropertyKind::Init {
                return unsupported("getters and setters", p.span);
            }
            if p.computed {
                let k = self.temp()?;
                let key = p
                    .key
                    .as_expression()
                    .expect("a computed key is an expression");
                self.expr_to(key, k)?;
                let v = self.temp()?;
                self.expr_to(&p.value, v)?;
                self.asm.define_keyed(dst, k, v);
                self.free(v);
                self.free(k);
                continue;
            }
            let name = match &p.key {
                PropertyKey::StaticIdentifier(id) => id.name.as_str().to_owned(),
                PropertyKey::StringLiteral(lit) => match lit.value.as_str() {
                    Some(text) => text.to_owned(),
                    None => return unsupported("strings with lone surrogates", lit.span),
                },
                PropertyKey::NumericLiteral(n) => ferrojs_bytecode::number::to_js_string(n.value),
                other => return unsupported("this property key", other.span()),
            };
            if name == "__proto__" && !p.shorthand && !p.method {
                // `__proto__: v` sets the prototype (B.3.1), not a property.
                return unsupported("__proto__ in object literals", p.span);
            }
            let c = self.string_const(&name)?;
            let v = self.temp()?;
            self.expr_to(&p.value, v)?;
            self.asm.define_named(dst, c, v);
            self.free(v);
        }
        Ok(())
    }
}

/// The registers of a call: callee, `this`, and the argument temporaries.
struct CallOperands {
    callee: Reg,
    this: Reg,
    args: Reg,
    argc: Count,
    temps: Vec<Reg>,
}

/// Finds the uses of a function's parameters in its parameter
/// initializers: (initializer's parameter index, used parameter index,
/// whether the use is inside a nested function).
struct ParamRefs<'s, 'p> {
    scoping: &'s Scoping,
    params: &'p [SymbolId],
    current: usize,
    closures: u32,
    found: Vec<(usize, usize, bool)>,
}

impl<'a> Visit<'a> for ParamRefs<'_, '_> {
    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        let sym = it
            .reference_id
            .get()
            .and_then(|r| self.scoping.get_reference(r).symbol_id());
        if let Some(j) = sym.and_then(|s| self.params.iter().position(|&p| p == s)) {
            self.found.push((self.current, j, self.closures > 0));
        }
    }

    fn visit_function(&mut self, it: &Function<'a>, flags: ScopeFlags) {
        self.closures += 1;
        walk::walk_function(self, it, flags);
        self.closures -= 1;
    }

    fn visit_arrow_function_expression(&mut self, it: &ArrowFunctionExpression<'a>) {
        self.closures += 1;
        walk::walk_arrow_function_expression(self, it);
        self.closures -= 1;
    }
}

/// Records, per binding, where its first use starts and where its
/// declaration ends.
struct UseOrder<'s> {
    scoping: &'s Scoping,
    first_use: HashMap<SymbolId, u32>,
    decl_end: HashMap<SymbolId, u32>,
}

impl<'a> Visit<'a> for UseOrder<'_> {
    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        let sym = it
            .reference_id
            .get()
            .and_then(|r| self.scoping.get_reference(r).symbol_id());
        if let Some(sym) = sym {
            let e = self.first_use.entry(sym).or_insert(it.span.start);
            *e = (*e).min(it.span.start);
        }
    }

    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
        if let BindingPattern::BindingIdentifier(id) = &it.id {
            self.decl_end.insert(symbol_of(id), it.span.end);
        }
        walk::walk_variable_declarator(self, it);
    }
}

/// A register holding an operand, and whether it is a temporary to free.
#[derive(Clone, Copy)]
struct Operand {
    reg: Reg,
    temp: bool,
}

/// `let` and `const` (the bindings with a TDZ that this part compiles).
fn is_lexical(flags: SymbolFlags) -> bool {
    flags.intersects(SymbolFlags::BlockScopedVariable | SymbolFlags::ConstVariable)
}

fn is_loop(s: &Statement<'_>) -> bool {
    matches!(
        s,
        Statement::WhileStatement(_)
            | Statement::DoWhileStatement(_)
            | Statement::ForStatement(_)
            | Statement::ForInStatement(_)
            | Statement::ForOfStatement(_)
    )
}

/// True if evaluating `e` writes `dst` only in its final instruction, after
/// every operand has been read (so `dst` may be a local `e` reads).
fn writes_last(e: &Expression<'_>) -> bool {
    match e {
        Expression::ParenthesizedExpression(p) => writes_last(&p.expression),
        Expression::NumericLiteral(_)
        | Expression::BooleanLiteral(_)
        | Expression::NullLiteral(_)
        | Expression::StringLiteral(_)
        | Expression::Identifier(_)
        | Expression::BinaryExpression(_) => true,
        Expression::UnaryExpression(u) => u.operator != UnaryOperator::Void,
        _ => false,
    }
}

/// Conservatively: may evaluating `e` assign a local?
fn has_side_effects(e: &Expression<'_>) -> bool {
    match e {
        Expression::ParenthesizedExpression(p) => has_side_effects(&p.expression),
        Expression::NumericLiteral(_)
        | Expression::BooleanLiteral(_)
        | Expression::NullLiteral(_)
        | Expression::StringLiteral(_)
        | Expression::Identifier(_) => false,
        Expression::TemplateLiteral(t) => t.expressions.iter().any(has_side_effects),
        Expression::UnaryExpression(u) => has_side_effects(&u.argument),
        Expression::BinaryExpression(b) => has_side_effects(&b.left) || has_side_effects(&b.right),
        Expression::LogicalExpression(l) => has_side_effects(&l.left) || has_side_effects(&l.right),
        Expression::ConditionalExpression(c) => {
            has_side_effects(&c.test)
                || has_side_effects(&c.consequent)
                || has_side_effects(&c.alternate)
        }
        Expression::SequenceExpression(s) => s.expressions.iter().any(has_side_effects),
        _ => true,
    }
}

/// Calls `f` on every identifier reference in `e` that this subset can
/// contain (anything else makes compilation fail anyway).
fn visit_identifiers<'a>(e: &Expression<'a>, f: &mut impl FnMut(&IdentifierReference<'a>)) {
    match e {
        Expression::Identifier(id) => f(id),
        Expression::ParenthesizedExpression(p) => visit_identifiers(&p.expression, f),
        Expression::UnaryExpression(u) => visit_identifiers(&u.argument, f),
        Expression::BinaryExpression(b) => {
            visit_identifiers(&b.left, f);
            visit_identifiers(&b.right, f);
        }
        Expression::LogicalExpression(l) => {
            visit_identifiers(&l.left, f);
            visit_identifiers(&l.right, f);
        }
        Expression::ConditionalExpression(c) => {
            visit_identifiers(&c.test, f);
            visit_identifiers(&c.consequent, f);
            visit_identifiers(&c.alternate, f);
        }
        Expression::SequenceExpression(s) => {
            for e in &s.expressions {
                visit_identifiers(e, f);
            }
        }
        Expression::TemplateLiteral(t) => {
            for e in &t.expressions {
                visit_identifiers(e, f);
            }
        }
        Expression::AssignmentExpression(a) => {
            if let AssignmentTarget::AssignmentTargetIdentifier(id) = &a.left {
                f(id);
            }
            visit_identifiers(&a.right, f);
        }
        Expression::UpdateExpression(u) => {
            if let SimpleAssignmentTarget::AssignmentTargetIdentifier(id) = &u.argument {
                f(id);
            }
        }
        _ => {}
    }
}

fn statement_name(s: &Statement<'_>) -> &'static str {
    match s {
        Statement::ForInStatement(_) => "for-in",
        Statement::ForOfStatement(_) => "for-of",
        Statement::WithStatement(_) => "with",
        Statement::ClassDeclaration(_) => "classes",
        _ => "this statement",
    }
}

fn expression_name(e: &Expression<'_>) -> &'static str {
    match e {
        Expression::TaggedTemplateExpression(_) => "tagged templates",
        Expression::BigIntLiteral(_) => "BigInt",
        Expression::RegExpLiteral(_) => "regular expressions",
        Expression::NewExpression(_) => "`new`",
        Expression::ArrayExpression(_) => "array literals",
        Expression::ChainExpression(_) => "optional chaining",
        Expression::ThisExpression(_) => "this",
        _ => "this expression",
    }
}
