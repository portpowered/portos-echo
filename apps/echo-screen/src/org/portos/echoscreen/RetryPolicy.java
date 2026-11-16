package org.portos.echoscreen;

/** Initial page-load backoff. Time and scheduling belong to the caller. */
final class RetryPolicy {
    private long delay = 1000;

    long nextDelay() {
        long result = delay;
        delay = Math.min(30000, delay * 2);
        return result;
    }

    void reset() {
        delay = 1000;
    }
}
