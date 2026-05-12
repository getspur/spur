class Foo:
    def outer(self):
        def inner():
            pass

        inner()
