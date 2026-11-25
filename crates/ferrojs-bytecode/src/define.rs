//! The op-definition macro (D-020, D-031).
//!
//! `define_ops!` takes the list of every op and generates, from that one
//! list: the `Opcode` enum and its metadata table, one operand struct per
//! op (in `operands`), the decoded `Instruction` enum, the decoder, the
//! encoder (`Instruction::encode`) and one `Assembler` method per op, and
//! one handler function per op (in `handlers`) plus the `execute`
//! dispatch, and one step function per op (decode and run, in `steps`)
//! behind the table-driven `step` the interpreter uses (D-052). Handlers are generic over [`OpCtx`](crate::OpCtx), so later
//! tiers can instantiate the same bodies.
//!
//! Syntax of one op:
//!
//! ```text
//! /// doc comment
//! Name(snake_name) { field: Kind, ... }
//!     flags(flag, ...)
//!     handler(cx) { body returning Flow }
//! ```
//!
//! `snake_name` names the assembler method and the handler function
//! (`macro_rules!` can't change case). Operand fields are in scope in the
//! body by name; `cx` is `&mut impl OpCtx`. Flags: `may_throw`, `may_gc`,
//! `has_ic`, `jump`, `branch`, `loop_edge`, `terminator`.

use core::fmt;

use crate::operand::OperandKind;

/// Static facts about an op, used by the verifier, the disassembler and
/// (later) the JIT.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Flags(u8);

impl Flags {
    pub const NONE: Flags = Flags(0);
    /// May throw an exception (including an interrupt termination).
    pub const MAY_THROW: Flags = Flags(1 << 0);
    /// May allocate, collect, or run user code.
    pub const MAY_GC: Flags = Flags(1 << 1);
    /// Has an `Ic` operand it records feedback into.
    pub const HAS_IC: Flags = Flags(1 << 2);
    /// Unconditional jump: never falls through.
    pub const JUMP: Flags = Flags(1 << 3);
    /// Conditional jump: may fall through.
    pub const BRANCH: Flags = Flags(1 << 4);
    /// Loop back-edge: the only kind of jump allowed to go backward, and
    /// the one that checks for interrupts (D-007).
    pub const LOOP_EDGE: Flags = Flags(1 << 5);
    /// Leaves the function (return or throw): never falls through.
    pub const TERMINATOR: Flags = Flags(1 << 6);

    #[inline]
    pub const fn with(self, other: Flags) -> Flags {
        Flags(self.0 | other.0)
    }

    #[inline]
    pub const fn contains(self, other: Flags) -> bool {
        self.0 & other.0 == other.0
    }

    #[inline]
    pub const fn intersects(self, other: Flags) -> bool {
        self.0 & other.0 != 0
    }

    /// True if execution never continues with the next instruction.
    #[inline]
    pub const fn ends_block(self) -> bool {
        self.intersects(Flags::JUMP.with(Flags::TERMINATOR))
    }
}

