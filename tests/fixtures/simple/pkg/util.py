"""Utility helpers."""

CONSTANT = 42


def helper(value, *, flag=False):
    """Add one."""
    local = value + CONSTANT
    return local


class Greeter:
    """Greets."""

    greeting = "hello"

    def __init__(self, name):
        self.name = name

    def greet(self):
        return f"{self.greeting} {self.name}"
