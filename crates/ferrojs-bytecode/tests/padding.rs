//! The `padding-ops` feature (D-052) really adds its 150 ops, so the
//! dispatch gate (`tools/dispatch-flat.sh`, which runs this first)
//! compares two different op counts. Without the feature, nothing to check.

#[cfg(feature = "padding-ops")]
#[test]
fn padding_adds_150_ops_that_dispatch_through_the_table() {
    use ferrojs_bytecode::testing::ToyCtx;
    use ferrojs_bytecode::{step, Assembler, Flow, Limits, Opcode, Reg};
    use ferrojs_value::Value;

    let names: Vec<&str> = Opcode::ALL.iter().map(|op| op.name()).collect();
    let pads = names.iter().filter(|n| n.starts_with("Pad")).count();
    assert_eq!(pads, 150, "padding ops in {names:?}");
    // The last padding op runs through `step` like any other.
    let mut a = Assembler::new();
    a.pad149(Reg(0));
    let code = a.finish().unwrap();
    let mut cx = ToyCtx::new(&Limits {
        registers: 1,
        ..Limits::default()
    });
    assert_eq!(step(&mut cx, &code, 0), (Flow::Next, code.len()));
    assert_eq!(cx.regs[0], Value::from_i32(149));
}