impl fmt::Display for Flags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const NAMES: [(Flags, &str); 7] = [
            (Flags::MAY_THROW, "may_throw"),
            (Flags::MAY_GC, "may_gc"),
            (Flags::HAS_IC, "has_ic"),
            (Flags::JUMP, "jump"),
            (Flags::BRANCH, "branch"),
            (Flags::LOOP_EDGE, "loop_edge"),
            (Flags::TERMINATOR, "terminator"),
        ];
        let mut first = true;
        for (flag, name) in NAMES {
            if self.contains(flag) {
                if !first {
                    f.write_str(", ")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        Ok(())
    }
}

/// Metadata for one op.
#[derive(Clone, Copy, Debug)]
pub struct OpInfo {
    pub name: &'static str,
    /// Operand kinds in encoding order.
    pub operands: &'static [OperandKind],
    pub flags: Flags,
    /// Total operand units (an `EnvSlot` counts two).
    pub units: usize,
}

macro_rules! __op_flag {
    (may_throw) => {
        $crate::Flags::MAY_THROW
    };
    (may_gc) => {
        $crate::Flags::MAY_GC
    };
    (has_ic) => {
        $crate::Flags::HAS_IC
    };
    (jump) => {
        $crate::Flags::JUMP
    };
    (branch) => {
        $crate::Flags::BRANCH
    };
    (loop_edge) => {
        $crate::Flags::LOOP_EDGE
    };
    (terminator) => {
        $crate::Flags::TERMINATOR
    };
}

/// Defines every op; see the module docs. Invoked once, in `ops.rs`.
macro_rules! define_ops {
    ($(
        $(#[$attr:meta])*
        $name:ident ( $snake:ident ) { $( $field:ident : $kind:ident ),* $(,)? }
            flags( $( $flag:ident ),* $(,)? )
            handler( $cx:ident ) $body:block
    )*) => {
        /// Every op, numbered from 0 in definition order.
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
        #[repr(u8)]
        pub enum Opcode {
            $( $(#[$attr])* $name, )*
        }

        static OP_INFO: &[$crate::OpInfo] = &[
            $( $crate::OpInfo {
                name: stringify!($name),
                operands: &[ $( <$kind as $crate::Operand>::KIND ),* ],
                flags: $crate::Flags::NONE $( .with(__op_flag!($flag)) )*,
                units: 0 $( + <$kind as $crate::Operand>::KIND.units() )*,
            }, )*
        ];

        impl Opcode {
            /// All ops in opcode order: `ALL[op as usize] == op`.
            pub const ALL: &'static [Opcode] = &[ $( Opcode::$name ),* ];

            /// The op with this opcode byte, if any.
            #[inline]
            pub fn from_byte(b: u8) -> Option<Opcode> {
                Self::ALL.get(b as usize).copied()
            }

            #[inline]
            pub fn info(self) -> &'static $crate::OpInfo {
                &OP_INFO[self as usize]
            }

            #[inline]
            pub fn name(self) -> &'static str {
                self.info().name
            }

            #[inline]
            pub fn flags(self) -> $crate::Flags {
                self.info().flags
            }
        }

        // Opcode bytes must stay clear of the prefix bytes.
        const _: () = assert!(Opcode::ALL.len() <= $crate::PREFIX_WIDE as usize);

        /// One struct per op holding its decoded operands.
        pub mod operands {
            #[allow(unused_imports)]
            use super::*;
            $(
                $(#[$attr])*
                #[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
                pub struct $name { $( pub $field: $kind ),* }
            )*
        }

        /// A decoded instruction.
        #[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
        pub enum Instruction {
            $( $name(operands::$name), )*
        }

        impl Instruction {
            #[inline]
            pub fn opcode(&self) -> Opcode {
                match self { $( Instruction::$name(_) => Opcode::$name, )* }
            }

            /// Calls `f` with each operand, in encoding order.
            #[allow(unused_variables)]
            pub fn for_each_operand(&self, f: &mut dyn FnMut($crate::OperandValue)) {
                match self {
                    $( Instruction::$name(o) => {
                        $( f($crate::Operand::value(o.$field)); )*
                    } )*
                }
            }

            #[allow(unused_variables)]
            pub(crate) fn push_units(&self, out: &mut Vec<$crate::Unit>) {
                match self {
                    $( Instruction::$name(o) => {
                        $( $crate::Operand::push_units(o.$field, out); )*
                    } )*
                }
            }
        }

        #[allow(unused_variables)]
        pub(crate) fn decode_operands(
            op: Opcode,
            r: &mut $crate::Reader<'_>,
        ) -> Result<Instruction, $crate::DecodeErrorKind> {
            Ok(match op {
                $( Opcode::$name => Instruction::$name(operands::$name {
                    $( $field: <$kind as $crate::Operand>::read(r)?, )*
                }), )*
            })
        }

        impl $crate::Assembler {
            $(
                $(#[$attr])*
                pub fn $snake(&mut self $(, $field: <$kind as $crate::Operand>::Asm )*) {
                    self.begin(Opcode::$name);
                    $( <$kind as $crate::Operand>::push_asm($field, self.units_mut()); )*
                    self.end();
                }
            )*
        }

        /// One handler per op: the fast path, calling named runtime
        /// functions on the context for the rest.
        pub mod handlers {
            #[allow(unused_imports)]
            use super::*;
            $(
                $(#[$attr])*
                #[inline]
                #[allow(unused_variables)]
                pub fn $snake<C: $crate::OpCtx + ?Sized>(
                    $cx: &mut C,
                    operands::$name { $( $field ),* }: operands::$name,
                ) -> $crate::Flow $body
            )*
        }

        /// Runs one decoded instruction against `cx`.
        #[inline]
        pub fn execute<C: $crate::OpCtx + ?Sized>(cx: &mut C, insn: Instruction) -> $crate::Flow {
            match insn {
                $( Instruction::$name(o) => handlers::$snake(cx, o), )*
            }
        }

        /// One step function per op (D-052): decodes the op's narrow
        /// operands and runs its handler. Each is its own function, so how
        /// one op compiles doesn't depend on how many others there are.
        pub mod steps {
            #[allow(unused_imports)]
            use super::*;
            $(
                $(#[$attr])*
                #[allow(unused_mut)]
                pub fn $snake<C: $crate::OpCtx + ?Sized>(cx: &mut C, code: &[u8], pc: usize) -> ($crate::Flow, usize) {
                    let mut r = $crate::Reader::narrow(code, pc + 1);
                    let o = operands::$name {
                        $( $field: <$kind as $crate::Operand>::read(&mut r)
                            .expect("verified bytecode decodes"), )*
                    };
                    (handlers::$snake(cx, o), r.pos() - pc)
                }
            )*
        }

        /// The step function of every op, by opcode byte.
        pub struct StepTable<C: ?Sized>(core::marker::PhantomData<C>);

        impl<C: $crate::OpCtx + ?Sized> StepTable<C> {
            #[allow(clippy::type_complexity)]
            pub const STEPS: [fn(&mut C, &[u8], usize) -> ($crate::Flow, usize); Opcode::ALL.len()] =
                [ $( steps::$snake::<C>, )* ];
        }

        /// Decodes and runs the instruction at `pc` of verified code
        /// (D-052): one indexed call through [`StepTable`] for narrow
        /// instructions, [`decode`](crate::decode) and [`execute`] for
        /// prefixed ones. Returns the flow and the instruction's length.
        /// The cost per instruction doesn't grow with the number of ops.
        ///
        /// # Panics
        /// If the code at `pc` doesn't decode (it must be verified).
        #[inline]
        pub fn step<C: $crate::OpCtx + ?Sized>(cx: &mut C, code: &[u8], pc: usize) -> ($crate::Flow, usize) {
            match StepTable::<C>::STEPS.get(code[pc] as usize) {
                Some(f) => f(cx, code, pc),
                None => step_prefixed(cx, code, pc),
            }
        }

        #[cold]
        #[inline(never)]
        fn step_prefixed<C: $crate::OpCtx + ?Sized>(cx: &mut C, code: &[u8], pc: usize) -> ($crate::Flow, usize) {
            let (insn, len) = $crate::decode(code, pc).expect("verified bytecode decodes");
            (execute(cx, insn), len)
        }
    };
}
