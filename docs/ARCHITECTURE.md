# FerroJS architecture

How the engine is put together, in enough detail that a run with no memory
of earlier runs can build the next piece without re-deciding it. The
binding choices are in `docs/DECISIONS.md` (D-018 to D-024 were written
with this file); this file explains how they fit together. When the code
and this file disagree, fix one of them in the same change.

Status: written in task 1.1 (2026-11-20), before any of Phase 1 exists.
Sections say which task builds them.

## 1. Crates and the path of a script

```
source text
  └─ oxc parser + oxc_semantic ─────────────── ferrojs-compiler (1.5, 1.6)
       └─ early-error pass, scope analysis
            └─ bytecode compiler ──► FunctionTemplate + BytecodeArray (heap)
                                        └─ bytecode verifier ── ferrojs-bytecode (1.4)
                                             └─ interpreter ─── ferrojs-vm (1.7, 1.8)
                                                  └─ baseline JIT (Phase 3), Warp (Phase 5)
```

| Crate | Holds | `unsafe` |
| --- | --- | --- |
| `ferrojs-value` | NaN-boxed `Value` (D-001) | forbidden |
| `ferrojs-core` | heap cage, allocator, GC, handle machinery, later JIT memory and trampolines | allowed, audited |
| `ferrojs-bytecode` | op definitions (§4), encoder, decoder, disassembler, verifier | forbidden |
| `ferrojs-compiler` | oxc integration, early errors, scope analysis, bytecode generation | forbidden |
| `ferrojs-vm` | interpreter, objects and shapes, realms, runtime functions, built-ins (split into `ferrojs-builtins` when it grows) | forbidden |
| `ferrojs` | embedding API (1.9) | forbidden |
| `ferrojs-cli` | the `ferrojs` binary | forbidden |

Dependencies only point toward the lower layers: `ferrojs-value` at the
bottom, `ferrojs-core` and `ferrojs-bytecode` above it, `ferrojs-compiler`
on `ferrojs-bytecode`, `ferrojs-vm` on all of those, then `ferrojs` and
`ferrojs-cli`. Nothing depends on `ferrojs-vm` except the top two. `ferrojs-core`
knows nothing about JavaScript semantics; it knows cell layouts only through
the trace tables `ferrojs-vm` registers (§6.4).

## 2. Values

`ferrojs_value::Value` is the raw 64-bit encoding (D-001). It has no
lifetime and is what heap cells and frame slots store. Code outside
`ferrojs-core` and the interpreter's register access never holds a raw
`Value` that may contain a heap reference across a point that can collect;
it uses the branded handles of §6.

Internal marker values that must never reach JavaScript (the TDZ
"uninitialized" marker, array holes, "no value" in frames) need one more
encoding. Task 1.6 adds it as `TAG_OTHER` with a reserved kind and logs a
decision; `from_bits_checked` must keep rejecting it for cache input unless
the cache format says otherwise.

## 3. Frames and the frame stack (D-021)

### 3.1 The frame stack

Each engine instance has one frame stack: a contiguous array of 64-bit
slots, reserved up front (default 1 Mi slots = 8 MB of address space,
committed lazily) so slot addresses never move. It lives outside the heap
cage. Frames link to each other by **slot index** (`u32`), never by
address, so a frame stack can be inspected, copied (generators) and
described without raw pointers.

Interpreted frames, native-function frames and (later) baseline-JIT frames
all use the same layout. The optimizing tier may use its own frames but
must be able to materialize this layout on deoptimization.

### 3.2 Frame layout

`fp` is the slot index of a frame's first header slot.

```
fp + 0   callee        Value   the function object being run
fp + 1   this          Value
fp + 2   new.target    Value   undefined for [[Call]]
fp + 3   environment   Value   current lexical Environment cell (or undefined)
fp + 4   caller fp     raw     u32 slot index, u32::MAX for the entry frame
fp + 5   return pc     raw     bytecode offset in the caller to resume at
fp + 6   return info   raw     caller's destination register (u32) | flags (u32)
fp + 7   argc          raw     number of arguments actually passed
fp + 8   r0 ...                registers; r0..r(P-1) are the P declared parameters
fp + 8 + R                     extra arguments beyond P (argc - P of them, if any)
```

- `H = 8` header slots. Value slots are scanned by the GC; raw slots are
  skipped by position, so no stack maps are needed for interpreted or
  native frames.
- **Every register always holds a valid `Value`.** The call sequence fills
  missing parameters and all non-parameter registers with `undefined`
  before the first instruction runs. This is what makes precise stack
  scanning trivial; the baseline JIT must keep values in these slots at
  every point that can collect.
