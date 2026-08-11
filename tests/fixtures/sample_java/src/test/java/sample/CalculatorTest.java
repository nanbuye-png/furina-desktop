package sample;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.Test;

class CalculatorTest {
    @Test
    void addReturnsSum() {
        assertEquals(5, Calculator.add(2, 3));
    }

    @Test
    void multiplyReturnsProduct() {
        assertEquals(12, Calculator.multiply(3, 4));
    }
}
