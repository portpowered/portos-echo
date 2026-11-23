package org.portos.echoscreen;

import java.util.function.Supplier;

/** Main-thread state machine; callbacks carry an attempt identity, never a URL. */
final class ScreenLoad {
    private static final long LOAD_TIMEOUT_MS = 4000;

    interface Port {
        boolean userUnlocked();
        void cancel();
        void waiting(String message);
        void load(Secret url, long attempt);
        void schedule(long delay, Runnable action);
        void clearSchedule();
        void showPage();
    }

    private enum Phase { IDLE, LOADING, BACKOFF, SHOWING, DESTROYED }

    private final Port port;
    private final RetryPolicy retry = new RetryPolicy();
    private Supplier<ScreenConfig> pendingConfig;
    private Secret url;
    private long attempt;
    private Phase phase = Phase.IDLE;

    ScreenLoad(Port port) {
        this.port = port;
    }

    void start(Supplier<ScreenConfig> configuration) {
        if (phase == Phase.DESTROYED) return;
        phase = Phase.IDLE;
        port.cancel();
        url = null;
        pendingConfig = configuration;
        retry.reset();
        port.waiting(ScreenConfig.WAITING);
        onUserUnlocked();
    }

    void onUserUnlocked() {
        if (pendingConfig == null || !port.userUnlocked()) return;
        Supplier<ScreenConfig> configuration = pendingConfig;
        pendingConfig = null;
        ScreenConfig config = configuration.get();
        url = config.url;
        if (url == null) {
            port.waiting(config.message);
        } else {
            begin();
        }
    }

    private void begin() {
        phase = Phase.LOADING;
        long id = ++attempt;
        port.schedule(LOAD_TIMEOUT_MS, () -> error(id, true));
        port.load(url, id);
    }

    void error(long id, boolean mainFrame) {
        if (id == attempt && mainFrame && phase == Phase.LOADING) waitForRetry(id);
    }

    void rendererGone(long id) {
        if (id == attempt && (phase == Phase.LOADING || phase == Phase.SHOWING)) waitForRetry(id);
    }

    private void waitForRetry(long id) {
        phase = Phase.BACKOFF;
        port.cancel();
        port.waiting(ScreenConfig.WAITING);
        port.schedule(retry.nextDelay(), () -> retry(id));
    }

    private void retry(long id) {
        if (id == attempt && phase == Phase.BACKOFF) begin();
    }

    void finished(long id) {
        if (id != attempt || phase != Phase.LOADING) return;
        phase = Phase.SHOWING;
        port.clearSchedule();
        retry.reset();
        port.showPage();
    }

    void destroy() {
        phase = Phase.DESTROYED;
        pendingConfig = null;
        url = null;
        port.cancel();
    }
}