- `R` (register count, including parameters) is fixed per function by the
  compiler and stored in its `FunctionTemplate`. Extra arguments sit after
  the registers so every parameter and register has a fixed offset; the
  prologue of a function that uses `arguments` or a rest parameter copies
  them into a heap object.
- A call copies `this` and the arguments from the caller's registers into
  the new frame (`fp_new = fp + 8 + R + extra` of the caller). Copying a
  few slots is cheaper than the bookkeeping of overlapping frames, and it
  keeps the layout simple for tail calls and generators.
- Stack overflow is checked once at function entry: `fp_new + 8 + R +
  extra + max_call_reserve <= limit`, else a `RangeError` is thrown in the
  caller. Re-entrant calls from Rust into JavaScript (built-ins calling
  user code) also count native recursion depth against a separate limit.

### 3.3 Calls and returns

`Call dst, callee, this, args_start, argc` (and `New`, `CallSpread`)
reads a contiguous register range. If the callee is a bytecode function
the interpreter pushes a frame and continues in the same Rust loop (no
Rust recursion). If it is a native function, the interpreter pushes a
frame with the same layout (so stack traces, scanning and `argc` work the
same) and calls the Rust function with a view of its arguments.

`Return src` writes the result to the caller's register named in
`return info`, pops the frame and resumes at `return pc`. The entry
frame's return hands the value back to the Rust caller.

### 3.4 Proper tail calls (D-021)

ECMA-262 requires calls in tail position in strict code not to grow the
stack. The compiler marks those calls (`TailCall`, `TailCallSpread`); the
interpreter then:

1. evaluates callee, `this` and arguments into registers as usual;
2. copies them into a scratch area at the stack top;
3. overwrites the current frame from `fp` with the new callee's header,
   keeping the current frame's `caller fp`, `return pc` and `return info`;
4. copies the arguments back into the new register area and fills the
   rest with `undefined`.

The stack depth stays constant, so `tco` Test262 tests with 100,000 calls
pass at any stack size. A tail call to a native function just calls it and
returns its result. Generator, async and module top-level bodies have no
tail positions by spec. Frames removed this way do not appear in
`Error.prototype.stack` (as in JavaScriptCore). The baseline JIT must
implement the same sequence; it shares the layout, so it can.

### 3.5 Generators and async functions

A suspended generator copies its frame's Value slots (header Value slots
and registers) into its heap `GeneratorObject` and restores them on
resume. Because every slot of the layout is fixed-offset, this is a plain
copy. Async functions are generators driven by promise jobs (1.12, 1.14).

### 3.6 Exceptions

A thrown value is stored in the engine's `pending_exception` slot (a GC
root), never in a Rust `Result` payload. Handlers return a control value
(`Flow::Throw`). The interpreter looks up the current pc in the function's
exception table (pc ranges → handler pc and the register to receive the
exception), unwinding frames until one matches or the entry frame is
reached.

## 4. Bytecode (D-019) and the op-definition language (D-020)

### 4.1 Format

- Register-based, three-address: `Add dst, lhs, rhs`. No accumulator.
- Little-endian. One opcode byte, then operands. Operands are 1 byte by
  default; a `Wide` prefix makes every operand of the next instruction 2
  bytes and `ExtraWide` 4 bytes. The encoder picks the narrowest form.
- Operand kinds:

  | Kind | Meaning |
  | --- | --- |
  | `Reg` | register index (`r0` is `fp + 8`) |
  | `Imm` | signed integer immediate |
  | `Const` | index into the function's constant pool (numbers, strings, templates, regexp data) |
  | `Ic` | index into the function's feedback vector (§7) |
  | `Label` | signed jump offset, relative to the start of the instruction |
  | `Count` | an argument or element count |
  | `EnvSlot` | (depth, slot) pair addressing a captured variable |

- Each function compiles to an immutable `BytecodeArray` heap cell owned
  by a `FunctionTemplate` cell, which also holds: name, parameter count
  `P`, register count `R`, flags (strict, arrow, generator, async, uses
  `arguments`, …), constant pool, exception table, source-position table,
  and the feedback layout (the kind of each IC slot). A closure is a
  `JsFunction` cell: template + captured environment.
- Phase 1 keeps one feedback vector per `FunctionTemplate`, allocated on
  first call.
- Bytecode can come from a cache file later, so it is untrusted (Safety
  rule 7): the **verifier** checks every register is `< R`, every jump
  lands on an instruction boundary inside the function, every constant
  and IC index is in range and of the right kind, and that the function
  ends in a terminator. The compiler's output is verified in debug builds
  and tests; cached bytecode is always verified.

