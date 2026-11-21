package org.portos.echoscreen;

import java.io.IOException;
import java.io.StringReader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

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
        lifecycle();
        System.out.println("camera JVM: signed vectors, tamper and stale proof, numeric pairing, largest JPEG mode, release");
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

    private static void lifecycle() {
        for (int acquired = 0; acquired <= 3; acquired++) {
            StillCapture.Lifetime operation = new StillCapture.Lifetime();
            List<Integer> closed = new ArrayList<>();
            for (int index = 1; index <= acquired; index++) {
                final int resource = index;
                check(operation.keep(() -> {
                    closed.add(resource);
                    if (resource == 2) throw new IOException();
                }));
            }
            check(operation.beginExposure() == (acquired == 3));
            check(!operation.beginExposure());
            check(operation.close() && operation.closed() && !operation.close());
            for (int index = 0; index < acquired; index++) check(closed.get(index) == acquired - index);
            check(!operation.keep(() -> closed.add(4)));
            check(closed.size() == acquired + 1 && closed.get(acquired) == 4);
            check(!operation.beginExposure());
        }
    }
}
