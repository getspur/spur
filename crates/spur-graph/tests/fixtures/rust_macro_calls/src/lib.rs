fn foo() {}
fn bar() {}
fn baz(_: i32) {}
fn qux() -> i32 {
    1
}

fn a() {
    vec![foo(), bar()];
}

fn b() {
    json!({ "k": baz(1) });
}

fn c() {
    assert_eq!(qux(), 1);
}

fn d() {
    vec![1, 2, 3];
}

fn e() {
    my_macro!(other_macro!());
}
