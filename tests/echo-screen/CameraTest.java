package org.portos.echoscreen;

import java.io.IOException;
import java.io.StringReader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Paths;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.List;
import java.util.function.BiConsumer;
import java.util.function.Consumer;

public final class CameraTest {
    private static void check(boolean condition) {
        if (!condition) throw new AssertionError("native camera policy differs");
    }

    private static void declined(CameraWire wire, String frame) throws Exception {
        try {
            wire.frame(new StringReader(frame));
        } catch (IOException refused) {
            return;
        }
        throw new AssertionError("invalid camera frame accepted");
    }

    public static void main(String[] args) throws Exception {
        String nonce = "0123456789abcdef0123456789abcdef";
        CameraWire wire = new CameraWire(new Secret("synthetic-screen-pair"), nonce);
        List<String> vectors = Files.readAllLines(Paths.get(args[0]), StandardCharsets.UTF_8);
        check(wire.attach().equals(vectors.get(0).split("\t")[0]));
        for (int index = 1; index < 4; index++) {
            String[] fields = vectors.get(index).split("\t");
            String frame = sse(vectors.get(index));
            CameraWire.Frame verified = wire.frame(new StringReader(frame));
            check(fields[3].equals(verified.event));
            if (index == 2) {
                check(nonce.equals(verified.id) && "0".equals(verified.camera) && verified.remaining == 2999);
                declined(wire, frame.replace("2999", "2998"));
            }
            declined(wire, frame.replace("id: " + fields[0] + "\n", ""));
            declined(new CameraWire(new Secret("synthetic-screen-pair"), "00000000000000000000000000000000"),
                    frame);
        }
        byte[] busy = "{\"error\":\"busy\"}".getBytes(StandardCharsets.UTF_8);
        check(wire.result(nonce, "application/json", "", "", "", busy)
                .equals(vectors.get(4).split("\t")[0]));
        replay(wire, sse(vectors.get(1)), sse(vectors.get(2)));
        pairing();
        modes();
        captureAdmission();
        captureFailures();
        captureCancellation();
        System.out.println("camera JVM: signed vectors, pairing, zero opens on refusal, one exposure, typed failures, deadline, cleanup and recovery");
    }

    private static String sse(String vector) {
        String[] fields = vector.split("\t");
        return "event: " + fields[3] + "\nid: " + fields[0] + "\ndata: " + fields[4] + "\n\n";
    }

    private static void replay(CameraWire wire, String ready, String capture) throws Exception {
        try {
            wire.new Stream().next(new StringReader(capture));
            throw new AssertionError("capture preceded signed ready");
        } catch (IOException refused) {
            // The first authenticated event must establish the stream.
        }
        CameraWire.Stream stream = wire.new Stream();
        StringReader frames = new StringReader(ready + capture + capture);
        check("ready".equals(stream.next(frames).event));
        check("capture".equals(stream.next(frames).event));
        try {
            stream.next(frames);
            throw new AssertionError("replayed capture accepted");
        } catch (IOException refused) {
            // The authenticated capture ID is single-use within its session.
        }
    }

    private static void pairing() {
        String query = "/?events=%2Fevents%3Ftoken%3Dsample%252Bsynthetic"
                + "&actions=%2Factions%3Ftoken%3Dsample%252Bsynthetic";
        CameraPairing paired = CameraPairing.parse(new Secret("http://127.0.0.1:16900" + query));
        check(paired != null && paired.origin.equals("http://127.0.0.1:16900")
                && paired.token.expose().equals("sample+synthetic"));
        for (String rejected : Arrays.asList("localhost:16900", "127.0.0.1:0", "user@127.0.0.1:16900")) {
            check(CameraPairing.parse(new Secret("http://" + rejected + query)) == null);
        }
        check(CameraPairing.parse(new Secret("http://127.0.0.1:16900" + query + "&events=other")) == null);
        check(CameraPairing.parse(new Secret("http://127.0.0.1:16900" + query.replace("%2Factions", "%2Fevents"))) == null);
    }

