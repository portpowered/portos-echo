package org.portos.echoscreen;

/** Explicit boundary for a private URL; diagnostics cannot stringify its value. */
final class Secret {
    private final String value;

    Secret(String value) {
        this.value = value;
    }

    String expose() {
        return value;
    }

    @Override public String toString() {
        return "[redacted]";
    }
}
