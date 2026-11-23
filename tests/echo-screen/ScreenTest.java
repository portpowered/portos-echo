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
        int reads;
        int writes;
        public Secret read() { reads++; return saved; }
        public void save(Secret value) { saved = value; writes++; }
    }

    private static final class Port implements ScreenLoad.Port {
        final List<Long> loads = new ArrayList<>();
        boolean unlocked = true;
        int shown;
        String message = "";
        Secret url;
        Runnable pending;
        long now;
        long due;
        long activeAttempt;
        public boolean userUnlocked() { return unlocked; }
        public void cancel() { activeAttempt = 0; clearSchedule(); }
        public void waiting(String value) { message = value; }
        public void load(Secret value, long id) {
            equal(activeAttempt, 0L);
            equal(message, ScreenConfig.WAITING);
            activeAttempt = id;
            url = value;
            loads.add(id);
        }
        public void schedule(long delay, Runnable action) {
            equal(pending == null, true);
            due = now + delay;
            pending = action;
        }
        public void clearSchedule() { pending = null; }
        public void showPage() { shown++; message = ""; }
        void advanceTo(long time) {
            while (pending != null && due <= time) {
                now = due;
                Runnable action = pending;
                pending = null;
                action.run();
            }
            now = time;
        }
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
        load.start(() -> config);
        equal(port.loads, Arrays.asList(1L));
        load.error(1, false);
        equal(port.due, 4000L);
        load.error(1, true);
        load.error(1, true);
        load.finished(1);
        equal(port.message, "Waiting for bridge");
        equal(port.due, 1000L);
        equal(port.shown, 0);
        Runnable stale = port.pending;
        port.advanceTo(1000);
        equal(port.loads, Arrays.asList(1L, 2L));
        stale.run();
        load.error(1, true);
        load.finished(1);
        equal(port.loads, Arrays.asList(1L, 2L));
        load.error(2, true);
        equal(port.due, 3000L);
        port.advanceTo(3000);
        load.finished(3);
        load.finished(3);
        load.error(3, true);
        equal(port.shown, 1);
        equal(port.pending == null, true);
        load.start(() -> config);
        load.error(4, true);
        equal(port.due, 4000L);
        stale = port.pending;
        load.start(() -> configured(new Store(), "https://example.invalid/?token=synthetic-replacement"));
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
        load.rendererGone(5);
        load.start(() -> config);
        equal(port.loads, Arrays.asList(1L, 2L, 3L, 4L, 5L));
        equal(port.pending == null, true);
        equal(port.shown, 1);
        System.out.println("screen-load: initial-retry=1; subframe-retry=0; loaded-retry=0; stale-load=0");
    }

    private static void stalledLoad() {
        Port port = new Port();
        ScreenLoad load = new ScreenLoad(port);
        load.start(() -> configured(new Store(), "http://localhost/"));
        equal(port.message, "Waiting for bridge");
        port.advanceTo(4000);
        equal(port.activeAttempt, 0L);
        load.finished(1);
        port.advanceTo(5000);
        equal(port.message, "Waiting for bridge");
        equal(port.loads, Arrays.asList(1L, 2L));
        equal(port.shown, 0);
        load.finished(2);
        load.finished(2);
        port.advanceTo(9000);
        equal(port.message, "");
        equal(port.shown, 1);
        equal(port.loads, Arrays.asList(1L, 2L));
        load.rendererGone(2);
        equal(port.message, "Waiting for bridge");
        port.advanceTo(10000);
        equal(port.loads, Arrays.asList(1L, 2L, 3L));
        System.out.println("screen-stall: waiting-at=0,5000ms; retired-at=4000ms; retry-at=5000ms; cleared=1");
    }

    private static void directBoot() {
        Port port = new Port();
        port.unlocked = false;
        ScreenLoad load = new ScreenLoad(port);
        Store store = new Store();
        int[] setupCalls = {0};
        load.start(() -> ScreenConfig.select(false, null, store));
        load.start(() -> {
            setupCalls[0]++;
            return configured(store, "http://localhost/");
        });
        load.onUserUnlocked();
        equal(port.message, "Waiting for bridge");
        equal(store.reads, 0);
        equal(store.writes, 0);
        equal(setupCalls[0], 0);
        equal(port.loads.size(), 0);
        port.unlocked = true;
        load.onUserUnlocked();
        load.onUserUnlocked();
        equal(setupCalls[0], 1);
        equal(store.reads, 0);
        equal(store.writes, 1);
        equal(port.loads, Arrays.asList(1L));

        Port destroyedPort = new Port();
        destroyedPort.unlocked = false;
        ScreenLoad destroyed = new ScreenLoad(destroyedPort);
        destroyed.start(() -> ScreenConfig.select(false, null, store));
        destroyed.destroy();
        destroyedPort.unlocked = true;
        destroyed.onUserUnlocked();
        equal(store.reads, 0);
        equal(destroyedPort.loads.size(), 0);
        System.out.println("screen-unlock: locked-reads=0; locked-writes=0; locked-setup=0; locked-loads=0; unlocked-loads=1");
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
        load.start(() -> ScreenConfig.select(false, null, new Store()));
        equal(port.message, "Screen URL is not configured");
        equal(port.loads.size(), 0);
        load.start(() -> ScreenConfig.select(false, null, store));
        equal(port.url.expose(), replacement.url.expose());
        load.error(1, true);
        Runnable stale = port.pending;
        load.start(() -> ScreenConfig.select(true, null, store));
        stale.run();
        load.finished(1);
        equal(port.loads, Arrays.asList(1L));
        equal(port.shown, 0);
        equal(port.message, "Screen URL is invalid");
        equal(port.pending == null, true);
        System.out.println("screen-config: restore=true; top=16; bottom=10; url=[redacted]");
    }

    private static void chrome() {
        for (boolean fullPanel : new boolean[] {false, true}) {
            ScreenConfig.Chrome legacy = ScreenConfig.chrome(true, fullPanel, true);
            equal(legacy.top, 16);
            equal(legacy.bottom, 10);
        }
        equal(ScreenConfig.chrome(false, false, true), ScreenConfig.Chrome.LEGACY);
        equal(ScreenConfig.chrome(false, true, false), ScreenConfig.Chrome.LEGACY);
        ScreenConfig.Chrome page = ScreenConfig.chrome(false, true, true);
        equal(page.top, 0);
        equal(page.bottom, 0);
        System.out.println("screen-chrome: legacy=16/10; uncertain or untrusted=legacy; confirmed=0/0");
    }

    public static void main(String[] args) {
        retryPolicy();
        loadPolicy();
        stalledLoad();
        directBoot();
        configuration();
        chrome();
    }
}
