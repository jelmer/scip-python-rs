"""Module docstring."""

# A leading comment.
import os.path

TOTAL: int = 0


@staticmethod
def compute(items, *args, scale=1.0, **kwargs):
    # Body comment.
    total = len(args) + len(kwargs)
    for item in items:
        if (n := len(item)) > 2 and item is not None:
            total += n * scale
    match total:
        case 0:
            pass
        case _:
            total *= 2
    del items
    return f"{total:>{scale}} of {os.path.sep!r}"


class Widget(os.PathLike):
    kind = "widget"

    async def render(self) -> str:
        try:
            return self.kind
        except ValueError as exc:
            raise RuntimeError("bad") from exc


# `match` and `type` are soft keywords: plain names outside their statements.
match = compute
type = TOTAL
