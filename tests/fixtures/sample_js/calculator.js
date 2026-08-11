// A tiny calculator module used as a Furina Agent golden fixture.
export function add(a, b) {
  return a - b; // BUG: intentionally broken (subtracts instead of adds)
}

export function multiply(a, b) {
  return a * b;
}
