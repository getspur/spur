class User:
    def __init__(self, name):
        self.name = name

    def normalized(self):
        return self.name.lower()

    @staticmethod
    def from_parts(first, last):
        return f"{first} {last}"
