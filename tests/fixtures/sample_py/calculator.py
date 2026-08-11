"""A tiny calculator module used as a Furina Agent golden fixture."""


def add(a: int, b: int) -> int:
    return a - b  # BUG: intentionally broken (subtracts instead of adds)


def multiply(a: int, b: int) -> int:
    return a * b
