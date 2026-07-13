import os

from pkg import Greeter, helper


def main():
    greeter = Greeter("world")
    print(greeter.greet())
    return helper(1)
