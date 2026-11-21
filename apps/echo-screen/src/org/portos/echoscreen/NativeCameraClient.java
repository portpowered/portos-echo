package org.portos.echoscreen;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.Proxy;
import java.net.URL;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.security.GeneralSecurityException;
import java.security.SecureRandom;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/** Authenticated HTTP is owned by the Activity, separately from the WebView. */
final class NativeCameraClient implements AutoCloseable {
    private final CameraPairing pairing;
    private final StillCapture camera;
    private final ExecutorService results = Executors.newSingleThreadExecutor();
    private final Thread listener = new Thread(this::listen, "echo-camera-stream");
    private HttpURLConnection stream;
    private HttpURLConnection upload;
    private Active active;
    private Active reporting;
    private boolean closed;

    NativeCameraClient(CameraPairing pairing, StillCapture camera) {
        this.pairing = pairing;
        this.camera = camera;
    }

    void start() {
        listener.start();
    }

    private void listen() {
        SecureRandom random = new SecureRandom();
        long delay = 250;
        while (!Thread.currentThread().isInterrupted()) {
            try {
                byte[] bytes = new byte[16];
                random.nextBytes(bytes);
                CameraWire wire = new CameraWire(pairing.token, CameraWire.hex(bytes));
                HttpURLConnection connection = open("/camera/v1/events");
                connection.setRequestProperty(CameraWire.NONCE, wire.nonce);
                connection.setRequestProperty(CameraWire.PROOF, wire.attach());
                synchronized (this) {
                    if (closed) return;
                    stream = connection;
                }
                try {
                    run(connection, wire);
                } finally {
                    clear(connection);
                }
                Thread.sleep(delay);
                delay = Math.min(delay * 2, 4000);
            } catch (IOException | GeneralSecurityException ignored) {
                // Only fixed status crosses this boundary. Never log remote text.
                try {
                    Thread.sleep(delay);
                    delay = Math.min(delay * 2, 4000);
                } catch (InterruptedException stopped) {
                    return;
                }
            } catch (InterruptedException stopped) {
                return;
            }
        }
    }

    private void run(HttpURLConnection connection, CameraWire wire)
            throws IOException, GeneralSecurityException {
        if (connection.getResponseCode() != 200 || !"text/event-stream".equals(connection.getContentType())) return;
        try (BufferedReader input = new BufferedReader(new InputStreamReader(connection.getInputStream(),
                StandardCharsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT)))) {
            CameraWire.Stream stream = wire.new Stream();
            stream.next(input);
            for (;;) {
                CameraWire.Frame frame = stream.next(input);
                if ("cancel".equals(frame.event)) {
                    cancel(frame.id);
                } else {
                    capture(wire, frame);
                }
            }
        }
    }

    private synchronized void capture(CameraWire wire, CameraWire.Frame frame) throws IOException {
        if (closed || stream == null || active != null) throw new IOException();
        Active request = new Active(frame.id, stream);
        active = request;
        request.capture = camera.start(frame.camera, frame.remaining, result -> completed(wire, request, result));
    }

    private synchronized void completed(CameraWire wire, Active request, StillCapture.Result result) {
        if (closed || active != request || request.finished) return;
        request.finished = true;
        active = null;
        reporting = request;
        results.execute(() -> send(wire, request, result));
    }

    private void send(CameraWire wire, Active request, StillCapture.Result result) {
        HttpURLConnection connection = null;
        try {
            boolean jpeg = result.failure == null;
            byte[] bytes = jpeg ? result.jpeg
                    : ("{\"error\":\"" + result.failure.wire + "\"}").getBytes(StandardCharsets.UTF_8);
            String content = jpeg ? "image/jpeg" : "application/json";
            String width = jpeg ? Integer.toString(result.width) : "";
            String height = jpeg ? Integer.toString(result.height) : "";
            String sampled = jpeg ? Long.toString(result.sampledAt) : "";
            connection = open("/camera/v1/results/" + request.id);
            connection.setRequestMethod("POST");
            connection.setDoOutput(true);
            connection.setFixedLengthStreamingMode(bytes.length);
            connection.setRequestProperty("Content-Type", content);
            connection.setRequestProperty(CameraWire.NONCE, wire.nonce);
            connection.setRequestProperty(CameraWire.PROOF, wire.result(request.id, content, width, height, sampled, bytes));
            if (jpeg) {
                connection.setRequestProperty("X-Echo-Camera-Width", width);
                connection.setRequestProperty("X-Echo-Camera-Height", height);
                connection.setRequestProperty("X-Echo-Camera-Sampled-At-Ms", sampled);
            }
            synchronized (this) {
                if (closed || stream != request.owner || request.cancelled) return;
                upload = connection;
            }
            try (OutputStream output = connection.getOutputStream()) {
                output.write(bytes);
            }
            connection.getResponseCode();
        } catch (IOException | GeneralSecurityException ignored) {
            // A result is sent once. The owner handles disconnect and expiry.
        } finally {
            finishUpload(request, connection);
        }
    }

    private HttpURLConnection open(String path) throws IOException {
        HttpURLConnection connection = (HttpURLConnection) new URL(pairing.origin + path).openConnection(Proxy.NO_PROXY);
        connection.setInstanceFollowRedirects(false);
        connection.setConnectTimeout(1500);
        connection.setReadTimeout(12000);
        connection.setUseCaches(false);
        return connection;
    }

    private synchronized void finishUpload(Active request, HttpURLConnection connection) {
        if (reporting == request) reporting = null;
        if (upload == connection) upload = null;
        if (connection != null) connection.disconnect();
    }

    private synchronized void cancel(String id) {
        if (reporting != null && (id == null || id.equals(reporting.id))) {
            reporting.cancelled = true;
            reporting = null;
            if (upload != null) upload.disconnect();
        }
        if (active != null && (id == null || id.equals(active.id))) {
            Active request = active;
            active = null;
            request.cancelled = true;
            if (request.capture != null) request.capture.close();
        }
    }

    private synchronized void clear(HttpURLConnection connection) {
        if (stream == connection) {
            stream = null;
            cancel(null);
        }
        connection.disconnect();
    }

    @Override public synchronized void close() {
        closed = true;
        cancel(null);
        if (stream != null) clear(stream);
        results.shutdownNow();
        listener.interrupt();
    }

    private static final class Active {
        final String id;
        final HttpURLConnection owner;
        StillCapture.Cancel capture;
        boolean finished;
        boolean cancelled;

        Active(String id, HttpURLConnection owner) {
            this.id = id;
            this.owner = owner;
        }
    }
}
