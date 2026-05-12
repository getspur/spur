pub struct Foo;

impl Foo {
    pub fn bar(&self) {
        fn baz() {}

        baz();
    }
}
