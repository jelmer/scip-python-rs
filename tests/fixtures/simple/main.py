import os

from pkg import Greeter, helper


def main():
    greeter = Greeter("world")
    print(greeter.greet())
    return helper(1)


home = os.getenv("HOME")
message = "hello world".upper()
