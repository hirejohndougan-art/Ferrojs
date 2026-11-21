//! Bytecode generation, parts 1a and 1b (tasks 1.6a/b, D-033, D-034).
//!
//! Compiles one function body to register bytecode (D-019): parameters
//! are `r0..rP`, every other binding of the function gets a fixed register
//! after them, and temporaries are allocated stack-wise above the locals.
//! This handles functions whose bindings are all uncaptured locals, with
//! number, boolean, `null` and `undefined` values, the operators on them,
//! structured control flow and `switch`. `let`/`const` bindings that may
//! be used in their temporal dead zone hold the uninitialized marker until
//! declared and are checked at every use; the rest are proven safe at
//! compile time. Assigning a `const` throws a `TypeError`. Everything else
//! is reported as [`CompileError::Unsupported`], never miscompiled: nested
//! functions, closures, calls, objects, strings, globals and `try`.
//!
//! Register-writing contract: `expr_to(e, dst)` may write `dst` before it
//! has finished reading `e`'s operands, so `dst` must be a register `e`
//! doesn't read: a fresh temporary, or a local `e` provably doesn't
//! mention (see [`Gen::assign_local`]).

use std::collections::{HashMap, HashSet};

use ferrojs_bytecode::{label_offset, AsmLabel, Assembler, Const, Handler, Ic, Imm, Limits, Reg};
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
    /// Register count `R`, parameters included.
    pub registers: u32,
    /// Number constants; strings join them in 1.6c.
    pub constants: Vec<f64>,
    /// Feedback slots.
    pub ics: u32,
    pub strict: bool,
    pub code: Vec<u8>,
    /// The exception table, innermost ranges first.
    pub handlers: Vec<Handler>,
}

impl CompiledFunction {
    /// What the verifier checks this function's code against.
    pub fn limits(&self) -> Limits {
        Limits {
            registers: self.registers,
            constants: self.constants.len() as u32,
            ics: self.ics,
        }
    }

    /// The constant pool as values.
    pub fn constant_values(&self) -> Vec<Value> {
        self.constants.iter().map(|&d| Value::number(d)).collect()
    }
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
/// whole (task 1.6c).
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

/// Compiles `func`, which must belong to `parsed`.
pub fn compile_function<'a>(parsed: &Parsed<'a>, func: &Function<'a>) -> CResult<CompiledFunction> {
    if func.generator || func.r#async {
        return unsupported("generator and async functions", func.span);
    }
    let Some(body) = &func.body else {
        return unsupported("function without a body", func.span);
    };
    let scoping = parsed.semantic.scoping();
    let scope = func.scope_id.get().expect("semantic analysis ran");
    let mut g = Gen::new(scoping, scope);

    // Parameters first, in order: r0..rP. A repeated name (sloppy mode)
    // binds the last parameter with that name.
    let params = &func.params;
    if let Some(rest) = &params.rest {
        return unsupported("rest parameters", rest.span);
    }
    let count = params.items.len() as u32;
    for (i, p) in params.items.iter().enumerate() {
        if p.initializer.is_some() {
            return unsupported("default parameter values", p.span);
        }
        let BindingPattern::BindingIdentifier(id) = &p.pattern else {
            return unsupported("destructuring parameters", p.span);
        };
        g.locals.insert(symbol_of(id), Reg(i as u32));
    }
    g.next_reg = count;
    g.claim_locals(count);
    g.find_early_references(body);

    g.enter_lexical(body.statements.iter());
    for stmt in &body.statements {
        g.statement(stmt)?;
    }
    let r = g.temp()?;
    g.asm.load_undefined(r);
    g.asm.ret(r);
    g.free(r);

    let (code, labels) = g
        .asm
        .finish_with_labels()
        .map_err(|e| CompileError::TooLarge(format!("{e:?}")))?;
    let at = |l| label_offset(&labels, l).expect("handler labels are bound");
    let handlers = g
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
        name: func
            .id
            .as_ref()
            .map(|id| id.name.as_str().to_owned())
            .unwrap_or_default(),
        params: count,
        registers: g.max_reg.max(count),
        constants: g.constants,
        ics: g.ics,
        strict: scoping.scope_flags(scope).is_strict_mode(),
        code,
        handlers,
    })
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
}

const K_NORMAL: i32 = 0;
const K_THROW: i32 = 1;
const K_RETURN: i32 = 2;
const K_JUMPS: i32 = 3;