    private static void modes() {
        StillCapture.Mode best = new StillCapture.Mode(1920, 1080);
        check(StillCapture.Mode.largest(Arrays.asList(new StillCapture.Mode(2000, 1080),
                new StillCapture.Mode(1440, 1080), new StillCapture.Mode(1920, 1081), best)) == best);
        check(StillCapture.Mode.largest(Arrays.asList(new StillCapture.Mode(0, 100),
                new StillCapture.Mode(10, 1200))) == null);
        byte[] jpeg = {(byte) 255, (byte) 216, (byte) 255, (byte) 217};
        check(StillCapture.Result.jpeg(jpeg, 1920, 1080, 125).jpeg == jpeg);
        check(StillCapture.Result.jpeg(jpeg, 1921, 1080, 125).failure == StillCapture.Failure.CAMERA);
    }

    private static void captureAdmission() {
        CameraFake fake = new CameraFake();
        fake.start("invalid", 3000);
        fake.assertFailure(StillCapture.Failure.CAMERA);
        fake.permitted = false;
        fake.start("0", 3000);
        fake.assertFailure(StillCapture.Failure.PERMISSION);
        fake.permitted = true;
        fake.start("0", 0);
        fake.assertFailure(StillCapture.Failure.TIMEOUT);
        check(fake.opens == 0 && fake.exposures == 0 && fake.closed.isEmpty());
        fake.start("0", 3000);
        fake.expose();
        fake.sessionReady.accept(fake.session);
        fake.imageReady.run();
        fake.imageReady.run();
        fake.assertJpeg();
        check(fake.opens == 1 && fake.exposures == 1 && fake.reads == 1);
        check(fake.closed.equals(Arrays.asList("session", "device", "reader")));
        check(fake.clock.deadline == null);
    }

    private static void captureFailures() {
        CameraFake fake = new CameraFake();
        fake.start("0", 3000);
        fake.deviceFailed.accept(fake.device, StillCapture.Failure.BUSY);
        fake.assertFailure(StillCapture.Failure.BUSY);
        check(fake.exposures == 0 && fake.closed.equals(Arrays.asList("device", "reader")));

        fake.closed.clear();
        fake.failDeviceClose = true;
        fake.start("0", 3000);
        fake.deviceReady.accept(fake.device);
        fake.sessionFailed.accept(fake.session);
        fake.assertFailure(StillCapture.Failure.CAMERA);
        check(fake.exposures == 0 && fake.closed.equals(Arrays.asList("session", "device", "reader")));
        fake.failDeviceClose = false;

        fake.closed.clear();
        fake.start("0", 3000);
        fake.expose();
        fake.exposureFailed.run();
        fake.assertFailure(StillCapture.Failure.CAMERA);
        check(fake.exposures == 1 && fake.closed.equals(Arrays.asList("session", "device", "reader")));

        fake.closed.clear();
        fake.start("0", 3000);
        fake.expose();
        fake.clock.advance(2999);
        check(fake.results.isEmpty() && fake.closed.isEmpty());
        fake.clock.advance(1);
        fake.imageReady.run();
        fake.assertFailure(StillCapture.Failure.TIMEOUT);
        check(fake.opens == 4 && fake.exposures == 2 && fake.reads == 0);
        check(fake.closed.equals(Arrays.asList("session", "device", "reader")));
    }

    private static void captureCancellation() {
        CameraFake fake = new CameraFake();
        fake.start("0", 3000).close();
        fake.clock.run();
        check(fake.closed.equals(Arrays.asList("reader")));
        fake.deviceReady.accept(fake.device);
        check(fake.closed.equals(Arrays.asList("reader", "device")) && fake.sessionReady == null);

        fake.closed.clear();
        StillCapture.Cancel configuring = fake.start("0", 3000);
        fake.deviceReady.accept(fake.device);
        configuring.close();
        fake.clock.run();
        fake.sessionReady.accept(fake.session);
        check(fake.closed.equals(Arrays.asList("device", "reader", "session")) && fake.exposures == 0);

        fake.closed.clear();
        StillCapture.Cancel exposing = fake.start("0", 3000);
        fake.expose();
        Runnable staleImage = fake.imageReady;
        exposing.close();
        fake.clock.run();
        check(fake.closed.equals(Arrays.asList("session", "device", "reader")) && fake.results.isEmpty());

        fake.closed.clear();
        fake.start("0", 3000);
        fake.expose();
        staleImage.run();
        check(fake.closed.isEmpty() && fake.results.isEmpty() && fake.reads == 0);
        fake.imageReady.run();
        fake.assertJpeg();
        check(fake.opens == 4 && fake.exposures == 2 && fake.reads == 1);
        check(fake.closed.equals(Arrays.asList("session", "device", "reader")));
    }

