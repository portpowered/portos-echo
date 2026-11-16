package org.portos.echoscreen;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

/** T1 policy tests: direct calls, fake scheduler/store, no Android or sleeps. */
public final class ScreenTest {
    private static void equal(Object actual, Object expected) {
        if (!expected.equals(actual)) throw new AssertionError("policy assertion failed");
    }

    private static final class Store implements ScreenConfig.Store {
        Secret saved;
        int writes;
        public Secret read() { return saved; }
        public void save(Secret value) { saved = value; writes++; }
    }

    private static final class Port implements ScreenLoad.Port {
        final List<Long> delays = new ArrayList<>();
        final List<Long> loads = new ArrayList<>();
        int shown;
        int cancels;
        String message;
        Secret url;
        Runnable pending;
        public void cancel() { cancels++; pending = null; }
        public void waiting(String value) { message = value; }
        public void load(Secret value, long id) { url = value; loads.add(id); }
        public void schedule(long delay, Runnable action) { delays.add(delay); pending = action; }
        public void showPage() { shown++; }
    }

    private static ScreenConfig configured(Store store, String value) {
        return ScreenConfig.select(true, new Secret(value), store);
    }

    private static void retryPolicy() {
        RetryPolicy policy = new RetryPolicy();
        long[] expected = {1000, 2000, 4000, 8000, 16000, 30000, 30000};
        for (long value : expected) equal(policy.nextDelay(), value);
        for (int i = 0; i < 1000; i++) equal(policy.nextDelay(), 30000L);
        policy.reset();
        equal(policy.nextDelay(), 1000L);
        System.out.println("retry-policy: delays=1000,2000,4000,8000,16000,30000,30000; cap=30000; reset=1000");
    }

    private static void loadPolicy() {
        Port port = new Port();
        ScreenLoad load = new ScreenLoad(port);
        ScreenConfig config = configured(new Store(), "http://localhost/?token=synthetic-only");
        load.start(config);
        equal(port.loads, Arrays.asList(1L));
        load.error(1, false);
        equal(port.delays.size(), 0);
        load.error(1, true);
        load.error(1, true);
        load.finished(1);
        equal(port.message, "Waiting for bridge");
        equal(port.delays, Arrays.asList(1000L));
        equal(port.shown, 0);
        Runnable stale = port.pending;
        stale.run();
        equal(port.loads, Arrays.asList(1L, 2L));
        stale.run();
        load.error(1, true);
        load.finished(1);
        equal(port.loads, Arrays.asList(1L, 2L));
        load.error(2, true);
        equal(port.delays, Arrays.asList(1000L, 2000L));
        port.pending.run();
        load.finished(3);
        load.error(3, true);
        equal(port.shown, 1);
        equal(port.delays, Arrays.asList(1000L, 2000L));
        load.start(config);
        load.error(4, true);
        equal(port.delays, Arrays.asList(1000L, 2000L, 1000L));
        stale = port.pending;
        load.start(configured(new Store(), "https://example.invalid/?token=synthetic-replacement"));
        stale.run();
        load.finished(4);
        equal(port.loads, Arrays.asList(1L, 2L, 3L, 4L, 5L));
        equal(port.url.expose(), "https://example.invalid/?token=synthetic-replacement");
        load.error(5, true);
        stale = port.pending;
        load.destroy();
        stale.run();
        load.finished(5);
        load.error(5, true);
        load.start(config);
        equal(port.loads, Arrays.asList(1L, 2L, 3L, 4L, 5L));
        equal(port.pending == null, true);
        equal(port.shown, 1);
        equal(port.cancels, 6);
        System.out.println("screen-load: initial-retry=1; subframe-retry=0; loaded-retry=0; stale-load=0");
    }

    private static void configuration() {
        Store store = new Store();
        equal(ScreenConfig.select(false, null, store).message, "Screen URL is not configured");
        ScreenConfig first = configured(store, "http://localhost/?events=synthetic-only&actions=synthetic-only");
        equal(store.writes, 1);
        equal(ScreenConfig.select(false, null, store).url.expose(), first.url.expose());
        equal(store.writes, 1);
        equal(first.message, "Waiting for bridge");
        for (String invalid : new String[] {"", "relative", "file:///tmp/page", "http://fake@localhost/",
                "http://localhost/\r\n", "http://localhost:99999/", "https:///page"}) {
            ScreenConfig bad = configured(store, invalid);
            equal(bad.message, "Screen URL is invalid");
            equal(bad.url == null, true);
            equal(store.saved.expose(), first.url.expose());
        }
        equal(ScreenConfig.select(true, null, store).message, "Screen URL is invalid");
        ScreenConfig replacement = configured(store, "https://example.invalid/screen");
        equal(store.writes, 2);
        equal(store.saved.expose(), replacement.url.expose());
        equal(replacement.url.toString(), "[redacted]");
        equal(ScreenConfig.DIAGNOSTIC, "url=[redacted]");
        equal(ScreenConfig.TOP, 16);
        equal(ScreenConfig.BOTTOM, 10);
        Port port = new Port();
        ScreenLoad load = new ScreenLoad(port);
        load.start(first);
        load.error(1, true);
        Runnable stale = port.pending;
        load.start(ScreenConfig.select(true, null, store));
        stale.run();
        load.finished(1);
        equal(port.loads, Arrays.asList(1L));
        equal(port.shown, 0);
        equal(port.message, "Screen URL is invalid");
        equal(port.pending == null, true);
        System.out.println("screen-config: restore=true; top=16; bottom=10; url=[redacted]");
    }

    public static void main(String[] args) {
        retryPolicy();
        loadPolicy();
        configuration();
    }
}