/// Where an assignment stores, and what it must check first.
#[derive(Clone, Copy)]
struct Target {
    reg: Reg,
    sym: SymbolId,
    /// Lexical binding that may be in its TDZ: check before storing.
    tdz: bool,
    /// `const`: the store throws a `TypeError`.
    constant: bool,
}

struct Gen<'s> {
    scoping: &'s Scoping,
    scope: ScopeId,
    asm: Assembler,
    locals: HashMap<SymbolId, Reg>,
    /// Lexical bindings (`let`, `const`) whose declaration has been
    /// compiled.
    initialized: HashSet<SymbolId>,
    /// Lexical bindings that may be used in their TDZ: they start as the
    /// marker and every use checks it (D-034). The rest are proven safe.
    needs_tdz: HashSet<SymbolId>,
    next_reg: u32,
    max_reg: u32,
    constants: Vec<f64>,
    constant_index: HashMap<u64, u32>,
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
}

const MAX_OPERAND: u32 = i32::MAX as u32;

impl<'s> Gen<'s> {
    fn new(scoping: &'s Scoping, scope: ScopeId) -> Gen<'s> {
        Gen {
            scoping,
            scope,
            asm: Assembler::new(),
            locals: HashMap::new(),
            initialized: HashSet::new(),
            needs_tdz: HashSet::new(),
            next_reg: 0,
            max_reg: 0,
            constants: Vec::new(),
            constant_index: HashMap::new(),
            ics: 0,
            breakables: Vec::new(),
            pending_labels: Vec::new(),
            handlers: Vec::new(),
            finallies: Vec::new(),
            catch_names: Vec::new(),
        }
    }

    /// The function scope that owns `scope`: the nearest enclosing scope
    /// that is a function (or the top level).
    fn owner(&self, mut scope: ScopeId) -> ScopeId {
        loop {
            if self
                .scoping
                .scope_flags(scope)
                .intersects(ScopeFlags::Function | ScopeFlags::Top)
            {
                return scope;
            }
            match self.scoping.scope_parent_id(scope) {
                Some(p) => scope = p,
                None => return scope,
            }
        }
    }

    /// Gives every binding declared in this function (outside its nested
    /// functions) a register after the parameters, in symbol order.
    fn claim_locals(&mut self, params: u32) {
        let mut next = params;
        for sym in self.scoping.symbol_ids() {
            if self.locals.contains_key(&sym) {
                continue;
            }
            if self.owner(self.scoping.symbol_scope_id(sym)) == self.scope {
                self.locals.insert(sym, Reg(next));
                next += 1;
            }
        }
        self.next_reg = next;
        self.max_reg = next;
    }

    /// Finds the lexical bindings with a use that comes textually before
    /// the end of their declaration (so it may run in the TDZ). Within one
    /// function without closures, textual order is execution order for
    /// everything except `switch`, which [`Gen::switch`] handles.
    fn find_early_references(&mut self, body: &FunctionBody<'_>) {
        let mut v = UseOrder {
            scoping: self.scoping,
            first_use: HashMap::new(),
            decl_end: HashMap::new(),
        };
        v.visit_function_body(body);
        for (sym, end) in v.decl_end {
            if v.first_use.get(&sym).is_some_and(|&start| start < end)
                && is_lexical(self.scoping.symbol_flags(sym))
            {
                self.needs_tdz.insert(sym);
            }
        }
    }

    /// At the start of a scope: puts the TDZ marker into the lexical
    /// bindings declared directly in `stmts` that need run-time checks.
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
                let i = self.constants.len() as u32;
                if i >= MAX_OPERAND {
                    return Err(CompileError::TooLarge("constants".into()));
                }
                self.constants.push(d);
                self.constant_index.insert(bits, i);
                i
            }
        };
        self.asm.load_const(dst, Const(index));
        Ok(())
    }

    // ---- bindings ----

    /// The register of the local an identifier reference resolves to.
    fn resolve(&self, id: &IdentifierReference<'_>) -> Option<SymbolId> {
        let rid = id.reference_id.get()?;
        self.scoping.get_reference(rid).symbol_id()
    }

    fn local_for(&self, sym: SymbolId, span: oxc_span::Span) -> CResult<Reg> {
        match self.locals.get(&sym) {
            Some(&r) => Ok(r),
            None => unsupported("variables of an enclosing function (closures)", span),
        }
    }

    /// The register to read for `id`, after its TDZ check.
    fn read_local(&mut self, id: &IdentifierReference<'_>) -> CResult<Option<Reg>> {
        let Some(sym) = self.resolve(id) else {
            return Ok(None);
        };
        let r = self.local_for(sym, id.span)?;
        if self.binding_kind(sym, id.span)? {
            self.asm.check_initialized(r);
        }
        Ok(Some(r))
    }

    /// Rejects bindings this part can't handle; returns whether a use
    /// needs a run-time TDZ check.
    fn binding_kind(&self, sym: SymbolId, span: oxc_span::Span) -> CResult<bool> {
        let flags = self.scoping.symbol_flags(sym);
        if flags.intersects(SymbolFlags::Function | SymbolFlags::Class) {
            return unsupported("function and class bindings", span);
        }
        if !is_lexical(flags) {
            return Ok(false);
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

    /// Where an assignment to `id` stores.
    fn write_target(&self, id: &IdentifierReference<'_>) -> CResult<Target> {
        let Some(sym) = self.resolve(id) else {
            return unsupported("assignment to a global variable", id.span);
        };
        let reg = self.local_for(sym, id.span)?;
        let tdz = self.binding_kind(sym, id.span)?;
        let constant = self
            .scoping
            .symbol_flags(sym)
            .contains(SymbolFlags::ConstVariable);
        Ok(Target {
            reg,
            sym,
            tdz,
            constant,
        })
    }

    /// Reads an assignment target's current value (with its TDZ check),
    /// for compound and logical assignments and updates.
    fn read_target(&mut self, t: Target) {
        if t.tdz {
            self.asm.check_initialized(t.reg);
        }
    }

    /// Stores `e`'s value in `t`: SetMutableBinding, after `e` ran.
    fn store(&mut self, t: Target, e: &Expression<'_>) -> CResult<()> {
        if t.constant || t.tdz {
            let v = self.temp()?;
            self.expr_to(e, v)?;
            self.store_reg(t, v);
            self.free(v);
            Ok(())
        } else {
            self.assign_local(t.reg, t.sym, e)
        }
    }

    /// Stores register `v` in `t`.
    fn store_reg(&mut self, t: Target, v: Reg) {
        if t.tdz {
            self.asm.check_initialized(t.reg);
        }
        if t.constant {
            self.asm.throw_const_assign();
        } else if v != t.reg {
            self.asm.mov(t.reg, v);
        }
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
            Statement::ForStatement(f) => {
                match &f.init {
                    None => {}
                    Some(ForStatementInit::VariableDeclaration(d)) => {
                        self.mark_uninitialized(d);
                        self.declaration(d)?
                    }
                    Some(init) => self.effect(init.to_expression())?,
                }
                let top = self.asm.new_label();
                let (cont, done) = (self.asm.new_label(), self.asm.new_label());
                self.asm.bind(top);
                if let Some(test) = &f.test {
                    self.branch_if_false(test, done)?;
                }
                self.loop_body(labels, &f.body, done, cont)?;
                self.asm.bind(cont);
                if let Some(update) = &f.update {
                    self.effect(update)?;
                }
                self.asm.jump_loop(top);
                self.asm.bind(done);
                Ok(())
            }
            Statement::BreakStatement(b) => {
                let target = self.find_target(b.label.as_ref(), false, b.span)?;
                self.jump_to(target, false)
            }
            Statement::ContinueStatement(c) => {
                let target = self.find_target(c.label.as_ref(), true, c.span)?;
                self.jump_to(target, true)
            }
            Statement::ReturnStatement(r) => {
                if self.finallies.is_empty() {
                    match &r.argument {
                        Some(e) => {
                            let v = self.expr_any(e)?;
                            self.asm.ret(v.reg);
                            self.release(v);
                        }
                        None => {
                            let t = self.temp()?;
                            self.asm.load_undefined(t);
                            self.asm.ret(t);
                            self.free(t);
                        }
                    }
                    return Ok(());
                }
                let value = self.finallies.last().expect("checked").value;
                match &r.argument {
                    Some(e) => self.expr_to(e, value)?,
                    None => self.asm.load_undefined(value),
                }
                self.return_value(value);
                Ok(())
            }
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

    /// Jumps to breakable `index` (its continue or break label), running
    /// the `finally` blocks in between first.
    fn jump_to(&mut self, index: usize, is_continue: bool) -> CResult<()> {
        let b = &self.breakables[index];
        let label = if is_continue {
            b.continue_to.expect("find_target checked")
        } else {
            b.break_to
        };
        if b.finally_depth == self.finallies.len() {
            self.asm.jump(label);
            return Ok(());
        }
        let f = self.finallies.last_mut().expect("deeper than the target");
        let i = match f.jumps.iter().position(|&j| j == (index, is_continue)) {
            Some(i) => i,
            None => {
                f.jumps.push((index, is_continue));
                f.jumps.len() - 1
            }
        };
        let (kind, fin) = (f.kind, f.fin);
        let k = K_JUMPS
            .checked_add(i32::try_from(i).unwrap_or(i32::MAX))
            .ok_or_else(|| CompileError::TooLarge("jumps out of finally".into()))?;
        self.asm.load_int(kind, Imm(k));
        self.asm.jump(fin);
        Ok(())
    }

    /// Returns the value in register `value`, which must be the innermost
    /// open finally's value register if there is one.
    fn return_value(&mut self, value: Reg) {
        match self.finallies.last_mut() {
            None => self.asm.ret(value),
            Some(f) => {
                debug_assert_eq!(f.value, value);
                f.returns = true;
                let (kind, fin) = (f.kind, f.fin);
                self.asm.load_int(kind, Imm(K_RETURN));
                self.asm.jump(fin);
            }
        }
    }

    fn try_statement(&mut self, t: &TryStatement<'_>) -> CResult<()> {
        let Some(finalizer) = &t.finalizer else {
            let handler = t.handler.as_ref().expect("try has catch or finally");
            return self.try_catch(&t.block, handler);
        };
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
        });
        self.asm.bind(start);
        let body = match &t.handler {
            Some(c) => self.try_catch(&t.block, c),
            None => self.block(&t.block),
        };
        let f = self.finallies.pop().expect("pushed above");
        body?;
        self.asm.bind(end);
        self.asm.load_int(kind, Imm(K_NORMAL));
        self.asm.jump(fin);
        // A throw anywhere in the try block or the catch block lands here.
        self.handlers.push((start, end, handler, value));
        self.asm.bind(handler);
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
        Ok(())
    }

    fn try_catch(&mut self, block: &BlockStatement<'_>, c: &CatchClause<'_>) -> CResult<()> {
        let (exception, temp, name) = match &c.param {
            None => (self.temp()?, true, None),
            Some(p) => {
                let BindingPattern::BindingIdentifier(id) = &p.pattern else {
                    return unsupported("destructuring catch parameters", p.span);
                };
                let r = self.local_for(symbol_of(id), id.span)?;
                (r, false, Some(id.name.as_str().to_owned()))
            }
        };
        let (start, end, handler, done) = (
            self.asm.new_label(),
            self.asm.new_label(),
            self.asm.new_label(),
            self.asm.new_label(),
        );
        self.asm.bind(start);
        self.block(block)?;
        self.asm.bind(end);
        self.asm.jump(done);
        self.handlers.push((start, end, handler, exception));
        self.asm.bind(handler);
        let pushed = name.is_some();
        if let Some(n) = name {
            self.catch_names.push(n);
        }
        let r = self.block(&c.body);
        if pushed {
            self.catch_names.pop();
        }
        r?;
        self.asm.bind(done);
        if temp {
            self.free(exception);
        }
        Ok(())
    }

    fn block(&mut self, b: &BlockStatement<'_>) -> CResult<()> {
        self.enter_lexical(b.body.iter());
        self.statements(&b.body)
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
            let r = self.local_for(sym, id.span)?;
            match &decl.init {
                Some(init) => self.assign_local(r, sym, init)?,
                // `let x;` sets undefined each time it runs (loops!); `var x;`
                // does nothing.
                None if d.kind != VariableDeclarationKind::Var => self.asm.load_undefined(r),
                None => {}
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
            if let Some(r) = self.read_local(id)? {
                return Ok(Operand {
                    reg: r,
                    temp: false,
                });
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
            Expression::Identifier(id) => match self.read_local(id)? {
                Some(r) => {
                    if r != dst {
                        self.asm.mov(dst, r);
                    }
                    Ok(())
                }
                // Unresolved: a global. These three are non-writable,
                // non-configurable properties of the global object, and
                // nothing in this subset can shadow them dynamically
                // (`with` and direct `eval` are unsupported).
                None => match id.name.as_str() {
                    "undefined" => {
                        self.asm.load_undefined(dst);
                        Ok(())
                    }
                    "NaN" => self.load_number(dst, f64::NAN),
                    "Infinity" => self.load_number(dst, f64::INFINITY),
                    _ => unsupported("global variables", id.span),
                },
            },
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
            other => unsupported(expression_name(other), other.span()),
        }
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
            UnaryOperator::Typeof => return unsupported("typeof (needs strings)", u.span),
            UnaryOperator::Delete => return unsupported("delete", u.span),
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

    fn binary(&mut self, b: &BinaryExpression<'_>, dst: Reg) -> CResult<()> {
        if matches!(b.operator, BinaryOperator::In | BinaryOperator::Instanceof) {
            return unsupported("`in` and `instanceof` (need objects)", b.span);
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
                    B::StrictEquality
                    | B::StrictInequality
                    | B::Equality
                    | B::Inequality
                    | B::In
                    | B::Instanceof => unreachable!("handled above"),
                }
            }
        }
        Ok(())
    }

    fn assignment(&mut self, a: &AssignmentExpression<'_>, dst: Reg) -> CResult<()> {
        let AssignmentTarget::AssignmentTargetIdentifier(id) = &a.left else {
            return unsupported("assignment to a property or a pattern", a.left.span());
        };
        let t = self.write_target(id)?;
        match a.operator {
            AssignmentOperator::Assign => self.store(t, &a.right)?,
            op if op.is_logical() => {
                self.read_target(t);
                let end = self.asm.new_label();
                match op {
                    AssignmentOperator::LogicalAnd => self.asm.jump_if_false(t.reg, end),
                    AssignmentOperator::LogicalOr => self.asm.jump_if_true(t.reg, end),
                    _ => self.asm.jump_if_not_nullish(t.reg, end),
                }
                self.store(t, &a.right)?;
                self.asm.bind(end);
            }
            op => {
                // `x op= e`: the old x is read before e runs.
                self.read_target(t);
                let bop = op.to_binary_operator().expect("arithmetic assignment");
                let old = if has_side_effects(&a.right) {
                    let r = self.temp()?;
                    self.asm.mov(r, t.reg);
                    Operand { reg: r, temp: true }
                } else {
                    Operand {
                        reg: t.reg,
                        temp: false,
                    }
                };
                let rhs = self.expr_any(&a.right)?;
                if t.constant || t.tdz {
                    let v = self.temp()?;
                    self.binary_op(bop, v, old.reg, rhs.reg)?;
                    self.store_reg(t, v);
                    self.free(v);
                } else {
                    self.binary_op(bop, t.reg, old.reg, rhs.reg)?;
                }
                self.release(rhs);
                self.release(old);
            }
        }
        if t.reg != dst {
            self.asm.mov(dst, t.reg);
        }
        Ok(())
    }

    /// `x++`, `--x`, …: the result (the old value converted with ToNumeric,
    /// or the new value) goes to `dst` if there is one.
    fn update(&mut self, u: &UpdateExpression<'_>, dst: Option<Reg>) -> CResult<()> {
        let t = match &u.argument {
            SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => self.write_target(id)?,
            other => return unsupported("update of a property", other.span()),
        };
        self.read_target(t);
        let ic = self.ic()?;
        let step = |g: &mut Self, d: Reg, s: Reg| match u.operator {
            UpdateOperator::Increment => g.asm.inc(d, s, ic),
            UpdateOperator::Decrement => g.asm.dec(d, s, ic),
        };
        match dst {
            Some(dst) if !u.prefix => {
                let ic = self.ic()?;
                self.asm.to_numeric(dst, t.reg, ic);
                if t.constant {
                    let v = self.temp()?;
                    step(self, v, dst);
                    self.store_reg(t, v);
                    self.free(v);
                } else {
                    step(self, t.reg, dst);
                }
            }
            _ => {
                if t.constant {
                    let v = self.temp()?;
                    step(self, v, t.reg);
                    self.store_reg(t, v);
                    self.free(v);
                } else {
                    step(self, t.reg, t.reg);
                }
                if let Some(dst) = dst {
                    self.asm.mov(dst, t.reg);
                }
            }
        }
        Ok(())
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
        | Expression::Identifier(_) => false,
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
        Statement::FunctionDeclaration(_) => "nested functions",
        Statement::ClassDeclaration(_) => "classes",
        _ => "this statement",
    }
}

fn expression_name(e: &Expression<'_>) -> &'static str {
    match e {
        Expression::StringLiteral(_) | Expression::TemplateLiteral(_) => "strings",
        Expression::BigIntLiteral(_) => "BigInt",
        Expression::RegExpLiteral(_) => "regular expressions",
        Expression::CallExpression(_) | Expression::NewExpression(_) => "calls",
        Expression::FunctionExpression(_) | Expression::ArrowFunctionExpression(_) => {
            "nested functions"
        }
        Expression::ObjectExpression(_) | Expression::ArrayExpression(_) => "objects and arrays",
        Expression::ThisExpression(_) => "this",
        _ => "this expression",
    }
}