### 4.2 Defining an op once

All ops are defined in one file, `ferrojs-bytecode/src/ops.rs`, inside a
single declarative X-macro:

```rust
ops! {
    /// `dst = lhs + rhs` (ApplyStringOrNumericBinaryOperator, `+`).
    Add { dst: Reg, lhs: Reg, rhs: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            let (a, b) = (cx.reg(lhs), cx.reg(rhs));
            if let (Some(x), Some(y)) = (a.as_int32(), b.as_int32()) {
                if let Some(r) = x.checked_add(y) {
                    cx.note(ic, Feedback::INT32);
                    cx.set(dst, Value::int32(r));
                    return Flow::Next;
                }
            }
            cx.slow(dst, |rt| rt.add(a, b))
        }
}
```

From this one list the macro generates:

1. `enum Opcode` and the operand structs;
2. the encoder (`Assembler::add(dst, lhs, rhs, ic)`), choosing operand width;
3. the decoder and disassembler (used by dump tools and tests);
4. the verifier's per-op operand table;
5. a metadata table (`flags`, operand kinds, is-jump, is-terminator);
6. the interpreter's dispatch `match`, calling each handler body;
7. later (Phase 3) one copy-and-patch stencil per op, by instantiating the
   same handler body against a stencil context whose operand reads are
   patchable holes;
8. later (Phase 5) an optional `lower { … }` clause per op for the SSA
   builder. Ops with ICs lower through their CacheIR instead (§7), which
   is why the handler/IC split matters.

Handler bodies are generic over an `OpCtx` trait (`reg`, `set`, `note`,
`slow`, `jump`, `throw`, …). They contain only the fast path; anything
long goes into a named runtime function (`rt.add`) so stencils stay small
and every tier calls the same slow path. This is what stops tiers from
disagreeing (plan: "Single op definitions").

Rules for op definitions:

- An op that may run user code, allocate or throw says so in `flags`; the
  verifier and later the JIT rely on the flags, and debug builds assert
  that an op without `may_gc` did not collect.
- Loop back-edges use `JumpLoop`, never a plain backward `Jump`: it is the
  op that checks the interrupt flag (D-007) and, later, counts toward tier-up.
  Function entry checks the flag too.
- No op reads the bytecode stream past its own operands.

### 4.3 Interpreter loop

`ferrojs-vm` runs a `loop { match opcode { … } }` over the current
function's bytecode with `pc` and `fp` in locals. Register access is
bounds-checked safe Rust in Phase 1. If benchmarks show the checks matter,
a verified-bytecode fast path with unchecked access moves into
`ferrojs-core` under the risky-review rule, justified by the verifier.
Threaded dispatch through guaranteed tail calls is a later option once
Rust's `become` is stable; the baseline JIT, not dispatch tricks, is the
planned big speed step.

## 5. The heap (D-002, D-022)

### 5.1 The cage

- Each engine instance reserves 4 GB of address space plus a 64 KB guard
  at the end, `PROT_NONE` (`mmap` on Unix, `VirtualAlloc(MEM_RESERVE)` on
  Windows). Memory is committed per chunk as needed.
- A reference is a 32-bit **byte offset** from the cage base. Every cell is
  8-byte aligned, so the low 3 bits of every reference are 0 (room for a
  scaled large-heap mode later).
- Offsets below 64 KB are never committed. Offset 0 is the null reference
  and faults on access.
- `base + u32 + small displacement` always lands inside the reservation, so
  any JIT bug that computes a wrong offset stays inside the cage. This only
  holds for a full 4 GB cage (`Cage::is_full_size`); smaller cages (tests,
  Miri) rely on the checked accessors (D-027).
- Under Miri (D-016) the cage is a small `std::alloc` region with the same
  interface.

### 5.2 Chunks and allocation (Phase 1)

- The cage is carved into 256 KB chunks, each with a header recording its
  kind: small-object chunk for one size class, large-object chunk run, or
  (Phase 5) nursery.
- Size classes: 16, 24, 32, 48, 64, 96, 128, … up to 8 KB, roughly 1.5×
  steps. Bigger cells get their own run of chunks.
- Allocation bumps through a fresh chunk of the right class; after a
  collection, sweeping threads free cells into per-class free lists, which
  allocation uses first.
- Mark bits live in a side bitmap per chunk (one bit per 8-byte granule),
  not in cell headers: sweeping touches less memory, concurrent marking
  later doesn't race on headers, and snapshotted cells carry no GC state.

### 5.3 Cell layout

