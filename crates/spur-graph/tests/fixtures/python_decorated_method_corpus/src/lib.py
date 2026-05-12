class Foo:
    @property
    def name(self):
        return self._name

    @staticmethod
    def helper():
        pass

    @classmethod
    def from_str(cls, s):
        return cls()
