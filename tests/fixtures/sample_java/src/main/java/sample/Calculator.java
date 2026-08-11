package sample;

public class Calculator {
    public static int add(int a, int b) {
        return a - b; // BUG: intentionally broken (subtracts instead of adds)
    }

    public static int multiply(int a, int b) {
        return a * b;
    }
}