Every heap cell starts with one 8-byte header:

```
bits  0..8    CellKind   (Object, Array, Function, String, Symbol, BigInt,
                          Shape, FunctionTemplate, BytecodeArray,
                          Environment, FeedbackVector, CacheIrStub, …)
bits  8..16   flags      kind-specific
bits 16..32   small      kind-specific (e.g. inline slot count)
bits 32..64   shape      u32 offset of the object's Shape (objects only; 0 otherwise)
```

The `CellKind` byte alone tells the GC the cell's size and where its
references are (§6.4). Cells never contain raw machine addresses (D-009):
native functions are indexes into a registered table, embedder data is an
index into an external table with an optional finalizer, and Rust
collections never hang off a cell (a `Map`'s hash table is itself a heap
array).

### 5.4 Strings

Immutable cells: length, cached hash, and either Latin-1 or UTF-16 code
units (JavaScript strings are sequences of 16-bit units, so lone
surrogates are representable). Property-key strings are interned into
**atoms** through a weak table, so key comparison in shapes is reference
comparison. Ropes and slices are a Phase 2 optimization behind the same
interface.

## 6. GC interface (D-023)

### 6.1 Collector, Phase 1

Precise, stop-the-world, non-moving mark-sweep (task 1.3). Marking uses an
explicit mark stack, never recursion. Phase 5 replaces it with a copying
nursery and a mark-compact old generation **without changing the
interface below**, which is why no code may assume an object stays put.

### 6.2 Branded handles

Collection may happen at **any allocation and any call that can run
JavaScript**. Rooting mistakes are caught by the borrow checker:

- `Cx<'h>` is the engine context. Anything that may collect takes
  `&mut Cx`.
- `cx.nogc()` returns a `NoGc<'a>` token borrowing `cx` shared.
- `Gc<'a, T>` and `JsValue<'a>` are unrooted handles: `Copy`, a 32-bit
  offset plus a lifetime tied to a `NoGc<'a>`. While one is alive, `cx`
  is borrowed shared, so calling anything that takes `&mut Cx` is a
  compile error. An unrooted handle can't be held across a collection.
- Functions that may collect return `Unbound<T>`: `#[must_use]`, with no
  operations except `.bind(cx.nogc())`. This is how a fresh result gets a
  lifetime without keeping `cx` mutably borrowed.
- To keep something across a possible collection, root it:
  `let r: Root<'s, T> = cx.root(handle)` pushes it on the **handle stack**
  inside a `cx.scope(|cx| …)` block; `r.get(cx.nogc())` gives back an
  unrooted handle (updated if the object moved). Leaving the scope
  truncates the handle stack.
- `Global<T>` is a root in a table that outlives scopes, for embedders and
  engine caches; dropping it frees the slot.

Known hole: an `Unbound<T>` stored in a variable instead of bound
immediately can outlive a collection. Mitigations: `Unbound` has no
accessors, and the stress mode below catches the bug in tests.

This keeps the plan's intent (gc-arena style lifetime branding, rooting
errors at compile time) while allowing collection inside built-ins that
call back into JavaScript, which pure gc-arena (collection only between
`mutate` calls) does not.

### 6.3 Roots

1. The frame stack: every Value slot of every frame (§3.2).
2. The handle stack (scoped roots).
3. The global root table.
4. Engine roots: realms and their intrinsics, `pending_exception`, the
   job queue, the native function table's associated cells. The atom table
   and (Phase 2) WeakMap/WeakRef/FinalizationRegistry are weak, handled
   with ephemeron semantics.

### 6.4 Tracing and barriers

- Cell kinds are a closed set. `ferrojs-vm` declares each kind's layout in
  one `cells!` table (fixed fields, which words are `Value`s or
  references, how to compute the size of variable-length kinds); this
  generates the trace and size functions the collector calls.
- **Every write of a reference into a heap cell goes through
  `ferrojs-core`'s write API**, which runs the write barrier. The barrier
  is empty in Phase 1, but the call is mandatory so the generational and
  incremental collectors only fill it in. Direct stores that skip it are a
  bug.

### 6.5 Self-checks

- **GC stress mode** (`EngineOptions::gc_stress`, or
  `FERROJS_GC_STRESS=1`): collect on every allocation. Part of the test
  suite from task 1.3; later run nightly over Test262.
- Debug builds run a **heap verifier** after each collection: every
  reference in a live cell or root points at the header of a live cell in
  a committed chunk.
- Debug builds **poison** freed cells (kind `Freed`), so a use after free
  fails at its next access instead of reading stale data.

## 7. Objects, shapes and inline caches (D-024)