    private static final class Clock implements StillCapture.Scheduler {
        final ArrayDeque<Runnable> tasks = new ArrayDeque<>();
        long now;
        long expires;
        Runnable deadline;

        @Override public long now() { return now; }
        @Override public void post(Runnable task) { tasks.add(task); }
        @Override public void postDelayed(Runnable task, long delay) {
            check(deadline == null);
            deadline = task;
            expires = now + delay;
        }
        @Override public void remove(Runnable task) {
            if (deadline == task) deadline = null;
        }

        void run() {
            while (!tasks.isEmpty()) tasks.remove().run();
        }

        void advance(long elapsed) {
            now += elapsed;
            if (deadline != null && now >= expires) deadline.run();
            run();
        }
    }

    private static final class CameraFake implements StillCapture.Backend {
        final byte[] jpeg = {(byte) 255, (byte) 216, (byte) 255, (byte) 217};
        final Clock clock = new Clock();
        final StillCapture camera = new StillCapture.SingleShot(this, clock);
        final List<String> closed = new ArrayList<>();
        final List<StillCapture.Result> results = new ArrayList<>();
        boolean permitted = true;
        boolean failDeviceClose;
        int opens;
        int exposures;
        int reads;
        Consumer<StillCapture.Device> deviceReady;
        BiConsumer<StillCapture.Device, StillCapture.Failure> deviceFailed;
        Consumer<StillCapture.Session> sessionReady;
        Consumer<StillCapture.Session> sessionFailed;
        Runnable exposureFailed;
        Runnable imageReady;

        final StillCapture.Device device = new StillCapture.Device() {
            @Override public void configure(StillCapture.Reader reader, Consumer<StillCapture.Session> ready,
                    Consumer<StillCapture.Session> failed) {
                sessionReady = ready;
                sessionFailed = failed;
            }
            @Override public void close() {
                closed.add("device");
                if (failDeviceClose) throw new IllegalStateException();
            }
        };
        final StillCapture.Session session = new StillCapture.Session() {
            @Override public void expose(Runnable failed) {
                exposures++;
                exposureFailed = failed;
            }
            @Override public void close() { closed.add("session"); }
        };

        @Override public boolean permitted() { return permitted; }

        @Override public Iterable<StillCapture.Mode> modes(String id) {
            if (!"0".equals(id)) return Collections.emptyList();
            return Arrays.asList(new StillCapture.Mode(4000, 3000),
                    new StillCapture.Mode(640, 480), new StillCapture.Mode(1920, 1080));
        }

        @Override public StillCapture.Reader reader(StillCapture.Mode mode, Runnable ready) {
            check(mode.width == 1920 && mode.height == 1080);
            imageReady = ready;
            return new StillCapture.Reader() {
                @Override public StillCapture.Result read() {
                    reads++;
                    return StillCapture.Result.jpeg(jpeg, mode.width, mode.height, 125);
                }
                @Override public void close() { closed.add("reader"); }
            };
        }

        @Override public void open(String id, Consumer<StillCapture.Device> opened,
                BiConsumer<StillCapture.Device, StillCapture.Failure> failed) {
            check("0".equals(id));
            opens++;
            deviceReady = opened;
            deviceFailed = failed;
        }

        StillCapture.Cancel start(String id, int timeout) {
            StillCapture.Cancel cancel = camera.start(id, timeout, results::add);
            clock.run();
            return cancel;
        }

        void expose() {
            deviceReady.accept(device);
            sessionReady.accept(session);
        }

        void assertFailure(StillCapture.Failure expected) {
            check(results.size() == 1);
            StillCapture.Result result = results.remove(0);
            check(result.failure == expected && result.jpeg == null);
        }

        void assertJpeg() {
            check(results.size() == 1);
            StillCapture.Result result = results.remove(0);
            check(result.failure == null && result.jpeg == jpeg && result.width == 1920
                    && result.height == 1080 && result.sampledAt == 125);
        }
    }
}
