package org.portos.echoscreen;

/** Main-thread state machine; callbacks carry an attempt identity, never a URL. */
final class ScreenLoad {
    interface Port {
        void cancel();
        void waiting(String message);
        void load(Secret url, long attempt);
        void schedule(long delay, Runnable retry);
        void showPage();
    }

    private final Port port;
    private final RetryPolicy retry = new RetryPolicy();
    private Secret url;
    private long attempt;
    private boolean failed;
    private boolean loaded;
    private boolean destroyed;

    ScreenLoad(Port port) {
        this.port = port;
    }

    void start(ScreenConfig config) {
        if (destroyed) return;
        attempt++;
        port.cancel();
        url = config.url;
        failed = false;
        loaded = false;
        retry.reset();
        port.waiting(config.message);
        if (url != null) port.load(url, attempt);
    }

    void error(long id, boolean mainFrame) {
        if (destroyed || id != attempt || !mainFrame || loaded || failed || url == null) return;
        failed = true;
        port.waiting("Waiting for bridge");
        port.schedule(retry.nextDelay(), () -> retry(id));
    }

    private void retry(long id) {
        if (destroyed || id != attempt || !failed) return;
        attempt++;
        failed = false;
        port.cancel();
        port.load(url, attempt);
    }

    void finished(long id) {
        if (destroyed || id != attempt || failed || url == null) return;
        loaded = true;
        retry.reset();
        port.showPage();
    }

    void destroy() {
        destroyed = true;
        attempt++;
        url = null;
        port.cancel();
    }
}