### 7.1 Object layout

```
header        CellKind + flags + inline slot count + Shape
elements      reference to the indexed-property store (or 0)
slots         reference to out-of-line named-property slots (or 0)
inline slots  N Values (N chosen at allocation, default 4)
```

Named properties live in inline slots first, then out-of-line. Integer
index keys go to the elements store, with kinds `Packed`, `Holey` and
`Sparse` (dictionary) plus sealed/frozen flags; an `Array`'s length lives
in its elements header.

### 7.2 Shapes

A `Shape` cell describes one object layout: parent shape, the property
key added (atom or symbol), its attributes, its slot index, the
**prototype**, the object flags (extensible, …) and the transitions out
of it (one inline transition or a table). Keeping the prototype in the
shape means one shape check proves both own layout and prototype
identity.

- Root shapes are per (realm, prototype, inline slot count).
- Adding a property follows or creates a transition, so objects built the
  same way share shapes.
- Shapes with more than 8 properties build a lazy key → slot lookup table.
- **Dictionary mode**: deleting a property other than the last added, more
  than 128 properties, or changing an attribute switches the object to a
  unique dictionary shape with a heap hash table. Dictionary shapes never
  appear in IC stubs.
- Exotic objects (Array, String, arguments, TypedArray, Proxy, module
  namespace, …) are identified by `CellKind`; ordinary internal methods
  are the inline fast path and exotic ones dispatch through a static
  per-kind table (snapshot-friendly, no function pointers in the heap).

### 7.3 Inline caches in the CacheIR model (D-003)

- Every op with `has_ic` names a slot in the function's **feedback
  vector**. A slot is `Uninit → Mono → Poly (≤ 4 stubs) → Mega`.
- A stub is a `CacheIrStub` cell: a short program in a small separate
  instruction set (`GuardShape`, `GuardIsInt32`, `LoadFixedSlot`,
  `LoadDynamicSlot`, `StoreFixedSlot`, `LoadProto`, `CallNative`, …)
  plus a side array of **stub fields** (shapes, slot offsets, atoms). The
  code part is shared between stubs that differ only in fields, so each
  distinct stub code is compiled once by the baseline JIT.
- The interpreter runs stubs with a small CacheIR interpreter and a
  hand-written fast path for the commonest code (`GuardShape` +
  `LoadFixedSlot`/`LoadDynamicSlot`).
- Megamorphic sites use one per-engine (shape, key) → slot cache.
- Arithmetic and comparison ops record type feedback (bit set of seen
  operand types) in their slot of the same vector.
- Phase 3: the baseline JIT compiles stub code to machine code. Phase 5:
  Warp builds SSA from the stubs a site has seen. The interpreter, the
  baseline JIT and Warp all consume the same stub format, which is the
  point of D-003.
- Phase 1 stubs hold their shapes strongly. Phase 5 makes them weak and
  purges stubs whose shapes die.

Task 1.8 builds shapes and property access without ICs; ICs arrive when
the interpreter is measured against QuickJS (1.15) or at the latest in
Phase 3, in this format.

## 8. Variables, environments and realms

- Scope analysis (oxc_semantic plus our pass, D-018) decides where each
  binding lives: bindings never captured by a closure are registers;
  captured ones are slots in heap `Environment` cells (parent reference +
  slots), addressed with `EnvSlot` operands.
- `with`, sloppy-mode direct `eval` and the global scope use dynamic
  lookup ops (`LoadName`, `StoreName`) that walk environment records,
  including object environments.
- TDZ: `let`/`const`/`class` bindings start as the internal uninitialized
  marker (§2); reads check it and throw `ReferenceError`.
- A **realm** cell holds its global object, global environment and an
  intrinsics array indexed by an `Intrinsic` enum. Each function records
  its realm.

## 9. Host integration

- Interrupts (D-007): one atomic interrupt word per engine, settable from
  any thread, checked at `JumpLoop` and function entry; handlers for time
  limits, termination and (later) the debugger.
- Nondeterminism (D-008): randomness, clocks and job ordering come from
  embedder hooks (task 1.11).
- Errors surface to the embedder as a rooted `Global` of the thrown value
  plus its phase (parse, early, runtime), matching what the Test262 runner
  needs (D-011).

## 10. What is deliberately left for later

- JIT code memory, W^X and trampolines (Phase 3, in `ferrojs-core`).
- The generational moving collector and concurrent marking (Phase 5).
- Bytecode and profile cache file format (Phase 4).
- Spec-generated built-ins (Phase 2).
- Debugger hooks (Phase 2) and the DevTools protocol (Phase 4).
